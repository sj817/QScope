import { useState } from "react";
import { terms } from "./labels";

export type Advanced = Record<string, string>;
export type FilterState = {
  search: string;
  direction: string;
  controls: boolean;
  advanced: Advanced;
};
export type Chip = {
  id: string;
  label: string;
  title?: string;
  next: FilterState;
};

// 命令包含与排除规则由命令筛选管理，本面板管理其他条件。
const fields = [
  ["seq", "精确 SEQ", "十进制整数"],
  ["tid", "线程 ID", ""],
  ["min_size", "最小包长（B）", "0"],
  ["max_size", "最大包长（B）", ""],
  ["event_from", "起始事件 ID", ""],
  ["event_to", "结束事件 ID", ""],
];
const kindOptions = [
  ["", "全部类型"],
  ["packet", "协议包"],
  ["status", "生命周期"],
  ["loss", "丢弃"],
  ["end", "结束"],
];
const flags: Record<string, string> = {
  hide_heartbeat: "排除心跳 / 保活",
  hide_login: "排除登录相关命令",
  error_only: "仅错误码非零",
  truncated_only: "仅截断包",
};
const kinds: Record<string, string> = {
  packet: "仅协议包",
  status: "仅生命周期 / 状态",
  loss: "仅丢弃事件",
  end: "仅结束事件",
};
function toLocal(value?: string) {
  if (!value) return "";
  try {
    const date = new Date(
      Number((BigInt(value) - 116444736000000000n) / 10000n),
    );
    return new Date(date.getTime() - date.getTimezoneOffset() * 60000)
      .toISOString()
      .slice(0, 23);
  } catch {
    return "";
  }
}
export function patterns(value?: string): string[] {
  return (value || "")
    .split(/[,\n]/)
    .map((s) => s.trim())
    .filter(Boolean);
}
export function withPattern(
  value: string | undefined,
  pattern: string,
): string {
  return [...new Set([...patterns(value), pattern])].join(",");
}
// = 精确匹配命令，* 和 ? 匹配全名，其他文本匹配名称子串。
export const exact = (command: string) => `=${command}`;
export function ruleLabel(pattern: string): string {
  return pattern.startsWith("=")
    ? pattern.slice(1)
    : /[*?]/.test(pattern)
      ? pattern
      : `含 ${pattern}`;
}
function without(advanced: Advanced, key: string): Advanced {
  const next = { ...advanced };
  delete next[key];
  return next;
}
function timeLabel(value: string) {
  return toLocal(value).replace("T", " ") || value;
}
// 将本地时间转换为 FILETIME，空值表示无限制，非法值返回 null。
function parseLocal(text: string): string | null | undefined {
  const trimmed = text.trim();
  if (!trimmed) return undefined;
  const match =
    /^(\d{4})-(\d{1,2})-(\d{1,2})[ T](\d{1,2}):(\d{2})(?::(\d{2})(?:\.(\d{1,3}))?)?$/.exec(
      trimmed,
    );
  if (!match) return null;
  const [, y, mo, d, h, mi, sec = "0", ms = "0"] = match;
  const date = new Date(+y, +mo - 1, +d, +h, +mi, +sec, +ms.padEnd(3, "0"));
  if (
    Number.isNaN(date.getTime()) ||
    date.getMonth() !== +mo - 1 ||
    date.getDate() !== +d ||
    +h > 23 ||
    +mi > 59 ||
    +sec > 59
  )
    return null;
  return String(BigInt(date.getTime()) * 10000n + 116444736000000000n);
}

// 每个生效条件对应一个可移除标签，命令列表按规则拆分。
export function filterChips(state: FilterState): Chip[] {
  const { advanced: a } = state;
  const chips: Chip[] = [];
  const set = (advanced: Advanced): FilterState => ({ ...state, advanced });
  if (state.search)
    chips.push({
      id: "search",
      label: `搜索：${state.search}`,
      next: { ...state, search: "" },
    });
  if (state.direction !== "all")
    chips.push({
      id: "direction",
      label: state.direction === "inbound" ? "只看接收" : "只看发送",
      next: { ...state, direction: "all" },
    });
  if (state.controls)
    chips.push({
      id: "controls",
      label: "含采集事件",
      title: terms.controls,
      next: {
        ...state,
        controls: false,
        advanced: a.kinds && a.kinds !== "packet" ? without(a, "kinds") : a,
      },
    });
  for (const key of ["include", "exclude"])
    for (const pattern of patterns(a[key])) {
      const rest = patterns(a[key]).filter((p) => p !== pattern);
      chips.push({
        id: `${key}:${pattern}`,
        label: `${key === "include" ? "只看" : "排除"}：${ruleLabel(pattern)}`,
        next: set(
          rest.length ? { ...a, [key]: rest.join(",") } : without(a, key),
        ),
      });
    }
  const simple: [string, (v: string) => string][] = [
    ["seq", (v) => `SEQ = ${v}`],
    ["tid", (v) => `线程 ${v}`],
    ["min_size", (v) => `包长 ≥ ${v} B`],
    ["max_size", (v) => `包长 ≤ ${v} B`],
    ["event_from", (v) => `事件 ID ≥ ${v}`],
    ["event_to", (v) => `事件 ID ≤ ${v}`],
    ["time_from", (v) => `晚于 ${timeLabel(v)}`],
    ["time_to", (v) => `早于 ${timeLabel(v)}`],
    ["kinds", (v) => kinds[v] || v],
  ];
  for (const [key, label] of simple)
    if (a[key])
      chips.push({ id: key, label: label(a[key]), next: set(without(a, key)) });
  for (const [key, label] of Object.entries(flags))
    if (a[key] === "true")
      chips.push({ id: key, label, next: set(without(a, key)) });
  return chips;
}

