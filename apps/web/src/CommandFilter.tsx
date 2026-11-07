import { useEffect, useRef, useState } from "react";
import type { CommandList } from "./api.generated";
import { message, request } from "./api";
import { readPreference, writePreference } from "./labels";
import { exact, patterns, ruleLabel } from "./AdvancedFilters";
import { Icon } from "./views";

type Rules = { include?: string; exclude?: string };
type Suggestion = {
  command: string;
  count: bigint;
  inbound: bigint;
  outbound: bigint;
};
const MAX_RULES = 16;

function merge(lists: CommandList[]): {
  commands: Suggestion[];
  truncated: boolean;
} {
  const totals = new Map<string, Suggestion>();
  for (const list of lists)
    for (const item of list.commands) {
      const known = totals.get(item.command) || {
        command: item.command,
        count: 0n,
        inbound: 0n,
        outbound: 0n,
      };
      totals.set(item.command, {
        command: item.command,
        count: known.count + BigInt(item.count),
        inbound: known.inbound + BigInt(item.inbound),
        outbound: known.outbound + BigInt(item.outbound),
      });
    }
  return {
    commands: [...totals.values()],
    truncated: lists.some((list) => list.truncated),
  };
}
type Order = "name" | "length";
const byName = (a: Suggestion, b: Suggestion) =>
  a.command.localeCompare(b.command, "en", {
    numeric: true,
    sensitivity: "base",
  }) || (a.command < b.command ? -1 : a.command > b.command ? 1 : 0);
const orders: Record<Order, (a: Suggestion, b: Suggestion) => number> = {
  name: byName,
  length: (a, b) => a.command.length - b.command.length || byName(a, b),
};
const join = (values: string[]) =>
  values.length ? values.join(",") : undefined;

