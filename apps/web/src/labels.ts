import type { ApiError, Session } from "./api.generated";

// One-line explanations for terms a first-time user meets in the workspace.
export const terms = {
  pid: "PID：操作系统给这次 QQ 进程分配的编号，结合进程出生标识区分不同启动，编号可能被复用。",
  seq: "SEQ：QQ 为每个请求分配的序号，可以在搜索框里精确查找。",
  dropped:
    "丢弃：采集队列来不及写入、没有记录下来的包数量。不为 0 时这份记录不完整。",
  truncated: "截断：包体超过单包保存上限，只保存了前一部分；原始长度仍有记录。",
  partialTail: "文件仍在写入，最后一条记录还不完整，这是实时记录的正常现象。",
  controls: "采集事件：探针自己的启动、丢包和结束记录，不是 QQ 的协议包。",
  account: "账号取自 MSF 发送对象的 uin 字段，仍待真实 QQ 登录核验。",
  recording:
    "全部收发已经在自动记录。录制片段只是把接下来的一段另存成独立文件，便于分享和对比。",
  follow: "开启时列表停在最新的包；滚动、点选或用方向键查看某个包会自动关闭。",
  freeze: "冻结后列表不再插入新包，便于逐条查看；新包仍写入原始文件。",
  compare:
    "勾选 2–8 个包可对比。Ctrl+点击勾选，Shift+点击连选，或右键“加入对比”。",
};

const kinds: Record<string, string> = {
  status: "状态",
  loss: "丢包",
  end: "结束",
};
export function kindLabel(kind: string): string {
  return kinds[kind] || kind;
}

export function isLive(session: Session): boolean {
  return (
    session.source_kind === "capture" &&
    session.producer_alive === true &&
    !session.clean_end &&
    !session.error
  );
}

export type RecordTone = "live" | "warning" | "neutral";
export function recordState(session: Session): {
  text: string;
  tone: RecordTone;
  hint: string;
} {
  if (session.error)
    return {
      text: "文件异常",
      tone: "warning",
      hint: friendlyError(session.error).title,
    };
  if (session.has_more && session.records === "0")
    return { text: "尚未读取", tone: "neutral", hint: "打开后才会建立索引" };
  if (session.source_kind === "imported")
    return {
      text: "导入文件",
      tone: "neutral",
      hint: "从 .qscap 导入的只读副本",
    };
  if (session.source_kind === "recording")
    return {
      text: "录制片段",
      tone: "neutral",
      hint: "从某次启动中另存的一段",
    };
  if (session.source_kind === "http")
    return {
      text: session.clean_end ? "HTTP · 已结束" : "HTTP 代理",
      tone: "neutral",
      hint: "HTTP 代理对照记录",
    };
  if (session.clean_end)
    return { text: "已结束", tone: "neutral", hint: "记录完整结束" };
  if (session.producer_alive === false)
    return {
      text: "已中断",
      tone: "warning",
      hint: "QQ 已退出，但没有写入结束标记；已记录的包都可以查看。",
    };
  if (session.producer_alive === true)
    return {
      text: "运行中",
      tone: "live",
      hint: "这次 QQ 仍在运行，列表会实时刷新",
    };
  return {
    text: "状态未知",
    tone: "neutral",
    hint: "无法确认这次 QQ 是否仍在运行",
  };
}

// Transient reading state beside the row count; how the record ended belongs to its tab.
export function readingNote(session: Session): string {
  if (session.has_more) return "正在建立索引…";
  if (session.partial_tail) return "最后一条正在写入";
  return "";
}

export function friendlyError(error: ApiError | string): {
  title: string;
  detail: string;
} {
  if (typeof error === "string") return { title: error, detail: "" };
  const at =
    error.offset === null ? "" : `第 ${error.offset.toLocaleString()} 字节处`;
  switch (error.code) {
    case "native_record":
    case "native_header":
      return {
        title: `文件${at}结构异常`,
        detail: "这之前的包仍可查看。文件可能被其他程序改写或已损坏。",
      };
    case "source_changed":
      return {
        title: "原始文件内容已变化",
        detail: "读取之后文件被改写，请在历史记录里重新扫描。",
      };
    case "capture_io":
      return {
        title: "读取抓包文件失败",
        detail: "文件可能被移动、删除或正被其他程序独占。",
      };
    case "worker":
      return {
        title: "本地服务繁忙",
        detail: "稍后重试；仍失败时重新启动 QScope。",
      };
    default:
      return { title: error.message, detail: "" };
  }
}

// QQ numbers stay strings. Masking is a display choice only.
export function maskAccount(value: string): string {
  if (value.length >= 6) return `${value.slice(0, 2)}****${value.slice(-2)}`;
  if (value.length >= 3)
    return `${value[0]}${"*".repeat(value.length - 2)}${value.at(-1)}`;
  return "*".repeat(value.length);
}
export function accountText(value: string, reveal: boolean): string {
  return reveal ? value : maskAccount(value);
}

export function readPreference(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}
export function writePreference(key: string, value: string): void {
  try {
    localStorage.setItem(key, value);
  } catch {
    /* Private browser storage may be disabled. */
  }
}
