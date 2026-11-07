import { join, resolve } from "node:path";
import { command, json } from "../../scripts/build-utils.ts";
import { mkdir, rm } from "node:fs/promises";
import { randomUUID } from "node:crypto";

const repo = resolve(import.meta.dirname, "../..");
if (process.platform === "win32" && process.arch === "x64") {
  const marker = await json<{ build_id: string }>(
    join(repo, "build/native/qqnt-windows-x64/current.json"),
  );
  if (!/^[a-f0-9]{64}$/.test(marker.build_id))
    throw new Error("原生构建标识无效");
  const output = join(repo, ".tmp/native-tests", randomUUID());
  await mkdir(output, { recursive: true });
  try {
    await command(
      join(
        repo,
        "build/native/qqnt-windows-x64",
        marker.build_id.slice(0, 16),
        "probe-selftest.exe",
      ),
      [output],
      { cwd: repo },
    );
  } finally {
    await rm(output, { recursive: true, force: true });
  }
} else if (process.platform === "darwin" && process.arch === "arm64") {
  await command(process.execPath, [join(import.meta.dirname, "macos.ts")], {
    cwd: repo,
  });
} else {
  throw new Error(
    `当前平台尚无采集器测试适配 ${process.platform}-${process.arch}`,
  );
}
