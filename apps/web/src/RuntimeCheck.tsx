import { useEffect, useState } from "react";
import type { RuntimeComponent, RuntimeHealth } from "./api.generated";
import { message, request } from "./api";
import { Icon } from "./views";

const stateLabels: Record<string, string> = {
  ok: "正常",
  pending: "首次启动时准备",
  missing: "缺失",
  damaged: "已损坏",
  blocked: "被拦截",
  unsupported: "不支持",
};

function good(item: RuntimeComponent) {
  return item.state === "ok" || item.state === "pending";
}

// Checks the parts the research QQ needs and fixes what can be fixed in place. A component that
// needs a new QScope package leads to the update section instead.
export function RuntimeCheck({
  attention,
  notify,
  onReinstall,
}: {
  attention: boolean;
  notify: (text: string) => void;
  onReinstall: () => void;
}) {
  const [health, setHealth] = useState<RuntimeHealth | null>(null);
  const [busy, setBusy] = useState<"" | "checking" | "repairing">("");
  const [error, setError] = useState("");
  async function run(repair: boolean) {
    setBusy(repair ? "repairing" : "checking");
    setError("");
    try {
      const result = await request<RuntimeHealth>(
        repair ? "/settings/qq/repair" : "/settings/qq/health",
        repair ? { method: "POST" } : undefined,
      );
      setHealth(result);
      if (repair) {
        notify(
          result.preparing
            ? "已清理损坏的 QQ 文件，正在重新校验并解包"
            : result.healthy
              ? "运行组件已修复"
              : "部分组件仍需处理",
        );
      }
    } catch (error) {
      setError(message(error));
    } finally {
      setBusy("");
    }
  }
  // A problem shows the check right away; otherwise it runs on request.
  useEffect(() => {
    if (attention && !health && !busy) void run(false);
  }, [attention]);
  const problems = health?.components.filter((item) => !good(item)) || [];
  const reinstall = problems.some((item) => item.repair === "reinstall");
  const unsupported =
    problems.length > 0 &&
    problems.every((item) => item.state === "unsupported");
  return (
    <div
      className={`runtime-check ${problems.length ? "attention" : health ? "" : "unchecked"}`}
      aria-label="运行环境"
    >
      <div className="runtime-head">
        <span className="runtime-title">
          <Icon
            name={!health ? "refresh" : problems.length ? "warning" : "check"}
            size={14}
          />
          <strong>运行环境</strong>
          <span>
            {busy === "checking"
              ? "正在检查…"
              : busy === "repairing"
                ? "正在修复…"
                : !health
                  ? "检查 QScope CLI、采集探针和 QQ 文件"
                  : unsupported
                    ? "此平台采集适配中"
                    : problems.length
                      ? `${problems.length} 项需要处理`
                      : "全部正常"}
          </span>
        </span>
        <span className="runtime-actions">
          {health?.repairable && (
            <button
              className="primary-button"
              disabled={Boolean(busy)}
              onClick={() => void run(true)}
            >
              一键修复
            </button>
          )}
          {reinstall && (
            <button className="primary-button" onClick={onReinstall}>
              <Icon name="download" size={14} />
              下载新版修复
            </button>
          )}
          <button
            className="text-button"
            disabled={Boolean(busy)}
            onClick={() => void run(false)}
          >
            {health ? "重新检查" : "检查"}
          </button>
        </span>
      </div>
      {error && (
        <div className="inline-error" role="alert">
          {error}
        </div>
      )}
      {health && (
        <ul className="runtime-list">
          {health.components.map((item) => (
            <li key={item.id} className={good(item) ? "" : `bad ${item.state}`}>
              <span className="runtime-mark" aria-hidden="true">
                <Icon name={good(item) ? "check" : "warning"} size={12} />
              </span>
              <strong>{item.label}</strong>
              <span className="runtime-detail" title={item.detail}>
                {item.detail}
              </span>
              <span
                className={`install-state ${good(item) ? "installed" : "conflict"}`}
              >
                {stateLabels[item.state] || item.state}
              </span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