export function AdvancedFilters({
  value,
  controls,
  pick,
  onApply,
  onClose,
}: {
  value: Advanced;
  controls: boolean;
  pick?: string;
  onApply: (value: Advanced, controls: boolean) => void;
  onClose: () => void;
}) {
  const [draft, setDraft] = useState(value);
  const [showControls, setShowControls] = useState(controls);
  const [times, setTimes] = useState(() => ({
    time_from: timeLabel(value.time_from || ""),
    time_to: timeLabel(value.time_to || ""),
  }));
  const [error, setError] = useState("");
  const [previousValue, setPreviousValue] = useState(value);
  const [previousControls, setPreviousControls] = useState(controls);
  if (previousValue !== value) {
    setPreviousValue(value);
    setDraft(value);
    setTimes({
      time_from: value.time_from ? timeLabel(value.time_from) : "",
      time_to: value.time_to ? timeLabel(value.time_to) : "",
    });
  }
  if (previousControls !== controls) {
    setPreviousControls(controls);
    setShowControls(controls);
  }
  const set = (key: string, next: string) =>
    setDraft((previous) => ({ ...previous, [key]: next }));
  function submit(event: React.FormEvent) {
    event.preventDefault();
    const next: Advanced = Object.fromEntries(
      Object.entries(draft).filter(
        ([key, v]) =>
          v !== "" && v !== "false" && key !== "time_from" && key !== "time_to",
      ),
    );
    for (const key of ["time_from", "time_to"] as const) {
      const parsed = parseLocal(times[key]);
      if (parsed === null) {
        setError("时间格式为 2026-11-05 06:06:40.000，毫秒和秒可以省略。");
        return;
      }
      if (parsed) next[key] = parsed;
    }
    for (const key of [
      "seq",
      "tid",
      "min_size",
      "max_size",
      "event_from",
      "event_to",
    ])
      if (next[key] && !/^\d+$/.test(next[key])) {
        setError("序号、线程、包长及事件 ID 应为非负十进制整数。");
        return;
      }
    for (const [a, b] of [
      ["min_size", "max_size"],
      ["time_from", "time_to"],
      ["event_from", "event_to"],
    ])
      if (next[a] && next[b] && BigInt(next[a]) > BigInt(next[b])) {
        setError("范围起始值不能大于结束值。");
        return;
      }
    setError("");
    onApply(
      next,
      showControls || Boolean(next.kinds && next.kinds !== "packet"),
    );
    onClose();
  }
  // 重置仅清理本面板条件，命令规则由命令筛选管理。
  function reset() {
    const kept = Object.fromEntries(
      Object.entries(value).filter(
        ([key]) => key === "include" || key === "exclude",
      ),
    );
    setShowControls(false);
    onApply(kept, false);
    onClose();
  }
  const time = (key: "time_from" | "time_to", label: string) => (
    <div className="filter-time">
      <label>
        {label}
        <input
          aria-label={label}
          className="mono"
          placeholder="如 2026-11-05 06:06:40"
          value={times[key]}
          maxLength={32}
          onChange={(e) =>
            setTimes((previous) => ({ ...previous, [key]: e.target.value }))
          }
        />
      </label>
      <button
        type="button"
        className="text-button"
        disabled={!pick}
        title={
          pick ? `填入选中包的时间 ${timeLabel(pick)}` : "先在列表中选中一个包"
        }
        onClick={() => {
          if (pick)
            setTimes((previous) => ({ ...previous, [key]: timeLabel(pick) }));
        }}
      >
        用选中包
      </button>
    </div>
  );
  return (
    <form className="advanced-filters" aria-label="高级筛选" onSubmit={submit}>
      <div className="filter-grid">
        {fields.map(([key, label, placeholder]) => (
          <label key={key}>
            {label}
            <input
              aria-label={label}
              className="mono"
              inputMode="numeric"
              value={draft[key] || ""}
              maxLength={30}
              placeholder={placeholder}
              onChange={(e) => set(key, e.target.value)}
            />
          </label>
        ))}
      </div>
      {time("time_from", "开始时间")}
      {time("time_to", "结束时间")}
      <div className="filter-kinds">
        <span>记录类型</span>
        <div className="choice-group" role="radiogroup" aria-label="记录类型">
          {kindOptions.map(([key, label]) => (
            <button
              type="button"
              role="radio"
              key={key}
              aria-checked={(draft.kinds || "") === key}
              onClick={() => set("kinds", key)}
            >
              {label}
            </button>
          ))}
        </div>
      </div>
      <div className="filter-options">
        <label className="checkbox-label" title={terms.controls}>
          <input
            type="checkbox"
            checked={showControls}
            onChange={(e) => setShowControls(e.target.checked)}
          />
          显示采集事件（启动、丢包、结束）
        </label>
        {Object.entries(flags).map(([key, label]) => (
          <label className="checkbox-label" key={key}>
            <input
              type="checkbox"
              checked={draft[key] === "true"}
              onChange={(e) => set(key, String(e.target.checked))}
            />
            {label}
          </label>
        ))}
      </div>
      {error && (
        <div className="warn-text filter-error" role="alert">
          {error}
        </div>
      )}
      <div className="filter-actions">
        <span className="muted">按命令筛选请用旁边的“命令”。</span>
        <button type="button" className="text-button" onClick={reset}>
          重置
        </button>
        <button type="button" className="text-button" onClick={onClose}>
          收起
        </button>
        <button className="primary-button" type="submit">
          应用筛选
        </button>
      </div>
    </form>
  );
}
