import { randomUUID } from "node:crypto";
import { mkdir, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";
import { spawn } from "node:child_process";
import { buildNative } from "./build-native.ts";
import { command, writeJson } from "./build-utils.ts";

const repo = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const { values } = parseArgs({
  options: {
    mode: { type: "string", default: "run" },
    profile: { type: "string", default: "research" },
    "data-dir": { type: "string" },
    "package-dir": { type: "string" },
    prebuilt: { type: "string" },
    installer: { type: "string" },
    port: { type: "string", default: "0" },
    duration: { type: "string", default: "0" },
    "no-capture": { type: "boolean", default: false },
    diagnostics: { type: "boolean", default: false },
    "wait-for-debugger": { type: "boolean", default: false },
    help: { type: "boolean", default: false },
  },
});
if (values.help) {
  console.log(
    "node tools/qq.ts --mode prepare|self-test|run [--installer PATH] [--profile research] [--port 19860]\n  [--duration SECONDS] [--no-capture] [--diagnostics] [--wait-for-debugger]\n  [--data-dir PATH] [--package-dir PATH] [--prebuilt PATH]",
  );
  process.exit(0);
}
if (process.platform !== "win32" || process.arch !== "x64")
  throw new Error(
    `QQ collector adapter is unavailable for ${process.platform}-${process.arch}`,
  );
if (!["prepare", "self-test", "run"].includes(values.mode!))
  throw new Error("Unsupported QQ operation");
if (!/^[A-Za-z0-9_-]{1,32}$/.test(values.profile!))
  throw new Error("Invalid QQ profile name");
const port = Number(values.port),
  duration = Number(values.duration);
if (!Number.isInteger(port) || (port !== 0 && (port < 1024 || port > 65535)))
  throw new Error("Invalid control bridge port");
if (!Number.isInteger(duration) || duration < 0 || duration > 86400)
  throw new Error("Invalid capture duration");
const native = values.prebuilt ? resolve(values.prebuilt) : await buildNative();
await command("cargo", ["build", "-p", "qscope-cli", "--locked"], {
  cwd: repo,
});
const cli = join(repo, "target/debug/qscope-cli.exe");
const data = resolve(values["data-dir"] || join(repo, "artifacts"));
const run = join(data, "native-launches", randomUUID());
await mkdir(run, { recursive: true });
const request = join(run, "request.json");
const spec = {
  schema_version: 1,
  kind: "qscope-native-host",
  profile: values.profile,
  port,
  data_directory: data,
  package_directory: resolve(
    values["package-dir"] || join(data, "qq/9.9.37-261103-x64"),
  ),
  resources_directory: repo,
  prebuilt_directory: native,
  installer_path: values.installer ? resolve(values.installer) : "",
  receipt_path: join(run, "launcher.json"),
  progress_path: join(run, "progress.json"),
  duration_seconds: duration,
  capture: !values["no-capture"],
  startup_diagnostics: values.diagnostics,
  wait_for_debugger: values["wait-for-debugger"],
};
await writeJson(request, spec);
if (values.mode === "run") {
  const preflight = join(run, "preflight");
  await mkdir(preflight);
  const preflightRequest = join(preflight, "request.json");
  await writeJson(preflightRequest, {
    ...spec,
    receipt_path: join(preflight, "launcher.json"),
  });
  await command(cli, ["__collector", "self-test", preflightRequest], {
    cwd: repo,
    timeout: 90_000,
  });
}
console.log(
  `QQ operation ${values.mode}\nEvidence ${run}\nStop file ${join(run, "stop")}`,
);
const child = spawn(cli, ["__collector", values.mode!, request], {
  cwd: repo,
  stdio: "inherit",
  windowsHide: true,
});
const stop = () => {
  void writeFile(join(run, "stop"), "stop").catch((error) =>
    console.error(error.message),
  );
};
process.on("SIGINT", stop);
process.on("SIGTERM", stop);
child.once("error", (error) => {
  console.error(error.message);
  process.exitCode = 1;
});
child.once("close", (code) => {
  process.removeListener("SIGINT", stop);
  process.removeListener("SIGTERM", stop);
  process.exitCode = code ?? 1;
});
