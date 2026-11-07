// 确认当前用户尚未安装 QScope 后，验证安装、修复与卸载。
import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { createHash, randomUUID } from "node:crypto";
import {
  appendFile,
  copyFile,
  mkdir,
  readFile,
  readdir,
  rm,
  writeFile,
} from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { promisify } from "node:util";

const execute = promisify(execFile);
const repo = resolve(import.meta.dirname, "../..");
if (process.platform !== "win32")
  throw new Error("Inno acceptance requires Windows");
const reference = JSON.parse(
  await readFile(join(repo, "build/desktop/current.json"), "utf8"),
);
const release: {
  release_id: string;
  cli_sha256: string;
  desktop_sha256: string;
  assets: { path: string }[];
} = JSON.parse(
  await readFile(join(dirname(reference.manifest), "result.json"), "utf8"),
);
const installerAsset = release.assets.find((asset) =>
  asset.path.endsWith("-setup.exe"),
);
assert.ok(installerAsset);
const installer = installerAsset.path;
const id = randomUUID();
const root = join(repo, ".tmp/installer", id);
const output = join(repo, "artifacts/architecture/installer", id);
const installation = join(root, "安装 目录");
assert.ok(process.env.LOCALAPPDATA);
assert.ok(process.env.USERPROFILE);
const userData = join(process.env.LOCALAPPDATA, "QScope");
const maintenance = join(userData, "maintenance");
const canary = join(userData, "installer-test-" + id + ".json");
const key =
  "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\io.github.sj817.qscope_is1";
await mkdir(root, { recursive: true });
await mkdir(output, { recursive: true });
const manifest: Record<string, unknown> & { passed: boolean } = {
  schema_version: 1,
  question:
    "Validate the actual Inno installer, repair, managed qs ownership, uninstall and retained user data",
  source: "synthetic",
  time_utc: new Date().toISOString(),
  release_id: release.release_id,
  passed: false,
};

async function query(path: string, value?: string) {
  try {
    return (
      await execute(
        "reg.exe",
        ["query", path, ...(value ? ["/v", value] : [])],
        { windowsHide: true, encoding: "buffer" },
      )
    ).stdout;
  } catch (error) {
    if ((error as { code?: number }).code === 1) return null;
    throw error;
  }
}
async function cli(program: string, data: string, ...args: string[]) {
  const command =
    args[0] === "cli"
      ? ["cli", "--directory", join(data, "command-bin"), ...args.slice(1)]
      : args;
  return JSON.parse(
    (
      await execute(program, ["--json", "--data-dir", data, ...command], {
        windowsHide: true,
        timeout: 30_000,
      })
    ).stdout,
  );
}
const hash = (bytes: Buffer) =>
  createHash("sha256").update(bytes).digest("hex");
const pathBefore = await query("HKCU\\Environment", "Path");
for (const scope of ["HKCU", "HKLM"])
  assert.equal(
    await query(scope + "\\" + key),
    null,
    "An existing QScope installation must be preserved",
  );
assert.deepEqual(
  await readdir(maintenance).catch((error) => {
    if (error.code === "ENOENT") return [];
    throw error;
  }),
  [],
  "An existing uninstaller must be preserved",
);
assert.equal(
  await readFile(join(process.env.USERPROFILE, ".local/bin/qs.exe")).then(
    () => true,
    (error) => {
      if (error.code === "ENOENT") return false;
      throw error;
    },
  ),
  false,
  "Run this acceptance before registering the final global qs",
);
await mkdir(userData, { recursive: true });
await writeFile(canary, JSON.stringify({ owner: id }), { flag: "wx" });
const canaryHash = hash(await readFile(canary));
try {
  const arguments_ = [
    "/SP-",
    "/VERYSILENT",
    "/SUPPRESSMSGBOXES",
    "/NORESTART",
    "/NOICONS",
    "/TASKS=",
    "/DIR=" + installation,
  ];
  await execute(
    installer,
    [...arguments_, "/LOG=" + join(output, "install.log")],
    { windowsHide: true, timeout: 120_000 },
  );
  assert.deepEqual((await readdir(installation)).sort(), [
    "QScope.exe",
    "qscope-cli.exe",
  ]);
  assert.equal(
    hash(await readFile(join(installation, "qscope-cli.exe"))),
    release.cli_sha256,
  );
  const state = join(root, "test data");
  assert.equal(
    (await cli(join(installation, "qscope-cli.exe"), state, "doctor"))
      .embedded_webui,
    true,
  );
  await execute(
    installer,
    [...arguments_, "/LOG=" + join(output, "repair.log")],
    { windowsHide: true, timeout: 120_000 },
  );
  assert.equal(
    hash(await readFile(join(installation, "QScope.exe"))),
    release.desktop_sha256,
  );
  manifest.repair = true;

  const first = join(root, "first/qscope-cli.exe");
  const second = join(root, "second/qscope-cli.exe");
  await mkdir(dirname(first));
  await mkdir(dirname(second));
  await copyFile(join(installation, "qscope-cli.exe"), first);
  await copyFile(first, second);
  const installed = await cli(first, state, "cli", "install");
  assert.equal(installed.installed, true);
  assert.equal((await cli(first, state, "cli", "install")).installed, true);
  assert.equal(hash(await readFile(installed.path)), release.cli_sha256);
  await appendFile(first, Buffer.from([0]));
  assert.equal((await cli(first, state, "cli", "install")).installed, true);
  assert.equal(
    hash(await readFile(installed.path)),
    hash(await readFile(first)),
  );
  assert.equal(
    (await cli(second, state, "cli", "uninstall", "--if-owned")).skipped,
    true,
  );
  assert.equal((await cli(first, state, "cli", "status")).installed, true);
  assert.equal((await cli(first, state, "cli", "uninstall")).installed, false);
  assert.deepEqual(await query("HKCU\\Environment", "Path"), pathBefore);
  manifest.global_command_lifecycle = true;

  await writeFile(join(installation, "keep.txt"), id);
  const uninstaller = (await readdir(maintenance)).find((name) =>
    /^unins\d+\.exe$/.test(name),
  );
  assert.ok(uninstaller);
  await execute(
    join(maintenance, uninstaller),
    [
      "/VERYSILENT",
      "/SUPPRESSMSGBOXES",
      "/NORESTART",
      "/LOG=" + join(output, "uninstall.log"),
    ],
    { windowsHide: true, timeout: 120_000 },
  );
  for (let attempt = 0; attempt < 100; attempt++) {
    if ((await query("HKCU\\" + key)) === null) break;
    await new Promise((done) => setTimeout(done, 100));
  }
  assert.equal(await query("HKCU\\" + key), null);
  assert.deepEqual(await readdir(installation), ["keep.txt"]);
  assert.equal(await readFile(join(installation, "keep.txt"), "utf8"), id);
  assert.equal(hash(await readFile(canary)), canaryHash);
  assert.deepEqual(await query("HKCU\\Environment", "Path"), pathBefore);
  manifest.user_data_retained = true;
  manifest.passed = true;
} catch (error) {
  manifest.error = error instanceof Error ? error.stack : String(error);
  throw error;
} finally {
  if (
    await readFile(canary).then(
      (bytes) => hash(bytes) === canaryHash,
      () => false,
    )
  )
    await rm(canary);
  await writeFile(
    join(output, "manifest.json"),
    JSON.stringify(manifest, null, 2),
  );
}
console.log(JSON.stringify({ output, passed: manifest.passed }));
