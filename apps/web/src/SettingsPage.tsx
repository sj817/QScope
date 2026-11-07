import { useCallback, useEffect, useRef, useState } from "react";
import type { KeyboardEvent } from "react";
import type {
  DevtoolsFrontend,
  IntegrationSettings,
  QQSetup,
  SkillAgent,
  SkillScope,
  SkillStatus,
  UpdateStatus,
} from "./api.generated";
import { abortError, copyText, message, request, size } from "./api";
import { Icon } from "./views";
import { Modal } from "./Modal";
import { RuntimeCheck } from "./RuntimeCheck";
import { UpdatePanel } from "./UpdatePanel";

const jsonPost = (body: unknown): RequestInit => ({
  method: "POST",
  headers: { "Content-Type": "application/json" },
  body: JSON.stringify(body),
});
const busyStages = new Set([
  "preparing",
  "checking",
  "signing",
  "downloading",
  "verifying",
  "extracting",
  "ready",
  "starting",
]);
const fetchStages = new Set([
  "preparing",
  "checking",
  "signing",
  "downloading",
  "verifying",
]);
const unpackStages = new Set(["extracting", "ready"]);
const skillStates: Record<string, string> = {
  missing: "未安装",
  installed: "已安装",
  update_available: "可更新",
  conflict: "内容有改动",
};

// 本机保存安装范围及项目目录，重新打开 AI 接入时恢复用户选择的目标。
const TARGET_KEY = "qscope.skill-target";
function savedTarget(): { scope: SkillScope; project: string } {
  const fallback: SkillScope = (
    window as Window & { __QSCOPE_DESKTOP__?: boolean }
  ).__QSCOPE_DESKTOP__
    ? "user"
    : "project";
  try {
    const saved = JSON.parse(localStorage.getItem(TARGET_KEY) || "null");
    if (saved?.scope === "user" || saved?.scope === "project")
      return {
        scope: saved.scope,
        project: typeof saved.project === "string" ? saved.project : "",
      };
  } catch {
    /* 存储不可用时使用默认目标 */
  }
  return { scope: fallback, project: "" };
}
function saveTarget(scope: SkillScope, project: string) {
  try {
    localStorage.setItem(TARGET_KEY, JSON.stringify({ scope, project }));
  } catch {
    /* 本次选择仅保留在内存 */
  }
}

// 当前设置区域与地址同步，刷新或分享链接时恢复相同区域。
type Section = "qq" | "ai" | "update";
const sections: Section[] = ["qq", "ai", "update"];
function sectionFromUrl(): Section {
  const value = new URLSearchParams(location.search).get("section");
  return value === "ai" || value === "update" ? value : "qq";
}

