import { useEffect, useRef, useState } from "react";
import type {
  EventPage,
  EventSummary,
  MergedPage,
  Session,
} from "./api.generated";
import { abortError, message, request } from "./api";

export const MAX_ROWS = 5000;
export type Row = { key: string; sessionId: string; event: EventSummary };
export type Source =
  | { kind: "session"; id: string }
  | { kind: "account"; account: string; anchor: string };
type Filter = {
  q: string;
  direction: string;
  controls: boolean;
  advanced?: Record<string, string>;
};

export function rowKey(sessionId: string, eventId: string): string {
  return `${sessionId}:${eventId}`;
}
function params(filter: Filter): URLSearchParams {
  return new URLSearchParams({
    ...filter.advanced,
    q: filter.q,
    direction: filter.direction,
    controls: String(filter.controls),
    limit: "500",
  });
}
function merge(a: Row[], b: Row[]): Row[] {
  const seen = new Set(a.map((r) => r.key));
  return [...a, ...b.filter((r) => !seen.has(r.key))];
}
function sourceKey(source: Source | null): string {
  return !source
    ? ""
    : source.kind === "session"
      ? `session:${source.id}`
      : `account:${source.account}:${source.anchor}`;
}

type Page = {
  rows: Row[];
  cursor: string;
  hasEarlier: boolean;
  hasMore: boolean;
  indexing: boolean;
  session: Session;
  segments: Session[];
};
async function fetchPage(
  source: Source,
  query: URLSearchParams,
  signal?: AbortSignal,
): Promise<Page> {
  if (source.kind === "session") {
    const page = await request<EventPage>(
      `/sessions/${source.id}/events?${query}`,
      { signal },
    );
    return {
      rows: page.events.map((event) => ({
        key: rowKey(source.id, event.event_id),
        sessionId: source.id,
        event,
      })),
      cursor: page.cursor,
      hasEarlier: page.has_earlier,
      hasMore: page.has_more || page.session.has_more,
      indexing: false,
      session: page.session,
      segments: [page.session],
    };
  }
  const page = await request<MergedPage>(
    `/accounts/${encodeURIComponent(source.account)}/events?${query}`,
    { signal },
  );
  const last = page.segments.at(-1)!;
  return {
    rows: page.events.map((item) => ({
      key: rowKey(item.session_id, item.event.event_id),
      sessionId: item.session_id,
      event: item.event,
    })),
    cursor: page.cursor,
    hasEarlier: page.has_earlier,
    hasMore: page.has_more || page.indexing || last.has_more,
    indexing: page.indexing,
    session: last,
    segments: page.segments,
  };
}

