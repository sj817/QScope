// Run after build-desktop.ts: verify the real signed packages in isolated test directories.
import assert from "node:assert/strict";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { command, json, sha256 } from "./build-utils.ts";

assert.equal(process.platform, "darwin");
assert.equal(process.arch, "arm64");
const repo = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const reference = await json<{ manifest: string }>(
  join(repo, "build/desktop/current.json"),
);
const result = await json<{ assets: { path: string; sha256: string }[] }>(
  join(dirname(reference.manifest), "result.json"),
);
async function asset(suffix: string): Promise<string> {
  const value = result.assets.find((asset) => asset.path.endsWith(suffix));
  assert.ok(value, `Missing packaged artifact ${suffix}`);
  assert.equal(await sha256(value.path), value.sha256);
  return value.path;
}
await command(
  "cargo",
  [
    "test",
    "-p",
    "qscope-automation",
    "--features",
    "update",
    "--locked",
    "update::macos::tests::packaged_app_and_cli_preserve_signatures_modes_and_rollback_backup",
    "--",
    "--ignored",
    "--exact",
  ],
  {
    cwd: repo,
    env: {
      QSCOPE_MAC_UPDATE_ARCHIVE: await asset("-macos-arm64.zip"),
      QSCOPE_MAC_CLI_UPDATE_ARCHIVE: await asset("-macos-arm64-cli-webui.zip"),
    },
  },
);
