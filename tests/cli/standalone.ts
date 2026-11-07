import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import {
  chmod,
  mkdir,
  mkdtemp,
  readFile,
  rm,
  writeFile,
} from "node:fs/promises";
import { join, resolve } from "node:path";
import { inflateRawSync } from "node:zlib";
import { command, json, sha256 } from "../../scripts/build-utils.ts";

const repo = resolve(import.meta.dirname, "../..");
const built = await json<{
  schema_version: number;
  target: string;
  archive: string;
  archive_sha256: string;
  cli_sha256: string;
}>(join(repo, "build/cli/current.json"));
assert.equal(built.schema_version, 1);
assert.equal(built.target, `${process.platform}-${process.arch}`);
assert.equal(await sha256(built.archive), built.archive_sha256);
const archive = await readFile(built.archive);
const name = process.platform === "win32" ? "qscope-cli.exe" : "qscope-cli";
assert.equal(archive.readUInt32LE(0), 0x04034b50);
assert.equal(archive.readUInt16LE(8), 8);
const size = archive.readUInt32LE(18);
const filenameLength = archive.readUInt16LE(26);
const extraLength = archive.readUInt16LE(28);
assert.equal(archive.subarray(30, 30 + filenameLength).toString(), name);
const start = 30 + filenameLength + extraLength;
const bytes = inflateRawSync(archive.subarray(start, start + size), {
  maxOutputLength: 128 * 1024 ** 2,
});
assert.equal(bytes.length, archive.readUInt32LE(22));
assert.equal(
  createHash("sha256").update(bytes).digest("hex"),
  built.cli_sha256,
);
const end = archive.length - 22;
assert.equal(archive.readUInt32LE(end), 0x06054b50);
assert.equal(archive.readUInt16LE(end + 10), 1);
const central = archive.readUInt32LE(end + 16);
assert.equal(archive.readUInt32LE(central), 0x02014b50);
assert.equal(archive.readUInt32LE(central + 38) >>> 16, 0o100755);

const parent = join(repo, ".tmp");
await mkdir(parent, { recursive: true });
const root = await mkdtemp(join(parent, "standalone-"));
try {
  const cli = join(root, name);
  await writeFile(cli, bytes);
  if (process.platform !== "win32") await chmod(cli, 0o755);
  const data = join(root, "用户 数据");
  const options = { cwd: root, capture: true, env: { PATH: "" } };
  const doctor = JSON.parse(
    await command(cli, ["--json", "--data-dir", data, "doctor"], options),
  );
  assert.equal(doctor.embedded_webui, true);
  assert.equal(doctor.embedded_devtools, true);
  assert.equal(doctor.live_capture_supported, process.platform !== "linux");
  assert.equal(doctor.embedded_probe, process.platform !== "linux");
  JSON.parse(
    await command(
      cli,
      ["--json", "--data-dir", data, "decode", "--hex", "08 01"],
      options,
    ),
  );
  console.log(
    JSON.stringify({
      passed: true,
      target: built.target,
      archive: built.archive,
    }),
  );
} finally {
  await rm(root, { recursive: true, force: true });
}
