import { useCallback, useEffect, useRef, useState } from "react";
import type {
  ByteRange,
  EventSummary,
  FieldNode,
  FieldText,
  PacketDetail,
} from "./api.generated";
import { abortError, copyText, fileTime, message, request, size } from "./api";
import { Direction, HexView, Icon } from "./views";
import {
  FieldTree,
  initialExpanded,
  mergeNodes,
  visibleChildren,
} from "./JsonTree";
import { FieldValueDialog } from "./FieldValueDialog";
import { ContextMenu, type MenuRequest } from "./ContextMenu";
import { kindLabel, readPreference, terms, writePreference } from "./labels";

const formats = [
  { id: "pb", name: "Protobuf" },
  { id: "tlv16be", name: "TLV16-BE" },
  { id: "jce", name: "JCE / Tars" },
  { id: "http", name: "HTTP" },
  { id: "hex", name: "Hex" },
];

// 标题中的带标签信息支持点击复制适合粘贴的值。
function Fact({
  label,
  value,
  title,
  tone,
  onCopy,
}: {
  label: string;
  value: string;
  title?: string;
  tone?: "warn";
  onCopy?: () => void;
}) {
  return (
    <div className={`detail-fact ${tone || ""}`} title={title}>
      <dt>{label}</dt>
      <dd className="mono">
        {onCopy ? (
          <button
            aria-label={`复制 ${label}`}
            title={`${title ? `${title}\n` : ""}点击复制`}
            onClick={onCopy}
          >
            {value}
            <Icon name="copy" size={11} />
          </button>
        ) : (
          value
        )}
      </dd>
    </div>
  );
}

function containing(
  nodes: FieldNode[],
  offset: number,
  parents: string[] = [],
): { node: FieldNode; parents: string[] } | null {
  for (const node of nodes) {
    if (offset >= node.range.start && offset < node.range.end) {
      return (
        containing(visibleChildren(node), offset, [...parents, node.id]) || {
          node,
          parents,
        }
      );
    }
  }
  return null;
}
function ancestors(
  nodes: FieldNode[],
  id: string,
  parents: string[] = [],
): string[] | null {
  for (const node of nodes) {
    if (node.id === id) return parents;
    const found = ancestors(visibleChildren(node), id, [...parents, node.id]);
    if (found) return found;
  }
  return null;
}

function allBranches(nodes: FieldNode[]): string[] {
  return nodes.flatMap((node) =>
    visibleChildren(node).length
      ? [node.id, ...allBranches(visibleChildren(node))]
      : [],
  );
}

export type DetailExtras = {
  compared?: boolean;
  onCompare?: () => void;
  compareFull?: boolean;
  onLocate?: () => void;
  onBack?: () => void;
  outside?: boolean;
  launch?: string;
};