// 选择抓包中出现的命令，或输入包含 * 和 ? 的规则。
export function CommandFilter({
  sources,
  rules,
  onChange,
  notify,
  onOpen,
}: {
  sources: string[];
  rules: Rules;
  onChange: (next: Rules, text: string) => void;
  notify: (text: string) => void;
  onOpen?: () => void;
}) {
  const [open, setOpen] = useState(false);
  const [text, setText] = useState("");
  const [list, setList] = useState<{
    commands: Suggestion[];
    truncated: boolean;
  } | null>(null);
  const [error, setError] = useState("");
  const [order, setOrder] = useState<Order>(() =>
    readPreference("qscope.commands.order") === "length" ? "length" : "name",
  );
  const root = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const include = patterns(rules.include),
    exclude = patterns(rules.exclude);
  const active = include.length + exclude.length;
  const sourceKey = sources.join(",");
  const [previousRequest, setPreviousRequest] = useState({ open, sourceKey });
  if (
    previousRequest.open !== open ||
    previousRequest.sourceKey !== sourceKey
  ) {
    setPreviousRequest({ open, sourceKey });
    if (open) setError("");
  }

  useEffect(() => {
    if (!open) return;
    const controller = new AbortController();
    Promise.all(
      sourceKey
        .split(",")
        .filter(Boolean)
        .map((id) =>
          request<CommandList>(`/sessions/${id}/commands`, {
            signal: controller.signal,
          }),
        ),
    )
      .then((lists) => {
        if (!controller.signal.aborted) setList(merge(lists));
      })
      .catch((failure) => {
        if (!controller.signal.aborted) setError(message(failure));
      });
    const outside = (event: PointerEvent) => {
      if (!root.current?.contains(event.target as Node)) setOpen(false);
    };
    document.addEventListener("pointerdown", outside);
    return () => {
      controller.abort();
      document.removeEventListener("pointerdown", outside);
    };
  }, [open, sourceKey]);

  function set(mode: "include" | "exclude", pattern: string, on: boolean) {
    const target = mode === "include" ? include : exclude;
    if (on && !target.includes(pattern) && target.length >= MAX_RULES) {
      notify(`${mode === "include" ? "只看" : "排除"}规则最多 ${MAX_RULES} 条`);
      return;
    }
    const next = {
      include: include.filter((p) => p !== pattern),
      exclude: exclude.filter((p) => p !== pattern),
    };
    if (on) next[mode].push(pattern);
    const label = ruleLabel(pattern);
    onChange(
      { include: join(next.include), exclude: join(next.exclude) },
      on
        ? `${mode === "include" ? "只看" : "已排除"} ${label}`
        : `已取消 ${label}`,
    );
  }
  function addTyped(mode: "include" | "exclude") {
    const pattern = text.trim();
    if (!pattern) return;
    if (pattern.length > 128 || /[,\n]/.test(pattern)) {
      notify("一条规则最多 128 字节，不能包含逗号");
      return;
    }
    set(mode, pattern, true);
    setText("");
  }
  const query = text.trim().toLowerCase();
  const shown = (list?.commands || [])
    .filter((item) => !query || item.command.toLowerCase().includes(query))
    .sort(orders[order]);
  const sortBy = (next: Order) => {
    setOrder(next);
    writePreference("qscope.commands.order", next);
  };
  const known = new Set(list?.commands.map((item) => exact(item.command)));
  const typedRules = [
    ...include.map((p) => ["include", p] as const),
    ...exclude.map((p) => ["exclude", p] as const),
  ].filter(([, p]) => !known.has(p));

  return (
    <div className="command-filter" ref={root}>
      <button
        ref={trigger}
        className={`text-button filter-button ${open || active ? "active" : ""}`}
        aria-label="命令筛选"
        aria-haspopup="dialog"
        aria-expanded={open}
        title="按命令只看或排除"
        onClick={() => {
          if (!open) onOpen?.();
          setOpen((value) => !value);
        }}
      >
        <Icon name="packet" size={13} />
        命令{active > 0 && <b className="filter-count mono">{active}</b>}
        <Icon name="down" size={11} />
      </button>
      {open && (
        <div
          className="command-popover"
          role="dialog"
          aria-label="命令筛选"
          onKeyDown={(event) => {
            if (event.key === "Escape") {
              event.stopPropagation();
              setOpen(false);
              trigger.current?.focus();
            }
          }}
        >
          <div className="command-search">
            <Icon name="search" size={13} />
            <input
              aria-label="查找命令或输入规则"
              placeholder="查找命令，或输入 OidbSvc* 这样的规则"
              value={text}
              maxLength={128}
              autoFocus
              onChange={(e) => setText(e.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter") {
                  event.preventDefault();
                  addTyped(event.shiftKey ? "exclude" : "include");
                }
              }}
            />
            <div className="command-order" role="group" aria-label="命令排序">
              {(
                [
                  ["name", "字母"],
                  ["length", "长短"],
                ] as const
              ).map(([value, label]) => (
                <button
                  key={value}
                  aria-pressed={order === value}
                  title={
                    value === "name"
                      ? "按字母排序，数字按大小"
                      : "短的在前，同样长按字母"
                  }
                  onClick={() => sortBy(value)}
                >
                  {label}
                </button>
              ))}
            </div>
          </div>
          {text.trim() && (
            <div className="command-typed">
              <button
                className="text-button"
                aria-label={`只看规则 ${text.trim()}`}
                onClick={() => addTyped("include")}
              >
                <Icon name="filter" size={12} />
                只看 <b className="mono">{text.trim()}</b>
                <kbd>Enter</kbd>
              </button>
              <button
                className="text-button"
                aria-label={`排除规则 ${text.trim()}`}
                onClick={() => addTyped("exclude")}
              >
                <Icon name="exclude" size={12} />
                排除<kbd>Shift+Enter</kbd>
              </button>
              <span className="command-hint">
                {/[*?]/.test(text)
                  ? "* 和 ? 匹配完整命令名"
                  : "按包含匹配，加 * ? 可匹配完整命令名"}
              </span>
            </div>
          )}
          {typedRules.length > 0 && (
            <div className="command-rules">
              {typedRules.map(([mode, pattern]) => (
                <span key={mode + pattern} className={`command-rule ${mode}`}>
                  {mode === "include" ? "只看" : "排除"}{" "}
                  <b className="mono">{ruleLabel(pattern)}</b>
                  <button
                    aria-label={`移除规则 ${ruleLabel(pattern)}`}
                    onClick={() => set(mode, pattern, false)}
                  >
                    <Icon name="close" size={10} />
                  </button>
                </span>
              ))}
            </div>
          )}
          <div className="command-list" role="list" aria-label="本记录中的命令">
            {error ? (
              <div className="command-empty warn-text">{error}</div>
            ) : !list ? (
              <div className="command-empty">
                <span className="small-spinner" aria-hidden="true" />
                正在读取命令…
              </div>
            ) : shown.length === 0 ? (
              <div className="command-empty">
                {query
                  ? "没有匹配的命令，可以直接按 Enter 作为规则"
                  : "这份记录里还没有协议包"}
              </div>
            ) : (
              shown.slice(0, 300).map((item) => {
                const rule = exact(item.command),
                  on = include.includes(rule),
                  off = exclude.includes(rule);
                return (
                  <div
                    key={item.command}
                    role="listitem"
                    className={`command-option ${on ? "included" : ""} ${off ? "excluded" : ""}`}
                  >
                    <span className="mono command-name" title={item.command}>
                      {item.command}
                    </span>
                    <span
                      className="mono command-count"
                      title={`收 ${item.inbound} · 发 ${item.outbound}`}
                    >
                      {item.count.toLocaleString()}
                    </span>
                    <button
                      className="command-toggle"
                      aria-pressed={on}
                      aria-label={`只看 ${item.command}`}
                      title="只看这个命令"
                      onClick={() => set("include", rule, !on)}
                    >
                      只看
                    </button>
                    <button
                      className="command-toggle exclude"
                      aria-pressed={off}
                      aria-label={`排除 ${item.command}`}
                      title="排除这个命令"
                      onClick={() => set("exclude", rule, !off)}
                    >
                      排除
                    </button>
                  </div>
                );
              })
            )}
          </div>
          <div className="command-footer">
            <span>
              {list
                ? `${list.commands.length.toLocaleString()} 个命令${list.truncated ? "，只列出最常见的" : ""}${shown.length > 300 ? " · 先显示 300 个，输入可缩小范围" : ""}`
                : " "}
            </span>
            {active > 0 && (
              <button
                className="text-button"
                onClick={() => onChange({}, "已清除命令筛选")}
              >
                清除命令筛选
              </button>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
