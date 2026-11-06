import type { ApiError } from "./api.generated";

// Blocked browser storage keeps the token for this page only.
let token: string | null = null;
try {
  token = sessionStorage.getItem("qscope-token");
} catch {
  /* Storage disabled. */
}
const bootstrap = new URLSearchParams(location.hash.slice(1)).get(
  "qscope-token",
);
if (bootstrap && /^[0-9a-f]{64}$/.test(bootstrap)) {
  token = bootstrap;
  try {
    sessionStorage.setItem("qscope-token", bootstrap);
  } catch {
    /* Storage disabled. */
  }
  history.replaceState(history.state, "", location.pathname + location.search);
}

export async function request<T>(
  path: string,
  init: RequestInit = {},
): Promise<T> {
  const response = await fetch(`/api/v1${path}`, {
    ...init,
    headers: {
      "X-QScope-Client": "web-v1",
      ...(token ? { Authorization: `Bearer ${token}` } : {}),
      ...init.headers,
    },
  });
  if (!response.ok) {
    const text = await response.text();
    let message = `${response.status} ${response.statusText}`;
    try {
      message = (JSON.parse(text) as ApiError).message || message;
    } catch {
      if (text.length < 300) message = text || message;
    }
    throw new Error(message);
  }
  return response.json() as Promise<T>;
}

export function connectBackend(): () => void {
  const client_id = Array.from(
    crypto.getRandomValues(new Uint8Array(24)),
    (value) => value.toString(16).padStart(2, "0"),
  ).join("");
  let connected = false;
  let pending = false;
  let stopped = false;
  const update = async () => {
    if (pending || stopped) return;
    pending = true;
    try {
      await request("/service/control", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          action: connected ? "heartbeat" : "acquire",
          client_id,
        }),
      });
      connected = true;
    } catch {
      connected = false;
    } finally {
      pending = false;
    }
  };
  const release = () => {
    void request("/service/control", {
      method: "POST",
      keepalive: true,
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ action: "release", client_id }),
    }).catch(() => {});
    connected = false;
  };
  void update();
  const timer = setInterval(() => void update(), 10_000);
  window.addEventListener("pagehide", release);
  window.addEventListener("pageshow", update);
  return () => {
    stopped = true;
    clearInterval(timer);
    window.removeEventListener("pagehide", release);
    window.removeEventListener("pageshow", update);
    release();
  };
}

export function fileTime(value: string, full = false): string {
  try {
    const milliseconds = (BigInt(value) - 116444736000000000n) / 10000n;
    const date = new Date(Number(milliseconds));
    if (Number.isNaN(date.getTime())) return "—";
    return full
      ? date.toLocaleString("zh-CN", { hour12: false })
      : `${date.toLocaleTimeString("zh-CN", { hour12: false })}.${date.getMilliseconds().toString().padStart(3, "0")}`;
  } catch {
    return "—";
  }
}

export function size(value: string | number): string {
  const bytes = Number(value);
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

export function abortError(error: unknown): boolean {
  return error instanceof DOMException && error.name === "AbortError";
}

export function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export async function copyText(text: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
    return;
  } catch {
    const input = document.createElement("textarea");
    input.value = text;
    input.style.position = "fixed";
    input.style.opacity = "0";
    const dialogs = document.querySelectorAll("dialog[open]");
    const parent = dialogs[dialogs.length - 1] || document.body;
    const previous = document.activeElement;
    parent.appendChild(input);
    input.focus();
    input.select();
    const copied = document.execCommand("copy");
    input.remove();
    if (previous instanceof HTMLElement) previous.focus();
    if (!copied) throw new Error("复制失败");
  }
}
