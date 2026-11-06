import { useEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties } from "react";
import type {
  Catalog,
  PacketDetail,
  Session,
  UpdateStatus,
} from "./api.generated";
import {
  abortError,
  connectBackend,
  copyText,
  message,
  request,
  size,
} from "./api";
import { Detail } from "./Detail";
import { PosterBackdrop, useAppearance } from "./Appearance";
import { groupSessions, launchNumber, members, SessionBar } from "./SessionBar";
import { MAX_ROWS, rowKey, useFeed, type Row, type Source } from "./useFeed";
import { Icon, PacketTable, Splitter } from "./views";
import {
  AdvancedFilters,
  exact,
  filterChips,
  patterns,
  withPattern,
  type Advanced,
  type FilterState,
} from "./AdvancedFilters";
import {
  ComparePopup,
  PacketPopup,
  type PacketChoice,
} from "./WorkspaceDialogs";
import { RecordingControl } from "./RecordingControl";
import { SettingsPage } from "./SettingsPage";
import { Modal } from "./Modal";
import {
  ContextMenu,
  type MenuGroup,
  type MenuItem,
  type MenuRequest,
} from "./ContextMenu";
import { CommandFilter } from "./CommandFilter";
import {
  friendlyError,
  isLive,
  readingNote,
  readPreference,
  terms,
  writePreference,
} from "./labels";

const MAX_COMPARE = 8;
function count(value?: string): string {
  try {
    return new Intl.NumberFormat("en-US").format(BigInt(value || "0"));
  } catch {
    return "—";
  }
}
function total(
  sessions: Session[],
  field:
    | "packets"
    | "inbound"
    | "outbound"
    | "dropped_total"
    | "truncated_packets"
    | "file_bytes",
): string {
  try {
    return sessions
      .reduce((sum, s) => sum + BigInt(s[field] || "0"), 0n)
      .toString();
  } catch {
    return "0";
  }
}
function restoredSession(): string | null {
  const id = new URLSearchParams(location.search).get("session");
  return id && /^[a-f0-9]{24}$/.test(id) ? id : null;
}
function storedRatio(): number {
  const value = Number(readPreference("qscope.layout.listRatio"));
  return Number.isFinite(value) && value >= 0.15 && value <= 0.7 ? value : 0.3;
}
type Toast = { text: string; undo?: () => void };

