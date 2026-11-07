import { useEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties } from "react";
import type { FieldNode, FieldText } from "./api.generated";
import { abortError, copyText, message, request } from "./api";
import { formatJson, jsonLinesText } from "./jsonText";
import type { JsonLine } from "./jsonText";
import { Modal } from "./Modal";
import { useWindow } from "./views";

const LINE = 20;
const PAD = 6;

// 格式化 JSON 使用虚拟列表，并复用字段树的行号、缩进线和配色。
function JsonReader({ lines }: { lines: JsonLine[] }) {
  const scroll = useRef<HTMLDivElement>(null);
  const { start, end } = useWindow(scroll, LINE, lines.length);
  const columns = useMemo(
    () => lines.reduce((widest, line) => Math.max(widest, line.width), 0),
    [lines],
  );
  const gutter = String(lines.length).length;
  return (
    <div
      ref={scroll}
      className="json-reader"
      role="region"
      aria-label="格式化 JSON"
      tabIndex={0}
      style={{ "--gutter": `${gutter + 2}ch` } as CSSProperties}
    >
      <div
        className="json-reader-body"
        style={{
          height: lines.length * LINE + 2 * PAD,
          width: `calc(${columns + gutter + 6}ch + 24px)`,
        }}
      >
        <div className="json-reader-window" style={{ top: start * LINE + PAD }}>
          {lines.slice(start, end).map((line, offset) => (
            <div key={start + offset} className="json-reader-line">
              <span className="json-reader-number" aria-hidden="true">
                {start + offset + 1}
              </span>
              <span
                className="json-reader-indent"
                style={{ "--depth": line.depth } as CSSProperties}
              />
              {line.tokens.map((token, index) => (
                <span
                  key={index}
                  className={
                    token.kind === "bracket"
                      ? `json-bracket level-${token.level}`
                      : `json-token-${token.kind}`
                  }
                >
                  {token.text}
                </span>
              ))}
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}

export function FieldValueDialog({
  sessionId,
  eventId,
  node,
  query,
  onClose,
  notify,
}: {
  sessionId: string;
  eventId: string;
  node: FieldNode;
  query: string;
  onClose: () => void;
  notify: (text: string) => void;
}) {
  const [value, setValue] = useState<FieldText | null>(null);
  const [error, setError] = useState("");
  const [view, setView] = useState<"json" | "text">("json");
  const input = useRef<HTMLTextAreaElement>(null);
  useEffect(() => {
    const controller = new AbortController();
    const params = new URLSearchParams(query);
    params.set("node_id", node.id);
    params.set("mode", "value");
    void request<FieldText>(
      `/sessions/${sessionId}/events/${eventId}/text?${params}`,
      { signal: controller.signal },
    )
      .then(setValue)
      .catch((error) => {
        if (!abortError(error)) setError(message(error));
      });
    return () => controller.abort();
  }, [sessionId, eventId, node.id, query]);
  // 完整 JSON 对象或数组默认格式化展示，原文保留在相邻标签页。
  const json = useMemo(() => (value ? formatJson(value.text) : null), [value]);
  const showJson = Boolean(json) && view === "json";
  function copy(text: string, done: string) {
    void copyText(text)
      .then(() => notify(done))
      .catch(() => {
        if (showJson) {
          notify("复制失败，请切换到原文后按 Ctrl+C");
          return;
        }
        input.current?.focus();
        input.current?.select();
        notify("内容已选中，请按 Ctrl+C 复制");
      });
  }
  return (
    <Modal
      title={`字段 ${node.presentation.key} · 完整值`}
      className="value-dialog"
      onClose={onClose}
    >
      <div className="full-value-toolbar">
        {json && (
          <div
            className="format-tabs value-views"
            role="tablist"
            aria-label="值的显示方式"
          >
            <button
              role="tab"
              aria-selected={view === "json"}
              onClick={() => setView("json")}
            >
              JSON
            </button>
            <button
              role="tab"
              aria-selected={view === "text"}
              onClick={() => setView("text")}
            >
              原文
            </button>
          </div>
        )}
        <span>
          {node.presentation.type_name} · {node.presentation.value_length} B{" "}
          {value && `· ${value.text.length.toLocaleString()} 字符`}
          {json && ` · ${json.length.toLocaleString()} 行`}
        </span>
        {showJson && json ? (
          <button
            className="text-button"
            onClick={() => copy(jsonLinesText(json), "格式化 JSON 已复制")}
          >
            复制格式化
          </button>
        ) : (
          <button
            className="text-button"
            disabled={!value}
            onClick={() => {
              input.current?.focus();
              input.current?.select();
            }}
          >
            全选
          </button>
        )}
        <button
          className="primary-button"
          disabled={!value}
          onClick={() => {
            if (value) copy(value.text, "完整字段值已复制");
          }}
        >
          复制完整值
        </button>
      </div>
      {error ? (
        <div className="inline-error" role="alert">
          {error}
        </div>
      ) : showJson && json ? (
        <JsonReader lines={json} />
      ) : (
        <textarea
          ref={input}
          className="full-value-text"
          aria-label="完整字段值"
          readOnly
          spellCheck={false}
          value={value?.text || ""}
          placeholder="正在读取完整字段值…"
        />
      )}
      <div className="value-note">
        {json
          ? "识别为 JSON，按原文逐字着色，数字不做换算；复制完整值始终是原始文本。"
          : "从原始包体读取，不受预览长度限制；UTF-8 与嵌套结构仍是候选解释。"}
      </div>
    </Modal>
  );
}
