import assert from "node:assert/strict";
import { spawn, execFile } from "node:child_process";
import { randomUUID, createHash } from "node:crypto";
import {
  mkdir,
  readFile,
  readdir,
  writeFile,
  copyFile,
} from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { chromium, expect } from "@playwright/test";
import { promisify } from "node:util";
import http from "node:http";
import { pathToFileURL } from "node:url";
import type { ChildProcess, ExecFileOptions } from "node:child_process";
import type { Browser } from "@playwright/test";
import type { AddressInfo } from "node:net";
import type { Catalog } from "../../apps/web/src/api.generated.ts";
import { seed, header, record } from "../web/fixture.mjs";

const execute = promisify(execFile);
const repo = resolve(import.meta.dirname, "../..");

const reference = JSON.parse(
  await readFile(join(repo, "build/desktop/current.json"), "utf8"),
);
const release: { release_id: string; assets: { path: string }[] } = JSON.parse(
  await readFile(join(dirname(reference.manifest), "result.json"), "utf8"),
);
const root = join(repo, ".tmp/packaged-cli", randomUUID());
const output = join(repo, "artifacts/architecture/packaged-cli", randomUUID());
await mkdir(root, { recursive: true });
await mkdir(output, { recursive: true });
const installation = join(root, "命令行 版本");
await mkdir(installation);
const archive = release.assets.find((asset) =>
  asset.path.endsWith("-cli-webui.zip"),
);
assert.ok(archive);
// Windows tar 的参数受代码页影响，工作目录通过 Unicode 进程接口传递。
await execute("tar", ["-xf", archive.path], { cwd: installation });
assert.deepEqual(await readdir(installation), ["qscope-cli.exe"]);
const cli = join(installation, "qscope-cli.exe");
const fixture = await seed(join(root, "synthetic"));
const data = join(root, "用户 数据");
const children: ChildProcess[] = [];
const manifest: Record<string, unknown> & {
  passed: boolean;
  evidence: string[];
  cold_ready_ms?: number;
} = {
  schema_version: 1,
  question:
    "Validate the shipped CLI and embedded WebUI without Node, 7-Zip, or a native host executable",
  source: "synthetic",
  time_utc: new Date().toISOString(),
  release_id: release.release_id,
  evidence: [],
  passed: false,
};
let browser: Browser | undefined;
let bridge: http.Server | undefined;

async function invoke(args: string[], options: ExecFileOptions = {}) {
  const result = await execute(cli, ["--json", "--data-dir", data, ...args], {
    cwd: fixture.root,
    windowsHide: true,
    timeout: 30_000,
    ...options,
  });
  return JSON.parse(String(result.stdout));
}
async function descriptor(directory = data) {
  return JSON.parse(await readFile(join(directory, "service.json"), "utf8"));
}
async function api(
  server: { endpoint: string; token: string },
  path: string,
  body?: unknown,
) {
  const response = await fetch(server.endpoint + "/api/v1" + path, {
    method: body ? "POST" : "GET",
    headers: {
      "X-QScope-Client": "web-v1",
      Authorization: "Bearer " + server.token,
      ...(body ? { "Content-Type": "application/json" } : {}),
    },
    body: body ? JSON.stringify(body) : undefined,
  });
  assert.equal(response.ok, true, await response.clone().text());
  return response.json();
}
function start(program: string, args: string[]) {
  const child = spawn(program, args, {
    cwd: fixture.root,
    windowsHide: true,
    env: { ...process.env, PATH: "" },
    stdio: "ignore",
  });
  children.push(child);
  return child;
}

