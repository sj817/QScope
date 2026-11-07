// 使用 showcase.mjs 合成抓包生成真实 WebUI 的 README 截图。
// 先运行 pnpm build:backend 和 pnpm build:webui，再运行 pnpm screenshots [--out DIR]。
import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import { access, cp, copyFile, mkdir, readFile, rm } from "node:fs/promises";
import { join, resolve } from "node:path";
import { parseArgs } from "node:util";
import { chromium } from "@playwright/test";
import { seedShowcase } from "./showcase.mjs";

const repo = resolve(import.meta.dirname, "../..");
const { values } = parseArgs({
  options: {
    out: { type: "string" },
    port: { type: "string", default: "19843" },
  },
});
const out = resolve(values.out || join(repo, ".github/screenshots"));
const port = Number(values.port);
const base = `http://127.0.0.1:${port}`;
const exe = (name) =>
  join(
    repo,
    "target/debug",
    process.platform === "win32" ? `${name}.exe` : name,
  );

async function exists(path) {
  try {
    await access(path);
    return true;
  } catch {
    return false;
  }
}

// 为隔离工作区准备已构建的 CLI 和采集器，让设置页显示等待下载 QQ 的完整开发环境。
async function provision(root) {
  if (await exists(exe("qscope-cli"))) {
    await mkdir(join(root, "target/debug"), { recursive: true });
    await copyFile(
      exe("qscope-cli"),
      join(root, "target/debug", exe("qscope-cli").split(/[\\/]/).at(-1)),
    );
  }
  const native = join(repo, "build/native/qqnt-windows-x64");
  try {
    const current = JSON.parse(
      await readFile(join(native, "current.json"), "utf8"),
    );
    const build = current.build_id.slice(0, 16);
    await cp(
      join(native, build),
      join(root, "build/native/qqnt-windows-x64", build),
      { recursive: true },
    );
    await copyFile(
      join(native, "current.json"),
      join(root, "build/native/qqnt-windows-x64/current.json"),
    );
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
  }
}

async function ready(url) {
  for (let attempt = 0; attempt < 100; attempt++) {
    try {
      if ((await fetch(url)).ok) return;
    } catch {
      /* 服务尚未开始监听 */
    }
    await new Promise((done) => setTimeout(done, 200));
  }
  throw new Error(`The showcase server did not start at ${url}`);
}

async function api(path) {
  const response = await fetch(base + "/api/v1" + path, {
    headers: { "X-QScope-Client": "web-v1" },
  });
  if (!response.ok) throw new Error(`${path}: HTTP ${response.status}`);
  return response.json();
}

async function shoot(browser, theme, shots) {
  const context = await browser.newContext({
    viewport: { width: 1440, height: 900 },
    deviceScaleFactor: 2,
    locale: "zh-CN",
    timezoneId: "Asia/Shanghai",
  });
  await context.addInitScript((value) => {
    try {
      localStorage.setItem("qscope.appearance", value);
    } catch {
      /* 页面继续使用系统主题 */
    }
  }, theme);
  const page = await context.newPage();
  page.on("pageerror", (error) => console.error("page error:", error.message));
  try {
    for (const [name, run] of shots) {
      await run(page);
      await page.waitForTimeout(400);
      const path = join(out, `${name}-${theme}.png`);
      await page.screenshot({ path, animations: "disabled", caret: "hide" });
      console.log(path);
    }
  } finally {
    await context.close();
  }
}

const root = join(repo, ".tmp/readme-screenshots", randomUUID());
const data = await seedShowcase(root);
await provision(root);
await mkdir(out, { recursive: true });
const server = spawn(
  exe("qscope-web"),
  [
    "serve",
    "--port",
    String(port),
    "--captures",
    data.captures,
    "--imports",
    data.imports,
    "--ui-dir",
    join(repo, "apps/web/dist"),
    "--workspace",
    root,
  ],
  { cwd: repo, stdio: ["ignore", "ignore", "inherit"], windowsHide: true },
);
const exited = new Promise((done) => server.on("exit", done));
try {
  await ready(base);
  const catalog = await api("/sessions");
  const session = catalog.sessions.find((entry) => entry.account === "10001");
  const events = (await api(`/sessions/${session.id}/events?limit=500`)).events;
  // 按时间写入两条纯文本、富内容消息、JSON 卡片和另外两条纯文本。
  const pushes = events
    .filter((event) => event.command.endsWith("OlPushService.MsgPush"))
    .sort((left, right) =>
      Number(BigInt(left.event_id) - BigInt(right.event_id)),
    );
  const [, earlier, rich, card] = pushes;
  const row = (event) => `选择包 ${event.event_id}: ${event.command}`;
  const list = (page) => page.getByRole("listbox", { name: "数据包列表" });
  const open = async (page, event) => {
    await page.goto(`${base}/?session=${session.id}`);
    await list(page)
      .getByRole("option", { name: row(event) })
      .click();
    await page.getByRole("treeitem").first().waitFor();
  };
  const browser = await chromium.launch({
    channel: process.env.QSCOPE_BROWSER_CHANNEL || "msedge",
  });
  try {
    const workspace = ["workspace", (page) => open(page, rich)];
    await shoot(browser, "light", [
      workspace,
      [
        "value",
        async (page) => {
          await open(page, card);
          const value = page
            .locator(".json-value")
            .filter({ hasText: "com.example.qscope.demo" })
            .last();
          await value.hover();
          await page.getByRole("button", { name: "查看完整值" }).last().click();
          await page.getByRole("dialog").waitFor();
        },
      ],
      [
        "compare",
        async (page) => {
          await open(page, rich);
          await page
            .getByRole("button", { name: "对比模式", exact: true })
            .click();
          await list(page)
            .getByRole("option", { name: row(earlier) })
            .click();
          await list(page)
            .getByRole("option", { name: row(rich) })
            .click();
          await page
            .getByRole("button", { name: "对比 2", exact: true })
            .click();
          await page
            .getByRole("dialog", { name: "多包对比 · 2 个包" })
            .waitFor();
        },
      ],
      [
        "commands",
        async (page) => {
          await open(page, rich);
          await page
            .getByRole("button", { name: "命令筛选", exact: true })
            .click();
          await page.getByRole("dialog").waitFor();
        },
      ],
      [
        // 运行环境检查保持收起，展开内容会暴露本机路径。
        "settings",
        async (page) => {
          await page.goto(`${base}/?page=settings&section=qq`);
          await page.getByRole("button", { name: "下载并启动 QQ" }).waitFor();
        },
      ],
    ]);
    await shoot(browser, "dark", [workspace]);
  } finally {
    await browser.close();
  }
} finally {
  server.kill();
  await exited;
  await rm(root, { recursive: true, force: true });
}
