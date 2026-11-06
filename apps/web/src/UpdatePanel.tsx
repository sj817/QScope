import { useEffect, useState } from "react";
import type { QQSetup, Recording, UpdateStatus } from "./api.generated";
import { message, request, size } from "./api";
import { Icon } from "./views";

const jsonPost = (body: unknown): RequestInit => ({
  method: "POST",
  headers: { "Content-Type": "application/json" },
  body: JSON.stringify(body),
});
const installLabels: Record<string, string> = {
  installer: "安装版",
  portable: "便携版",
  cli: "仅 CLI",
  development: "开发构建",
};
// Stages that change on their own; the panel polls while one is showing.
const moving = new Set(["checking", "downloading", "closing"]);

type Tasks = { qq: boolean; text: string };
// What installing would close, read while an install or restart is on offer.
function useRunningTasks(watch: boolean): Tasks | null {
  const [tasks, setTasks] = useState<Tasks | null>(null);
  useEffect(() => {
    if (!watch) return;
    let stopped = false;
    async function read() {
      const parts: string[] = [];
      let qq = false;
      const setup = await request<QQSetup>("/settings/qq").catch(() => null);
      if (setup?.stage === "running") {
        qq = true;
        parts.push(setup.pid ? `研究 QQ（PID ${setup.pid}）` : "研究 QQ");
      }
      const recordings = await request<Recording[]>("/recordings").catch(
        () => [],
      );
      const recording = recordings.filter(
        (item) => item.state === "recording",
      ).length;
      if (recording) parts.push(`${recording} 段录制`);
      // Latin letters and digits keep a space from the Chinese conjunction.
      const text = parts.reduce(
        (all, part) =>
          all
            ? `${all}${/[A-Za-z0-9]$/.test(all) ? " " : ""}和${/^[A-Za-z0-9]/.test(part) ? " " : ""}${part}`
            : part,
        "",
      );
      if (!stopped) setTasks(text ? { qq, text } : null);
    }
    void read();
    const timer = setInterval(read, 3000);
    return () => {
      stopped = true;
      clearInterval(timer);
    };
  }, [watch]);
  return watch ? tasks : null;
}

function stageText(update: UpdateStatus): string {
  switch (update.stage) {
    case "checking":
      return "正在检查 GitHub 上的最新版本…";
    case "available":
      return `发现新版本 ${update.latest}`;
    case "latest":
      return "已是最新版本";
    case "downloading":
      return `正在下载 ${update.latest} 并校验签名`;
    case "closing":
      return "安装包已校验，正在保存录制并关闭研究 QQ…";
    case "installing":
      return "安装程序已启动，QScope 会关闭并在安装后重新打开";
    case "ready":
      return update.install === "portable"
        ? `${update.latest} 已下载并校验，重启后生效`
        : `${update.latest} 已替换，重新运行 qs web 后生效`;
    case "restarting":
      return "正在关闭并打开新版 QScope…";
    case "error":
      return update.error || "更新失败";
    default:
      return update.checked_unix
        ? `上次检查 ${new Date(Number(update.checked_unix) * 1000).toLocaleString()}`
        : "尚未检查";
  }
}