export function Detail({
  sessionId,
  event,
  notify,
  onPopup,
  extras = {},
}: {
  sessionId: string;
  event: EventSummary;
  notify: (text: string) => void;
  onPopup?: () => void;
  extras?: DetailExtras;
}) {
  const [format, setFormat] = useState(
    event.command.startsWith("HTTP ") ? "http" : "pb",
  );
  const [detail, setDetail] = useState<PacketDetail | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [range, setRange] = useState<{ offset: number; length?: number }>({
    offset: 0,
  });
  const [startText, setStartText] = useState("0");
  const [lengthText, setLengthText] = useState("");
  const [rangeOpen, setRangeOpen] = useState(false);
  const [selection, setSelection] = useState<ByteRange | null>(null);
  const [selectedNode, setSelectedNode] = useState<string | null>(null);
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [highlight, setHighlight] = useState("");
  const [fullValue, setFullValue] = useState<FieldNode | null>(null);
  const [menu, setMenu] = useState<MenuRequest | null>(null);
  const [hexOpen, setHexOpen] = useState(
    () => readPreference("qscope.hex.collapsed") !== "true",
  );
  const hexScroll = useRef<HTMLDivElement>(null);
  const treeScroll = useRef<HTMLDivElement>(null);
  const [pageOffset, setPageOffset] = useState<number | null>(null);
  // 后续字段页沿用首批展开状态，全部折叠时继续保持折叠。
  const openNew = useRef(true);
  const rangeAnchor = useRef<HTMLDivElement>(null);
  const packet = event.kind === "packet";
  const [expandedFor, setExpandedFor] = useState({ highlight, detail });
  if (expandedFor.highlight !== highlight || expandedFor.detail !== detail) {
    setExpandedFor({ highlight, detail });
    if (highlight && detail) setExpanded(new Set(allBranches(detail.nodes)));
  }
  function toggleHex() {
    const next = !hexOpen;
    setHexOpen(next);
    writePreference("qscope.hex.collapsed", String(!next));
  }
  useEffect(() => {
    if (!rangeOpen) return;
    const outside = (event: PointerEvent) => {
      if (!rangeAnchor.current?.contains(event.target as Node))
        setRangeOpen(false);
    };
    document.addEventListener("pointerdown", outside);
    return () => document.removeEventListener("pointerdown", outside);
  }, [rangeOpen]);

  const query = new URLSearchParams({ format, offset: String(range.offset) });
  if (range.length !== undefined) query.set("length", String(range.length));
  const detailUrl = `/sessions/${sessionId}/events/${event.event_id}?${query}`;
  const [previousUrl, setPreviousUrl] = useState(detailUrl);
  const queryChanged = previousUrl !== detailUrl;
  if (queryChanged) {
    setPreviousUrl(detailUrl);
    setLoading(true);
    setError("");
    setPageOffset(null);
  }
  useEffect(() => {
    const controller = new AbortController();
    openNew.current = true;
    request<PacketDetail>(detailUrl, { signal: controller.signal })
      .then((result) => {
        if (!controller.signal.aborted) {
          setDetail(result);
          setSelectedNode(null);
          setExpanded(initialExpanded(result.nodes));
          setLoading(false);
        }
      })
      .catch((error) => {
        if (!abortError(error)) {
          setError(message(error));
          setLoading(false);
        }
      });
    return () => {
      controller.abort();
    };
  }, [detailUrl]);
  const nextOffset = detail?.next_node_offset;
  // 搜索持续请求后续页，手动加载与自动加载共用同一个请求状态。
  if (
    !queryChanged &&
    highlight &&
    nextOffset != null &&
    !loading &&
    !error &&
    pageOffset === null
  )
    setPageOffset(nextOffset);
  const loadingMore = pageOffset !== null;
  const loadMore = useCallback(() => {
    if (nextOffset == null || loading || pageOffset !== null) return;
    setError("");
    setPageOffset(nextOffset);
  }, [nextOffset, loading, pageOffset]);
  useEffect(() => {
    if (pageOffset === null || loading) return;
    const controller = new AbortController();
    void request<PacketDetail>(`${detailUrl}&node_offset=${pageOffset}`, {
      signal: controller.signal,
    })
      .then((page) => {
        if (controller.signal.aborted) return;
        setPageOffset(null);
        setDetail(
          (previous) =>
            previous && {
              ...previous,
              nodes: mergeNodes(previous.nodes, page.nodes),
              next_node_offset: page.next_node_offset,
              view_limited: page.view_limited,
            },
        );
        if (openNew.current)
          setExpanded(
            (previous) =>
              new Set([...previous, ...initialExpanded(page.nodes)]),
          );
      })
      .catch((error) => {
        if (!abortError(error)) {
          setError(message(error));
          setPageOffset(null);
        }
      });
    return () => controller.abort();
  }, [detailUrl, pageOffset, loading]);

  function scrollHex(start: number) {
    if (hexScroll.current)
      hexScroll.current.scrollTop = Math.max(
        0,
        Math.floor(start / 16) * 24 - 24,
      );
  }
  function selectField(node: FieldNode) {
    setSelectedNode(node.id);
    setSelection(node.range);
    scrollHex(node.range.start);
  }
  function selectByte(offset: number) {
    const found = detail ? containing(detail.nodes, offset) : null;
    if (found) {
      setSelectedNode(found.node.id);
      setSelection(found.node.range);
      setExpanded((previous) => new Set([...previous, ...found.parents]));
    } else {
      setSelectedNode(null);
      setSelection({ start: offset, end: offset + 1 });
    }
  }
  async function copy(selected = false) {
    if (!detail) return;
    const hex =
      selected && selection
        ? detail.payload_hex.slice(selection.start * 2, selection.end * 2)
        : detail.payload_hex;
    try {
      await copyText(hex);
      notify(
        selected
          ? "选区 Hex 已复制"
          : event.truncated
            ? "已复制已保存部分的 Hex（原包已截断）"
            : "包体 Hex 已复制",
      );
    } catch {
      notify("复制失败，请检查浏览器剪贴板权限");
    }
  }
  async function copyPlain(text: string, done: string) {
    try {
      await copyText(text);
      notify(done);
    } catch {
      notify("复制失败，请检查浏览器剪贴板权限");
    }
  }
  function download() {
    if (!detail) return;
    const bytes = new Uint8Array(detail.payload_hex.length / 2);
    for (let i = 0; i < bytes.length; i++)
      bytes[i] = parseInt(detail.payload_hex.slice(i * 2, i * 2 + 2), 16);
    const url = URL.createObjectURL(
      new Blob([bytes], { type: "application/octet-stream" }),
    );
    const anchor = document.createElement("a");
    anchor.href = url;
    anchor.download = `qscope-event-${event.event_id}.bin`;
    anchor.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  }
  function applyRange(offset: number, length?: number) {
    if (
      !Number.isSafeInteger(offset) ||
      offset < 0 ||
      (length !== undefined && (!Number.isSafeInteger(length) || length < 0)) ||
      offset + (length ?? 0) > event.captured_length
    ) {
      setError("解码区域超出包体，起始和长度应为非负整数。");
      return;
    }
    setRange({ offset, length });
    setStartText(String(offset));
    setLengthText(length === undefined ? "" : String(length));
  }

  const textQuery = new URLSearchParams({
    format,
    offset: String(range.offset),
  });
  if (range.length !== undefined) textQuery.set("length", String(range.length));
  async function copyField(
    mode: "value" | "subtree" | "structure",
    nodeId = selectedNode,
  ) {
    if (mode !== "structure" && !nodeId) return;
    const params = new URLSearchParams(textQuery);
    params.set("mode", mode);
    if (nodeId && mode !== "structure") params.set("node_id", nodeId);
    try {
      const result = await request<FieldText>(
        `/sessions/${sessionId}/events/${event.event_id}/text?${params}`,
      );
      await copyText(result.text);
      notify(
        !result.complete
          ? "已复制；结构含未解码或限量部分"
          : mode === "value"
            ? "完整字段值已复制"
            : "JSON 已复制，整数保留全部位数",
      );
    } catch (error) {
      notify(message(error));
    }
  }
  function locateField(node: FieldNode) {
    if (!hexOpen) toggleHex();
    setSelectedNode(node.id);
    setSelection(node.range);
    if (detail)
      setExpanded(
        (previous) =>
          new Set([...previous, ...(ancestors(detail.nodes, node.id) || [])]),
      );
    requestAnimationFrame(() => scrollHex(node.range.start));
  }
  // 右键打开菜单，选中字段通过菜单动作修改。
  function fieldMenu(
    node: FieldNode,
    path: string,
    x: number,
    y: number,
    origin: HTMLElement,
  ) {
    const branch = visibleChildren(node).length > 0;
    const open = expanded.has(node.id);
    const content =
      node.presentation.value_range.end > node.presentation.value_range.start
        ? node.presentation.value_range
        : node.range;
    setMenu({
      x,
      y,
      title: `字段 ${path}`,
      returnFocus: origin,
      groups: [
        {
          label: "查看",
          items: [
            {
              id: "locate",
              label: "在 Hex 中定位",
              icon: "target",
              run: () => locateField(node),
            },
            {
              id: "full",
              label: "查看完整值",
              icon: "popup",
              disabled: branch ? "对象字段请复制子树" : undefined,
              run: () => setFullValue(node),
            },
            {
              id: "expand",
              label: open ? "折叠子树" : "展开子树",
              icon: open ? "up" : "down",
              disabled: branch ? undefined : "没有子字段",
              run: () => {
                const next = new Set(expanded);
                if (open) next.delete(node.id);
                else next.add(node.id);
                setExpanded(next);
              },
            },
          ],
        },
        {
          label: "复制",
          items: [
            {
              id: "value",
              label: "复制值",
              icon: "copy",
              run: () => void copyField("value", node.id),
            },
            {
              id: "path",
              label: "复制字段路径",
              icon: "path",
              run: () => void copyPlain(path, `字段路径 ${path} 已复制`),
            },
            {
              id: "subtree",
              label: "复制子树 JSON",
              icon: "copy",
              disabled: branch ? undefined : "没有子字段",
              run: () => void copyField("subtree", node.id),
            },
            {
              id: "hex",
              label: "复制此字段 Hex",
              icon: "hex",
              disabled: detail ? undefined : "包体尚未读取",
              run: () => {
                if (detail)
                  void copyPlain(
                    detail.payload_hex.slice(
                      node.range.start * 2,
                      node.range.end * 2,
                    ),
                    "字段 Hex 已复制",
                  );
              },
            },
          ],
        },
        {
          label: "解析",
          items: [
            {
              id: "parse",
              label: "只解析此字段的内容",
              icon: "filter",
              disabled:
                content.end > content.start ? undefined : "字段没有内容字节",
              run: () => applyRange(content.start, content.end - content.start),
            },
          ],
        },
      ],
    });
  }

  // 低频包操作共用菜单，为命令名称保留宽度。
  function moreMenu(button: HTMLElement) {
    const box = button.getBoundingClientRect();
    const view = [
      ...(extras.onLocate
        ? [
            {
              id: "locate",
              label: "在列表中定位",
              icon: "target",
              disabled: extras.outside ? "此包不在当前筛选结果中" : undefined,
              run: extras.onLocate,
            },
          ]
        : []),
      ...(onPopup
        ? [{ id: "popup", label: "弹窗查看", icon: "popup", run: onPopup }]
        : []),
    ];
    const unread = detail ? undefined : "包体尚未读取";
    setMenu({
      x: box.left,
      y: box.bottom + 4,
      title: `${packet ? "包" : "采集事件"} #${event.event_id}`,
      returnFocus: button,
      groups: [
        ...(view.length ? [{ label: "查看", items: view }] : []),
        {
          label: "复制与导出",
          items: [
            {
              id: "command",
              label: packet ? "复制命令" : "复制事件名",
              icon: "copy",
              run: () =>
                void copyPlain(
                  event.command,
                  packet ? "命令已复制" : "事件名已复制",
                ),
            },
            ...(packet
              ? [
                  {
                    id: "hex",
                    label: event.truncated
                      ? "复制包体 Hex（仅已保存部分）"
                      : "复制包体 Hex",
                    icon: "hex",
                    disabled: unread,
                    run: () => void copy(),
                  },
                  {
                    id: "export",
                    label: "导出原始包体",
                    icon: "download",
                    disabled: unread,
                    run: download,
                  },
                ]
              : []),
          ],
        },
      ],
    });
  }

  const ranged = range.offset !== 0 || range.length !== undefined;
  const status = loading ? (
    <span className="decode-badge">
      <span className="small-spinner" aria-hidden="true" />
      <span className="badge-text">解码中</span>
    </span>
  ) : detail?.error ? (
    <span
      className="decode-badge warning"
      title={`${detail.error.message}。偏移 ${detail.error.offset} 之后保留为原始字节。`}
    >
      <Icon name="warning" size={12} />
      <span className="badge-text">部分解析</span>
    </span>
  ) : detail ? (
    <span
      className="decode-badge ok"
      title="结构完整；字段含义与嵌套解释仍需确认。右键字段可复制或定位。"
    >
      <Icon name="check" size={12} />
      <span className="badge-text">结构完整</span>
    </span>
  ) : null;

  const copyFact = (label: string, value: string) => () =>
    void copyPlain(value, `${label} 已复制`);
  return (
    <section className="detail-pane" aria-label="包详情">
      <header className="detail-head">
        <div className="detail-title">
          {extras.onBack && (
            <button
              className="text-button compact-back"
              onClick={extras.onBack}
            >
              <Icon name="left" size={13} />
              返回列表
            </button>
          )}
          {packet ? (
            <Direction value={event.direction} />
          ) : (
            <span className="control-tag">{kindLabel(event.kind)}</span>
          )}
          <h2 className="mono" title={event.command}>
            {event.command}
          </h2>
          <span className="detail-id mono">#{event.event_id}</span>
          {extras.outside && (
            <span
              className="detail-outside"
              role="status"
              title="此包不在当前筛选结果中，详情保持不变。"
            >
              不在筛选结果中
            </span>
          )}
          <div className="detail-actions">
            {extras.onLocate && (
              <button
                className="icon-button"
                title="在列表中定位此包"
                aria-label="在列表中定位"
                disabled={extras.outside}
                onClick={extras.onLocate}
              >
                <Icon name="target" size={15} />
              </button>
            )}
            {packet && extras.onCompare && (
              <button
                className="text-button"
                aria-pressed={extras.compared}
                disabled={!extras.compared && extras.compareFull}
                title={
                  extras.compareFull && !extras.compared
                    ? "最多对比 8 个包"
                    : terms.compare
                }
                onClick={extras.onCompare}
              >
                <Icon name="compare" size={14} />
                <span className="action-label">
                  {extras.compared ? "移出对比" : "加入对比"}
                </span>
              </button>
            )}
            <button
              className="text-button"
              title="复制 JSON 结构，整数保留全部位数"
              aria-label="复制 JSON 结构"
              disabled={!detail?.nodes.length}
              onClick={() => void copyField("structure")}
            >
              <Icon name="copy" size={14} />
              <span className="action-label">复制 JSON</span>
            </button>
            <button
              className="icon-button"
              title="更多：弹窗查看、复制 Hex、导出"
              aria-label="更多操作"
              aria-haspopup="menu"
              onClick={(event) => moreMenu(event.currentTarget)}
            >
              <Icon name="more" size={15} />
            </button>
          </div>
        </div>
        <dl className="detail-facts">
          {packet && (
            <Fact
              label="SEQ"
              value={event.sequence}
              title={terms.seq}
              onCopy={copyFact("SEQ", event.sequence)}
            />
          )}
          <Fact label="时间" value={fileTime(event.timestamp_filetime)} />
          {packet && (
            <Fact
              label="大小"
              value={
                event.truncated
                  ? `${size(event.captured_length)} / ${size(event.original_length)} 已截断`
                  : size(event.captured_length)
              }
              title={event.truncated ? terms.truncated : undefined}
              tone={event.truncated ? "warn" : undefined}
            />
          )}
          {extras.launch && <Fact label="启动" value={extras.launch} />}
          <Fact
            label="线程"
            value={String(event.tid)}
            onCopy={copyFact("线程 ID", String(event.tid))}
          />
          {packet && (
            <Fact
              label="错误码"
              value={String(event.error_code)}
              title="QQ 在这条记录上报告的错误码"
              tone={event.error_code !== 0 ? "warn" : undefined}
            />
          )}
          {detail && packet && (
            <Fact
              label="SHA256"
              value={`${detail.payload_sha256.slice(0, 12)}…`}
              title={`包体 SHA256 ${detail.payload_sha256}`}
              onCopy={copyFact("SHA256", detail.payload_sha256)}
            />
          )}
        </dl>
      </header>
      <div className={`detail-grid ${packet && hexOpen ? "with-bytes" : ""}`}>
        <div className="detail-main">
          {packet && (
            <div className="detail-tools">
              <div className="tools-view">
                <div
                  className="format-tabs"
                  role="tablist"
                  aria-label="查看格式"
                >
                  {formats.map((option) => (
                    <button
                      role="tab"
                      aria-selected={format === option.id}
                      key={option.id}
                      onClick={() => {
                        setFormat(option.id);
                        if (option.id === "hex" && !hexOpen) toggleHex();
                      }}
                    >
                      {option.name}
                    </button>
                  ))}
                </div>
                {status}
                {detail?.view_limited && detail.next_node_offset == null && (
                  <span
                    className="decode-badge warning"
                    title="字段超过视图上限，只显示了前一部分；原始字节仍完整"
                  >
                    已限量
                  </span>
                )}
              </div>
              {/* 空间足够时工具共用一行，否则独立换行。 */}
              <div className="tools-tree">
                {format !== "hex" && (
                  <>
                    <label
                      className={`tree-search ${highlight ? "active" : ""}`}
                      title="查找并高亮字段"
                    >
                      <Icon name="search" size={13} />
                      <input
                        type="search"
                        aria-label="高亮字段"
                        placeholder="查找字段…"
                        value={highlight}
                        maxLength={128}
                        onChange={(e) => setHighlight(e.target.value)}
                      />
                    </label>
                    <button
                      className="text-button tool-button"
                      aria-label="全部展开"
                      title="展开所有字段"
                      disabled={!detail}
                      onClick={() => {
                        openNew.current = true;
                        setExpanded(new Set(allBranches(detail?.nodes || [])));
                      }}
                    >
                      <Icon name="unfold" size={14} />
                      <span className="tool-label">展开</span>
                    </button>
                    <button
                      className="text-button tool-button"
                      aria-label="全部折叠"
                      title="折叠所有字段"
                      disabled={!detail}
                      onClick={() => {
                        openNew.current = false;
                        setExpanded(new Set());
                      }}
                    >
                      <Icon name="fold" size={14} />
                      <span className="tool-label">折叠</span>
                    </button>
                  </>
                )}
                <div className="range-anchor" ref={rangeAnchor}>
                  <button
                    className={`text-button tool-button range-button ${ranged ? "active" : ""}`}
                    aria-label="解析范围"
                    aria-expanded={rangeOpen}
                    title="只解析包体中的一段，偏移按字节计"
                    onClick={() => setRangeOpen((open) => !open)}
                  >
                    <Icon name="range" size={13} />
                    {ranged ? (
                      <span className="mono">{`[${range.offset}, ${range.length === undefined ? "末尾" : range.offset + range.length})`}</span>
                    ) : (
                      <span className="tool-label">范围</span>
                    )}
                  </button>
                  {rangeOpen && (
                    <form
                      className="range-popover"
                      onKeyDown={(e) => {
                        if (e.key === "Escape") {
                          e.stopPropagation();
                          setRangeOpen(false);
                        }
                      }}
                      onSubmit={(e) => {
                        e.preventDefault();
                        const start = startText.trim() || "0",
                          length = lengthText.trim();
                        if (
                          !/^[0-9]{1,15}$/.test(start) ||
                          (length && !/^[0-9]{1,15}$/.test(length))
                        ) {
                          setError(
                            "解码区域超出包体，起始和长度应为非负整数。",
                          );
                          return;
                        }
                        applyRange(
                          Number(start),
                          length ? Number(length) : undefined,
                        );
                        setRangeOpen(false);
                      }}
                    >
                      <label>
                        起始
                        <input
                          aria-label="解码起始偏移"
                          className="mono"
                          inputMode="numeric"
                          maxLength={15}
                          value={startText}
                          onChange={(e) => setStartText(e.target.value)}
                          autoFocus
                        />
                        <span>字节</span>
                      </label>
                      <label>
                        长度
                        <input
                          aria-label="解码长度"
                          className="mono"
                          inputMode="numeric"
                          maxLength={15}
                          placeholder="至末尾"
                          value={lengthText}
                          onChange={(e) => setLengthText(e.target.value)}
                        />
                        <span>字节</span>
                      </label>
                      <div className="range-actions">
                        <button
                          className="text-button"
                          type="button"
                          disabled={!ranged}
                          onClick={() => {
                            applyRange(0);
                            setRangeOpen(false);
                          }}
                        >
                          整包
                        </button>
                        <button className="primary-button" type="submit">
                          应用
                        </button>
                      </div>
                    </form>
                  )}
                </div>
                {!hexOpen && (
                  <button
                    className="text-button tool-button"
                    aria-label="展开原始字节"
                    title="在旁边显示原始字节"
                    onClick={toggleHex}
                  >
                    <Icon name="hex" size={13} />
                    <span className="tool-label">字节</span>
                  </button>
                )}
              </div>
            </div>
          )}
          {error && (
            <div className="inline-error" role="alert">
              {error}
            </div>
          )}
          {packet && format !== "hex" && (
            <div className="decode-area">
              <div className="tree-scroll" ref={treeScroll}>
                {!loading && detail && (
                  <>
                    <FieldTree
                      nodes={detail.nodes}
                      selected={selectedNode}
                      expanded={expanded}
                      setExpanded={setExpanded}
                      onSelect={selectField}
                      highlight={highlight}
                      onView={setFullValue}
                      onCopy={(node, mode) => void copyField(mode, node.id)}
                      onContext={fieldMenu}
                      scrollRef={treeScroll}
                      more={
                        detail.next_node_offset != null
                          ? {
                              loading: loadingMore,
                              onMore: () => void loadMore(),
                            }
                          : undefined
                      }
                    />
                    {detail.undecoded && (
                      <button
                        className="undecoded-row"
                        onClick={() => {
                          setSelection(detail.undecoded);
                          scrollHex(detail.undecoded!.start);
                        }}
                      >
                        <Icon name="warning" size={13} />
                        未解码区域{" "}
                        <span className="mono">
                          [{detail.undecoded.start}, {detail.undecoded.end})
                        </span>
                      </button>
                    )}
                    {detail.nodes.length === 0 && !detail.undecoded && (
                      <div className="tree-empty">当前区域没有字段</div>
                    )}
                  </>
                )}
              </div>
            </div>
          )}
          {packet && format === "hex" && (
            <div className="tree-empty">
              Hex 视图不做解码，直接查看原始字节。
            </div>
          )}
          {!packet && (
            <div className="control-description">
              <strong>采集事件 · {kindLabel(event.kind)}</strong>
              <p>
                {terms.controls}
                {event.kind === "loss" && ` ${terms.dropped}`}{" "}
                截至这条记录累计丢弃{" "}
                <b className="mono">{event.dropped_total}</b> 个包。
              </p>
            </div>
          )}
        </div>
        {packet && hexOpen && (
          <section className="bytes-pane" aria-label="原始字节面板">
            <div className="bytes-caption">
              <strong>原始字节</strong>
              <span className="muted mono">{event.captured_length} B</span>
              <span
                className={`bytes-selection mono ${selection ? "active" : ""}`}
                aria-label="字节选区"
              >
                {selection
                  ? `[${selection.start}, ${selection.end}) · ${selection.end - selection.start} B`
                  : "点字段或字节互相定位"}
              </span>
              <button
                className="icon-button"
                aria-label="复制选区 Hex"
                title="复制选区 Hex"
                disabled={!selection}
                onClick={() => void copy(true)}
              >
                <Icon name="copy" size={13} />
              </button>
              <button
                className="icon-button"
                aria-label="只解析选区"
                title="只解析选中的字节"
                disabled={!selection}
                onClick={() => {
                  if (selection)
                    applyRange(
                      selection.start,
                      selection.end - selection.start,
                    );
                }}
              >
                <Icon name="filter" size={13} />
              </button>
              <button
                className="icon-button bytes-collapse"
                aria-label="折叠原始字节"
                title="收起原始字节"
                onClick={toggleHex}
              >
                <Icon name="arrow" size={13} />
              </button>
            </div>
            <HexView
              hex={detail?.payload_hex || ""}
              selection={selection}
              onByte={selectByte}
              scrollRef={hexScroll}
            />
          </section>
        )}
      </div>
      {fullValue && (
        <FieldValueDialog
          sessionId={sessionId}
          eventId={event.event_id}
          node={fullValue}
          query={textQuery.toString()}
          onClose={() => setFullValue(null)}
          notify={notify}
        />
      )}
      {menu && <ContextMenu request={menu} onClose={() => setMenu(null)} />}
    </section>
  );
}
