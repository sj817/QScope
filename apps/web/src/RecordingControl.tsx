import { useEffect, useRef, useState } from "react";
import type { Recording, Session } from "./api.generated";
import { abortError, message, request, size } from "./api";
import { isLive, terms } from "./labels";
import { Icon } from "./views";

export function RecordingControl({
  session,
  notify,
  onSaved,
}: {
  session?: Session;
  notify: (text: string) => void;
  onSaved: (path: string) => void;
}) {
  const [jobs, setJobs] = useState<Recording[]>([]);
  const [busy, setBusy] = useState(false);
  const [stopping, setStopping] = useState<string | null>(null);
  const pending = useRef(new Set<string>());
  const callbacks = useRef({ notify, onSaved });
  useEffect(() => {
    callbacks.current = { notify, onSaved };
  });
  const active = jobs.find((job) => job.state === "recording");
  function accept(result: Recording[]) {
    setJobs(result);
    for (const job of result) {
      if (!pending.current.has(job.id) || job.state === "recording") continue;
      pending.current.delete(job.id);
      setStopping(null);
      if (job.state === "error")
        callbacks.current.notify(job.error || "录制异常，已保留文件");
      else {
        callbacks.current.notify(
          `录制片段已保存 · ${job.packets} 包，已在新标签打开`,
        );
        callbacks.current.onSaved(job.path);
      }
    }
  }
  useEffect(() => {
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout>;
    async function poll() {
      try {
        const result = await request<Recording[]>("/recordings", {
          signal: controller.signal,
        });
        if (!controller.signal.aborted) accept(result);
      } catch (error) {
        if (!abortError(error)) {
          /* 连接错误由目录报告，待保存任务跨重连保留。 */
        }
      }
      if (!controller.signal.aborted) timer = setTimeout(poll, 750);
    }
    void poll();
    return () => {
      controller.abort();
      clearTimeout(timer);
    };
  }, []);
  async function toggle() {
    setBusy(true);
    try {
      if (active) {
        pending.current.add(active.id);
        setStopping(active.id);
        accept(
          await request<Recording[]>(`/recordings/${active.id}/stop`, {
            method: "POST",
          }),
        );
      } else if (session) {
        const started = await request<Recording>("/recordings", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ session_id: session.id }),
        });
        setJobs((previous) => [...previous, started]);
        notify("开始录制片段；筛选和冻结列表不影响录制内容");
      }
    } catch (error) {
      notify(message(error));
      setStopping(null);
    } finally {
      setBusy(false);
    }
  }
  // 录制入口仅在 QQ 运行时显示。
  if (!active && (!session || !isLive(session))) return null;
  return (
    <button
      className={`record-button ${active ? "recording" : ""}`}
      disabled={busy || Boolean(stopping)}
      aria-label={active ? "停止录制" : "开始录制"}
      title={
        active
          ? `正在保存 ${active.packets} 包 · ${size(active.bytes)}；停止后生成独立文件`
          : terms.recording
      }
      onClick={() => void toggle()}
    >
      <Icon name={active ? "stop" : "record"} size={13} />
      {stopping
        ? "正在保存…"
        : busy
          ? "处理中…"
          : active
            ? `停止 · ${active.packets} 包`
            : "录制片段"}
    </button>
  );
}