export function UpdatePanel({
  update,
  onChange,
  notify,
}: {
  update: UpdateStatus | null;
  onChange: (update: UpdateStatus) => void;
  notify: (text: string) => void;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [proxy, setProxy] = useState(update?.proxy || "");
  const [saving, setSaving] = useState(false);
  useEffect(() => {
    setProxy(update?.proxy || "");
  }, [update?.proxy]);
  useEffect(() => {
    if (!update || !moving.has(update.stage)) return;
    const timer = setTimeout(() => {
      void request<UpdateStatus>("/update")
        .then(onChange)
        .catch(() => {});
    }, 1000);
    return () => clearTimeout(timer);
  }, [update]);
  async function act(path: string, body: unknown = {}) {
    setBusy(true);
    setError("");
    try {
      onChange(await request<UpdateStatus>(path, jsonPost(body)));
    } catch (error) {
      setError(message(error));
    } finally {
      setBusy(false);
    }
  }
  async function save(checkOnLaunch = update?.check_on_launch ?? true) {
    setSaving(true);
    setError("");
    try {
      const result = await request<UpdateStatus>(
        "/update/settings",
        jsonPost({ check_on_launch: checkOnLaunch, proxy }),
      );
      onChange(result);
      notify(
        result.proxy
          ? `更新代理已保存：${result.proxy}`
          : "更新将直接连接 GitHub",
      );
    } catch (error) {
      setError(message(error));
    } finally {
      setSaving(false);
    }
  }
  const offered =
    update?.stage === "available" ||
    (update?.stage === "error" && Boolean(update.latest)) ||
    (update?.stage === "ready" && update.install === "portable");
  const tasks = useRunningTasks(offered);
  const closing = tasks ? (tasks.qq ? "关闭 QQ 并" : "保存录制并") : "";
  const downloaded = Number(update?.downloaded || "0");
  const total = Number(update?.total || "0");
  const locked =
    busy ||
    Boolean(
      update &&
      (moving.has(update.stage) ||
        update.stage === "installing" ||
        update.stage === "restarting"),
    );
  return (
    <section className="settings-pane" aria-labelledby="update-settings-title">
      <header className="pane-head">
        <div>
          <h3 id="update-settings-title">更新</h3>
          <p>
            从 GitHub Releases 获取新版本，校验发布签名和 SHA256
            后安装。安装包校验通过后，才会保存录制并关闭研究 QQ。
          </p>
        </div>
        {update?.stage === "available" && (
          <span className="install-state update_available">有新版本</span>
        )}
      </header>

      <div className="qq-card update-card">
        <div className="qq-card-main">
          <span className="qq-mark" aria-hidden="true">
            <Icon name="download" size={20} />
          </span>
          <div className="qq-card-text">
            <strong>QScope {update?.current || "…"}</strong>
            <span>
              {update
                ? `${installLabels[update.install] || update.install} · `
                : ""}
              {update ? stageText(update) : "正在读取"}
            </span>
          </div>
        </div>
        <div className="qq-card-actions">
          {update?.notes_url && update.latest && (
            <a
              className="text-button"
              href={update.notes_url}
              target="_blank"
              rel="noreferrer"
            >
              更新说明
            </a>
          )}
          {update?.stage === "available" ||
          (update?.stage === "error" && update.latest) ? (
            <button
              className="primary-button"
              disabled={locked}
              onClick={() => void act("/update/install")}
            >
              {closing
                ? `${closing}更新到 ${update.latest}`
                : `下载并安装 ${update.latest}`}
            </button>
          ) : update?.stage === "ready" && update.install === "portable" ? (
            <button
              className="primary-button"
              disabled={locked}
              onClick={() => void act("/update/restart")}
            >
              {closing ? `${closing}重启更新` : "重启完成更新"}
            </button>
          ) : (
            <button
              className="primary-button"
              disabled={locked || !update || Boolean(update.unsupported)}
              title={update?.unsupported || undefined}
              onClick={() => void act("/update/check")}
            >
              {update?.stage === "checking" ? (
                <>
                  <span className="small-spinner" aria-hidden="true" />
                  正在检查
                </>
              ) : (
                "检查更新"
              )}
            </button>
          )}
        </div>
        {update?.stage === "downloading" && total > 0 && (
          <div className="qq-download-progress">
            <progress
              max={100}
              value={Math.min(100, (downloaded / total) * 100)}
              aria-label="更新下载进度"
            />
            <span className="mono">
              {size(downloaded)} / {size(total)}
            </span>
          </div>
        )}
      </div>
      {tasks && (
        <p className="settings-hint">
          更新前会关闭{tasks.text}：录制先保存，抓包写入结束标记。更新后在「QQ
          抓包」里重新启动 QQ。
        </p>
      )}
      {update?.unsupported && (
        <p className="settings-hint">{update.unsupported}</p>
      )}
      {error && (
        <div className="inline-error" role="alert">
          {error}
        </div>
      )}

      <h4 className="pane-subhead">检查与网络</h4>
      <div className="update-options">
        <label className="update-toggle">
          <input
            type="checkbox"
            checked={update?.check_on_launch ?? true}
            disabled={!update || saving}
            onChange={(event) => void save(event.target.checked)}
          />
          <span>
            <strong>打开 QScope 时检查更新</strong>
            <small>每天最多一次，只提示，不会自动下载</small>
          </span>
        </label>
        <form
          className="update-proxy"
          onSubmit={(event) => {
            event.preventDefault();
            void save();
          }}
        >
          <label htmlFor="update-proxy">更新代理</label>
          <input
            id="update-proxy"
            type="text"
            inputMode="url"
            spellCheck={false}
            placeholder="http://127.0.0.1:7890"
            value={proxy}
            maxLength={256}
            onChange={(event) => setProxy(event.target.value)}
          />
          <button
            className="text-button"
            type="submit"
            disabled={!update || saving || proxy === (update.proxy || "")}
          >
            {saving ? "正在保存…" : "保存"}
          </button>
          <small>
            只支持 http:// 代理，用于检查和下载 QScope 更新；留空则直接连接。
          </small>
        </form>
      </div>
    </section>
  );
}
