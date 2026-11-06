import { useEffect, useLayoutEffect, useRef, useState } from "react";
import type { CSSProperties, ReactNode, RefObject } from "react";
import type { ByteRange, Session } from "./api.generated";
import { fileTime, size } from "./api";
import { kindLabel, terms } from "./labels";
import type { Row } from "./useFeed";

export function Icon({
  name,
  size: pixels = 16,
}: {
  name: string;
  size?: number;
}) {
  const paths: Record<string, React.ReactNode> = {
    folder: (
      <>
        <path d="M3 7h6l2 2h10v10a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7Z" />
        <path d="M3 7V5a2 2 0 0 1 2-2h5l2 2h7a2 2 0 0 1 2 2v2" />
      </>
    ),
    plus: <path d="M12 5v14M5 12h14" />,
    search: (
      <>
        <circle cx="10.5" cy="10.5" r="6.5" />
        <path d="m16 16 4 4" />
      </>
    ),
    refresh: (
      <>
        <path d="M20 7v5h-5M4 17v-5h5" />
        <path d="M6.1 7a7 7 0 0 1 11.5-1L20 9M4 15l2.4 3A7 7 0 0 0 18 17" />
      </>
    ),
    pause: (
      <>
        <path d="M8 5v14M16 5v14" />
      </>
    ),
    play: <path d="m8 5 12 7-12 7V5Z" />,
    down: <path d="m5 9 7 7 7-7" />,
    up: <path d="m5 15 7-7 7 7" />,
    left: <path d="m15 5-7 7 7 7" />,
    copy: (
      <>
        <rect x="8" y="8" width="12" height="12" rx="2" />
        <path d="M16 8V4H4v12h4" />
      </>
    ),
    download: (
      <>
        <path d="M12 3v12m-5-5 5 5 5-5M4 16v5h16v-5" />
      </>
    ),
    file: (
      <>
        <path d="M5 3h9l5 5v13H5V3Z" />
        <path d="M14 3v6h5M8 14h8M8 17h5" />
      </>
    ),
    arrow: <path d="m8 5 7 7-7 7" />,
    close: <path d="m6 6 12 12M18 6 6 18" />,
    check: <path d="m5 12 4 4L19 6" />,
    sun: (
      <>
        <circle cx="12" cy="12" r="4" />
        <path d="M12 2v2m0 16v2M2 12h2m16 0h2M5 5l1.5 1.5m11 11L19 19M5 19l1.5-1.5m11-11L19 5" />
      </>
    ),
    moon: <path d="M20.8 13A8.9 8.9 0 0 1 11 3.2 9 9 0 1 0 20.8 13Z" />,
    warning: (
      <>
        <path d="m12 3 10 18H2L12 3Z" />
        <path d="M12 9v5m0 3v1" />
      </>
    ),
    packet: (
      <>
        <path d="m12 3 9 5-9 5-9-5 9-5Zm-9 9 9 5 9-5M3 16l9 5 9-5" />
      </>
    ),
    popup: (
      <>
        <path d="M14 3h7v7m0-7-11 11" />
        <path d="M10 3H3v18h18v-7" />
      </>
    ),
    compare: (
      <>
        <path d="M9 4H3v16h6M15 4h6v16h-6M12 2v20M6 9h1m10 6h1" />
      </>
    ),
    filter: <path d="M3 5h18l-7 8v7l-4-2v-5L3 5Z" />,
    exclude: (
      <>
        <circle cx="12" cy="12" r="8" />
        <path d="m6.5 6.5 11 11" />
      </>
    ),
    record: <circle cx="12" cy="12" r="6" fill="currentColor" stroke="none" />,
    stop: (
      <rect
        x="5"
        y="5"
        width="14"
        height="14"
        rx="2"
        fill="currentColor"
        stroke="none"
      />
    ),
    eye: (
      <>
        <path d="M2 12s3.6-7 10-7 10 7 10 7-3.6 7-10 7S2 12 2 12Z" />
        <circle cx="12" cy="12" r="3" />
      </>
    ),
    "eye-off": (
      <>
        <path d="M3 3l18 18M10.6 5.1A10 10 0 0 1 12 5c6.4 0 10 7 10 7a17 17 0 0 1-3.1 3.9M6.6 6.6A17 17 0 0 0 2 12s3.6 7 10 7a9.6 9.6 0 0 0 5.4-1.6" />
        <path d="M9.9 9.9a3 3 0 0 0 4.2 4.2" />
      </>
    ),
    target: (
      <>
        <circle cx="12" cy="12" r="7" />
        <circle cx="12" cy="12" r="2.5" />
        <path d="M12 2v3m0 14v3M2 12h3m14 0h3" />
      </>
    ),
    user: (
      <>
        <circle cx="12" cy="8" r="4" />
        <path d="M4 21a8 8 0 0 1 16 0" />
      </>
    ),
    merge: (
      <>
        <path d="M6 3v6a6 6 0 0 0 6 6h6" />
        <path d="M6 21v-6M15 12l3 3-3 3" />
      </>
    ),
    hex: (
      <>
        <path d="M4 7h16M4 12h16M4 17h10" />
      </>
    ),
    path: (
      <>
        <circle cx="5" cy="6" r="2" />
        <circle cx="12" cy="18" r="2" />
        <circle cx="19" cy="6" r="2" />
        <path d="M5 8v2a4 4 0 0 0 4 4h1m9-6v2a4 4 0 0 1-4 4h-1" />
      </>
    ),
    more: (
      <>
        <circle cx="5" cy="12" r="1.3" fill="currentColor" />
        <circle cx="12" cy="12" r="1.3" fill="currentColor" />
        <circle cx="19" cy="12" r="1.3" fill="currentColor" />
      </>
    ),
    unfold: <path d="m7 15 5 5 5-5M7 9l5-5 5 5" />,
    fold: <path d="m7 20 5-5 5 5M7 4l5 5 5-5" />,
    spark: (
      <path d="M12 3.5 13.9 9l5.6 1.9-5.6 1.9L12 18.5l-1.9-5.7-5.6-1.9L10.1 9 12 3.5ZM18.5 16v4M16.5 18h4" />
    ),
    range: <path d="M8 4H5v16h3M16 4h3v16h-3" />,
    settings: (
      <>
        <path d="m10 3-.7 2.2-2 .9-2-.6-2 3.4 1.5 1.7v2.3L3.3 15l2 3.4 2-.6 2 .9.7 2.3h4l.7-2.3 2-.9 2 .6 2-3.4-1.5-1.8v-2.3l1.5-1.7-2-3.4-2 .6-2-.9L14 3h-4Z" />
        <circle cx="12" cy="12" r="3" />
      </>
    ),
  };
  return (
    <svg
      width={pixels}
      height={pixels}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.65"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      {paths[name] || paths.packet}
    </svg>
  );
}

