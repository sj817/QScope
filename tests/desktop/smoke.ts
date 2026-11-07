// 使用合成抓包及隔离数据目录验收原生 WebView2。
import assert from "node:assert/strict";
import { spawn, execFile, execFileSync } from "node:child_process";
import { createHash, randomUUID } from "node:crypto";
import { appendFile, mkdir, readFile, writeFile, stat } from "node:fs/promises";
import http from "node:http";
import net from "node:net";
import { chromium, expect } from "@playwright/test";
import { dirname, join, resolve } from "node:path";
import { promisify } from "node:util";
import type { ChildProcess } from "node:child_process";
import type { Browser, Page } from "@playwright/test";
import type { AddressInfo } from "node:net";
import type { Catalog } from "../../apps/web/src/api.generated.ts";
import { seed, record } from "../web/fixture.mjs";

const execute = promisify(execFile);
const repo = resolve(import.meta.dirname, "../..");
if (process.platform !== "win32")
  throw new Error("This test requires the Windows WebView2 adapter");
const current = JSON.parse(
  await readFile(join(repo, "build/desktop/current.json"), "utf8"),
);
const built = JSON.parse(
  await readFile(join(dirname(current.manifest), "result.json"), "utf8"),
);
const executable = resolve(
  process.argv[2] || join(built.portable, "QScope.exe"),
);
const cliPath = join(dirname(executable), "qscope-cli.exe");
const output = resolve(
  process.argv[3] || join(repo, "artifacts/desktop", randomUUID()),
);
const fixture = await seed(join(repo, ".tmp/desktop-smoke", randomUUID()));
const data = join(fixture.root, "用户 数据");
interface Descriptor {
  owner: string;
  pid: number;
  endpoint: string;
  token: string;
}
interface OwnedApp {
  child: ChildProcess;
  directory: string;
  browser: Browser | null;
  descriptor: Descriptor | null;
  endpoint: string;
  birth: string;
  page?: Page;
}
type RunningApp = OwnedApp & { descriptor: Descriptor; page: Page };
const apps: OwnedApp[] = [];
const failures: string[] = [];
const original = createHash("sha256")
  .update(await readFile(join(fixture.captures, "closed.qscap")))
  .digest("hex");
await mkdir(output, { recursive: true });
const result: Record<string, unknown> & { passed: boolean } = {
  schema_version: 1,
  question:
    "Validate the thin desktop, shared backend, active recording lifetime and packaged integrations",
  source: "synthetic",
  time_utc: new Date().toISOString(),
  executable,
  fixture: fixture.root,
  passed: false,
};

async function availablePort() {
  const server = net.createServer();
  await new Promise<void>((done, reject) =>
    server.once("error", reject).listen(0, "127.0.0.1", done),
  );
  const port = (server.address() as AddressInfo).port;
  await new Promise<void>((done) => server.close(() => done()));
  return port;
}

async function cli(directory: string, ...args: string[]) {
  const { stdout } = await execute(
    cliPath,
    ["--json", "--data-dir", directory, ...args],
    { cwd: fixture.root, windowsHide: true, timeout: 30_000 },
  );
  return JSON.parse(stdout);
}

async function api(app: OwnedApp, path: string, body?: unknown) {
  assert.ok(app.descriptor);
  const response = await fetch(app.endpoint + "/api/v1" + path, {
    headers: {
      "X-QScope-Client": "web-v1",
      Authorization: "Bearer " + app.descriptor.token,
      "Content-Type": "application/json",
    },
    method: body ? "POST" : "GET",
    body: body ? JSON.stringify(body) : undefined,
  });
  assert.equal(response.ok, true, await response.clone().text());
  return response.json();
}

