import { spawn } from "node:child_process";
import { resolve } from "node:path";
import { seed } from "./fixture.mjs";

const repo = resolve(import.meta.dirname, "../..");
const fixture = await seed(
  process.env.QSCOPE_E2E_DIR || resolve(repo, ".tmp/webui-e2e"),
);
const executable = resolve(
  repo,
  "target/debug",
  process.platform === "win32" ? "qscope-web.exe" : "qscope-web",
);
const server = spawn(
  executable,
  [
    "serve",
    "--port",
    "19841",
    "--captures",
    fixture.captures,
    "--imports",
    fixture.imports,
    "--ui-dir",
    resolve(repo, "apps/web/dist"),
    "--workspace",
    fixture.root,
  ],
  { cwd: repo, stdio: "inherit", windowsHide: true },
);
for (const signal of ["SIGINT", "SIGTERM"])
  process.on(signal, () => {
    server.kill();
  });
server.on("error", (error) => {
  process.stderr.write(`${error.message}\n`);
  process.exitCode = 1;
});
server.on("exit", (code) => {
  process.exitCode = code || 0;
});
