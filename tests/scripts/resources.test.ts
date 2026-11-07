import assert from "node:assert/strict";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import test from "node:test";
import { writeZip } from "../../scripts/pack-resources.ts";
import { sha256 } from "../../scripts/build-utils.ts";

test("CLI 归档保留 Unix 可执行权限并可重复生成", async (t) => {
  const root = await mkdtemp(join(tmpdir(), "qscope-zip-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  await writeFile(join(root, "qscope-cli"), "synthetic CLI");
  const files = [
    {
      path: "qscope-cli",
      bytes: 13,
      sha256: await sha256(join(root, "qscope-cli")),
      mode: 0o100755,
    },
  ];
  await writeZip(root, files, join(root, "first.zip"));
  await writeZip(root, files, join(root, "second.zip"));
  const bytes = await readFile(join(root, "first.zip"));
  assert.deepEqual(bytes, await readFile(join(root, "second.zip")));
  const central = bytes.readUInt32LE(bytes.length - 6);
  assert.equal(bytes.readUInt32LE(central), 0x02014b50);
  assert.equal(bytes.readUInt16LE(central + 4) >> 8, 3);
  assert.equal(bytes.readUInt32LE(central + 38) >>> 16, 0o100755);
});