export function SettingsPage({
  notify,
  onCapture,
  update,
  onUpdate,
}: {
  notify: (text: string) => void;
  onCapture: (id: string) => void;
  update: UpdateStatus | null;
  onUpdate: (update: UpdateStatus) => void;
}) {
  const [section, setSection] = useState<Section>(sectionFromUrl);
  const [settings, setSettings] = useState<IntegrationSettings | null>(null);
  const [initial] = useState(savedTarget);
  const [scope, setScope] = useState<SkillScope>(initial.scope);
  const [project, setProject] = useState(initial.project);
  const [loading, setLoading] = useState(true);
  const [pending, setPending] = useState<{
    agent: SkillAgent;
    action: "install" | "uninstall";
  } | null>(null);
  const [error, setError] = useState("");
  const controller = useRef<AbortController | null>(null);
  const [qq, setQQ] = useState<QQSetup | null>(null);
  const [qqError, setQQError] = useState("");
  const [starting, setStarting] = useState(false);
  const [opening, setOpening] = useState(false);
  const [stopping, setStopping] = useState(false);
  const [frontend, setFrontend] = useState<DevtoolsFrontend | null>(null);
  const startedHere = useRef(false);
  const tabs = useRef(new Map<Section, HTMLButtonElement>());

  // directory 表示显示的项目，用户范围下保存此值供切回项目时恢复。
  const load = useCallback(async function load(
    nextScope: SkillScope,
    directory?: string,
    remembered = false,
  ) {
    // 立即保存选择，检查完成前关闭窗口也能恢复。
    if (!remembered) saveTarget(nextScope, directory || "");
    controller.current?.abort();
    const next = new AbortController();
    controller.current = next;
    setLoading(true);
    setError("");
    const query = new URLSearchParams({ scope: nextScope });
    if (nextScope === "project" && directory) query.set("project", directory);
    try {
      const result = await request<IntegrationSettings>(
        `/settings/integrations?${query}`,
        { signal: next.signal },
      );
      if (next.signal.aborted) return;
      setSettings(result);
      const projectDirectory =
        nextScope === "project" ? result.target_directory : directory || "";
      if (nextScope === "project") setProject(projectDirectory);
      saveTarget(nextScope, projectDirectory);
    } catch (error) {
      if (abortError(error)) return;
      // 已记住的项目移动后回退到默认目标。
      if (remembered && nextScope === "project" && directory) {
        void load("project");
        return;
      }
      setError(message(error));
    } finally {
      if (!next.signal.aborted) setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load(initial.scope, initial.project, true);
    return () => controller.current?.abort();
  }, [initial, load]);
  useEffect(() => {
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout>;
    async function poll() {
      try {
        setQQ(
          await request<QQSetup>("/settings/qq", { signal: controller.signal }),
        );
      } catch (error) {
        if (!abortError(error)) setQQError(message(error));
      }
      if (!controller.signal.aborted) timer = setTimeout(poll, 1500);
    }
    void poll();
    return () => {
      controller.abort();
      clearTimeout(timer);
    };
  }, []);
  useEffect(() => {
    if (startedHere.current && qq?.stage === "running" && qq.session_id) {
      startedHere.current = false;
      notify("QQ 已启动，扫码登录后继续抓包");
      onCapture(qq.session_id);
    }
  }, [qq, notify, onCapture]);

  function open(next: Section) {
    setSection(next);
    const url = new URL(location.href);
    if (next === "qq") url.searchParams.delete("section");
    else url.searchParams.set("section", next);
    history.replaceState(history.state, "", url);
  }
  function onTabKey(event: KeyboardEvent) {
    const moves: Record<string, number> = {
      ArrowDown: 1,
      ArrowRight: 1,
      ArrowUp: -1,
      ArrowLeft: -1,
    };
    if (!(event.key in moves) && event.key !== "Home" && event.key !== "End")
      return;
    event.preventDefault();
    const index = sections.indexOf(section);
    const next =
      event.key === "Home"
        ? sections[0]
        : event.key === "End"
          ? sections[sections.length - 1]
          : sections[
              (index + moves[event.key] + sections.length) % sections.length
            ];
    open(next);
    tabs.current.get(next)?.focus();
  }
  async function copy(value: string) {
    try {
      await copyText(value);
      notify("已复制");
    } catch (error) {
      notify(message(error));
    }
  }
  async function changeSkill(
    agent: SkillAgent,
    action: "install" | "uninstall",
  ) {
    if (!settings) return;
    setPending({ agent, action });
    setError("");
    try {
      const result = await request<SkillStatus>("/settings/skills", {
        ...jsonPost({
          agent,
          scope: settings.scope,
          project: settings.target_directory,
        }),
        method: action === "uninstall" ? "DELETE" : "POST",
      });
      setSettings((previous) =>
        previous
          ? {
              ...previous,
              targets: previous.targets.map((target) =>
                target.agent === agent ? result : target,
              ),
            }
          : previous,
      );
      notify(
        action === "uninstall"
          ? `${result.name} · ${result.detail}`
          : `${result.name} 已安装${agent === "claude-code" ? "，CLI 白名单已写入" : ""}`,
      );
    } catch (error) {
      setError(message(error));
    } finally {
      setPending(null);
    }
  }
  async function startQQ() {
    if (qq?.stage === "running" && qq.session_id) {
      onCapture(qq.session_id);
      return;
    }
    setStarting(true);
    setQQError("");
    startedHere.current = true;
    try {
      setQQ(await request<QQSetup>("/settings/qq", jsonPost({})));
    } catch (error) {
      setQQError(message(error));
      startedHere.current = false;
    } finally {
      setStarting(false);
    }
  }
  async function stopQQ() {
    setStopping(true);
    setQQError("");
    try {
      setQQ(await request<QQSetup>("/settings/qq/stop", jsonPost({})));
      notify("QQ 已关闭，抓包已保存");
    } catch (error) {
      setQQError(message(error));
    } finally {
      setStopping(false);
    }
  }
  async function openDevtools() {
    setOpening(true);
    setQQError("");
    try {
      const result = await request<DevtoolsFrontend>(
        "/settings/devtools/current/open",
        jsonPost({}),
      );
      const url = new URL(result.frontend_url);
      if (
        url.protocol !== "http:" ||
        url.hostname !== "127.0.0.1" ||
        url.username ||
        url.password
      )
        throw new Error("调试服务返回了无效地址");
      setFrontend(result);
    } catch (error) {
      setQQError(message(error));
    } finally {
      setOpening(false);
    }
  }
  const busy = starting || Boolean(qq && busyStages.has(qq.stage));
  const running = qq?.stage === "running";
  const unsupported = qq?.capture_supported === false;
  const downloaded = Number(qq?.downloaded || "0");
  const total = Number(qq?.total || "1");
  const qqState = unsupported
    ? "采集适配中"
    : running
      ? "正在采集"
      : busy
        ? "正在准备"
        : !qq
          ? "正在读取"
          : qq.installed
            ? "未运行"
            : "未下载";
  const attention =
    settings?.targets.filter(
      (t) => t.state === "update_available" || t.state === "conflict",
    ).length || 0;
  const installed =
    settings?.targets.filter((t) => t.state === "installed").length || 0;
  const agentState = !settings
    ? loading
      ? "正在检查"
      : "读取失败"
    : attention
      ? `${attention} 个需要处理`
      : `已安装 ${installed} / ${settings.targets.length}`;
  const steps = [
    {
      title: "下载并校验",
      detail: "首次使用自动下载官方安装包并校验",
      done: Boolean(qq?.installed),
      active: Boolean(qq && fetchStages.has(qq.stage)),
    },
    {
      title: "免安装解包",
      detail: "解包后以独立数据目录运行",
      done: Boolean(qq?.installed),
      active: Boolean(qq && unpackStages.has(qq.stage)),
    },
    {
      title: "扫码登录",
      detail: "在 QQ 窗口登录后操作，收发的包实时进入列表",
      done: false,
      active: running || qq?.stage === "starting",
    },
  ];
  const updateState = !update
    ? "正在读取"
    : update.stage === "available"
      ? `新版本 ${update.latest}`
      : `当前 ${update.current}`;
  const nav: [Section, string, string, string][] = [
    ["qq", "QQ 抓包", "packet", qqState],
    ["ai", "AI 接入", "spark", agentState],
    ["update", "更新", "download", updateState],
  ];

  return (
    <div className="settings-page">
      <div className="settings-nav">
        <div
          className="settings-tabs"
          role="tablist"
          aria-label="设置分区"
          aria-orientation="vertical"
          onKeyDown={onTabKey}
        >
          {nav.map(([id, label, icon, hint]) => (
            <button
              key={id}
              ref={(element) => {
                if (element) tabs.current.set(id, element);
                else tabs.current.delete(id);
              }}
              role="tab"
              id={`settings-tab-${id}`}
              aria-selected={section === id}
              aria-controls={`settings-panel-${id}`}
              tabIndex={section === id ? 0 : -1}
              className="settings-tab"
              onClick={() => open(id)}
            >
              <Icon name={icon} size={16} />
              <span className="settings-tab-text">
                <strong>{label}</strong>
                <small>{hint}</small>
              </span>
              {id === "qq" && running && (
                <span className="status-dot" aria-hidden="true" />
              )}
              {id === "ai" && attention > 0 && (
                <span className="settings-tab-badge" aria-hidden="true">
                  {attention}
                </span>
              )}
              {id === "update" && update?.stage === "available" && (
                <span className="settings-tab-badge" aria-hidden="true">
                  1
                </span>
              )}
            </button>
          ))}
        </div>
        {settings && (
          <div
            className="settings-nav-foot"
            title="WebUI 与 CLI 连接的本机地址"
          >
            <span>本机服务</span>
            <code>{settings.server_url}</code>
          </div>
        )}
      </div>

      <div
        className="settings-panel"
        role="tabpanel"
        id={`settings-panel-${section}`}
        aria-labelledby={`settings-tab-${section}`}
      >
        {section === "qq" ? (
          <section
            className="settings-pane"
            aria-labelledby="qq-settings-title"
          >
            <header className="pane-head">
              <div>
                <h3 id="qq-settings-title">QQ 抓包</h3>
                <p>
                  {unsupported
                    ? qq.unavailable_reason
                    : "用独立数据目录运行官方 QQ，登录和业务操作的收发包会实时进入抓包列表。"}
                </p>
              </div>
              <span
                className={`install-state ${running ? "installed" : busy ? "busy" : ""}`}
              >
                {qqState}
              </span>
            </header>

            <div className="qq-card">
              <div className="qq-card-main">
                <span className="qq-mark" aria-hidden="true">
                  <Icon name="packet" size={20} />
                </span>
                <div className="qq-card-text">
                  <strong>{qq?.build ? `QQ ${qq.build}` : "QQ"}</strong>
                  <span>
                    {qq?.platform || "正在读取平台"} ·{" "}
                    {unsupported
                      ? "可分析已有抓包文件"
                      : running && qq?.pid
                        ? `PID ${qq.pid}`
                        : qq?.installed
                          ? "已解包，可直接启动"
                          : "首次启动会先下载官方安装包"}
                  </span>
                </div>
              </div>
              <div className="qq-card-actions">
                {running && (
                  <button
                    className="text-button"
                    disabled={stopping}
                    title="结束研究 QQ，抓包文件会写入结束标记"
                    onClick={() => void stopQQ()}
                  >
                    {stopping ? "正在关闭…" : "关闭 QQ"}
                  </button>
                )}
                <button
                  className="text-button"
                  disabled={!running || opening}
                  title={running ? "调试 QQ 当前显示的页面" : "QQ 运行后可用"}
                  onClick={() => void openDevtools()}
                >
                  {opening ? "正在打开…" : "打开 DevTools"}
                </button>
                <button
                  className="primary-button"
                  disabled={
                    !qq?.available ||
                    busy ||
                    stopping ||
                    (running && !qq.session_id)
                  }
                  onClick={() => void startQQ()}
                >
                  {busy ? (
                    <>
                      <span className="small-spinner" aria-hidden="true" />
                      {qq?.message || "正在准备 QQ"}
                    </>
                  ) : unsupported ? (
                    "此平台暂未开放采集"
                  ) : running ? (
                    "查看当前抓包"
                  ) : qq?.installed ? (
                    "启动 QQ"
                  ) : (
                    "下载并启动 QQ"
                  )}
                </button>
              </div>
              {qq?.stage === "downloading" && (
                <div className="qq-download-progress">
                  <progress
                    max={100}
                    value={Math.min(
                      100,
                      (downloaded / Math.max(1, total)) * 100,
                    )}
                  />
                  <span className="mono">
                    {size(downloaded)} / {size(total)}
                  </span>
                </div>
              )}
            </div>

            {(qqError || qq?.error) && (
              <div className="inline-error" role="alert">
                {qqError || qq?.error}
              </div>
            )}
            <RuntimeCheck
              attention={Boolean(qq && (!qq.available || qq.error))}
              notify={notify}
              onReinstall={() => open("update")}
            />

            {!unsupported && (
              <>
                <h4 className="pane-subhead">流程</h4>
                <ol className="qq-steps">
                  {steps.map((step, index) => (
                    <li
                      key={step.title}
                      className={
                        step.done ? "done" : step.active ? "active" : ""
                      }
                    >
                      <span className="qq-step-mark" aria-hidden="true">
                        {step.done ? (
                          <Icon name="check" size={12} />
                        ) : (
                          index + 1
                        )}
                      </span>
                      <div>
                        <strong>{step.title}</strong>
                        <span>{step.detail}</span>
                      </div>
                    </li>
                  ))}
                </ol>
              </>
            )}
          </section>
        ) : section === "update" ? (
          <UpdatePanel update={update} onChange={onUpdate} notify={notify} />
        ) : (
          <section
            className="settings-pane"
            aria-labelledby="agent-settings-title"
          >
            <header className="pane-head">
              <div>
                <h3 id="agent-settings-title">AI 接入</h3>
                <p>
                  安装 QScope 技能后，AI 可以通过 CLI 读取抓包和控制界面。Claude
                  Code
                  会同时写入命令白名单。每次检查会核对技能版本、模板和已安装内容的
                  SHA256。
                </p>
              </div>
            </header>
            {error && (
              <div className="inline-error" role="alert">
                {error}
              </div>
            )}
            {loading && !settings && (
              <div className="settings-loading">
                <span className="small-spinner" aria-hidden="true" />
                正在检查技能…
              </div>
            )}
            {settings && (
              <>
                <h4 className="pane-subhead">安装范围与路径</h4>
                <div className="settings-group">
                  <div className="settings-row">
                    <span className="settings-row-label">范围</span>
                    <div
                      className="segmented"
                      role="group"
                      aria-label="技能安装范围"
                    >
                      <button
                        aria-pressed={scope === "project"}
                        disabled={loading || Boolean(pending)}
                        onClick={() => {
                          setScope("project");
                          void load("project", project);
                        }}
                      >
                        当前项目
                      </button>
                      <button
                        aria-pressed={scope === "user"}
                        disabled={loading || Boolean(pending)}
                        onClick={() => {
                          setScope("user");
                          void load("user", project);
                        }}
                      >
                        全局
                      </button>
                    </div>
                  </div>
                  {scope === "project" ? (
                    <form
                      className="settings-row"
                      onSubmit={(event) => {
                        event.preventDefault();
                        void load(scope, project);
                      }}
                    >
                      <label
                        className="settings-row-label"
                        htmlFor="skill-project-path"
                      >
                        项目目录
                      </label>
                      <input
                        id="skill-project-path"
                        className="settings-input mono"
                        value={project}
                        onChange={(event) => setProject(event.target.value)}
                        disabled={Boolean(pending)}
                      />
                      <button
                        className="text-button"
                        disabled={loading || Boolean(pending)}
                      >
                        更新目标
                      </button>
                    </form>
                  ) : (
                    <div className="settings-row">
                      <span className="settings-row-label">用户目录</span>
                      <code className="settings-code">
                        {settings.home_directory}
                      </code>
                    </div>
                  )}
                  {!settings.cli_available && (
                    <div className="settings-row">
                      <span className="settings-row-text warn-text">
                        还没有可用的 QScope
                        CLI，暂时无法安装技能。开发环境先运行 cargo build -p
                        qscope-cli。
                      </span>
                    </div>
                  )}
                </div>

                <div className="agent-list" role="list" aria-label="AI 工具">
                  {settings.targets.map((target) => (
                    <div
                      className="agent-row"
                      role="listitem"
                      key={target.agent}
                    >
                      <div
                        className="agent-text"
                        title={`${target.path}\n当前模板 SHA256 ${target.template_sha256}\n已安装内容 SHA256 ${target.installed_content_sha256 || "未安装"}`}
                      >
                        <strong>{target.name}</strong>
                        <span>
                          {target.installed_version
                            ? `v${target.installed_version}`
                            : "未记录版本"}
                          {target.installed_version !== target.version
                            ? ` → v${target.version}`
                            : " · 哈希已校验"}
                        </span>
                        <span
                          className={
                            target.state === "conflict"
                              ? "warn-text"
                              : undefined
                          }
                        >
                          {target.state === "conflict"
                            ? target.detail
                            : target.agent === "claude-code"
                              ? "技能与 CLI 白名单一并安装"
                              : "通过 CLI 使用抓包与界面控制"}
                        </span>
                      </div>
                      <span className={`install-state ${target.state}`}>
                        {skillStates[target.state] || target.state}
                      </span>
                      <div className="agent-actions">
                        {(target.state === "installed" ||
                          target.state === "update_available") && (
                          <button
                            className="text-button"
                            aria-label={`卸载 ${target.name} 技能`}
                            disabled={loading || Boolean(pending)}
                            onClick={() =>
                              void changeSkill(target.agent, "uninstall")
                            }
                          >
                            {pending?.agent === target.agent &&
                            pending.action === "uninstall"
                              ? "正在卸载…"
                              : "卸载"}
                          </button>
                        )}
                        <button
                          className="primary-button"
                          aria-label={`安装到 ${target.name}`}
                          disabled={
                            !settings.cli_available ||
                            loading ||
                            Boolean(pending) ||
                            target.state === "installed" ||
                            target.state === "conflict"
                          }
                          onClick={() =>
                            void changeSkill(target.agent, "install")
                          }
                        >
                          {pending?.agent === target.agent &&
                          pending.action === "install"
                            ? "正在安装…"
                            : target.state === "installed"
                              ? "已安装"
                              : target.state === "update_available"
                                ? "更新"
                                : "安装"}
                        </button>
                      </div>
                    </div>
                  ))}
                </div>
                <p className="settings-hint">
                  安装后在对应 AI
                  中重新加载技能或新开会话。金融相关开发、逆向、抓包和自动操作均在技能禁用范围内。
                </p>

                <h4 className="pane-subhead">其他 AI 工具</h4>
                <div className="settings-group">
                  <div className="settings-row">
                    <span className="settings-row-text">
                      把通用安装指令发给其他 AI 工具，按指令安装 QScope 技能。
                    </span>
                    <button
                      className="text-button"
                      onClick={() => void copy(settings.install_prompt)}
                    >
                      <Icon name="copy" size={13} />
                      复制通用安装指令
                    </button>
                  </div>
                  <details className="settings-details">
                    <summary>查看通用安装指令</summary>
                    <textarea
                      aria-label="通用安装指令"
                      readOnly
                      value={settings.install_prompt}
                    />
                  </details>
                </div>
              </>
            )}
          </section>
        )}
      </div>

      {frontend && (
        <Modal
          title="QQ DevTools · Chii"
          wide
          onClose={() => setFrontend(null)}
        >
          <div className="devtools-toolbar">
            <span>{frontend.frontend}</span>
            <a
              className="text-button"
              href={frontend.frontend_url}
              target="_blank"
              rel="noreferrer"
            >
              在新窗口打开
            </a>
            <button
              className="text-button"
              onClick={() => void copy(frontend.frontend_url)}
            >
              复制调试链接
            </button>
          </div>
          <iframe
            className="devtools-frame"
            title="QQ DevTools"
            src={frontend.frontend_url}
          />
        </Modal>
      )}
    </div>
  );
}
