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
  console.log(
    "node tools/webui.ts [--no-build] [--port 19840] [--captures PATH ...]",
  );
  console.log(
    "node tools/webui.ts --fresh [--reset] [--no-build] [--port 19842]",
  );
  console.log(
    "  --fresh  first-run preview: empty data directory and no captures under .tmp/webui-fresh/",
  );
  console.log(
    "  --reset  delete .tmp/webui-fresh/ first, including a QQ package downloaded there",
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
// A fresh preview keeps its own data directory, so the first-run screens appear without touching artifacts/.
const fresh = join(repo, ".tmp/webui-fresh");
if (values.reset) await rm(fresh, { recursive: true, force: true });
const storage = values.fresh ? join(fresh, "data") : undefined;
const defaults = join(
  storage ?? join(repo, "artifacts"),
  "qq/9.9.37-261103-x64/portable/profiles",
);
if (!values.captures?.length) await mkdir(defaults, { recursive: true });
const roots = (values.captures?.length ? values.captures : [defaults]).map(
  (path) => resolve(path),
);
for (const path of roots)
  if (!(await stat(path)).isDirectory())
    throw new Error(`Capture directory is unavailable (${path})`);
if (!values["no-build"]) {
  await pnpm(
    [
      "--dir",
      "collectors/devtools",
      "install",
      "--frozen-lockfile",
      "--ignore-scripts",
    ],
    repo,
  );
  await command(
    "cargo",
    ["run", "-p", "qscope-web", "--locked", "--", "types"],
    { cwd: repo },
  );
  await pnpm(["--dir", "apps/web", "install", "--frozen-lockfile"], repo);
  await pnpm(["--dir", "apps/web", "run", "build"], repo);
  await command(
    "cargo",
    ["build", "-p", "qscope-web", "-p", "qscope-cli", "--release", "--locked"],
    { cwd: repo },
  );
}
const executable = join(
  repo,
  "target/release",
  process.platform === "win32" ? "qscope-web.exe" : "qscope-web",
);
await stat(executable);
const imports = values.fresh
  ? join(fresh, "imports")
  : join(repo, "artifacts/captures/imported");
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
const child = spawn(executable, args, { cwd: repo, stdio: "inherit" });
// An attached terminal delivers Ctrl+C to both processes. Keep the wrapper alive for the server's flush.
process.on("SIGINT", () => {});
process.on("SIGTERM", () => child.kill("SIGTERM"));
child.once("error", (error) => {
  console.error(error.message);
  process.exitCode = 1;
});
child.once("close", (code) => {
  process.exitCode = code ?? 1;
});