export function Direction({ value }: { value: string }) {
  return (
    <span
      className={`direction ${value}`}
      title={
        value === "inbound"
          ? "QQ 收到的包"
          : value === "outbound"
            ? "QQ 发出的包"
            : undefined
      }
    >
      {value === "inbound" ? "↓ 收" : value === "outbound" ? "↑ 发" : "·"}
    </span>
  );
}

// A list row leads with the same 收/发 label as the detail header, or the event kind.
function RowMark({ event }: { event: Row["event"] }) {
  if (event.kind !== "packet")
    return (
      <span className="control-tag" title={terms.controls}>
        {kindLabel(event.kind)}
      </span>
    );
  return <Direction value={event.direction} />;
}

export function useWindow(
  ref: RefObject<HTMLDivElement | null>,
  rowHeight: number,
  count: number,
) {
  const [viewport, setViewport] = useState({ top: 0, height: 500 });
  useEffect(() => {
    const element = ref.current;
    if (!element) return;
    const update = () =>
      setViewport({ top: element.scrollTop, height: element.clientHeight });
    const observer = new ResizeObserver(update);
    observer.observe(element);
    element.addEventListener("scroll", update, { passive: true });
    update();
    return () => {
      observer.disconnect();
      element.removeEventListener("scroll", update);
    };
  }, [ref]);
  const start = Math.max(
    0,
    Math.min(count, Math.floor(viewport.top / rowHeight) - 8),
  );
  const end = Math.min(
    count,
    Math.ceil((viewport.top + viewport.height) / rowHeight) + 8,
  );
  return { start, end };
}

