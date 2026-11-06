import { useEffect, useRef, useState } from "react";
import type { Session } from "./api.generated";
import { fileTime, size } from "./api";
import { accountText, isLive, recordState, terms } from "./labels";
import { Icon } from "./views";

export { isLive as isCurrent } from "./labels";

export type Group = {
  key: string;
  account: string | null;
  launches: Session[];
  extras: Session[];
};
export function groupKey(session: Session): string {
  return session.account
    ? `account:${session.account}`
    : `session:${session.id}`;
}
const newest = (a: Session, b: Session) => {
  const x = BigInt(a.started_filetime),
    y = BigInt(b.started_filetime);
  return x === y ? b.id.localeCompare(a.id) : y > x ? 1 : -1;
};

// Launches are QQ processes recorded by the collector. Imports, recordings and
// proxy captures with the same account stay in the group as extras.
export function groupSessions(sessions: Session[]): Map<string, Group> {
  const result = new Map<string, Group>();
  for (const session of [...sessions].sort(newest)) {
    const key = groupKey(session);
    const group = result.get(key) || {
      key,
      account: session.account,
      launches: [],
      extras: [],
    };
    (session.source_kind === "capture" ? group.launches : group.extras).push(
      session,
    );
    result.set(key, group);
  }
  return result;
}
export function members(group: Group): Session[] {
  return [...group.launches, ...group.extras];
}
function sourceLabel(session: Session): string {
  return (
    (
      { imported: "导入", recording: "录制片段", http: "HTTP 代理" } as Record<
        string,
        string
      >
    )[session.source_kind] || ""
  );
}
function primary(group: Group, selected: string | null): Session {
  const all = members(group);
  return (
    all.find((s) => s.id === selected) || group.launches.find(isLive) || all[0]
  );
}
export function launchNumber(group: Group, session: Session): number {
  const ordered = [...group.launches].reverse();
  return ordered.findIndex((s) => s.id === session.id) + 1;
}

function AccountName({
  account,
  reveal,
}: {
  account: string | null;
  reveal: boolean;
}) {
  if (!account)
    return (
      <span
        className="account-name unknown"
        title="这份记录里没有账号状态：未登录、旧记录、导入文件或 HTTP 代理"
      >
        未识别账号
      </span>
    );
  // Shown until a real login confirms the probe reads the logged-in account.
  const hint = /^\d+$/.test(account)
    ? terms.account
    : `${terms.account}这个值不是数字 QQ 号，可能是内部 uid。`;
  return (
    <>
      <span className="account-name mono" title={hint}>
        {accountText(account, reveal)}
      </span>
      <span className="account-check" title={hint}>
        待核验
      </span>
    </>
  );
}

function LaunchList({
  group,
  selected,
  merged,
  reveal,
  onSelect,
  onMerge,
}: {
  group: Group;
  selected: string | null;
  merged: boolean;
  reveal: boolean;
  onSelect: (id: string) => void;
  onMerge: (id: string) => void;
}) {
  const item = (session: Session, label: string) => {
    const state = recordState(session);
    return (
      <button
        key={session.id}
        role="menuitemradio"
        aria-checked={!merged && selected === session.id}
        className={`launch-item ${!merged && selected === session.id ? "active" : ""}`}
        aria-label={`打开${label} PID ${session.pid} ${fileTime(session.started_filetime, true)}`}
        title={session.path}
        data-session-id={session.id}
        onClick={() => onSelect(session.id)}
      >
        <span className="launch-title">
          <strong>{label}</strong>
          <span className={`record-state ${state.tone}`} title={state.hint}>
            {state.text}
          </span>
        </span>
        <span className="launch-meta mono">
          <span>{fileTime(session.started_filetime, true)}</span>
          <span title={terms.pid}>PID {session.pid}</span>
          <span>
            {session.has_more && session.records === "0"
              ? size(session.file_bytes)
              : `${Number(session.packets).toLocaleString()} 包 · ${size(session.file_bytes)}`}
          </span>
        </span>
      </button>
    );
  };
  return (
    <div
      className="launch-list"
      role="menu"
      aria-label={
        group.account
          ? `${accountText(group.account, reveal)} 的启动记录`
          : "启动记录"
      }
    >
      {group.account && group.launches.length > 1 && (
        <button
          role="menuitemradio"
          aria-checked={merged}
          className={`launch-item merged ${merged ? "active" : ""}`}
          onClick={() => onMerge(group.launches[0].id)}
        >
          <span className="launch-title">
            <Icon name="merge" size={14} />
            <strong>合并查看全部 {group.launches.length} 次启动</strong>
          </span>
          <span className="launch-meta">
            按启动时间接成一条列表，每次启动之间有分隔行
          </span>
        </button>
      )}
      {group.launches.map((session) =>
        item(
          session,
          group.account
            ? `第 ${launchNumber(group, session)} 次启动`
            : "QQ 启动",
        ),
      )}
      {group.extras.length > 0 && (
        <div className="launch-section">录制片段与导入</div>
      )}
      {group.extras.map((session) =>
        item(session, sourceLabel(session) || "记录"),
      )}
    </div>
  );
}

