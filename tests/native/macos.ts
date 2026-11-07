// 使用合成包验证构建出的动态库。
import { mkdir, readFile } from "node:fs/promises";
import { randomUUID } from "node:crypto";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { command, writeJson } from "../../scripts/build-utils.ts";

if (process.platform !== "darwin" || process.arch !== "arm64")
  throw new Error("macOS ARM64 required");
const repo = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const current = JSON.parse(
  await readFile(join(repo, "build/macos-native/current.json"), "utf8"),
) as { output: string; build_id: string };
const build = resolve(current.output);
if (
  !/^[a-f0-9]{64}$/.test(current.build_id) ||
  build !== join(repo, "build/macos-native", current.build_id.slice(0, 16))
)
  throw new Error("Invalid native build marker");
const root = join(repo, ".tmp/macos-native-tests", randomUUID());
await mkdir(root, { recursive: true });
const module = join(build, "synthetic-wrapper.node");
const bytes = await readFile(module);
const symbols = await command("/usr/bin/xcrun", ["nm", "-g", module], {
  capture: true,
});
function target(name: string): string {
  const line = symbols.split("\n").find((line) => line.endsWith(" _" + name));
  if (!line) throw new Error(`Missing test symbol ${name}`);
  const address = Number.parseInt(line.split(" ")[0], 16);
  // 验证合成动态库的 __TEXT vmaddr 和 fileoff 均为零。
  let offset = 32;
  for (let i = 0; i < bytes.readUInt32LE(16); ++i) {
    if (
      bytes.readUInt32LE(offset) === 0x19 &&
      bytes.subarray(offset + 8, offset + 14).toString() === "__TEXT"
    ) {
      if (
        bytes.readBigUInt64LE(offset + 24) !== 0n ||
        bytes.readBigUInt64LE(offset + 40) !== 0n
      )
        throw new Error("Unexpected synthetic Mach-O layout");
      return (
        address.toString(16) +
        ":" +
        bytes.subarray(address, address + 16).toString("hex")
      );
    }
    offset += bytes.readUInt32LE(offset + 4);
  }
  throw new Error("Missing synthetic __TEXT");
}
const output = join(root, "synthetic.qscap");
await command(join(build, "probe-test"), [module], {
  env: {
    DYLD_INSERT_LIBRARIES: join(build, "qscope-probe.dylib"),
    QSCOPE_PROBE_EXECUTABLE: join(build, "probe-test"),
    QSCOPE_PROBE_MODULE: module,
    QSCOPE_PROBE_SEND: target("qscope_test_send"),
    QSCOPE_PROBE_RECEIVE: target("qscope_test_receive"),
    QSCOPE_PROBE_OUTPUT: output,
    QSCOPE_PROBE_STOP: join(root, "stop"),
    QSCOPE_PROBE_CLOSED: join(root, "closed"),
  },
  timeout: 20_000,
});
const capture = await readFile(output);
if (capture.subarray(0, 8).toString() !== "QSCAP01\0")
  throw new Error("Invalid native header");
let cursor = 64,
  next = 1n,
  packets = 0,
  loss = false,
  ended = false,
  truncated = false;
const commands: string[] = [];
let dropped = 0n;
while (cursor < capture.length) {
  if (
    capture.length - cursor < 80 ||
    capture.subarray(cursor, cursor + 4).toString() !== "QSR1"
  )
    throw new Error("Truncated synthetic output");
  const size = capture.readUInt32LE(cursor + 4),
    kind = capture.readUInt16LE(cursor + 8),
    payload = capture.readUInt32LE(cursor + 64),
    length = capture.readUInt32LE(cursor + 68);
  if (
    size !== 80 + payload + length ||
    cursor + size > capture.length ||
    capture.readBigUInt64LE(cursor + 24) !== next++
  )
    throw new Error("Invalid native record");
  const command = capture
    .subarray(cursor + 80, cursor + 80 + length)
    .toString();
  commands.push(command);
  const totalDropped = capture.readBigUInt64LE(cursor + 72);
  if (totalDropped < dropped) throw new Error("Loss counter moved backwards");
  dropped = totalDropped;
  if (kind === 1) {
    packets++;
    if (capture[cursor + 11] === 1)
      truncated =
        payload === 1024 * 1024 &&
        capture.readBigUInt64LE(cursor + 56) === 1048577n;
  }
  if (kind === 3 && command === "packet_read_failed") loss = true;
  if (kind === 4) ended = cursor + size === capture.length;
  cursor += size;
}
if (
  packets < 3 ||
  BigInt(packets) + dropped !== 404n ||
  !loss ||
  !ended ||
  !truncated ||
  !commands.includes("hooks_ready") ||
  !commands.includes("Synthetic.Receive.With.Long.Command")
)
  throw new Error("Native instrumentation assertions failed");
if ((await readFile(join(root, "closed"), "utf8")) !== "closed")
  throw new Error("Writer failed");
await writeJson(join(root, "result.json"), {
  packets,
  dropped_total: dropped.toString(),
  concurrent_attempts: 400,
  invalid_pointer_reported: loss,
  truncation_reported: truncated,
  clean_end: ended,
  commands,
});
console.log(
  JSON.stringify({
    root,
    packets,
    dropped_total: dropped.toString(),
    invalid_pointer_reported: loss,
    truncation_reported: truncated,
    clean_end: ended,
  }),
);