export const ROW_HEIGHT = 52;
export function domId(key: string): string {
  return `packet-${key.replace(":", "-")}`;
}
type Display =
  | { type: "packet"; row: Row }
  | { type: "boundary"; key: string; segment: Session; launch: number };
function display(rows: Row[], segments: Session[]): Display[] {
  if (segments.length < 2) return rows.map((row) => ({ type: "packet", row }));
  const result: Display[] = [];
  let previous = "";
  for (const row of rows) {
    if (row.sessionId !== previous) {
      const launch = segments.findIndex((s) => s.id === row.sessionId);
      if (launch >= 0)
        result.push({
          type: "boundary",
          key: `boundary:${row.sessionId}`,
          segment: segments[launch],
          launch: launch + 1,
        });
      previous = row.sessionId;
    }
    result.push({ type: "packet", row });
  }
  return result;
}

export type ListHandlers = {
  onSelect: (row: Row, via: "pointer" | "keyboard") => void;
  onToggle: (row: Row) => void;
  onRange: (rows: Row[]) => void;
  onContext: (row: Row, x: number, y: number) => void;
  onActivate: (row: Row) => void;
  onPopup: (row: Row) => void;
  onCopy: (row: Row) => void;
  onFollow: (follow: boolean) => void;
};

export function PacketTable({
  rows,
  segments,
  picking = false,
  selectedKey,
  contextKey,
  checked,
  follow,
  trimmed,
  jump,
  locate,
  loading,
  empty,
  listRef,
  handlers,
}: {
  rows: Row[];
  segments: Session[];
  picking?: boolean;
  selectedKey?: string;
  contextKey?: string;
  checked: Set<string>;
  follow: boolean;
  trimmed: number;
  jump: number;
  locate: { key: string; n: number } | null;
  loading: boolean;
  empty: ReactNode;
  listRef: RefObject<HTMLDivElement | null>;
  handlers: ListHandlers;
}) {
  const ref = listRef;
  const items = display(rows, segments);
  const { start, end } = useWindow(ref, ROW_HEIGHT, items.length);
  const anchor = useRef<string | undefined>(undefined);
  // The keyboard cursor follows clicks, arrows and right-clicks, so focus returns to the row a menu came from.
  const [cursor, setCursor] = useState<string | undefined>(undefined);
  const expected = useRef<number | null>(null);
  const lastTrimmed = useRef(trimmed);
  // Where the user left the list. A reload empties it for a moment, and on a slow machine the
  // browser lays that out and clamps scrollTop to 0; that is not a place the user chose.
  const resting = useRef(0);
  const wasEmpty = useRef(items.length === 0);
  // Only the user's own wheel, touch, keys or scrollbar drag may stop or resume following; a click
  // on a row is not a scroll, and a reload can follow it within a moment.
  const gesture = useRef(0);
  const pressed = useRef(false);
  useEffect(() => {
    const release = () => {
      pressed.current = false;
    };
    window.addEventListener("pointerup", release);
    window.addEventListener("pointercancel", release);
    return () => {
      window.removeEventListener("pointerup", release);
      window.removeEventListener("pointercancel", release);
    };
  }, []);
  const touched = (event: { timeStamp: number }) => {
    gesture.current = event.timeStamp;
  };
  const last = rows.at(-1)?.key;
  const selecting = picking || checked.size > 0;
  function scrollTo(top: number) {
    const element = ref.current;
    if (!element) return;
    const target = Math.max(
      0,
      Math.min(top, element.scrollHeight - element.clientHeight),
    );
    resting.current = target;
    if (Math.abs(element.scrollTop - target) < 1) return;
    expected.current = target;
    element.scrollTop = target;
  }
  function reveal(key: string, center = false) {
    const element = ref.current;
    const index = items.findIndex(
      (item) => item.type === "packet" && item.row.key === key,
    );
    if (!element || index < 0) return;
    const top = index * ROW_HEIGHT;
    if (center) scrollTo(top - element.clientHeight / 2 + ROW_HEIGHT / 2);
    else if (top < element.scrollTop) scrollTo(top);
    else if (top + ROW_HEIGHT > element.scrollTop + element.clientHeight)
      scrollTo(top + ROW_HEIGHT - element.clientHeight);
  }
  useLayoutEffect(() => {
    if (follow && ref.current) scrollTo(ref.current.scrollHeight);
  }, [last, follow, items.length]);
  useLayoutEffect(() => {
    // Rows came back after a reload: show the chosen packet if it is still there, otherwise
    // return to where the user was. Following is handled above.
    const empty = items.length === 0;
    const refilled = wasEmpty.current && !empty;
    wasEmpty.current = empty;
    if (!refilled || follow) return;
    const chosen =
      selectedKey &&
      items.some(
        (item) => item.type === "packet" && item.row.key === selectedKey,
      );
    if (chosen) reveal(selectedKey, true);
    else scrollTo(resting.current);
  }, [items.length === 0]);
  useLayoutEffect(() => {
    if (jump && ref.current) scrollTo(ref.current.scrollHeight);
  }, [jump]);
  useLayoutEffect(() => {
    if (locate) reveal(locate.key, true);
  }, [locate?.n]);
  useLayoutEffect(() => {
    // Rows removed from the head would otherwise shift what the user is reading.
    const removed = trimmed - lastTrimmed.current;
    lastTrimmed.current = trimmed;
    if (removed > 0 && !follow && ref.current)
      scrollTo(ref.current.scrollTop - removed * ROW_HEIGHT);
  }, [trimmed]);
  function onScroll(event: React.UIEvent) {
    const element = ref.current!;
    const programmatic =
      expected.current !== null &&
      Math.abs(element.scrollTop - expected.current) < 2;
    expected.current = null;
    const user = pressed.current || event.timeStamp - gesture.current < 600;
    if (programmatic || !user) return;
    resting.current = element.scrollTop;
    const bottom =
      element.scrollHeight - element.scrollTop - element.clientHeight <
      ROW_HEIGHT / 2;
    if (bottom !== follow) handlers.onFollow(bottom);
  }
  const packets = items.flatMap((item) =>
    item.type === "packet" ? [item.row] : [],
  );
  const present = (key?: string) =>
    key && packets.some((r) => r.key === key) ? key : undefined;
  const activeKey =
    present(contextKey) || present(cursor) || present(selectedKey);
  function openMenu(row: Row) {
    reveal(row.key);
    requestAnimationFrame(() => {
      const box =
        document.getElementById(domId(row.key))?.getBoundingClientRect() ||
        ref.current!.getBoundingClientRect();
      handlers.onContext(
        row,
        box.left + 48,
        box.top + Math.min(box.height, ROW_HEIGHT) - 6,
      );
    });
  }
  function onKeyDown(event: React.KeyboardEvent) {
    if (!packets.length) return;
    const index = packets.findIndex((r) => r.key === activeKey);
    const current = packets[index];
    if (
      (event.shiftKey && event.key === "F10") ||
      event.key === "ContextMenu"
    ) {
      event.preventDefault();
      if (current) openMenu(current);
      return;
    }
    const page = Math.max(
      1,
      Math.floor((ref.current?.clientHeight || 400) / ROW_HEIGHT) - 1,
    );
    const steps: Record<string, number> = {
      ArrowDown: 1,
      ArrowUp: -1,
      PageDown: page,
      PageUp: -page,
    };
    let next: number | null = null;
    if (event.key in steps)
      next =
        index < 0
          ? steps[event.key] > 0
            ? 0
            : packets.length - 1
          : index + steps[event.key];
    else if (event.key === "Home") next = 0;
    else if (event.key === "End") next = packets.length - 1;
    if (next !== null) {
      event.preventDefault();
      const row = packets[Math.max(0, Math.min(packets.length - 1, next))];
      handlers.onSelect(row, "keyboard");
      setCursor(row.key);
      reveal(row.key);
      anchor.current = row.key;
      return;
    }
    if (!current) return;
    if (event.key === "Enter") {
      event.preventDefault();
      if (event.shiftKey) handlers.onPopup(current);
      else handlers.onActivate(current);
    } else if (event.key === " ") {
      event.preventDefault();
      handlers.onToggle(current);
    } else if (
      (event.ctrlKey || event.metaKey) &&
      event.key.toLowerCase() === "c" &&
      !window.getSelection()?.toString()
    ) {
      event.preventDefault();
      handlers.onCopy(current);
    }
  }
  function onClick(event: React.MouseEvent, row: Row) {
    if (event.shiftKey && anchor.current) {
      const from = packets.findIndex((r) => r.key === anchor.current),
        to = packets.findIndex((r) => r.key === row.key);
      if (from >= 0 && to >= 0) {
        event.preventDefault();
        window.getSelection()?.removeAllRanges();
        handlers.onRange(
          packets.slice(Math.min(from, to), Math.max(from, to) + 1),
        );
        return;
      }
    }
    anchor.current = row.key;
    setCursor(row.key);
    // In compare mode a click ticks the row and previews it without leaving the list.
    if (picking) {
      handlers.onToggle(row);
      handlers.onSelect(row, "keyboard");
    } else if (event.ctrlKey || event.metaKey) handlers.onToggle(row);
    else handlers.onSelect(row, "pointer");
  }

  return (
    <div
      className={`packet-table ${selecting ? "selecting" : ""}`}
      style={{ "--packet-row-height": `${ROW_HEIGHT}px` } as CSSProperties}
    >
      <div
        ref={ref}
        className="packet-scroll"
        role="listbox"
        aria-label="数据包列表"
        aria-multiselectable={selecting || undefined}
        tabIndex={0}
        aria-activedescendant={activeKey ? domId(activeKey) : undefined}
        onScroll={onScroll}
        onWheel={touched}
        onTouchMove={touched}
        onPointerDown={(event) => {
          // The scrollbar belongs to the list element itself, unlike its rows.
          if (event.target === event.currentTarget) pressed.current = true;
        }}
        onKeyDown={(event) => {
          touched(event);
          onKeyDown(event);
        }}
      >
        {items.length === 0 ? (
          <div className="empty-table">
            {loading ? (
              <>
                <span className="small-spinner" aria-hidden="true" />
                <strong>正在读取采集记录</strong>
                <span>正在建立索引…</span>
              </>
            ) : (
              empty
            )}
          </div>
        ) : (
          <div
            style={{ height: items.length * ROW_HEIGHT, position: "relative" }}
          >
            <div
              style={{
                position: "absolute",
                top: start * ROW_HEIGHT,
                left: 0,
                right: 0,
              }}
            >
              {items.slice(start, end).map((item) => {
                if (item.type === "boundary") {
                  const s = item.segment;
                  return (
                    <div
                      key={item.key}
                      className="packet-boundary"
                      role="presentation"
                      title={s.path}
                    >
                      <Icon name="merge" size={14} />
                      <span className="boundary-text">
                        <strong>第 {item.launch} 次启动</strong>
                        <span className="mono">
                          PID {s.pid} · {fileTime(s.started_filetime, true)}
                        </span>
                      </span>
                    </div>
                  );
                }
                const { row } = item;
                const e = row.event;
                const packet = e.kind === "packet";
                const isChecked = checked.has(row.key);
                return (
                  <div
                    key={row.key}
                    id={domId(row.key)}
                    role="option"
                    aria-selected={selectedKey === row.key}
                    aria-checked={selecting ? isChecked : undefined}
                    aria-label={`选择包 ${e.event_id}: ${e.command}`}
                    className={`packet-row ${selectedKey === row.key ? "selected" : ""} ${isChecked ? "checked" : ""} ${contextKey === row.key ? "context-target" : ""} ${packet ? "" : "control"}`}
                    onContextMenu={(event) => {
                      event.preventDefault();
                      setCursor(row.key);
                      handlers.onContext(row, event.clientX, event.clientY);
                    }}
                    onClick={(event) => onClick(event, row)}
                  >
                    {selecting && (
                      <span className="row-check">
                        {packet && (
                          <input
                            type="checkbox"
                            tabIndex={-1}
                            aria-label={`对比选择 ${e.event_id}`}
                            title={terms.compare}
                            checked={isChecked}
                            onClick={(event) => event.stopPropagation()}
                            onChange={() => handlers.onToggle(row)}
                          />
                        )}
                      </span>
                    )}
                    <RowMark event={e} />
                    <span className="row-body">
                      <span className="row-line">
                        <span className="mono command" title={e.command}>
                          {e.command}
                        </span>
                        {e.truncated && (
                          <span
                            className="row-tag"
                            title={`${terms.truncated}原始长度 ${e.original_length} B`}
                          >
                            截断
                          </span>
                        )}
                        {packet && e.error_code !== 0 && (
                          <span
                            className="row-tag"
                            title="QQ 在这条记录上报告的错误码"
                          >
                            错误 {e.error_code}
                          </span>
                        )}
                        {e.kind === "loss" && (
                          <span className="row-tag" title={terms.dropped}>
                            累计丢弃 {e.dropped_total}
                          </span>
                        )}
                        {packet && (
                          <span className="mono row-size">
                            {size(e.captured_length)}
                          </span>
                        )}
                      </span>
                      <span className="row-line row-meta mono">
                        <span className="row-time">
                          {fileTime(e.timestamp_filetime)}
                        </span>
                        {packet && e.sequence !== "0" && (
                          <span className="row-seq" title={terms.seq}>
                            SEQ {e.sequence}
                          </span>
                        )}
                        <span className="row-id">#{e.event_id}</span>
                      </span>
                    </span>
                  </div>
                );
              })}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

export function Splitter({
  ratio,
  container,
  onChange,
}: {
  ratio: number;
  container: RefObject<HTMLElement | null>;
  onChange: (ratio: number, commit: boolean) => void;
}) {
  const dragging = useRef(false);
  function bounds() {
    const width = container.current?.getBoundingClientRect().width || 1600;
    const min = Math.min(0.5, 300 / width);
    return { min, max: Math.max(min, 1 - 520 / width) };
  }
  const clamp = (value: number) => {
    const { min, max } = bounds();
    return Math.min(max, Math.max(min, value));
  };
  function fromPointer(event: React.PointerEvent) {
    const box = container.current!.getBoundingClientRect();
    return clamp((event.clientX - box.left) / box.width);
  }
  function onKeyDown(event: React.KeyboardEvent) {
    const { min, max } = bounds();
    const next =
      event.key === "ArrowLeft"
        ? ratio - 0.02
        : event.key === "ArrowRight"
          ? ratio + 0.02
          : event.key === "Home"
            ? min
            : event.key === "End"
              ? max
              : event.key === "Enter"
                ? 0.3
                : null;
    if (next === null) return;
    event.preventDefault();
    onChange(clamp(next), true);
  }
  return (
    <div
      className="pane-splitter"
      role="separator"
      aria-orientation="vertical"
      aria-label="调整列表与详情宽度"
      tabIndex={0}
      aria-valuemin={Math.round(bounds().min * 100)}
      aria-valuemax={Math.round(bounds().max * 100)}
      aria-valuenow={Math.round(ratio * 100)}
      title="拖动调整宽度，双击恢复默认"
      onPointerDown={(event) => {
        dragging.current = true;
        event.currentTarget.setPointerCapture(event.pointerId);
      }}
      onPointerMove={(event) => {
        if (dragging.current) onChange(fromPointer(event), false);
      }}
      onPointerUp={(event) => {
        if (!dragging.current) return;
        dragging.current = false;
        onChange(fromPointer(event), true);
      }}
      onPointerCancel={() => {
        dragging.current = false;
      }}
      onDoubleClick={() => onChange(clamp(0.3), true)}
      onKeyDown={onKeyDown}
    />
  );
}

export function Highlight({ text, query }: { text: string; query: string }) {
  if (!query) return <>{text}</>;
  const index = text.toLocaleLowerCase().indexOf(query.toLocaleLowerCase());
  if (index < 0) return <>{text}</>;
  return (
    <>
      {text.slice(0, index)}
      <mark>{text.slice(index, index + query.length)}</mark>
      <Highlight text={text.slice(index + query.length)} query={query} />
    </>
  );
}

export function HexView({
  hex,
  selection,
  onByte,
  scrollRef,
}: {
  hex: string;
  selection: ByteRange | null;
  onByte: (offset: number) => void;
  scrollRef: RefObject<HTMLDivElement | null>;
}) {
  const bytes = hex.length / 2;
  const rows = Math.ceil(bytes / 16);
  const { start, end } = useWindow(scrollRef, 24, rows);
  // Small payloads give their space back to the field tree.
  return (
    <div
      className="hex-view"
      style={{ "--hex-rows": Math.max(2, rows) } as CSSProperties}
    >
      <div
        className="hex-scroll"
        ref={scrollRef}
        aria-label="原始字节"
        tabIndex={0}
      >
        <div className="hex-heading mono">
          <span>OFFSET</span>
          <span className="hex-byte-grid">
            {Array.from({ length: 16 }, (_, i) => (
              <span key={i}>
                {i.toString(16).padStart(2, "0").toUpperCase()}
              </span>
            ))}
          </span>
          <span>ASCII</span>
        </div>
        {bytes === 0 ? (
          <div className="no-bytes">此记录没有包体</div>
        ) : (
          <div className="hex-content" style={{ height: rows * 24 }}>
            <div
              style={{
                position: "absolute",
                top: start * 24,
                left: 0,
                right: 0,
              }}
            >
              {Array.from(
                { length: Math.max(0, end - start) },
                (_, i) => start + i,
              ).map((row) => (
                <div className="hex-line mono" key={row}>
                  <span className="hex-offset">
                    {(row * 16).toString(16).padStart(8, "0")}
                  </span>
                  <span className="hex-byte-grid">
                    {Array.from({ length: 16 }, (_, column) => {
                      const offset = row * 16 + column;
                      const selected =
                        selection &&
                        offset >= selection.start &&
                        offset < selection.end;
                      return offset < bytes ? (
                        <button
                          key={column}
                          tabIndex={-1}
                          aria-label={`字节 ${offset}`}
                          data-offset={offset}
                          className={`hex-byte ${selected ? "highlighted" : ""}`}
                          onClick={() => onByte(offset)}
                        >
                          {hex.slice(offset * 2, offset * 2 + 2)}
                        </button>
                      ) : (
                        <span key={column} />
                      );
                    })}
                  </span>
                  <span className="hex-ascii">
                    {Array.from(
                      { length: Math.min(16, bytes - row * 16) },
                      (_, column) => {
                        const offset = row * 16 + column;
                        const byte = parseInt(
                          hex.slice(offset * 2, offset * 2 + 2),
                          16,
                        );
                        return (
                          <span
                            key={column}
                            className={
                              selection &&
                              offset >= selection.start &&
                              offset < selection.end
                                ? "highlighted"
                                : ""
                            }
                          >
                            {byte >= 32 && byte <= 126
                              ? String.fromCharCode(byte)
                              : "·"}
                          </span>
                        );
                      },
                    )}
                  </span>
                </div>
              ))}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