async function launch(
  directory: string,
  withRoot = false,
  capture: string | null = null,
): Promise<RunningApp> {
  const port = await availablePort();
  const child = spawn(
    executable,
    [
      "--data-dir",
      directory,
      ...(withRoot ? ["--capture-root", fixture.captures] : []),
      ...(capture ? [capture] : []),
    ],
    {
      cwd: fixture.root,
      stdio: "ignore",
      env: {
        ...process.env,
        WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port} --remote-debugging-address=127.0.0.1`,
      },
    },
  );
  child.once("error", (error) => failures.push(error.message));
  const app: OwnedApp = {
    child,
    directory,
    browser: null,
    descriptor: null,
    endpoint: "",
    birth: "",
  };
  apps.push(app);
  const identified = JSON.parse(
    execFileSync(cliPath, ["__collector", "identify"], {
      input: JSON.stringify([{ id: "desktop", pid: child.pid }]),
      encoding: "utf8",
      windowsHide: true,
      timeout: 5000,
    }),
  );
  app.birth = identified[0].birth;
  assert.match(app.birth, /^\d+$/);
  await expect
    .poll(
      async () => {
        try {
          app.descriptor = JSON.parse(
            await readFile(join(directory, "service.json"), "utf8"),
          );
          return app.descriptor!.owner;
        } catch {
          return null;
        }
      },
      { timeout: 30_000 },
    )
    .toBe("qscope-service");
  assert.ok(app.descriptor);
  assert.notEqual(app.descriptor.pid, child.pid);
  app.endpoint = app.descriptor.endpoint;
  await expect
    .poll(
      async () => {
        try {
          return (await fetch(`http://127.0.0.1:${port}/json/version`)).ok;
        } catch {
          return false;
        }
      },
      { timeout: 30_000 },
    )
    .toBe(true);
  const browser = await chromium.connectOverCDP(`http://127.0.0.1:${port}`);
  app.browser = browser;
  await expect
    .poll(() => {
      app.page = browser
        .contexts()
        .flatMap((context) => context.pages())
        .find((page) => page.url().startsWith(app.endpoint));
      return Boolean(app.page);
    })
    .toBe(true);
  assert.ok(app.page);
  app.page.on("pageerror", (error) => failures.push(error.message));
  await app.page.waitForLoadState("domcontentloaded");
  assert.equal(await app.page.evaluate(() => window.__QSCOPE_DESKTOP__), true);
  return app as RunningApp;
}

async function close(app: OwnedApp) {
  if (app.child.exitCode === null) {
    const { stdout } = await execute(
      cliPath,
      [
        "__collector",
        "close-window",
        String(app.child.pid),
        app.birth,
        executable,
        "QScope",
      ],
      { windowsHide: true, timeout: 5000 },
    );
    assert.equal(JSON.parse(stdout).closed, true);
    await expect
      .poll(() => app.child.exitCode, { timeout: 20_000 })
      .not.toBe(null);
  }
  await app.browser?.close();
  app.browser = null;
}

const blocker = net.createServer();
await new Promise<void>((done) =>
  blocker
    .once("error", (error) => {
      if ((error as NodeJS.ErrnoException).code !== "EADDRINUSE")
        failures.push(error.message);
      done();
    })
    .listen(19840, "127.0.0.1", done),
);
const capability = "a".repeat(48);
const inspector = http.createServer((_, response) => {
  response.setHeader("Content-Type", "text/html");
  response.end(
    "<title>Synthetic DevTools</title><main>Synthetic inspector</main>",
  );
});
await new Promise<void>((done) => inspector.listen(0, "127.0.0.1", done));
const inspectorUrl = `http://127.0.0.1:${(inspector.address() as AddressInfo).port}/${capability}/front_end/chii_app.html`;

