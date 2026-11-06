import { useEffect, useId, useMemo, useRef, useState } from "react";
import type { CSSProperties } from "react";
import type { Comparison, EventSummary } from "./api.generated";
import { abortError, message, request, size } from "./api";
import { Detail } from "./Detail";
import { Icon, useWindow } from "./views";
import { Modal } from "./Modal";

export type PacketChoice = { sessionId: string; event: EventSummary };
export function choiceKey(item: PacketChoice) {
  return `${item.sessionId}:${item.event.event_id}`;
}

// A select-only combobox styled like the rest of the workbench.
function Picker<T extends string | number>({
  label,
  value,
  options,
  onChange,
}: {
  label: string;
  value: T;
  options: [T, string][];
  onChange: (value: T) => void;
}) {
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(0);
  const root = useRef<HTMLDivElement>(null);
  const id = useId();
  const current = Math.max(
    0,
    options.findIndex(([v]) => v === value),
  );
  useEffect(() => {
    if (!open) return;
    setActive(current);
    const outside = (event: PointerEvent) => {
      if (!root.current?.contains(event.target as Node)) setOpen(false);
    };
    document.addEventListener("pointerdown", outside);
    return () => document.removeEventListener("pointerdown", outside);
  }, [open]);
  const pick = (index: number) => {
    onChange(options[index][0]);
    setOpen(false);
  };
  function onKeyDown(event: React.KeyboardEvent) {
    const last = options.length - 1;
    if (!open) {
      if (["ArrowDown", "ArrowUp", "Enter", " "].includes(event.key)) {
        event.preventDefault();
        setOpen(true);
      }
      return;
    }
    const moves: Record<string, number> = {
      ArrowDown: Math.min(last, active + 1),
      ArrowUp: Math.max(0, active - 1),
      Home: 0,
      End: last,
    };
    if (event.key in moves) {
      event.preventDefault();
      setActive(moves[event.key]);
    } else if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      pick(active);
    } else if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      setOpen(false);
    } else if (event.key === "Tab") setOpen(false);
  }
  return (
    <div className="picker" ref={root}>
      <button
        type="button"
        role="combobox"
        aria-label={label}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-controls={open ? id : undefined}
        aria-activedescendant={open ? `${id}-${active}` : undefined}
        onClick={() => setOpen((value) => !value)}
        onKeyDown={onKeyDown}
      >
        <span>{options[current]?.[1]}</span>
        <Icon name="down" size={11} />
      </button>
      {open && (
        <div className="picker-list" role="listbox" id={id} aria-label={label}>
          {options.map(([v, text], index) => (
            <div
              key={String(v)}
              id={`${id}-${index}`}
              role="option"
              aria-selected={v === value}
              className={index === active ? "active" : ""}
              onPointerMove={() => setActive(index)}
              onClick={() => pick(index)}
            >
              {text}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

export function PacketPopup({
  item,
  onClose,
  notify,
}: {
  item: PacketChoice;
  onClose: () => void;
  notify: (text: string) => void;
}) {
  return (
    <Modal title="包详情弹窗" onClose={onClose}>
      <Detail sessionId={item.sessionId} event={item.event} notify={notify} />
    </Modal>
  );
}

function FieldComparison({
  data,
  different,
  baseline,
}: {
  data: Comparison;
  different: boolean;
  baseline: number;
}) {
  const rows = useMemo(
    () => data.rows.filter((row) => !different || row.changed),
    [data, different],
  );
  const scroll = useRef<HTMLDivElement>(null);
  const { start, end } = useWindow(scroll, 36, rows.length);
  const style = { "--compare-columns": data.columns.length } as CSSProperties;
  return (
    <div
      className="comparison-scroll"
      ref={scroll}
      aria-label="字段对比"
      style={style}
    >
      <div className="comparison-grid comparison-head">
        <strong>字段路径 / 重复项</strong>
        {data.columns.map((column, i) => (
          <strong key={i} title={column.event.command}>
            {i === baseline ? "基准 · " : ""}#{column.event.event_id} ·{" "}
            {column.event.command}
          </strong>
        ))}
      </div>
      <div className="comparison-virtual" style={{ height: rows.length * 36 }}>
        <div
          style={{ position: "absolute", top: start * 36, left: 0, right: 0 }}
        >
          {rows.slice(start, end).map((row) => (
            <div
              key={row.path}
              className={`comparison-grid comparison-row ${row.changed ? "has-difference" : ""}`}
            >
              <span className="mono" title={row.path}>
                {row.path}
              </span>
              {row.values.map((value, i) => (
                <span
                  key={i}
                  className={`mono ${value !== row.values[baseline] || row.fingerprints[i] !== row.fingerprints[baseline] ? "difference" : ""} ${value === null ? "missing" : ""}`}
                  title={
                    (value ?? "字段不存在") +
                    (row.fingerprints[i]
                      ? " · 字段字节 SHA256 " + row.fingerprints[i]
                      : "")
                  }
                >
                  {value ?? "— 不存在"}
                </span>
              ))}
            </div>
          ))}
        </div>
      </div>
      {!rows.length && (
        <div className="tree-empty">
          {different
            ? "没有字段差异"
            : "当前格式没有可对比的结构字段，请查看字节对比"}
        </div>
      )}
    </div>
  );
}

function ByteComparison({
  data,
  different,
  baseline,
}: {
  data: Comparison;
  different: boolean;
  baseline: number;
}) {
  const rows = useMemo(() => {
    const count = Math.ceil(
      Math.max(...data.columns.map((c) => c.payload_hex.length)) / 32,
    );
    return Array.from({ length: count }, (_, i) => i).filter(
      (i) =>
        !different ||
        data.columns.some(
          (c) =>
            c.payload_hex.slice(i * 32, i * 32 + 32) !==
            data.columns[baseline].payload_hex.slice(i * 32, i * 32 + 32),
        ),
    );
  }, [data, different, baseline]);
  const scroll = useRef<HTMLDivElement>(null);
  const { start, end } = useWindow(scroll, 36, rows.length);
  return (
    <div
      className="comparison-scroll byte-comparison"
      ref={scroll}
      aria-label="字节对比"
      style={{ "--compare-columns": data.columns.length } as CSSProperties}
    >
      <div className="comparison-grid comparison-head">
        <strong>OFFSET · 按偏移对齐</strong>
        {data.columns.map((c, i) => (
          <strong key={i}>
            {i === baseline ? "基准 · " : ""}#{c.event.event_id} ·{" "}
            {size(c.event.captured_length)}
          </strong>
        ))}
      </div>
      <div className="comparison-virtual" style={{ height: rows.length * 36 }}>
        <div
          style={{ position: "absolute", top: start * 36, left: 0, right: 0 }}
        >
          {rows.slice(start, end).map((row) => (
            <div className="comparison-grid comparison-row" key={row}>
              <span className="mono">
                {(row * 16).toString(16).padStart(8, "0")}
              </span>
              {data.columns.map((column, i) => (
                <span className="mono compare-bytes" key={i}>
                  {Array.from({ length: 16 }, (_, j) => {
                    const at = (row * 16 + j) * 2;
                    const byte = column.payload_hex.slice(at, at + 2);
                    const base = data.columns[baseline].payload_hex.slice(
                      at,
                      at + 2,
                    );
                    return (
                      <span
                        key={j}
                        className={byte !== base ? "difference" : ""}
                      >
                        {byte || "--"}
                      </span>
                    );
                  })}
                </span>
              ))}
            </div>
          ))}
        </div>
      </div>
      {!rows.length && <div className="tree-empty">没有字节差异</div>}
    </div>
  );
}

export function ComparePopup({
  items,
  onClose,
}: {
  items: PacketChoice[];
  onClose: () => void;
}) {
  const [format, setFormat] = useState("pb");
  const [mode, setMode] = useState("fields");
  const [baseline, setBaseline] = useState(0);
  const [different, setDifferent] = useState(true);
  const [data, setData] = useState<Comparison | null>(null);
  const [error, setError] = useState("");
  useEffect(() => {
    const controller = new AbortController();
    setData(null);
    setError("");
    void request<Comparison>("/compare", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        items: items.map((item) => ({
          session_id: item.sessionId,
          event_id: item.event.event_id,
        })),
        format,
      }),
      signal: controller.signal,
    })
      .then(setData)
      .catch((error) => {
        if (!abortError(error)) setError(message(error));
      });
    return () => controller.abort();
  }, [items, format]);
  return (
    <Modal title={`多包对比 · ${items.length} 个包`} wide onClose={onClose}>
      <div className="comparison-toolbar">
        <Picker
          label="对比格式"
          value={format}
          options={[
            ["pb", "Protobuf"],
            ["tlv16be", "TLV16-BE"],
            ["jce", "JCE / Tars"],
            ["hex", "仅字节"],
          ]}
          onChange={setFormat}
        />
        <button
          className={`text-button ${mode === "fields" ? "active" : ""}`}
          onClick={() => setMode("fields")}
        >
          字段
        </button>
        <button
          className={`text-button ${mode === "bytes" ? "active" : ""}`}
          onClick={() => setMode("bytes")}
        >
          字节
        </button>
        <span className="picker-label">
          基准
          <Picker
            label="对比基准"
            value={baseline}
            options={items.map(
              (item, i) =>
                [i, `#${item.event.event_id} · ${item.event.command}`] as [
                  number,
                  string,
                ],
            )}
            onChange={setBaseline}
          />
        </span>
        <label className="checkbox-label">
          <input
            type="checkbox"
            checked={different}
            onChange={(e) => setDifferent(e.target.checked)}
          />
          只看差异
        </label>
      </div>
      {error && (
        <div className="inline-error" role="alert">
          {error}
        </div>
      )}
      {!data && !error && (
        <div className="tree-empty">正在读取原始包并对比…</div>
      )}
      {data && (
        <>
          {data.view_limited && (
            <div className="inline-error">
              字段视图达到限额，原始字节仍完整可对比。
            </div>
          )}
          {data.columns.some((c) => c.error) && (
            <div className="comparison-note">
              部分包未完整解码，已保留解析出的字段与全部原始字节。
            </div>
          )}
          {mode === "fields" && format !== "hex" ? (
            <FieldComparison
              data={data}
              baseline={baseline}
              different={different}
            />
          ) : (
            <ByteComparison
              data={data}
              baseline={baseline}
              different={different}
            />
          )}
          <div className="comparison-note">
            按字段号、类型与重复项顺序对齐；嵌套结构仍为候选。不同包的账号、票据和随机值也会形成差异。
          </div>
        </>
      )}
    </Modal>
  );
}