try {
  const before = performance.now();
  // 后台继续存活时，冷启动命令也必须及时关闭调用方的输出管道。
  const cold = await invoke([
    "service",
    "start",
    "--port",
    "0",
    "--captures",
    fixture.captures,
  ]);
  assert.equal(cold.running, true);
  assert.equal(cold.pid, (await descriptor()).pid);
  manifest.detached_stdio_closed = true;
  const terminal = start(cli, [
    "--json",
    "--data-dir",
    data,
    "web",
    "--no-open",
    "--port",
    "0",
    "--captures",
    fixture.captures,
  ]);
  await expect
    .poll(
      async () => {
        try {
          return (await descriptor()).owner;
        } catch {
          return null;
        }
      },
      { timeout: 30_000 },
    )
    .toBe("qscope-service");
  let server = await descriptor();
  assert.notEqual(server.pid, terminal.pid);
  const logo = await fetch(server.endpoint + "/favicon.svg");
  assert.equal(logo.status, 200);
  assert.match(logo.headers.get("content-type") || "", /image\/svg\+xml/);
  assert.equal(
    await logo.text(),
    await readFile(join(repo, "apps/web/public/favicon.svg"), "utf8"),
  );
  manifest.brand_logo = true;
  assert.equal(
    (
      await fetch(server.endpoint + "/api/v1/sessions", {
        headers: { "X-QScope-Client": "web-v1" },
      })
    ).status,
    403,
  );
  const status = await invoke(["service", "start"]);
  assert.equal(status.pid, server.pid);
  manifest.cold_ready_ms = Math.round(performance.now() - before);
  const report = await invoke(["doctor"]);
  assert.equal(report.embedded_webui, true);
  assert.equal(report.embedded_devtools, true);
  assert.equal(report.embedded_probe, true);
  await assert.rejects(readdir(join(data, "cache/resources")));
  let catalog!: Catalog;
  await expect
    .poll(async () => {
      catalog = await api(server, "/sessions");
      return catalog.sessions.length;
    })
    .toBeGreaterThanOrEqual(3);
  const closed = catalog.sessions.find((session) =>
    session.path.endsWith("closed.qscap"),
  );
  assert.ok(closed);
  const original = createHash("sha256")
    .update(await readFile(join(fixture.captures, "closed.qscap")))
    .digest("hex");
  const packet = await invoke([
    "capture",
    "--server",
    "@" + join(data, "service.json"),
    "packet",
    closed.id,
    "17",
  ]);
  assert.equal(packet.event.command, "Test.SearchTarget");
  browser = await chromium.launch({
    channel: process.env.QSCOPE_BROWSER_CHANNEL || "msedge",
    headless: true,
  });
  const page = await browser.newPage();
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.goto(
    server.endpoint +
      "/?session=" +
      closed.id +
      "#qscope-token=" +
      server.token,
  );
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
  await expect(
    page.getByRole("button", { name: "字段 1 · varint: 150", exact: true }),
  ).toBeVisible();
  await assert.rejects(readdir(join(data, "cache/resources")));

  const resources = (await invoke(["runtime"])).resources;
  const forbidden: string[] = [];
  async function inspect(directory: string) {
    for (const item of await readdir(directory, { withFileTypes: true })) {
      const path = join(directory, item.name);
      if (item.isDirectory()) await inspect(path);
      else if (
        /\.(exe|dll)$/i.test(item.name) &&
        item.name !== "qscope-portable.dll"
      )
        forbidden.push(path);
    }
  }
  await inspect(resources);
  assert.deepEqual(forbidden, []);
  const owner = join(data, "guard/launcher.json");
  const spec = join(root, "guard.json");
  await writeFile(
    spec,
    JSON.stringify({
      schema_version: 1,
      kind: "qscope-native-host",
      profile: "guard",
      port: 0,
      data_directory: data,
      package_directory: join(data, "qq/guard"),
      resources_directory: resources,
      prebuilt_directory: join(resources, "native"),
      receipt_path: owner,
      duration_seconds: 0,
    }),
  );
  const guard = await invoke(["__collector", "self-test", spec], {
    timeout: 90_000,
  });
  assert.equal(guard.result, "self-test-passed");
  assert.equal(guard.public_setting_unchanged, true);
  assert.ok(
    Object.values(guard.guard_parent).every((value) => value !== false),
  );
  assert.ok(Object.values(guard.guard_child).every((value) => value !== false));
  await writeFile(join(output, "guard.json"), JSON.stringify(guard, null, 2));
  manifest.evidence.push("guard.json");

  // 使用另存名称的 QScope CLI 提供合成生产者身份。
  const producerPath = join(root, "QQ.exe");
  await copyFile(cli, producerPath);
  const producerData = join(root, "producer");
  start(producerPath, [
    "--data-dir",
    producerData,
    "service",
    "start",
    "--port",
    "0",
  ]);
  await expect
    .poll(async () => {
      try {
        return (await descriptor(producerData)).pid;
      } catch {
        return null;
      }
    })
    .toBeTruthy();
  const producerServer = await descriptor(producerData);
  const observed = await new Promise<{ birth: string }>((done, reject) => {
    const command = spawn(cli, ["--json", "__collector", "identify"], {
      windowsHide: true,
      stdio: ["pipe", "pipe", "pipe"],
    });
    let text = "";
    command.stdout.on("data", (bytes) => (text += bytes));
    command.on("error", reject);
    command.on("close", (code) =>
      code === 0
        ? done(JSON.parse(text)[0])
        : reject(new Error("Cannot identify synthetic producer")),
    );
    command.stdin.end(
      JSON.stringify([{ id: "producer", pid: producerServer.pid }]),
    );
  });
  const target = await browser.newPage();
  const targetFile = join(root, "synthetic-target.html");
  await writeFile(
    targetFile,
    '<!doctype html><title>Synthetic QQ renderer</title><main id="fixture">Synthetic DOM probe</main>',
  );
  await target.goto(pathToFileURL(targetFile).href);
  const token = "b".repeat(64);
  bridge = http.createServer(async (request, response) => {
    if (request.headers.authorization !== "Bearer " + token) {
      response.writeHead(403);
      response.end();
      return;
    }
    response.setHeader("Content-Type", "application/json");
    try {
      if (request.url === "/qscope/v1/info") {
        response.end(
          JSON.stringify({
            version: 1,
            pid: producerServer.pid,
            birth_filetime: observed.birth,
            build: "synthetic",
            endpoint: bridgeOrigin,
          }),
        );
        return;
      }
      let body = "";
      for await (const bytes of request) body += bytes;
      const action = JSON.parse(body);
      if (action.action === "windows") {
        response.end(
          JSON.stringify([
            {
              id: 1,
              title: "Synthetic QQ",
              visible: true,
              focused: true,
              width: 800,
              height: 600,
              web_contents_id: 1,
              url: target.url(),
              devtools_open: false,
            },
          ]),
        );
        return;
      }
      assert.equal(action.action, "inspect");
      assert.equal(action.window_id, 1);
      const source = await readFile(action.client_path, "utf8");
      assert.equal(
        createHash("sha256").update(source).digest("hex"),
        action.client_sha256,
      );
      let id = await target.evaluate(() => window.__qscopeChii?.id);
      if (!id) {
        await target.evaluate((url) => {
          window.ChiiServerUrl = url;
          window.ChiiTitle = "Synthetic QQ renderer";
        }, action.frontend_url);
        await target.evaluate(source);
        id = await target.evaluate(() => {
          const id = sessionStorage.getItem("chii-id");
          window.__qscopeChii = { id };
          return id;
        });
      }
      response.end(JSON.stringify({ target_id: id }));
    } catch (error) {
      response.writeHead(500);
      response.end(
        JSON.stringify({
          message: error instanceof Error ? error.message : String(error),
        }),
      );
    }
  });
  await new Promise<void>((done) => bridge!.listen(0, "127.0.0.1", done));
  const bridgeOrigin =
    "http://127.0.0.1:" + (bridge.address() as AddressInfo).port;
  const captureDirectory = join(fixture.captures, "producer/.qscope");
  await mkdir(captureDirectory, { recursive: true });
  await writeFile(
    join(captureDirectory, "devtools.json"),
    JSON.stringify({
      schema_version: 1,
      ready: true,
      pid: producerServer.pid,
      birth_filetime: observed.birth,
      endpoint: bridgeOrigin,
      token,
    }),
  );
  await writeFile(
    join(captureDirectory, "synthetic.qscap"),
    Buffer.concat([
      header(producerServer.pid, BigInt(observed.birth)),
      record(1, "Synthetic.Renderer"),
    ]),
  );
  await expect
    .poll(
      async () => (await api(server, "/settings/devtools/connections")).length,
      { timeout: 15_000 },
    )
    .toBe(1);
  const inspector = await api(server, "/settings/devtools/current/open", {});
  const frontend = await browser.newPage();
  await frontend.goto(inspector.frontend_url);
  await expect(
    frontend.getByRole("tab", { name: "Elements", exact: true }),
  ).toBeVisible({ timeout: 20_000 });
  await expect(
    frontend.getByText("Synthetic DOM probe", { exact: false }).first(),
  ).toBeVisible({ timeout: 20_000 });
  await frontend.screenshot({ path: join(output, "devtools.png") });
  manifest.evidence.push("devtools.png");
  await frontend.close();
  const backendIdentity = await new Promise<{ birth: string }>(
    (done, reject) => {
      const command = spawn(cli, ["__collector", "identify"], {
        windowsHide: true,
        stdio: ["pipe", "pipe", "pipe"],
      });
      let result = "";
      command.stdout.on("data", (bytes) => (result += bytes));
      command.once("error", reject);
      command.once("close", (code) =>
        code === 0
          ? done(JSON.parse(result)[0])
          : reject(new Error("Cannot identify owned backend")),
      );
      command.stdin.end(JSON.stringify([{ id: "backend", pid: server.pid }]));
    },
  );
  await invoke([
    "__collector",
    "stop",
    String(server.pid),
    backendIdentity.birth,
  ]);
  await invoke(["capture", "sessions"]);
  const previous = server;
  server = await descriptor();
  assert.notEqual(server.instance_id, previous.instance_id);
  assert.equal(server.endpoint, previous.endpoint);
  const targetsUrl = new URL(inspector.frontend_url);
  targetsUrl.pathname =
    targetsUrl.pathname.split("/").slice(0, 2).join("/") + "/targets";
  targetsUrl.search = "";
  await expect
    .poll(async () => (await (await fetch(targetsUrl)).json()).targets.length, {
      timeout: 20_000,
    })
    .toBe(1);
  const recovered = await api(server, "/settings/devtools/current/open", {});
  const recoveredFrontend = await browser.newPage();
  await recoveredFrontend.goto(recovered.frontend_url);
  await expect(
    recoveredFrontend
      .getByText("Synthetic DOM probe", { exact: false })
      .first(),
  ).toBeVisible({ timeout: 20_000 });
  manifest.backend_recovery = true;
  await api(producerServer, "/service/control", {
    action: "stop",
    client_id: "c".repeat(48),
  });
  assert.equal(
    createHash("sha256")
      .update(await readFile(join(fixture.captures, "closed.qscap")))
      .digest("hex"),
    original,
  );
  assert.deepEqual(errors, []);
  manifest.real_chii_dom = true;
  manifest.passed = true;
} catch (error) {
  manifest.error = error instanceof Error ? error.stack : String(error);
  throw error;
} finally {
  await browser?.close().catch(() => {});
  if (bridge) {
    bridge.closeAllConnections();
    await new Promise<void>((done) => bridge!.close(() => done()));
  }
  for (const directory of [data, join(root, "producer")]) {
    try {
      await api(await descriptor(directory), "/service/control", {
        action: "stop",
        client_id: "d".repeat(48),
      });
    } catch {
      /* 服务已经退出 */
    }
  }
  for (const child of children) if (child.exitCode === null) child.kill();
  await writeFile(
    join(output, "manifest.json"),
    JSON.stringify(manifest, null, 2),
  );
  console.log(
    JSON.stringify({
      output,
      passed: manifest.passed,
      cold_ready_ms: manifest.cold_ready_ms,
    }),
  );
}
