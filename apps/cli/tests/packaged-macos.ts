import assert from "node:assert/strict";
import { execFile, spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import { mkdir, readFile, readlink, stat, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { promisify } from "node:util";
import { setTimeout as delay } from "node:timers/promises";
import { seed } from "../../web/tests/fixture.mjs";
import { sha256 } from "../../../tools/build-utils.ts";

// Run after tools/build-desktop.ts; all input captures and runtime state are synthetic.
assert.equal(process.platform, "darwin");
assert.equal(process.arch, "arm64");
const execute = promisify(execFile);
const repo = resolve(import.meta.dirname, "../../..");
const reference = JSON.parse(
  await readFile(join(repo, "build/desktop/current.json"), "utf8"),
);
const release = JSON.parse(
  await readFile(join(dirname(reference.manifest), "result.json"), "utf8"),
);
assert.equal(release.target, "darwin-arm64");
const root = join(repo, ".tmp/packaged-macos", randomUUID());
const installation = join(root, "中文 安装目录");
const standalone = join(root, "独立 CLI");
await mkdir(installation, { recursive: true });
await mkdir(standalone);
const dmg = release.assets.find((asset: { path: string }) =>
  asset.path.endsWith(".dmg"),
);
if (dmg) {
  assert.equal(await sha256(dmg.path), dmg.sha256);
  const volume = join(root, "安装窗口");
  await mkdir(volume);
  await execute("/usr/bin/hdiutil", [
    "attach",
    "-readonly",
    "-nobrowse",
    "-noautoopen",
    "-mountpoint",
    volume,
    dmg.path,
  ]);
  try {
    assert.equal(await readlink(join(volume, "Applications")), "/Applications");
    await execute("/usr/bin/codesign", [
      "--verify",
      "--deep",
      "--strict",
      join(volume, "QScope.app"),
    ]);
    assert.equal(
      await sha256(join(volume, "QScope.app/Contents/MacOS/qscope-cli")),
      release.cli_sha256,
    );
  } finally {
    await execute("/usr/bin/hdiutil", ["detach", volume]);
  }
}
for (const [suffix, destination] of [
  ["-macos-arm64.zip", installation],
  ["-cli-webui.zip", standalone],
]) {
  const asset = release.assets.find((asset: { path: string }) =>
    asset.path.endsWith(suffix),
  );
  assert.ok(asset);
  assert.equal(await sha256(asset.path), asset.sha256);
  await execute("/usr/bin/ditto", ["-x", "-k", asset.path, destination]);
}
const app = join(installation, "QScope.app");
const cli = join(standalone, "qscope-cli");
await execute("/usr/bin/codesign", ["--verify", "--deep", "--strict", app]);
for (const executable of [
  cli,
  join(app, "Contents/MacOS/qscope-cli"),
  join(app, "Contents/MacOS/QScope"),
]) {
  assert.ok(
    (await stat(executable)).mode & 0o111,
    `${executable} must remain executable after extraction`,
  );
  await execute("/usr/bin/codesign", ["--verify", "--strict", executable]);
}
assert.equal(await sha256(cli), release.cli_sha256);
const fixture = await seed(join(root, "synthetic"));
const data = join(root, "用户 数据");
async function invoke(args: string[]) {
  const result = await execute(cli, ["--json", "--data-dir", data, ...args], {
    cwd: fixture.root,
    env: { ...process.env, PATH: "" },
    timeout: 30_000,
  });
  return JSON.parse(result.stdout);
}
const child = spawn(
  cli,
  [
    "--json",
    "--data-dir",
    data,
    "web",
    "--no-open",
    "--port",
    "0",
    "--captures",
    fixture.captures,
  ],
  {
    cwd: fixture.root,
    env: { ...process.env, PATH: "" },
    stdio: "ignore",
  },
);
child.on("error", () => {});
let descriptor: { endpoint: string; token: string; pid: number } | undefined;
try {
  const deadline = Date.now() + 30_000;
  while (!descriptor) {
    assert.equal(
      child.exitCode,
      null,
      "Packaged CLI exited before its backend became ready",
    );
    assert.ok(Date.now() < deadline, "Packaged backend startup deadline");
    try {
      descriptor = JSON.parse(
        await readFile(join(data, "service.json"), "utf8"),
      );
    } catch {
      await delay(100);
    }
  }
  const server = descriptor;
  async function api(path: string) {
    const response = await fetch(server.endpoint + "/api/v1" + path, {
      headers: {
        "X-QScope-Client": "web-v1",
        Authorization: "Bearer " + server.token,
      },
      signal: AbortSignal.timeout(10_000),
    });
    assert.ok(response.ok, await response.clone().text());
    return response.json();
  }
  assert.notEqual(server.pid, child.pid);
  assert.equal((await invoke(["service", "start"])).pid, server.pid);
  const page = await fetch(server.endpoint, {
    signal: AbortSignal.timeout(10_000),
  });
  assert.match(await page.text(), /QScope/);
  const qq = await api("/settings/qq");
  assert.equal(qq.capture_supported, true);
  assert.equal(qq.available, true);
  assert.equal(qq.build, "7.0.3-54395");
  assert.equal(qq.total, "487596067");
  assert.equal((await invoke(["doctor"])).embedded_probe, true);
  const health = await api("/settings/qq/health");
  assert.equal(health.repairable, false);
  assert.equal(health.healthy, true);
  assert.ok(
    health.components.some(
      (item: { id: string; state: string }) =>
        item.id === "host" && item.state === "ok",
    ),
  );
  const catalogue = await api("/sessions");
  assert.ok(
    catalogue.sessions.some((item: { path: string }) =>
      item.path.endsWith("closed.qscap"),
    ),
  );
  const decoded = await invoke(["decode", "--hex", "08 96 01"]);
  assert.equal(decoded.message.fields[0].value.unsigned, "150");
  const result = {
    schema_version: 1,
    question:
      "Does the signed macOS package retain both executables and serve synthetic captures without Node on PATH?",
    time_utc: new Date().toISOString(),
    source: "synthetic",
    module_identity: {
      release_id: release.release_id,
      cli_sha256: release.cli_sha256,
    },
    tools: {
      node: process.version,
      command: "node apps/cli/tests/packaged-macos.ts",
    },
    conditions: [
      ...(dmg
        ? ["Read-only DMG contains the signed app and Applications drag target"]
        : []),
      "Extracted ZIP in a Chinese path with spaces",
      "Empty child process PATH",
      "No QQ process or account",
    ],
    evidence: [{ path: reference.manifest, release_id: release.release_id }],
    passed: true,
    boundary:
      "Package, signature integrity and offline backend only; desktop window and real QQ capture require separate verification",
  };
  const evidence = join(
    repo,
    "artifacts/architecture/packaged-macos",
    release.release_id,
  );
  await mkdir(evidence, { recursive: true });
  await writeFile(
    join(evidence, "manifest.json"),
    JSON.stringify(result, null, 2) + "\n",
  );
  console.log(
    JSON.stringify({
      passed: true,
      root,
      app,
      capture: join(fixture.captures, "closed.qscap"),
      evidence,
    }),
  );
} finally {
  if (descriptor) await invoke(["service", "stop"]).catch(() => {});
  child.kill();
}
