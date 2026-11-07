import { useEffect, useMemo, useRef } from "react";
import type { CSSProperties, MouseEvent, RefObject } from "react";
import type { FieldNode } from "./api.generated";
import { Highlight, Icon, useWindow } from "./views";

export function visibleChildren(node: FieldNode): FieldNode[] {
  if (node.presentation.kind === "string") return [];
  return node.children.length === 1 && node.children[0].candidate
    ? node.children[0].children
    : node.children;
}
// 列出视图可显示的全部分支，虚拟列表控制完全展开的成本。
export function initialExpanded(nodes: FieldNode[]): Set<string> {
  const ids: string[] = [];
  const walk = (list: FieldNode[]) => {
    for (const node of list) {
      const children = visibleChildren(node);
      if (children.length) {
        ids.push(node.id);
        walk(children);
      }
    }
  };
  walk(nodes);
  return new Set(ids);
}
// 后续分页以外壳重复祖先，新增子节点合并到已有节点。
export function mergeNodes(base: FieldNode[], extra: FieldNode[]): FieldNode[] {
  if (!extra.length) return base;
  const index = new Map(base.map((node, i) => [node.id, i]));
  const out = [...base];
  for (const node of extra) {
    const at = index.get(node.id);
    if (at === undefined) {
      index.set(node.id, out.length);
      out.push(node);
    } else
      out[at] = {
        ...out[at],
        children: mergeNodes(out[at].children, node.children),
      };
  }
  return out;
}
export type FieldContext = (
  node: FieldNode,
  path: string,
  x: number,
  y: number,
  origin: HTMLElement,
) => void;

// 每个字段、结束括号或视图边缘占一行固定高度。
const LINE = 22;
type Line =
  | {
      kind: "field";
      node: FieldNode;
      depth: number;
      path: string;
      last: boolean;
      branch: boolean;
      open: boolean;
      count: number;
      position: number;
      size: number;
    }
  | { kind: "close"; key: string; depth: number; last: boolean }
  | { kind: "brace"; key: string; text: string }
  | { kind: "more" };
function flatten(
  nodes: FieldNode[],
  expanded: Set<string>,
  more: boolean,
): Line[] {
  const lines: Line[] = [{ kind: "brace", key: "open", text: "{" }];
  const walk = (list: FieldNode[], depth: number, parent: string) => {
    // 重复键在显示路径中携带出现次序。
    const counts = new Map<string, number>();
    for (const node of list)
      counts.set(
        node.presentation.key,
        (counts.get(node.presentation.key) || 0) + 1,
      );
    const seen = new Map<string, number>();
    list.forEach((node, index) => {
      const key = node.presentation.key;
      const occurrence = seen.get(key) || 0;
      seen.set(key, occurrence + 1);
      const path =
        (parent ? parent + "." : "") +
        ((counts.get(key) || 0) > 1 ? `${key}[${occurrence}]` : key);
      const children = visibleChildren(node);
      const open = children.length > 0 && expanded.has(node.id);
      const last = index === list.length - 1;
      lines.push({
        kind: "field",
        node,
        depth,
        path,
        last,
        branch: children.length > 0,
        open,
        count: children.length,
        position: index + 1,
        size: list.length,
      });
      if (open) {
        walk(children, depth + 1, path);
        lines.push({ kind: "close", key: node.id, depth, last });
      }
    });
  };
  walk(nodes, 0, "");
  if (more) lines.push({ kind: "more" });
  lines.push({ kind: "brace", key: "close", text: "}" });
  return lines;
}

// 选中字段所属对象的缩进线高亮，保持编辑器式层级提示。
function activeGuide(
  lines: Line[],
  selected: string | null,
): { from: number; to: number; level: number } | null {
  const at = lines.findIndex(
    (line) =>
      line.kind === "field" &&
      (line.node.id === selected ||
        line.node.children.some((c) => c.candidate && c.id === selected)),
  );
  if (at < 0) return null;
  const here = lines[at] as Extract<Line, { kind: "field" }>;
  let open = here.open ? at : -1;
  if (open < 0)
    for (let i = at - 1; i >= 0; i--) {
      const line = lines[i];
      if (line.kind === "field" && line.depth < here.depth) {
        open = i;
        break;
      }
    }
  if (open < 0) return null;
  const parent = lines[open] as Extract<Line, { kind: "field" }>;
  const close = lines.findIndex(
    (line, i) =>
      i > open && line.kind === "close" && line.key === parent.node.id,
  );
  return {
    from: open + 1,
    to: close < 0 ? lines.length : close,
    level: parent.depth,
  };
}
// 括号按嵌套层级循环使用三种颜色。
const bracket = (depth: number) => `json-bracket level-${(depth + 1) % 3}`;