// While frozen, polling continues so the index advances and arrivals can be
// counted; new rows wait in a bounded buffer and join the list on resume.
export function useFeed(
  source: Source | null,
  filter: Filter,
  frozen: boolean,
) {
  const [rows, setRows] = useState<Row[]>([]);
  const [session, setSession] = useState<Session | null>(null);
  const [segments, setSegments] = useState<Session[]>([]);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(false);
  const [indexing, setIndexing] = useState(false);
  const [hasEarlier, setHasEarlier] = useState(false);
  const [olderLoading, setOlderLoading] = useState(false);
  const [pending, setPending] = useState(0);
  const [appended, setAppended] = useState(0);
  const [trimmed, setTrimmed] = useState(0);
  const [reload, setReload] = useState(0);
  const key = sourceKey(source);
  const extraKey = JSON.stringify(filter.advanced || {});
  const frozenRef = useRef(frozen);
  const rowsRef = useRef(rows);
  const buffer = useRef<Row[]>([]);
  const overflow = useRef(false);
  const generation = useRef(0);
  useEffect(() => {
    frozenRef.current = frozen;
  }, [frozen]);

  function show(next: Row[]) {
    rowsRef.current = next;
    setRows(next);
  }
  function append(incoming: Row[]) {
    if (!incoming.length) return;
    const previous = rowsRef.current;
    const combined = merge(previous, incoming);
    const removed = Math.max(0, combined.length - MAX_ROWS);
    if (removed) {
      setHasEarlier(true);
      setTrimmed((n) => n + removed);
    }
    if (combined.length > previous.length)
      setAppended((n) => n + combined.length - previous.length);
    show(removed ? combined.slice(removed) : combined);
  }

  useEffect(() => {
    const current = ++generation.current;
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout>;
    let cursor: string | undefined;
    let initialized = false;
    buffer.current = [];
    overflow.current = false;
    show([]);
    setSession(null);
    setSegments([]);
    setError("");
    setHasEarlier(false);
    setPending(0);
    setIndexing(false);
    setLoading(Boolean(source));
    setOlderLoading(false);
    async function poll() {
      if (!source || controller.signal.aborted) return;
      let delay = 750;
      try {
        const query = params(filter);
        if (cursor !== undefined)
          query.set(
            source.kind === "session" ? "after" : "after_cursor",
            cursor,
          );
        const page = await fetchPage(source, query, controller.signal);
        if (generation.current !== current) return;
        setSession(page.session);
        setSegments(page.segments);
        setError("");
        setIndexing(page.indexing);
        if (page.indexing) delay = 30;
        else {
          if (!initialized) {
            show(page.rows);
            setHasEarlier(page.hasEarlier);
            setAppended(0);
            setTrimmed(0);
            setLoading(false);
            initialized = true;
          } else if (frozenRef.current) {
            const seen = new Set(buffer.current.map((r) => r.key));
            buffer.current.push(...page.rows.filter((r) => !seen.has(r.key)));
            if (buffer.current.length > MAX_ROWS) {
              buffer.current = buffer.current.slice(-MAX_ROWS);
              overflow.current = true;
            }
            setPending(buffer.current.length);
          } else append(page.rows);
          cursor = page.cursor;
          if (page.hasMore) delay = 30;
        }
      } catch (error) {
        if (!abortError(error) && generation.current === current) {
          setError(message(error));
          setLoading(false);
        }
        delay = 2000;
      }
      if (!controller.signal.aborted) timer = setTimeout(poll, delay);
    }
    void poll();
    return () => {
      controller.abort();
      clearTimeout(timer);
    };
    // The source and filter are described by their keys.
  }, [key, filter.q, filter.direction, filter.controls, extraKey, reload]);

  useEffect(() => {
    if (frozen || (!buffer.current.length && !overflow.current)) return;
    if (overflow.current) {
      overflow.current = false;
      buffer.current = [];
      setPending(0);
      setReload((n) => n + 1);
      return;
    }
    const waiting = buffer.current;
    buffer.current = [];
    setPending(0);
    append(waiting);
  }, [frozen]);

  async function earlier() {
    const first = rowsRef.current[0];
    if (!source || !first || olderLoading || rowsRef.current.length >= MAX_ROWS)
      return;
    const current = generation.current;
    setOlderLoading(true);
    try {
      const query = params(filter);
      if (source.kind === "session") query.set("before", first.event.event_id);
      else
        query.set(
          "before_cursor",
          `${first.sessionId}:${first.event.event_id}`,
        );
      const page = await fetchPage(source, query);
      if (generation.current !== current) return;
      show(merge(page.rows, rowsRef.current).slice(0, MAX_ROWS));
      setHasEarlier(page.hasEarlier);
      setSession(page.session);
      setSegments(page.segments);
      setError("");
    } catch (error) {
      if (generation.current === current) setError(message(error));
    } finally {
      if (generation.current === current) setOlderLoading(false);
    }
  }
  return {
    rows,
    session,
    segments,
    error,
    loading,
    indexing,
    hasEarlier,
    olderLoading,
    pending,
    appended,
    trimmed,
    earlier,
    latest: () => setReload((n) => n + 1),
  };
}
