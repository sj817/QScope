import { useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { Icon } from "./views";

export type MenuItem = {
  id: string;
  label: string;
  icon?: string;
  shortcut?: string;
  disabled?: string;
  run: () => void;
};
export type MenuGroup = { label?: string; items: MenuItem[] };
export type MenuRequest = {
  x: number;
  y: number;
  title: string;
  groups: MenuGroup[];
  returnFocus?: HTMLElement | null;
};

// Positioned from its measured size and flipped at viewport edges. Focus moves
// into the menu on open and returns to the invoking element on close. It is
// portalled to <body>: panes with backdrop-filter would otherwise become the
// containing block of the fixed menu and clip it.
export function ContextMenu({
  request,
  onClose,
}: {
  request: MenuRequest;
  onClose: () => void;
}) {
  const menu = useRef<HTMLDivElement>(null);
  const [position, setPosition] = useState<{
    left: number;
    top: number;
    maxHeight?: number;
  } | null>(null);
  const items = request.groups.flatMap((group) => group.items);
  const enabled = items.filter((item) => !item.disabled);
  const [active, setActive] = useState(enabled[0]?.id);
  // A menu opened by keyboard can appear under a resting cursor; only real
  // pointer movement takes the highlight away from the keyboard.
  const pointer = useRef<{ x: number; y: number } | null>(null);
  function hover(event: React.MouseEvent, item: MenuItem) {
    const last = pointer.current;
    pointer.current = { x: event.clientX, y: event.clientY };
    if (
      !item.disabled &&
      last &&
      (last.x !== event.clientX || last.y !== event.clientY)
    )
      setActive(item.id);
  }

  function close(restore = true) {
    onClose();
    if (restore)
      requestAnimationFrame(() =>
        request.returnFocus?.focus({ preventScroll: true }),
      );
  }
  function run(item: MenuItem) {
    if (item.disabled) return;
    close();
    item.run();
  }

  useLayoutEffect(() => {
    const box = menu.current!.getBoundingClientRect();
    const gap = 8;
    const left =
      request.x + box.width > innerWidth - gap
        ? Math.max(gap, request.x - box.width)
        : request.x;
    const tall = box.height > innerHeight - gap * 2;
    const top = tall
      ? gap
      : request.y + box.height > innerHeight - gap
        ? Math.max(gap, request.y - box.height)
        : request.y;
    setPosition({
      left,
      top,
      maxHeight: tall ? innerHeight - gap * 2 : undefined,
    });
  }, [request]);
  useLayoutEffect(() => {
    if (position)
      menu.current
        ?.querySelector<HTMLElement>(`[data-item="${active}"]`)
        ?.focus({ preventScroll: true });
  }, [active, position]);
  useLayoutEffect(() => {
    const outside = (event: PointerEvent) => {
      if (!menu.current?.contains(event.target as Node)) close(false);
    };
    const dismiss = () => close(false);
    document.addEventListener("pointerdown", outside, true);
    window.addEventListener("resize", dismiss);
    window.addEventListener("blur", dismiss);
    return () => {
      document.removeEventListener("pointerdown", outside, true);
      window.removeEventListener("resize", dismiss);
      window.removeEventListener("blur", dismiss);
    };
  });

  function onKeyDown(event: React.KeyboardEvent) {
    const index = enabled.findIndex((item) => item.id === active);
    const move = (next: number) => {
      event.preventDefault();
      if (enabled.length)
        setActive(enabled[(next + enabled.length) % enabled.length].id);
    };
    if (event.key === "ArrowDown") move(index + 1);
    else if (event.key === "ArrowUp") move(index - 1);
    else if (event.key === "Home") move(0);
    else if (event.key === "End") move(enabled.length - 1);
    else if (
      event.key === "Escape" ||
      event.key === "Tab" ||
      (event.key === "F10" && event.shiftKey)
    ) {
      event.preventDefault();
      event.stopPropagation();
      close();
    } else if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      const item = enabled[index];
      if (item) run(item);
    }
  }

  return createPortal(
    <div
      ref={menu}
      className="context-menu"
      role="menu"
      aria-label={request.title}
      tabIndex={-1}
      onKeyDown={onKeyDown}
      onContextMenu={(event) => event.preventDefault()}
      style={{
        left: position?.left ?? request.x,
        top: position?.top ?? request.y,
        maxHeight: position?.maxHeight,
        visibility: position ? "visible" : "hidden",
      }}
    >
      <div className="context-menu-title" aria-hidden="true">
        {request.title}
      </div>
      {request.groups.map((group, groupIndex) => (
        <div key={groupIndex} role="group" aria-label={group.label}>
          {groupIndex > 0 && (
            <div className="context-menu-separator" role="separator" />
          )}
          {group.label && (
            <div className="context-menu-group" aria-hidden="true">
              {group.label}
            </div>
          )}
          {group.items.map((item) => (
            <button
              key={item.id}
              type="button"
              role="menuitem"
              data-item={item.id}
              tabIndex={item.id === active ? 0 : -1}
              aria-label={item.label}
              aria-disabled={item.disabled ? true : undefined}
              aria-keyshortcuts={item.shortcut}
              aria-description={item.disabled}
              className={item.disabled ? "disabled" : ""}
              onMouseMove={(event) => hover(event, item)}
              onMouseDown={(event) => {
                if (item.disabled) event.preventDefault();
              }}
              onClick={() => run(item)}
            >
              <span className="context-menu-icon">
                {item.icon && <Icon name={item.icon} size={14} />}
              </span>
              <span className="context-menu-label">
                {item.label}
                {item.disabled && <small>{item.disabled}</small>}
              </span>
              {item.shortcut && <kbd>{item.shortcut}</kbd>}
            </button>
          ))}
        </div>
      ))}
    </div>,
    document.body,
  );
}