export function FieldTree({
  nodes,
  selected,
  expanded,
  setExpanded,
  onSelect,
  highlight = "",
  onView,
  onCopy,
  onContext,
  scrollRef,
  more,
}: {
  nodes: FieldNode[];
  selected: string | null;
  expanded: Set<string>;
  setExpanded: (value: Set<string>) => void;
  onSelect: (node: FieldNode) => void;
  highlight?: string;
  onView: (node: FieldNode) => void;
  onCopy: (node: FieldNode, mode: "value" | "subtree") => void;
  onContext?: FieldContext;
  scrollRef: RefObject<HTMLDivElement | null>;
  more?: { loading: boolean; onMore: () => void };
}) {
  const tree = useRef<HTMLDivElement>(null);
  const hasMore = Boolean(more);
  const lines = useMemo(
    () => flatten(nodes, expanded, hasMore),
    [nodes, expanded, hasMore],
  );
  const guide = useMemo(() => activeGuide(lines, selected), [lines, selected]);
  const { start, end } = useWindow(scrollRef, LINE, lines.length);
  const onMore = more?.onMore;
  const moreLoading = more?.loading;
  // 接近已加载视图末尾时继续加载字段。
  useEffect(() => {
    if (onMore && !moreLoading && end >= lines.length - 40) onMore();
  }, [end, lines.length, moreLoading, onMore]);
  useEffect(() => {
    const element = scrollRef.current;
    const at = lines.findIndex(
      (line) =>
        line.kind === "field" &&
        (line.node.id === selected ||
          line.node.children.some((c) => c.candidate && c.id === selected)),
    );
    if (!element || !tree.current || at < 0) return;
    const top =
      tree.current.getBoundingClientRect().top -
      element.getBoundingClientRect().top +
      element.scrollTop +
      at * LINE;
    if (top < element.scrollTop) element.scrollTop = top;
    else if (top + LINE > element.scrollTop + element.clientHeight)
      element.scrollTop = top + LINE - element.clientHeight;
  }, [selected, lines, scrollRef]);
  function toggle(node: FieldNode, open: boolean) {
    const next = new Set(expanded);
    if (open) next.delete(node.id);
    else next.add(node.id);
    setExpanded(next);
  }
  const guides = (depth: number, index: number) =>
    depth > 0 && (
      <span className="json-guides" aria-hidden="true">
        {guide && index >= guide.from && index < guide.to && (
          <span
            className="json-guide-active"
            style={{ "--level": guide.level } as CSSProperties}
          />
        )}
      </span>
    );
  function field(line: Extract<Line, { kind: "field" }>, index: number) {
    const { node, depth, path, last, branch, open } = line;
    const p = node.presentation;
    const selectedHere =
      selected === node.id ||
      node.children.some((c) => c.candidate && c.id === selected);
    const comma = !open && !last && <span className="json-punctuation">,</span>;
    const choose = () => onSelect(node);
    // 单击选择字段并切换对象折叠状态，双击选择完整值。
    const click = (event: MouseEvent) => {
      if (event.detail > 1 || window.getSelection()?.toString()) return;
      choose();
      if (branch) toggle(node, open);
    };
    const pick = (event: MouseEvent<HTMLElement>) => {
      const text = event.currentTarget.querySelector(".json-text");
      if (text) window.getSelection()?.selectAllChildren(text);
    };
    let value;
    if (branch)
      value = open ? (
        <span className={bracket(depth)}>{"{"}</span>
      ) : (
        <>
          <span className={bracket(depth)}>{"{"}</span>
          <span className="json-fold" aria-hidden="true">
            ⋯
          </span>
          <span className={bracket(depth)}>{"}"}</span>
          {comma}
          <span className="json-count">{line.count} 项</span>
        </>
      );
    else {
      const quoted = p.kind !== "number";
      const text = quoted
        ? JSON.stringify(p.kind === "bytes" ? `0x${p.value}` : p.value).slice(
            1,
            -1,
          )
        : p.value;
      value = (
        <>
          <span
            className={`field-content json-value ${quoted ? "value-string" : "value-number"}`}
            onMouseDown={(event) => {
              if (event.detail > 1) event.preventDefault();
            }}
            onDoubleClick={pick}
          >
            {quoted && '"'}
            <span className="json-text">
              <Highlight text={text} query={highlight} />
            </span>
            {quoted && '"'}
          </span>
          {comma}
        </>
      );
    }
    const type =
      p.type_name +
      (p.type_name === "bytes"
        ? p.kind === "object"
          ? " · message?"
          : p.kind === "string"
            ? " · UTF-8?"
            : ""
        : "");
    const action = (label: string, icon: "eye" | "copy", run: () => void) => (
      <button
        className="json-action"
        aria-label={label}
        title={label}
        onClick={(event) => {
          event.stopPropagation();
          run();
        }}
      >
        <Icon name={icon} size={13} />
      </button>
    );
    return (
      <div
        key={node.id}
        role="treeitem"
        aria-level={depth + 1}
        aria-posinset={line.position}
        aria-setsize={line.size}
        aria-expanded={branch ? open : undefined}
        aria-selected={selectedHere}
      >
        <div
          className={`field-row json-field-row ${branch ? "branch" : ""} ${selectedHere ? "selected" : ""}`}
          style={{ "--depth": depth } as CSSProperties}
          data-path={path}
          onClick={click}
          onContextMenu={(event) => {
            if (!onContext) return;
            event.preventDefault();
            event.stopPropagation();
            onContext(
              node,
              path,
              event.clientX,
              event.clientY,
              event.currentTarget.querySelector<HTMLElement>(".json-key") ||
                event.currentTarget,
            );
          }}
        >
          {guides(depth, index)}
          {branch ? (
            <button
              className="expand-button"
              tabIndex={-1}
              aria-label={`${open ? "折叠" : "展开"} ${node.label}`}
              onClick={(event) => {
                event.stopPropagation();
                toggle(node, open);
              }}
            >
              <span className={open ? "rotated" : ""}>
                <Icon name="arrow" size={11} />
              </span>
            </button>
          ) : (
            <span className="expand-space" />
          )}
          <div className="field-value json-pair">
            <span
              className="field-label json-key"
              role="button"
              tabIndex={0}
              aria-label={`字段 ${node.label}: ${node.value}`}
              onKeyDown={(event) => {
                if (event.key === "Enter" || event.key === " ") {
                  event.preventDefault();
                  choose();
                  if (branch) toggle(node, open);
                } else if (
                  branch &&
                  (event.key === "ArrowRight" || event.key === "ArrowLeft") &&
                  open === (event.key === "ArrowLeft")
                ) {
                  event.preventDefault();
                  toggle(node, open);
                } else if (
                  onContext &&
                  ((event.shiftKey && event.key === "F10") ||
                    event.key === "ContextMenu")
                ) {
                  event.preventDefault();
                  event.stopPropagation();
                  const box = event.currentTarget.getBoundingClientRect();
                  onContext(
                    node,
                    path,
                    box.left,
                    box.bottom + 4,
                    event.currentTarget,
                  );
                }
              }}
            >
              <Highlight text={JSON.stringify(p.key)} query={highlight} />
            </span>
            <span className="json-colon">:</span>
            {value}
            <span className="json-actions">
              {branch ? (
                action("复制子树 JSON", "copy", () => onCopy(node, "subtree"))
              ) : (
                <>
                  {action("查看完整值", "eye", () => onView(node))}
                  {action("复制值", "copy", () => onCopy(node, "value"))}
                </>
              )}
            </span>
          </div>
          <span className="json-field-meta">
            <span className="field-type" title={type}>
              {type}
            </span>
            <span className="field-size">{p.value_length} B</span>
            <span className="field-offset" title="字段在包体中的字节范围">
              [{node.range.start}, {node.range.end})
            </span>
          </span>
        </div>
      </div>
    );
  }
  return (
    <div
      ref={tree}
      className="field-tree json-tree"
      role="tree"
      aria-label="解码字段树"
      style={{ height: lines.length * LINE }}
    >
      <div className="json-window" style={{ top: start * LINE }}>
        {lines.slice(start, end).map((line, offset) =>
          line.kind === "field" ? (
            field(line, start + offset)
          ) : line.kind === "close" ? (
            <div
              key={`close:${line.key}`}
              role="none"
              className="json-close"
              style={{ "--depth": line.depth } as CSSProperties}
            >
              {guides(line.depth, start + offset)}
              <span className={bracket(line.depth)}>{"}"}</span>
              {!line.last && <span className="json-punctuation">,</span>}
            </div>
          ) : line.kind === "more" ? (
            <div key="more" role="none" className="json-more">
              {more?.loading ? (
                <>
                  <span className="small-spinner" aria-hidden="true" />
                  正在读取更多字段…
                </>
              ) : (
                <button className="text-button" onClick={more?.onMore}>
                  继续读取字段
                </button>
              )}
            </div>
          ) : (
            <div key={line.key} role="none" className="json-root-brace">
              <span className="json-bracket level-0">{line.text}</span>
            </div>
          ),
        )}
      </div>
    </div>
  );
}