try {
  const app = await launch(data, true);
  assert.notEqual(new URL(app.endpoint).port, "19840");
  const backendPid = app.descriptor.pid;
  let catalog!: Catalog;
  await expect
    .poll(async () => {
      catalog = await cli(data, "capture", "sessions");
      return catalog.sessions.length;
    })
    .toBeGreaterThanOrEqual(3);
  const session = catalog.sessions.find((session) =>
    session.path.endsWith("closed.qscap"),
  );
  assert.ok(session);
  const page = app.page;
  await page.goto(app.endpoint + "/?session=" + session.id);
  await page
    .getByRole("searchbox", { name: "搜索命令或序号" })
    .fill("SearchTarget");
  await page
    .getByRole("option", { name: "选择包 17: Test.SearchTarget" })
    .click();
  await expect(
    page.getByRole("heading", { name: "Test.SearchTarget" }),
  ).toBeVisible();
  await expect(page.locator(".brand-mark")).toHaveCSS(
    "mask-image",
    /favicon\.svg/,
  );
  await expect(page.locator(".brand-mark")).toHaveCSS(
    "background-color",
    "rgb(109, 40, 255)",
  );
  await page.screenshot({ path: join(output, "desktop-packet.png") });
  await page.getByRole("button", { name: "打开设置", exact: true }).click();
  await page.getByRole("tab", { name: /AI 接入/ }).click();
  await expect(
    page.getByRole("heading", { name: "安装范围与路径", exact: true }),
  ).toBeVisible();
  const setup = await api(app, "/settings/qq");
  assert.equal(setup.available, true);
  await page.getByRole("button", { name: "当前项目", exact: true }).click();
  await page.getByLabel("项目目录", { exact: true }).fill(fixture.root);
  await page.getByRole("button", { name: "更新目标", exact: true }).click();
  const install = page.getByRole("button", {
    name: "安装到 Claude Code",
    exact: true,
  });
  await expect(install).toBeEnabled();
  await install.click();
  await expect(install).toHaveText("已安装");
  const skill = await readFile(
    join(fixture.root, ".claude/skills/qscope/SKILL.md"),
    "utf8",
  );
  assert(skill.includes("任何金融相关任务直接拒绝"));
  assert(skill.includes("@" + join(data, "service.json")));
  assert(!skill.includes("{{"));
  const permissionsPath = join(fixture.root, ".claude/settings.local.json");
  assert(
    JSON.parse(await readFile(permissionsPath, "utf8")).permissions.allow.some(
      (rule: string) => rule.includes("qscope-cli.exe"),
    ),
  );
  await page
    .getByRole("button", { name: "卸载 Claude Code 技能", exact: true })
    .click();
  await expect(install).toHaveText("安装");
  await assert.rejects(
    readFile(join(fixture.root, ".claude/skills/qscope/SKILL.md")),
  );
  assert.equal(
    JSON.parse(await readFile(permissionsPath, "utf8")).permissions.allow
      .length,
    0,
  );
  await install.click();
  await expect(install).toHaveText("已安装");
  await page.screenshot({ path: join(output, "desktop-settings.png") });
  result.skill_install_uninstall = true;
  await page.route("**/api/v1/settings/qq", (route) =>
    route.fulfill({ json: { ...setup, stage: "running", pid: 123 } }),
  );
  await page.route("**/api/v1/settings/devtools/current/open", (route) =>
    route.fulfill({
      json: { opened: true, frontend_url: inspectorUrl, frontend: "Synthetic" },
    }),
  );
  await page.getByRole("tab", { name: /QQ 抓包/ }).click();
  await page
    .getByRole("button", { name: "打开 DevTools", exact: true })
    .click();
  await expect(
    page
      .frameLocator('iframe[title="QQ DevTools"]')
      .getByText("Synthetic inspector"),
  ).toBeVisible();
  assert.ok(app.browser);
  const popupPromise = app.browser.contexts()[0].waitForEvent("page");
  await page.getByRole("link", { name: "在新窗口打开", exact: true }).click();
  await expect(
    (await popupPromise).getByText("Synthetic inspector"),
  ).toBeVisible();
  result.devtools_frame_and_window = true;

  const duplicate = spawn(
    executable,
    ["--data-dir", data, join(fixture.captures, "closed.qscap")],
    { cwd: fixture.root, windowsHide: true, stdio: "ignore" },
  );
  const duplicateCode = await new Promise<number | null>((done, reject) => {
    duplicate.once("error", reject);
    duplicate.once("close", done);
    setTimeout(() => {
      duplicate.kill();
      reject(
        new Error("Duplicate desktop did not activate the original window"),
      );
    }, 15_000).unref();
  });
  assert.equal(duplicateCode, 0);
  await expect
    .poll(() => new URL(page.url()).searchParams.get("session"))
    .toBe(session.id);
  await expect(
    page.getByRole("searchbox", { name: "搜索命令或序号" }),
  ).toBeVisible();
  const live = catalog.sessions.find((session) =>
    session.path.endsWith("live.qscap"),
  );
  assert.ok(live);
  const recording = await api(app, "/recordings", { session_id: live.id });
  await appendFile(
    join(fixture.captures, "live.qscap"),
    record(2, "Desktop.Recorded", Buffer.from([8, 42])),
  );
  await expect
    .poll(
      async () =>
        (await api(app, "/recordings")).find(
          (job: { id: string }) => job.id === recording.id,
        ).packets,
    )
    .toBe("1");
  await close(app);
  assert.equal((await cli(data, "service", "status")).pid, backendPid);
  await assert.rejects(
    cli(data, "service", "stop"),
    (error: NodeJS.ErrnoException & { stderr?: string }) =>
      JSON.parse(error.stderr || "{}").code === "active_tasks",
  );
  await appendFile(
    join(fixture.captures, "live.qscap"),
    record(3, "Desktop.StillRecording", Buffer.from([8, 43])),
  );
  await expect
    .poll(
      async () =>
        (await api(app, "/recordings")).find(
          (job: { id: string }) => job.id === recording.id,
        ).packets,
    )
    .toBe("2");
  const reopened = await launch(data);
  assert.equal(reopened.descriptor.pid, backendPid);
  await cli(data, "capture", "record", "stop", recording.id);
  const { stdout } = await execute(
    cliPath,
    ["--json", "native", recording.path],
    { windowsHide: true },
  );
  const summary = stdout
    .trim()
    .split(/\r?\n/)
    .map((line) => JSON.parse(line))
    .find((value) => value.type === "native_summary");
  assert.equal(summary.clean_end, true);
  assert.equal(summary.packets, "2");
  result.recording_survives_window_close = true;
  await close(reopened);
  await cli(data, "service", "stop");
  const restored = await launch(data);
  await expect
    .poll(async () => (await api(restored, "/sessions")).sessions.length)
    .toBeGreaterThanOrEqual(4);
  assert((await stat(join(data, "cache/index/captures-v1.sqlite"))).size > 0);
  const independent = await launch(
    join(fixture.root, "另一个 实例"),
    false,
    join(fixture.captures, "closed.qscap"),
  );
  assert.notEqual(independent.descriptor.pid, restored.descriptor.pid);
  await expect
    .poll(async () => (await api(independent, "/sessions")).sessions.length)
    .toBe(1);
  assert.equal(
    (await api(independent, "/sessions")).sessions[0].id,
    new URL(independent.page.url()).searchParams.get("session"),
  );
  result.capture_file_launch_and_handoff = true;
  await close(independent);
  await cli(independent.directory, "service", "stop");
  await close(restored);
  await cli(data, "service", "stop");
  result.capture_roots_restored = true;
  result.data_scoped_desktop_instances = true;
  assert.equal(
    createHash("sha256")
      .update(await readFile(join(fixture.captures, "closed.qscap")))
      .digest("hex"),
    original,
  );
  assert.deepEqual(failures, []);
  result.passed = true;
} catch (error) {
  result.error = error instanceof Error ? error.stack : String(error);
  throw error;
} finally {
  for (const app of apps) {
    await app.browser?.close().catch(() => {});
    if (app.child.exitCode === null) app.child.kill();
    try {
      for (const recording of await api(app, "/recordings"))
        if (recording.state === "recording")
          await api(app, "/recordings/" + recording.id + "/stop", {});
      await cli(app.directory, "service", "stop");
    } catch {
      /* 已退出的应用完成进程清理 */
    }
  }
  inspector.closeAllConnections();
  await new Promise<void>((done) => inspector.close(() => done()));
  if (blocker.listening)
    await new Promise<void>((done) => blocker.close(() => done()));
  await writeFile(
    join(output, "manifest.json"),
    JSON.stringify(result, null, 2),
  );
}
console.log(
  JSON.stringify({
    passed: result.passed,
    manifest: join(output, "manifest.json"),
  }),
);