export function App() {
  useEffect(connectBackend, []);
  const { theme, toggleTheme } = useAppearance();
  const [settingsOpen, setSettingsOpen] = useState(
    () => new URLSearchParams(location.search).get("page") === "settings",
  );
  const [catalog, setCatalog] = useState<Catalog | null>(null);
  const [catalogError, setCatalogError] = useState("");
  const [sessionId, setSessionId] = useState<string | null>(restoredSession);
  const [merged, setMerged] = useState(
    () => new URLSearchParams(location.search).get("merged") === "1",
  );
  const [opened, setOpened] = useState<string[]>(() =>
    sessionId ? [sessionId] : [],
  );
  const sessionRef = useRef(sessionId);
  const initialSelectionDone = useRef(Boolean(sessionId));
  const [refresh, setRefresh] = useState(0);
  const [uploading, setUploading] = useState(false);
  const [search, setSearch] = useState("");
  const [query, setQuery] = useState("");
  const [direction, setDirection] = useState("all");
  const [controls, setControls] = useState(false);
  const [advanced, setAdvanced] = useState<Advanced>({});
  const [filtersOpen, setFiltersOpen] = useState(false);
  const [follow, setFollowState] = useState(true);
  const [followBase, setFollowBase] = useState(0);
  const [frozen, setFrozen] = useState<false | "user" | "earlier">(false);
  const [compactView, setCompactView] = useState<"list" | "detail">("list");
  const [selected, setSelected] = useState<Row | null>(null);
  const [toast, setToast] = useState<Toast | null>(null);
  const [basket, setBasket] = useState<PacketChoice[]>([]);
  const [picking, setPicking] = useState(false);
  const [popup, setPopup] = useState<PacketChoice | null>(null);
  const [comparison, setComparison] = useState<PacketChoice[] | null>(null);
  const [menu, setMenu] = useState<MenuRequest | null>(null);
  const [contextKey, setContextKey] = useState<string | null>(null);
  const [jump, setJump] = useState(0);
  const [locate, setLocate] = useState<{ key: string; n: number } | null>(null);
  const [diagnostics, setDiagnostics] = useState(false);
  const [savedPath, setSavedPath] = useState<string | null>(null);
  const [reveal, setReveal] = useState(
    () => readPreference("qscope.account.reveal") === "true",
  );
  const [ratio, setRatio] = useState(storedRatio);
  const toastTimer = useRef<ReturnType<typeof setTimeout> | undefined>(
    undefined,
  );
  const input = useRef<HTMLInputElement>(null);
  const searchInput = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const inspector = useRef<HTMLDivElement>(null);
  const autoSelected = useRef(false);
  useEffect(() => {
    sessionRef.current = sessionId;
  }, [sessionId]);

  const knownSessions = catalog?.sessions || [];
  const anchor = knownSessions.find((s) => s.id === sessionId);
  const account = anchor?.account || null;
  const source: Source | null = sessionId
    ? merged && account
      ? { kind: "account", account, anchor: sessionId }
      : { kind: "session", id: sessionId }
    : null;
  const accountView = source?.kind === "account";
  const filter = useMemo(
    () => ({ q: query, direction, controls, advanced }),
    [query, direction, controls, advanced],
  );
  const feed = useFeed(source, filter, Boolean(frozen));
  const session = accountView
    ? anchor
    : feed.session?.id === sessionId
      ? feed.session
      : anchor;
  const segments = accountView ? feed.segments : session ? [session] : [];
  const liveSession = accountView
    ? segments.find(isLive)
    : session && isLive(session)
      ? session
      : undefined;
  const live = Boolean(liveSession);
  const visibleSessions = useMemo(() => {
    const updates = new Map(segments.map((s) => [s.id, s]));
    const base = knownSessions.map((s) => updates.get(s.id) || s);
    return session && !base.some((s) => s.id === session.id)
      ? [...base, session]
      : base;
  }, [knownSessions, segments, session]);
  const filterState: FilterState = {
    search: query,
    direction,
    controls,
    advanced,
  };
  const chips = filterChips(filterState);
  // Search text stays in the input; every other condition is a token beside it.
  const tokens = chips.filter((chip) => chip.id !== "search");
  // Direction and command rules have their own controls, so the panel counts only the rest.
  const conditions = chips.filter(
    (chip) =>
      !["search", "direction"].includes(chip.id) &&
      !/^(include|exclude):/.test(chip.id),
  ).length;
  const checked = useMemo(
    () => new Set(basket.map((b) => rowKey(b.sessionId, b.event.event_id))),
    [basket],
  );
  const unseen = Math.max(0, feed.appended - followBase);
  const outside = Boolean(
    selected &&
    !feed.loading &&
    !feed.indexing &&
    !feed.rows.some((r) => r.key === selected.key),
  );
  const group = session
    ? [...groupSessions(visibleSessions).values()].find((g) =>
        members(g).some((s) => s.id === session.id),
      )
    : undefined;

  function navigateSettings(open: boolean, section?: string) {
    setSettingsOpen(open);
    const url = new URL(location.href);
    if (open) {
      url.searchParams.set("page", "settings");
      if (section) url.searchParams.set("section", section);
      history.pushState(null, "", url);
    } else {
      url.searchParams.delete("page");
      url.searchParams.delete("section");
      history.replaceState(null, "", url);
    }
  }
  useEffect(() => {
    const update = () =>
      setSettingsOpen(
        new URLSearchParams(location.search).get("page") === "settings",
      );
    window.addEventListener("popstate", update);
    return () => window.removeEventListener("popstate", update);
  }, []);

  // A launch checks GitHub at most once a day, only to point out a new version.
  const [update, setUpdate] = useState<UpdateStatus | null>(null);
  useEffect(() => {
    let timer: ReturnType<typeof setTimeout>;
    let polls = 0;
    const follow = (status: UpdateStatus) => {
      setUpdate(status);
      if (status.stage === "checking" && polls++ < 30)
        timer = setTimeout(
          () =>
            void request<UpdateStatus>("/update")
              .then(follow)
              .catch(() => {}),
          2000,
        );
    };
    void request<UpdateStatus>("/update/check", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ auto: true }),
    })
      .then(follow)
      .catch(() => {});
    return () => clearTimeout(timer);
  }, []);

  function notify(text: string, undo?: () => void) {
    clearTimeout(toastTimer.current);
    setToast({ text, undo });
    toastTimer.current = setTimeout(() => setToast(null), undo ? 6000 : 3000);
  }
  function setFollow(next: boolean) {
    if (next === follow) return;
    if (!next) setFollowBase(feed.appended);
    setFollowState(next);
  }
  function resumeFollow() {
    setFrozen(false);
    setFollowState(true);
    setJump((n) => n + 1);
  }
  function applyFilters(next: FilterState) {
    setSearch(next.search);
    setQuery(next.search);
    setDirection(next.direction);
    setControls(next.controls);
    setAdvanced(next.advanced);
  }
  function changeFilters(next: FilterState, text: string) {
    const previous = filterState;
    applyFilters(next);
    notify(text, () => applyFilters(previous));
  }
  function changeCommands(
    rules: { include?: string; exclude?: string },
    text: string,
  ) {
    const rest = { ...advanced };
    delete rest.include;
    delete rest.exclude;
    for (const key of ["include", "exclude"] as const)
      if (rules[key]) rest[key] = rules[key]!;
    changeFilters({ ...filterState, advanced: rest }, text);
  }
  function choose(row: Row, via: "pointer" | "keyboard") {
    setSelected(row);
    setFollow(false);
    if (via === "pointer") setCompactView("detail");
  }
  function toggleChoice(row: Row) {
    if (row.event.kind !== "packet") {
      notify("采集事件没有协议包体，不能对比");
      return;
    }
    const key = row.key;
    if (checked.has(key))
      setBasket((previous) =>
        previous.filter((b) => rowKey(b.sessionId, b.event.event_id) !== key),
      );
    else if (basket.length >= MAX_COMPARE)
      notify("最多对比 8 个包，请先移除一个");
    else
      setBasket((previous) => [
        ...previous,
        { sessionId: row.sessionId, event: row.event },
      ]);
  }
  function addRange(rows: Row[]) {
    const additions = rows.filter(
      (r) => r.event.kind === "packet" && !checked.has(r.key),
    );
    const room = MAX_COMPARE - basket.length;
    if (additions.length > room)
      notify(`最多对比 8 个包，只加入了前 ${Math.max(0, room)} 个`);
    setBasket((previous) => [
      ...previous,
      ...additions
        .slice(0, Math.max(0, room))
        .map((r) => ({ sessionId: r.sessionId, event: r.event })),
    ]);
  }
  async function copy(text: string, done: string) {
    try {
      await copyText(text);
      notify(done);
    } catch {
      notify("复制失败，请检查浏览器剪贴板权限");
    }
  }
  async function copyHex(items: PacketChoice[]) {
    try {
      const values = await Promise.all(
        items.map((item) =>
          request<PacketDetail>(
            `/sessions/${item.sessionId}/events/${item.event.event_id}?format=hex`,
          ).then((detail) => detail.payload_hex),
        ),
      );
      await copyText(values.join("\n"));
      notify(
        items.length > 1
          ? `已复制 ${items.length} 个包体 Hex，每行一个`
          : items.some((i) => i.event.truncated)
            ? "已复制已保存部分的 Hex（原包已截断）"
            : "包体 Hex 已复制",
      );
    } catch (error) {
      notify(message(error));
    }
  }
  function chooseSession(id: string) {
    initialSelectionDone.current = true;
    setOpened((previous) => [...new Set([...previous, id])]);
    setSessionId(id);
    setMerged(false);
    setFrozen(false);
    setFollowState(true);
    setCompactView("list");
  }
  function mergeSession(id: string) {
    chooseSession(id);
    setMerged(true);
  }
  function closeSessions(ids: string[]) {
    setOpened((previous) => previous.filter((open) => !ids.includes(open)));
    if (sessionId && ids.includes(sessionId)) {
      const replacement =
        catalog?.sessions.find(isLive) ||
        catalog?.sessions.find(
          (s) => !ids.includes(s.id) && opened.includes(s.id),
        );
      if (replacement) chooseSession(replacement.id);
      else {
        setSessionId(null);
        const url = new URL(location.href);
        url.searchParams.delete("session");
        url.searchParams.delete("merged");
        history.replaceState(null, "", url);
      }
    }
  }
  function toggleReveal() {
    setReveal((value) => {
      writePreference("qscope.account.reveal", String(!value));
      return !value;
    });
  }
  function onRatio(next: number, commit: boolean) {
    setRatio(next);
    if (commit) writePreference("qscope.layout.listRatio", next.toFixed(3));
  }

  useEffect(() => {
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout>;
    async function poll() {
      let delay = 3000;
      try {
        const result = await request<Catalog>("/sessions", {
          signal: controller.signal,
        });
        if (controller.signal.aborted) return;
        if (result.api_version !== 1)
          throw new Error("本地接口版本不兼容，请更新工作台。");
        setCatalog(result);
        setCatalogError("");
        if (
          !result.discovering &&
          !result.sessions.length &&
          !sessionRef.current &&
          !initialSelectionDone.current
        )
          setSettingsOpen(true);
        if (
          sessionRef.current &&
          !result.discovering &&
          !result.sessions.some((s) => s.id === sessionRef.current)
        ) {
          const missing = sessionRef.current;
          sessionRef.current = null;
          setSessionId(null);
          setOpened((previous) => previous.filter((id) => id !== missing));
          initialSelectionDone.current = false;
        }
        if (
          !sessionRef.current &&
          result.sessions.length &&
          (!initialSelectionDone.current || result.sessions.some(isLive))
        ) {
          const saved = new URLSearchParams(location.search).get("session");
          const target =
            result.sessions.find((s) => s.id === saved) ||
            result.sessions.find(isLive) ||
            (!result.discovering ? result.sessions[0] : undefined);
          if (target) {
            setSessionId(target.id);
            setOpened((previous) => [...new Set([...previous, target.id])]);
            initialSelectionDone.current = true;
          }
        }
        if (!sessionRef.current && result.discovering) delay = 250;
      } catch (error) {
        if (!abortError(error)) setCatalogError(message(error));
      }
      if (!controller.signal.aborted) timer = setTimeout(poll, delay);
    }
    void poll();
    return () => {
      controller.abort();
      clearTimeout(timer);
    };
  }, [refresh]);
  useEffect(() => {
    const timer = setTimeout(() => setQuery(search.trim()), 200);
    return () => clearTimeout(timer);
  }, [search]);
  const sourceKey = source ? JSON.stringify(source) : "";
  useEffect(() => {
    setSelected(null);
    setMenu(null);
    setContextKey(null);
    autoSelected.current = false;
    setFollowBase(0);
  }, [sourceKey]);
  useEffect(() => {
    setMenu(null);
    setContextKey(null);
  }, [query, direction, controls, advanced]);
  useEffect(() => {
    if (savedPath && catalog) {
      const target = catalog.sessions.find((s) => s.path === savedPath);
      if (target) {
        chooseSession(target.id);
        setSavedPath(null);
      }
    }
  }, [catalog, savedPath]);
  // The first rows of a view select the newest packet once; later rows never replace the detail.
  useEffect(() => {
    if (!selected && !autoSelected.current && feed.rows.length) {
      autoSelected.current = true;
      setSelected(feed.rows.at(-1)!);
    }
  }, [feed.rows, selected]);
  useEffect(() => {
    if (!sessionId) return;
    const url = new URL(location.href);
    url.searchParams.set("session", sessionId);
    if (merged) url.searchParams.set("merged", "1");
    else url.searchParams.delete("merged");
    history.replaceState(null, "", url);
  }, [sessionId, merged]);
  useEffect(() => {
    const handler = (event: KeyboardEvent) => {
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "k") {
        event.preventDefault();
        setCompactView("list");
        requestAnimationFrame(() => searchInput.current?.focus());
      }
    };
    window.addEventListener("keydown", handler);
    return () => {
      window.removeEventListener("keydown", handler);
      clearTimeout(toastTimer.current);
    };
  }, []);

  async function importFile(file: File | undefined) {
    if (!file) return;
    if (!file.name.toLowerCase().endsWith(".qscap")) {
      notify("请选择 .qscap 原始采集文件");
      return;
    }
    if (file.size > 512 * 1024 * 1024) {
      notify("导入文件最多 512 MiB");
      return;
    }
    setUploading(true);
    try {
      const imported = await request<Session>("/import", {
        method: "POST",
        headers: { "Content-Type": "application/octet-stream" },
        body: file,
      });
      chooseSession(imported.id);
      setRefresh((n) => n + 1);
      notify("记录已导入，原始文件保持不变");
    } catch (error) {
      notify(message(error));
    } finally {
      setUploading(false);
      if (input.current) input.current.value = "";
    }
  }

  function launchLabel(row: Row | null): string | undefined {
    if (!row || !group?.account || group.launches.length < 2) return undefined;
    const launch = group.launches.find((s) => s.id === row.sessionId);
    return launch
      ? `第 ${launchNumber(group, launch)} 次启动 · PID ${launch.pid}`
      : undefined;
  }
  function packetMenu(row: Row, x: number, y: number) {
    const e = row.event;
    const packet = e.kind === "packet";
    const choice: PacketChoice = { sessionId: row.sessionId, event: e };
    const inBasket = checked.has(row.key);
    setContextKey(row.key);
    const open: MenuItem = {
      id: "open",
      label: "在右侧打开",
      icon: "arrow",
      shortcut: "Enter",
      run: () => {
        setSelected(row);
        setFollow(false);
        setCompactView("detail");
      },
    };
    const popupItem: MenuItem = {
      id: "popup",
      label: "弹窗查看",
      icon: "popup",
      shortcut: "Shift+Enter",
      run: () => setPopup(choice),
    };
    let groups: MenuGroup[];
    if (inBasket && basket.length >= 2) {
      const lines = (pick: (item: PacketChoice) => string) =>
        basket.map(pick).join("\n");
      const commands = [...new Set(basket.map((b) => b.event.command))];
      groups = [
        {
          label: "查看",
          items: [
            {
              id: "compare",
              label: `对比所选 ${basket.length} 个包`,
              icon: "compare",
              run: () => setComparison([...basket]),
            },
            open,
            popupItem,
          ],
        },
        {
          label: "复制所选",
          items: [
            {
              id: "copy-commands",
              label: "复制所选命令",
              icon: "copy",
              run: () =>
                void copy(
                  lines((b) => b.event.command),
                  `已复制 ${basket.length} 个命令，每行一个`,
                ),
            },
            {
              id: "copy-seq",
              label: "复制所选 SEQ",
              icon: "copy",
              run: () =>
                void copy(
                  lines((b) => b.event.sequence),
                  `已复制 ${basket.length} 个 SEQ`,
                ),
            },
            {
              id: "copy-ids",
              label: "复制所选事件 ID",
              icon: "copy",
              run: () =>
                void copy(
                  lines((b) => b.event.event_id),
                  `已复制 ${basket.length} 个事件 ID`,
                ),
            },
            {
              id: "copy-hex",
              label: "复制所选包体 Hex",
              icon: "hex",
              run: () => void copyHex(basket),
            },
          ],
        },
        {
          label: "筛选",
          items: [
            {
              id: "only",
              label: `只看所选的 ${commands.length} 个命令`,
              icon: "filter",
              run: () =>
                changeFilters(
                  {
                    ...filterState,
                    advanced: {
                      ...advanced,
                      include: commands.map(exact).join(","),
                    },
                  },
                  `只看所选的 ${commands.length} 个命令`,
                ),
            },
            {
              id: "exclude",
              label: `排除所选的 ${commands.length} 个命令`,
              icon: "exclude",
              run: () =>
                changeFilters(
                  {
                    ...filterState,
                    advanced: {
                      ...advanced,
                      exclude: commands.reduce(
                        (value, c) => withPattern(value, exact(c)),
                        advanced.exclude,
                      ),
                    },
                  },
                  `已排除 ${commands.length} 个命令`,
                ),
            },
          ],
        },
        {
          label: "勾选",
          items: [
            {
              id: "uncheck",
              label: "移出此行",
              icon: "close",
              run: () => toggleChoice(row),
            },
            {
              id: "clear",
              label: "取消全部勾选",
              icon: "close",
              run: () => setBasket([]),
            },
          ],
        },
      ];
    } else {
      const view: MenuItem[] = [open, popupItem];
      if (packet)
        view.push({
          id: "compare",
          label: inBasket ? "移出对比" : "加入对比",
          icon: "compare",
          shortcut: "Space",
          disabled:
            !inBasket && basket.length >= MAX_COMPARE
              ? "最多对比 8 个包"
              : undefined,
          run: () => toggleChoice(row),
        });
      const copyItems: MenuItem[] = [
        {
          id: "copy-command",
          label: packet ? "复制命令" : "复制事件名",
          icon: "copy",
          shortcut: "Ctrl+C",
          run: () =>
            void copy(e.command, packet ? "命令已复制" : "事件名已复制"),
        },
      ];
      if (packet)
        copyItems.push({
          id: "copy-seq",
          label: "复制 SEQ",
          icon: "copy",
          run: () => void copy(e.sequence, "SEQ 已复制"),
        });
      copyItems.push({
        id: "copy-id",
        label: "复制事件 ID",
        icon: "copy",
        run: () => void copy(e.event_id, "事件 ID 已复制"),
      });
      if (packet)
        copyItems.push({
          id: "copy-hex",
          label: e.truncated ? "复制包体 Hex（仅已保存部分）" : "复制包体 Hex",
          icon: "hex",
          disabled: e.captured_length === 0 ? "此记录没有包体" : undefined,
          run: () => void copyHex([choice]),
        });
      const filterItems: MenuItem[] = packet
        ? [
            {
              id: "only",
              label: "只看此命令",
              icon: "filter",
              run: () =>
                changeFilters(
                  {
                    ...filterState,
                    advanced: { ...advanced, include: exact(e.command) },
                  },
                  patterns(advanced.include).length
                    ? `已改为只看 ${e.command}`
                    : `只看 ${e.command}`,
                ),
            },
            {
              id: "exclude",
              label: "排除此命令",
              icon: "exclude",
              disabled: patterns(advanced.exclude).includes(exact(e.command))
                ? "已经排除"
                : undefined,
              run: () =>
                changeFilters(
                  {
                    ...filterState,
                    advanced: {
                      ...advanced,
                      exclude: withPattern(advanced.exclude, exact(e.command)),
                    },
                  },
                  `已排除 ${e.command}`,
                ),
            },
            {
              id: "seq",
              label: "精确搜索此 SEQ",
              icon: "search",
              disabled: e.sequence === "0" ? "此记录没有 SEQ" : undefined,
              run: () =>
                changeFilters(
                  {
                    ...filterState,
                    advanced: { ...advanced, seq: e.sequence },
                  },
                  `只看 SEQ ${e.sequence}`,
                ),
            },
          ]
        : [
            {
              id: "hide-controls",
              label: "隐藏采集事件",
              icon: "exclude",
              run: () =>
                changeFilters(
                  { ...filterState, controls: false },
                  "已隐藏采集事件",
                ),
            },
          ];
      groups = [
        { label: "查看", items: view },
        { label: "复制", items: copyItems },
        { label: "筛选", items: filterItems },
      ];
    }
    setMenu({
      x,
      y,
      title:
        inBasket && basket.length >= 2
          ? `已选 ${basket.length} 个包`
          : `${packet ? "包" : "采集事件"} #${e.event_id}`,
      groups,
      returnFocus: listRef.current,
    });
  }

  const warnings = [
    catalogError,
    feed.error,
    session?.error ? friendlyError(session.error).title : "",
  ].filter(Boolean);
  const warningDetail = session?.error
    ? friendlyError(session.error).detail
    : "";
  const discoveryCount =
    (catalog?.discovery_errors.length || 0) +
    (catalog?.discovery_limited ? 1 : 0);
  const stats = accountView ? segments : session ? [session] : [];
  const dropped = total(stats, "dropped_total"),
    truncated = total(stats, "truncated_packets");
  const filtered = chips.length > 0;
  const emptyList = filtered ? (
    <>
      <Icon name="filter" size={26} />
      <strong>没有符合筛选的包</strong>
      <span>{chips.map((c) => c.label).join(" · ")}</span>
      <button
        className="text-button"
        onClick={() =>
          changeFilters(
            { search: "", direction: "all", controls: false, advanced: {} },
            "已清除全部筛选",
          )
        }
      >
        清除筛选
      </button>
    </>
  ) : live ? (
    <>
      <span className="small-spinner" aria-hidden="true" />
      <strong>等待第一个包</strong>
      <span>
        QQ 已启动。在 QQ 窗口扫码登录后，登录和业务请求会陆续出现在这里。
      </span>
    </>
  ) : (
    <>
      <Icon name="packet" size={26} />
      <strong>这份记录里没有协议包</strong>
      <span>可在“筛选”里打开“显示采集事件”，查看探针的启动和结束状态。</span>
    </>
  );
  const compact = compactView;
  const note = session
    ? readingNote(accountView ? segments.at(-1) || session : session)
    : "";

  return (
    <div className="app-shell">
      <PosterBackdrop />
      <header className="app-header">
        <a className="brand" href="/" aria-label="QScope 首页">
          <span className="brand-mark" aria-hidden="true" />
          <span>
            QScope<span className="brand-dot">.</span>
          </span>
        </a>
        <SessionBar
          sessions={visibleSessions}
          opened={opened}
          selected={sessionId}
          merged={accountView}
          reveal={reveal}
          onReveal={toggleReveal}
          loading={Boolean(catalog?.discovering) || (!catalog && !catalogError)}
          uploading={uploading}
          onImport={() => input.current?.click()}
          onSelect={(id) => {
            chooseSession(id);
            if (settingsOpen) navigateSettings(false);
          }}
          onMerge={(id) => {
            mergeSession(id);
            if (settingsOpen) navigateSettings(false);
          }}
          onClose={closeSessions}
          onRefresh={() => setRefresh((n) => n + 1)}
        />
        <div className="header-actions">
          <span
            className="local-badge"
            title={catalogError || "只读取本机抓包文件，不连接外部服务"}
          >
            <span className={`status-dot ${catalogError ? "bad" : ""}`} />
            {catalogError ? "离线" : "本机服务"}
          </span>
          <button
            className="appearance-button"
            aria-label={theme === "light" ? "切换深色外观" : "切换浅色外观"}
            title={theme === "light" ? "切换深色外观" : "切换浅色外观"}
            onClick={toggleTheme}
          >
            <Icon name={theme === "light" ? "moon" : "sun"} size={16} />
          </button>
          {update?.stage === "available" && (
            <button
              className="update-pill"
              title="查看更新说明并安装"
              onClick={() => navigateSettings(true, "update")}
            >
              <Icon name="download" size={13} />
              新版本 {update.latest}
            </button>
          )}
          <button
            className="pause-button"
            aria-label="打开设置"
            aria-haspopup="dialog"
            onClick={() => navigateSettings(true)}
          >
            <Icon name="settings" size={15} />
            设置
          </button>
        </div>
        <input
          ref={input}
          className="file-input"
          type="file"
          accept=".qscap"
          aria-label="导入抓包文件"
          onChange={(event) => void importFile(event.target.files?.[0])}
        />
      </header>
      <div className="app-body">
        <main
          className={`workspace compact-${compact}`}
          aria-label="抓包工作区"
        >
          {warnings.length > 0 && (
            <div className="warning-banner" role="alert">
              <Icon name="warning" size={15} />
              <span>
                {[...new Set(warnings)].join(" · ")}
                {warningDetail && <small>{warningDetail}</small>}
              </span>
              <button
                className="text-button"
                onClick={() => setRefresh((n) => n + 1)}
              >
                重试
              </button>
            </div>
          )}
          {session ? (
            <>
              <div className="compact-view-switch" aria-label="工作区视图">
                <button
                  aria-pressed={compact === "list"}
                  onClick={() => setCompactView("list")}
                >
                  <Icon name="packet" size={14} />
                  包列表
                </button>
                <button
                  aria-pressed={compact === "detail"}
                  disabled={!selected}
                  onClick={() => setCompactView("detail")}
                >
                  <Icon name="file" size={14} />
                  包详情
                  {selected && (
                    <span className="mono">#{selected.event.event_id}</span>
                  )}
                </button>
              </div>
              <div
                ref={inspector}
                className={`inspector compact-${compact}`}
                style={
                  {
                    "--list-width": `clamp(300px, ${(ratio * 100).toFixed(2)}%, calc(100% - 528px))`,
                  } as CSSProperties
                }
              >
                <section className="list-pane" aria-label="数据包面板">
                  <div className="list-head">
                    <div
                      className="search-box"
                      onClick={(event) => {
                        if (event.target === event.currentTarget)
                          searchInput.current?.focus();
                      }}
                    >
                      <Icon name="search" size={14} />
                      {tokens.length > 0 && (
                        <div
                          className="filter-chips"
                          role="list"
                          aria-label="生效的筛选条件"
                        >
                          {tokens.map((chip) => (
                            <span
                              className="filter-chip"
                              role="listitem"
                              key={chip.id}
                              title={chip.title || chip.label}
                            >
                              {chip.label}
                              <button
                                aria-label={`移除筛选 ${chip.label}`}
                                onClick={() =>
                                  changeFilters(
                                    chip.next,
                                    `已移除筛选 ${chip.label}`,
                                  )
                                }
                              >
                                <Icon name="close" size={10} />
                              </button>
                            </span>
                          ))}
                        </div>
                      )}
                      <input
                        ref={searchInput}
                        type="search"
                        aria-label="搜索命令或序号"
                        placeholder={
                          tokens.length ? "搜索…" : "搜索命令、SEQ 或事件 ID…"
                        }
                        value={search}
                        onChange={(e) => setSearch(e.target.value)}
                        onKeyDown={(event) => {
                          const last = tokens.at(-1);
                          if (event.key === "Backspace" && !search && last) {
                            event.preventDefault();
                            changeFilters(
                              last.next,
                              `已移除筛选 ${last.label}`,
                            );
                          }
                        }}
                      />
                      {chips.length > 0 ? (
                        <button
                          className="icon-button clear-filters"
                          aria-label="清除全部筛选"
                          title="清除全部筛选"
                          onClick={() =>
                            changeFilters(
                              {
                                search: "",
                                direction: "all",
                                controls: false,
                                advanced: {},
                              },
                              "已清除全部筛选",
                            )
                          }
                        >
                          <Icon name="close" size={12} />
                        </button>
                      ) : (
                        <kbd>Ctrl K</kbd>
                      )}
                    </div>
                    <div className="list-controls">
                      <div
                        className="segmented"
                        role="group"
                        aria-label="过滤方向"
                      >
                        {[
                          ["all", "全部"],
                          ["inbound", "↓ 收"],
                          ["outbound", "↑ 发"],
                        ].map(([value, label]) => (
                          <button
                            key={value}
                            aria-pressed={direction === value}
                            title={
                              value === "all"
                                ? "收发都显示"
                                : value === "inbound"
                                  ? "只看 QQ 收到的包"
                                  : "只看 QQ 发出的包"
                            }
                            onClick={() => setDirection(value)}
                          >
                            {label}
                          </button>
                        ))}
                      </div>
                      <CommandFilter
                        sources={
                          accountView ? segments.map((s) => s.id) : [session.id]
                        }
                        rules={{
                          include: advanced.include,
                          exclude: advanced.exclude,
                        }}
                        onChange={changeCommands}
                        notify={notify}
                        onOpen={() => setFiltersOpen(false)}
                      />
                      <button
                        className={`text-button filter-button ${filtersOpen || conditions ? "active" : ""}`}
                        aria-label="高级筛选"
                        aria-expanded={filtersOpen}
                        onClick={() => setFiltersOpen((v) => !v)}
                      >
                        <Icon name="filter" size={13} />
                        筛选
                        {conditions > 0 && (
                          <b className="filter-count mono">{conditions}</b>
                        )}
                      </button>
                      <button
                        className="text-button filter-button compare-mode"
                        aria-label="对比模式"
                        aria-pressed={picking}
                        title={
                          picking
                            ? "退出对比模式，点击行恢复为查看"
                            : "进入对比模式：点击行勾选 2–8 个包"
                        }
                        onClick={() => setPicking((v) => !v)}
                      >
                        <Icon name="compare" size={13} />
                        对比
                      </button>
                      <RecordingControl
                        session={liveSession}
                        notify={notify}
                        onSaved={(path) => {
                          setSavedPath(path);
                          setRefresh((n) => n + 1);
                        }}
                      />
                    </div>
                    {filtersOpen && (
                      <div
                        className="filter-popover"
                        onKeyDown={(event) => {
                          if (event.key === "Escape") {
                            event.stopPropagation();
                            setFiltersOpen(false);
                          }
                        }}
                      >
                        <AdvancedFilters
                          value={advanced}
                          controls={controls}
                          pick={selected?.event.timestamp_filetime}
                          onApply={(value, showControls) => {
                            setAdvanced(value);
                            setControls(showControls);
                          }}
                          onClose={() => setFiltersOpen(false)}
                        />
                      </div>
                    )}
                  </div>
                  {feed.hasEarlier && (
                    <button
                      className="earlier-button"
                      disabled={
                        feed.olderLoading || feed.rows.length >= MAX_ROWS
                      }
                      onClick={() => {
                        if (live) setFrozen("earlier");
                        setFollow(false);
                        void feed.earlier();
                      }}
                    >
                      <Icon name="up" size={12} />
                      {feed.rows.length >= MAX_ROWS
                        ? "当前显示已达 5,000 条，请用搜索定位历史包"
                        : feed.olderLoading
                          ? "正在读取…"
                          : "加载更早的记录"}
                    </button>
                  )}
                  <PacketTable
                    rows={feed.rows}
                    segments={accountView ? segments : []}
                    picking={picking}
                    selectedKey={selected?.key}
                    contextKey={contextKey || undefined}
                    checked={checked}
                    follow={follow && !frozen}
                    trimmed={feed.trimmed}
                    jump={jump}
                    locate={locate}
                    loading={feed.loading || feed.indexing}
                    empty={emptyList}
                    listRef={listRef}
                    handlers={{
                      onSelect: choose,
                      onToggle: toggleChoice,
                      onRange: addRange,
                      onContext: packetMenu,
                      onActivate: (row) => {
                        setSelected(row);
                        setCompactView("detail");
                      },
                      onPopup: (row) =>
                        setPopup({
                          sessionId: row.sessionId,
                          event: row.event,
                        }),
                      onCopy: (row) =>
                        void copy(row.event.command, "命令已复制"),
                      onFollow: (next) => {
                        if (!frozen) setFollow(next);
                      },
                    }}
                  />
                  {frozen ? (
                    <div className="list-pill frozen-pill" role="status">
                      <Icon name="pause" size={12} />
                      <span>
                        {frozen === "earlier"
                          ? "已加载更早的记录，暂停插入新包"
                          : "列表已冻结"}
                        {feed.pending > 0
                          ? ` · 新到 ${count(String(feed.pending))} 条未显示`
                          : ""}
                      </span>
                      <button onClick={resumeFollow}>继续刷新</button>
                    </div>
                  ) : (
                    live &&
                    !follow &&
                    unseen > 0 && (
                      <button
                        className="list-pill new-rows-pill"
                        onClick={resumeFollow}
                      >
                        <Icon name="down" size={13} />
                        新增 {count(String(unseen))} 条 · 回到最新
                      </button>
                    )
                  )}
                  <div className="list-footer">
                    {basket.length > 0 || picking ? (
                      <div
                        className="basket"
                        role="region"
                        aria-label="对比选择"
                      >
                        {basket.length > 0 ? (
                          <span>
                            已勾选 <b className="mono">{basket.length}</b>
                          </span>
                        ) : (
                          <span className="muted">点击行勾选 2–8 个包</span>
                        )}
                        <button
                          className="text-button"
                          disabled={basket.length < 2}
                          title={
                            basket.length < 2
                              ? "至少勾选 2 个包"
                              : "Ctrl+点击增减 · Shift+点击连选"
                          }
                          onClick={() => setComparison([...basket])}
                        >
                          <Icon name="compare" size={12} />
                          对比 {basket.length}
                        </button>
                        {basket.length > 0 && (
                          <button
                            className="text-button"
                            aria-label="清空对比选择"
                            onClick={() => setBasket([])}
                          >
                            清空
                          </button>
                        )}
                      </div>
                    ) : (
                      <span
                        className="list-summary"
                        title={
                          session.partial_tail ? terms.partialTail : undefined
                        }
                      >
                        <b className="mono">
                          {count(String(feed.rows.length))}
                        </b>{" "}
                        条{accountView ? " · 全部启动" : ""}
                        {filtered ? " · 已筛选" : ""}
                        {controls ? " · 含采集事件" : ""}
                        {note && (
                          <span className="reading-note"> · {note}</span>
                        )}
                      </span>
                    )}
                    <div className="footer-end">
                      {live ? (
                        <>
                          <button
                            className="text-button"
                            aria-pressed={Boolean(frozen)}
                            title={`${terms.freeze} 新包仍写入原始文件。`}
                            onClick={() =>
                              setFrozen((value) => (value ? false : "user"))
                            }
                          >
                            <Icon name={frozen ? "play" : "pause"} size={12} />
                            {frozen ? "继续刷新" : "冻结"}
                          </button>
                          <button
                            className="text-button follow-toggle"
                            aria-pressed={follow && !frozen}
                            title={terms.follow}
                            onClick={() =>
                              follow && !frozen
                                ? setFollow(false)
                                : resumeFollow()
                            }
                          >
                            <span
                              className={`status-dot ${follow && !frozen ? "" : "idle"}`}
                            />
                            {follow && !frozen ? "跟随最新" : "已停在此处"}
                          </button>
                        </>
                      ) : (
                        <button
                          className="text-button"
                          aria-label="滚动到最新记录"
                          title="跳到列表末尾"
                          onClick={() => {
                            setFollowState(true);
                            setJump((n) => n + 1);
                            if (feed.hasEarlier && feed.rows.length >= MAX_ROWS)
                              feed.latest();
                          }}
                        >
                          <Icon name="down" size={12} />
                          最新
                        </button>
                      )}
                    </div>
                  </div>
                </section>
                <Splitter
                  ratio={ratio}
                  container={inspector}
                  onChange={onRatio}
                />
                {selected ? (
                  <Detail
                    key={selected.key}
                    sessionId={selected.sessionId}
                    event={selected.event}
                    notify={notify}
                    onPopup={() =>
                      setPopup({
                        sessionId: selected.sessionId,
                        event: selected.event,
                      })
                    }
                    extras={{
                      compared: checked.has(selected.key),
                      compareFull: basket.length >= MAX_COMPARE,
                      onCompare: () => toggleChoice(selected),
                      outside,
                      launch: launchLabel(selected),
                      onLocate: () => {
                        setFollow(false);
                        setLocate((previous) => ({
                          key: selected.key,
                          n: (previous?.n || 0) + 1,
                        }));
                      },
                      onBack: () => setCompactView("list"),
                    }}
                  />
                ) : (
                  <section className="detail-pane detail-empty">
                    <div className="empty-illustration">
                      <Icon name="packet" size={34} />
                    </div>
                    <h2>
                      {feed.rows.length ? "选择一个数据包" : "列表里还没有包"}
                    </h2>
                    <p>
                      {feed.rows.length
                        ? "查看它的协议结构和原始字节"
                        : "有包之后，点选一行即可在这里查看"}
                    </p>
                    {feed.rows.length > 0 && (
                      <span className="key-hint">
                        ↑ ↓ 切换 · 右键更多操作 · Ctrl+点击加入对比
                      </span>
                    )}
                  </section>
                )}
              </div>
            </>
          ) : (
            <div className="welcome-empty">
              <div className="empty-illustration">
                <Icon name="packet" size={40} />
              </div>
              {catalogError ? (
                <>
                  <h2>本地服务连接中断</h2>
                  <p>检查 QScope 是否仍在运行，然后重试。</p>
                  <button
                    className="primary-button"
                    onClick={() => setRefresh((n) => n + 1)}
                  >
                    <Icon name="refresh" />
                    重试
                  </button>
                </>
              ) : !catalog ? (
                <h2>正在读取工作区</h2>
              ) : !catalog.sessions.length ? (
                <>
                  <h2>还没有抓包记录</h2>
                  <p>
                    启动 QQ → 在 QQ 窗口扫码登录 → 操作
                    QQ，收发的包会实时出现在这里。
                  </p>
                  <div className="welcome-actions">
                    <button
                      className="primary-button"
                      onClick={() => navigateSettings(true)}
                    >
                      <Icon name="play" />
                      启动 QQ 开始抓包
                    </button>
                    <button
                      className="text-button"
                      disabled={uploading}
                      onClick={() => input.current?.click()}
                    >
                      <Icon name="folder" />
                      导入 .qscap
                    </button>
                  </div>
                </>
              ) : (
                <>
                  <h2>打开一份抓包</h2>
                  <p>从顶部“历史”选择记录，或导入 .qscap 文件。</p>
                  <div className="welcome-actions">
                    <button
                      className="primary-button"
                      onClick={() =>
                        chooseSession(
                          (catalog.sessions.find(isLive) || catalog.sessions[0])
                            .id,
                        )
                      }
                    >
                      <Icon name="file" />
                      打开最近的记录
                    </button>
                    <button
                      className="text-button"
                      disabled={uploading}
                      onClick={() => input.current?.click()}
                    >
                      <Icon name="folder" />
                      导入 .qscap
                    </button>
                  </div>
                </>
              )}
            </div>
          )}
          <footer className="workspace-footer">
            <div className="capture-health" aria-label="采集统计">
              <span className="health-total">
                <b className="mono">{count(total(stats, "packets"))}</b> 包
              </span>
              <span className="direction-totals" title="接收 / 发送">
                <span className="health-in">
                  ↓ 收 <b className="mono">{count(total(stats, "inbound"))}</b>
                </span>
                <span className="health-out">
                  ↑ 发 <b className="mono">{count(total(stats, "outbound"))}</b>
                </span>
              </span>
              <button
                className={`footer-stat loss-counter ${dropped !== "0" ? "bad" : ""}`}
                title={`${terms.dropped}${dropped !== "0" ? " 点击查看丢包事件。" : ""}`}
                disabled={!session || dropped === "0"}
                onClick={() =>
                  changeFilters(
                    {
                      ...filterState,
                      controls: true,
                      advanced: { ...advanced, kinds: "loss" },
                    },
                    "只看丢包事件",
                  )
                }
              >
                丢弃 <b className="mono">{count(dropped)}</b>
              </button>
              <button
                className={`footer-stat truncate-counter ${truncated !== "0" ? "warn" : ""}`}
                title={`${terms.truncated}${truncated !== "0" ? " 点击只看截断包。" : ""}`}
                disabled={!session || truncated === "0"}
                onClick={() =>
                  changeFilters(
                    {
                      ...filterState,
                      advanced: { ...advanced, truncated_only: "true" },
                    },
                    "只看截断包",
                  )
                }
              >
                截断 <b className="mono">{count(truncated)}</b>
              </button>
              <span className="file-size">
                {size(total(stats, "file_bytes"))}
              </span>
            </div>
            <div className="footer-notices">
              {discoveryCount > 0 && (
                <div className="diagnostics-anchor">
                  <button
                    className="diagnostics-button"
                    aria-label="查看文件加载提示"
                    aria-expanded={diagnostics}
                    onClick={() => setDiagnostics((v) => !v)}
                  >
                    <Icon name="warning" size={12} />
                    {discoveryCount} 项文件提示
                  </button>
                  {diagnostics && (
                    <div
                      className="diagnostics-popover"
                      role="dialog"
                      aria-label="文件加载提示"
                    >
                      <div className="history-caption">
                        <strong>文件加载提示</strong>
                        <button
                          className="icon-button"
                          aria-label="收起文件加载提示"
                          onClick={() => setDiagnostics(false)}
                        >
                          <Icon name="close" />
                        </button>
                      </div>
                      <p>部分抓包文件未加载，请检查目录或文件格式。</p>
                      {catalog?.discovery_errors.map((text, i) => (
                        <p key={i}>{text}</p>
                      ))}
                      {catalog?.discovery_limited && (
                        <p>目录扫描或会话数量达到上限。</p>
                      )}
                      <button
                        className="text-button"
                        onClick={() => setRefresh((n) => n + 1)}
                      >
                        重新扫描
                      </button>
                    </div>
                  )}
                </div>
              )}
              <div className="appearance-credits">
                <a
                  className="font-credit"
                  href="/licenses/HarmonyOSSans-LICENSE.txt"
                  target="_blank"
                  rel="noreferrer"
                  title="HarmonyOS Sans SC 字体使用及许可说明"
                >
                  HarmonyOS Sans SC
                </a>
                <a
                  className="design-credit"
                  href="https://github.com/ikenxuan/karin-plugin-kkk"
                  target="_blank"
                  rel="noreferrer"
                >
                  外观 · ikenxuan
                </a>
              </div>
            </div>
          </footer>
        </main>
      </div>
      {settingsOpen && (
        <Modal
          title="QScope 设置"
          className="settings-dialog"
          onClose={() => navigateSettings(false)}
        >
          <SettingsPage
            notify={notify}
            update={update}
            onUpdate={setUpdate}
            onCapture={(id) => {
              chooseSession(id);
              navigateSettings(false);
              setRefresh((value) => value + 1);
            }}
          />
        </Modal>
      )}
      {toast && (
        <div
          className="toast"
          role="status"
          popover="manual"
          ref={(element) => {
            if (element && !element.matches(":popover-open"))
              element.showPopover?.();
          }}
        >
          <Icon name="check" size={16} />
          {toast.text}
          {toast.undo && (
            <button
              className="text-button"
              onClick={() => {
                toast.undo!();
                setToast(null);
              }}
            >
              撤销
            </button>
          )}
        </div>
      )}
      {menu && (
        <ContextMenu
          request={menu}
          onClose={() => {
            setMenu(null);
            setContextKey(null);
          }}
        />
      )}
      {popup && (
        <PacketPopup
          item={popup}
          onClose={() => setPopup(null)}
          notify={notify}
        />
      )}
      {comparison && (
        <ComparePopup items={comparison} onClose={() => setComparison(null)} />
      )}
    </div>
  );
}
