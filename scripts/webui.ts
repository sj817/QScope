import { mkdir, rm, stat } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";
import { spawn } from "node:child_process";
import { command, pnpm } from "./build-utils.ts";

const repo = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const { values } = parseArgs({
  options: {
    captures: { type: "string", multiple: true },
    port: { type: "string" },
    "no-build": { type: "boolean", default: false },
    fresh: { type: "boolean", default: false },
    reset: { type: "boolean", default: false },
    help: { type: "boolean", default: false },
  },
});
if (values.help) {
  console.log("pnpm dev [--no-build] [--port 19840] [--captures PATH ...]");
  console.log("pnpm dev --fresh [--reset] [--no-build] [--port 19842]");
  console.log("  --fresh  在 .tmp/webui-fresh/ 使用独立空目录预览首次启动");
  console.log(
    "  --reset  清除首次启动预览目录，包括该目录内已下载的 QQ 安装包",
  );
  process.exit(0);
}
if (values.reset && !values.fresh)
  throw new Error("--reset only applies to --fresh");
if (values.fresh && values.captures?.length)
  throw new Error("--fresh starts without capture directories");
const port = Number(values.port ?? (values.fresh ? "19842" : "19840"));
if (!Number.isInteger(port) || port < 1 || port > 65535)
  throw new Error("Invalid loopback port");
// 首次启动预览使用独立数据目录。
const fresh = join(repo, ".tmp/webui-fresh");
if (values.reset) await rm(fresh, { recursive: true, force: true });
const storage = values.fresh
  ? join(fresh, "data")
  : join(repo, ".tmp/dev/data");
const defaults = join(storage, "captures");
if (!values.captures?.length) await mkdir(defaults, { recursive: true });
const roots = (values.captures?.length ? values.captures : [defaults]).map(
  (path) => resolve(path),
);
for (const path of roots)
  if (!(await stat(path)).isDirectory())
    throw new Error(`Capture directory is unavailable (${path})`);
if (!values["no-build"]) {
  if (
    (process.platform === "win32" && process.arch === "x64") ||
    (process.platform === "darwin" && process.arch === "arm64")
  )
    await pnpm(["build:native"], repo);
  await command(
    "cargo",
    ["run", "-p", "qscope-web", "--locked", "--", "types"],
    { cwd: repo },
  );
  await pnpm(["--dir", "apps/web", "run", "build"], repo);
  await command(
    "cargo",
    ["build", "-p", "qscope-web", "-p", "qscope-cli", "--locked"],
    { cwd: repo },
  );
}
const executable = join(
  resolve(process.env.CARGO_TARGET_DIR || join(repo, "target")),
  "debug",
  process.platform === "win32" ? "qscope-web.exe" : "qscope-web",
);
await stat(executable);
const imports = values.fresh
  ? join(fresh, "imports")
  : join(storage, "imports");
const args = [
  "serve",
  "--port",
  String(port),
  "--ui-dir",
  join(repo, "apps/web/dist"),
  "--imports",
  imports,
];
if (storage) args.push("--storage", storage);
for (const root of roots) args.push("--captures", root);
if (values.fresh)
  console.log(
    `First-run preview with data in ${fresh}. Open http://127.0.0.1:${port}/`,
  );
else console.log(`开发 WebUI http://127.0.0.1:${port}/，数据目录 ${storage}`);
const child = spawn(executable, args, { cwd: repo, stdio: "inherit" });
// 终端会向两个进程发送 Ctrl+C，保留包装进程直到服务完成落盘。
process.on("SIGINT", () => {});
process.on("SIGTERM", () => child.kill("SIGTERM"));
child.once("error", (error) => {
  console.error(error.message);
  process.exitCode = 1;
});
child.once("close", (code) => {
  process.exitCode = code ?? 1;
});