// Import and the account mask sit with the records they affect, in each record menu.
function RecordActions({
  anyAccount,
  reveal,
  uploading,
  onReveal,
  onImport,
}: {
  anyAccount: boolean;
  reveal: boolean;
  uploading: boolean;
  onReveal: () => void;
  onImport: () => void;
}) {
  return (
    <div className="record-actions">
      {anyAccount && (
        <button
          className="text-button reveal-toggle"
          aria-pressed={reveal}
          aria-label={reveal ? "隐藏完整账号" : "显示完整账号"}
          title={reveal ? "隐藏完整账号" : "显示完整账号（默认打码）"}
          onClick={onReveal}
        >
          <Icon name={reveal ? "eye-off" : "eye"} size={14} />
          {reveal ? "隐藏账号" : "显示账号"}
        </button>
      )}
      <button
        className="text-button"
        disabled={uploading}
        title="导入一个 .qscap 原始采集文件，原文件不会被修改"
        onClick={onImport}
      >
        <Icon name="folder" size={14} />
        {uploading ? "导入中…" : "导入 .qscap"}
      </button>
    </div>
  );
}

export function SessionBar({
  sessions,
  opened,
  selected,
  merged,
  reveal,
  onReveal,
  loading,
  uploading,
  onImport,
  onSelect,
  onMerge,
  onClose,
  onRefresh,
}: {
  sessions: Session[];
  opened: string[];
  selected: string | null;
  merged: boolean;
  reveal: boolean;
  onReveal: () => void;
  loading: boolean;
  uploading: boolean;
  onImport: () => void;
  onSelect: (id: string) => void;
  onMerge: (id: string) => void;
  onClose: (ids: string[]) => void;
  onRefresh: () => void;
}) {
  const [historyOpen, setHistoryOpen] = useState(false);
  // History follows the tabs, so it opens under its button and is kept inside the window.
  const [historyAt, setHistoryAt] = useState({ left: 0, top: 0 });
  // Rendered outside the scrolling tab strip, so it is positioned from the tab.
  const [launches, setLaunches] = useState<string | null>(null);
  const [launchAt, setLaunchAt] = useState({ left: 0, top: 0 });
  const menu = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const bar = useRef<HTMLDivElement>(null);
  const grouped = groupSessions(sessions);
  const all = [...grouped.values()];
  const selectedGroup = all.find((g) =>
    members(g).some((s) => s.id === selected),
  );
  const tabs = all.filter(
    (g) =>
      g === selectedGroup ||
      members(g).some((s) => isLive(s) || opened.includes(s.id)),
  );
  const tabRefs = useRef(new Map<string, HTMLButtonElement>());
  const anyAccount = all.some((g) => g.account);
  const launchGroup = tabs.find((g) => g.key === launches);
  const known = all.filter((g) => g.account);
  const unknown: Group = {
    key: "unknown",
    account: null,
    launches: all.filter((g) => !g.account).flatMap((g) => g.launches),
    extras: all.filter((g) => !g.account).flatMap((g) => g.extras),
  };
  useEffect(() => {
    if (!historyOpen && !launches) return;
    const outside = (event: PointerEvent) => {
      if (!bar.current?.contains(event.target as Node)) {
        setHistoryOpen(false);
        setLaunches(null);
      }
    };
    const escape = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      if (historyOpen) {
        setHistoryOpen(false);
        trigger.current?.focus();
      }
      if (launches) {
        tabRefs.current.get(launches)?.focus();
        setLaunches(null);
      }
    };
    document.addEventListener("pointerdown", outside);
    document.addEventListener("keydown", escape);
    return () => {
      document.removeEventListener("pointerdown", outside);
      document.removeEventListener("keydown", escape);
    };
  }, [historyOpen, launches]);
  useEffect(() => {
    if (selectedGroup)
      tabRefs.current
        .get(selectedGroup.key)
        ?.scrollIntoView({ block: "nearest", inline: "nearest" });
  }, [selectedGroup?.key]);
  const choose = (id: string) => {
    onSelect(id);
    setLaunches(null);
    setHistoryOpen(false);
  };
  const combine = (id: string) => {
    onMerge(id);
    setLaunches(null);
    setHistoryOpen(false);
  };
  const importFile = () => {
    setLaunches(null);
    setHistoryOpen(false);
    onImport();
  };
  const actions = (
    <RecordActions
      anyAccount={anyAccount}
      reveal={reveal}
      uploading={uploading}
      onReveal={onReveal}
      onImport={importFile}
    />
  );
  function toggleLaunches(key: string, tab: HTMLElement) {
    const box = tab.getBoundingClientRect();
    setLaunchAt({
      left: Math.max(8, Math.min(box.left, innerWidth - 368)),
      top: box.bottom + 6,
    });
    setHistoryOpen(false);
    setLaunches((open) => (open === key ? null : key));
  }

  return (
    <div className="session-bar" aria-label="采集记录" ref={bar}>
      <div
        className="session-tablist"
        role="tablist"
        aria-label="采集记录快捷切换"
        onKeyDown={(event) => {
          if (
            !["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key) ||
            !tabs.length ||
            (event.target as HTMLElement).getAttribute("role") !== "tab"
          )
            return;
          event.preventDefault();
          const index = tabs.findIndex((tab) => tab === selectedGroup);
          const next =
            event.key === "Home"
              ? 0
              : event.key === "End"
                ? tabs.length - 1
                : (index +
                    (event.key === "ArrowRight" ? 1 : -1) +
                    tabs.length) %
                  tabs.length;
          onSelect(primary(tabs[next], selected).id);
          tabRefs.current.get(tabs[next].key)?.focus();
        }}
      >
        {tabs.map((group) => {
          const session = primary(group, selected);
          const active = group === selectedGroup;
          const state = recordState(session);
          const count = members(group).length;
          const sub =
            active && merged
              ? `合并 ${group.launches.length} 次启动`
              : session.source_kind === "capture"
                ? `PID ${session.pid}`
                : sourceLabel(session);
          return (
            <div
              key={group.key}
              className={`session-tab ${active ? "active" : ""}`}
            >
              <button
                role="tab"
                aria-selected={active}
                tabIndex={active ? 0 : -1}
                ref={(element) => {
                  if (element) tabRefs.current.set(group.key, element);
                  else tabRefs.current.delete(group.key);
                }}
                onClick={(event) =>
                  active
                    ? toggleLaunches(
                        group.key,
                        event.currentTarget.parentElement!,
                      )
                    : onSelect(session.id)
                }
                title={`${state.hint}\n${fileTime(session.started_filetime, true)}\n${session.path}${active ? "\n点击切换启动记录或导入" : ""}`}
              >
                {isLive(session) ? (
                  <span className="status-dot" />
                ) : (
                  <Icon name={group.account ? "user" : "file"} size={14} />
                )}
                <AccountName account={group.account} reveal={reveal} />
                <span
                  className="session-sub mono"
                  title={
                    session.source_kind === "capture" ? terms.pid : undefined
                  }
                >
                  {sub}
                </span>
                <span className={`record-state ${state.tone}`}>
                  {state.text}
                </span>
              </button>
              <button
                className="launch-toggle"
                aria-label={`选择${group.account ? accountText(group.account, reveal) : "未识别账号"}的启动记录`}
                aria-haspopup="menu"
                aria-expanded={launches === group.key}
                title={count > 1 ? `${count} 份记录 · 导入` : "启动记录 · 导入"}
                onClick={(event) =>
                  toggleLaunches(group.key, event.currentTarget.parentElement!)
                }
              >
                {count > 1 && (
                  <span className="launch-count mono">{count}</span>
                )}
                <Icon name="down" size={12} />
              </button>
              {!members(group).some(isLive) && (
                <button
                  className="close-record"
                  aria-label={`关闭记录视图 ${session.pid} ${session.id}`}
                  title="关闭视图，保留抓包文件"
                  onClick={() => onClose(members(group).map((s) => s.id))}
                >
                  <Icon name="close" size={12} />
                </button>
              )}
            </div>
          );
        })}
        {!tabs.length && (
          <span className="no-record">
            {loading ? "正在读取采集记录…" : "尚未打开抓包"}
          </span>
        )}
      </div>
      {launchGroup && (
        <div
          className="launch-popover"
          style={{ left: launchAt.left, top: launchAt.top }}
        >
          <LaunchList
            group={launchGroup}
            selected={selected}
            merged={launchGroup === selectedGroup && merged}
            reveal={reveal}
            onSelect={choose}
            onMerge={combine}
          />
          {actions}
        </div>
      )}
      <div className="history-menu" ref={menu}>
        <button
          ref={trigger}
          className={`history-button ${historyOpen ? "active" : ""}`}
          aria-label={`历史记录 ${sessions.length}`}
          aria-expanded={historyOpen}
          onClick={(event) => {
            const box = event.currentTarget.getBoundingClientRect();
            setHistoryAt({
              left: Math.max(8, Math.min(box.left, innerWidth - 408)),
              top: box.bottom + 8,
            });
            setLaunches(null);
            setHistoryOpen((open) => !open);
          }}
          title="全部抓包文件，同一账号的多次启动归在一起"
        >
          <Icon name="folder" size={15} />
          <span>历史</span>
          <span className="history-count">{sessions.length}</span>
          <Icon name="down" size={12} />
        </button>
        {historyOpen && (
          <div
            className="history-popover"
            role="dialog"
            aria-label="历史抓包"
            style={{ left: historyAt.left, top: historyAt.top }}
          >
            <div className="history-caption">
              <strong>历史抓包</strong>
              <button
                className="icon-button"
                aria-label="刷新记录"
                title="重新扫描抓包目录"
                onClick={onRefresh}
              >
                <Icon name="refresh" size={14} />
              </button>
            </div>
            <p>
              同一个 QQ
              号的多次启动归在一起；每次启动对应一份原始文件。关闭视图不会删除文件。
            </p>
            <div className="history-items">
              {[
                ...known,
                ...(unknown.launches.length || unknown.extras.length
                  ? [unknown]
                  : []),
              ].map((group) => (
                <section
                  key={group.key}
                  className="history-group"
                  aria-label={
                    group.account
                      ? `账号 ${accountText(group.account, reveal)}`
                      : "未识别账号"
                  }
                >
                  <div className="history-group-title">
                    <Icon name={group.account ? "user" : "file"} size={14} />
                    <AccountName account={group.account} reveal={reveal} />
                    {group.account && (
                      <span className="muted">
                        {group.launches.length} 次启动
                      </span>
                    )}
                  </div>
                  <LaunchList
                    group={group}
                    selected={selected}
                    merged={group === selectedGroup && merged}
                    reveal={reveal}
                    onSelect={choose}
                    onMerge={combine}
                  />
                </section>
              ))}
              {!all.length && <div className="history-empty">暂无历史记录</div>}
            </div>
            {actions}
          </div>
        )}
      </div>
    </div>
  );
}
