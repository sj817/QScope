// Synthetic native transport fixtures; contains no QQ account or captured data.
import { mkdir, writeFile, readFile } from "node:fs/promises";
import { resolve, join } from "node:path";
import { createHash } from "node:crypto";

export const epoch = 134383000000000000n;
export const longText =
  '前缀 "quoted" \\path ' + "很长的值0123456789".repeat(180) + " END-完整尾部";
function varint(value) {
  const bytes = [];
  let rest = BigInt(value);
  do {
    let byte = Number(rest & 127n);
    rest >>= 7n;
    if (rest) byte |= 128;
    bytes.push(byte);
  } while (rest);
  return Buffer.from(bytes);
}
export function header(pid = 123, started = epoch) {
  const bytes = Buffer.alloc(64);
  bytes.write("QSCAP01\0");
  for (const [offset, value] of [
    [8, 1],
    [12, 64],
    [16, pid],
    [56, 1048576],
  ])
    bytes.writeUInt32LE(value, offset);
  for (const [offset, value] of [
    [24, started],
    [32, started],
    [48, 10000000n],
  ])
    bytes.writeBigUInt64LE(value, offset);
  return bytes;
}
export function record(
  id,
  command,
  payload = Buffer.from([8, 1]),
  options = {},
) {
  const cmd = Buffer.from(command);
  const bytes = Buffer.alloc(80);
  bytes.write("QSR1");
  bytes.writeUInt32LE(80 + cmd.length + payload.length, 4);
  bytes.writeUInt16LE(options.kind || 1, 8);
  bytes[10] = options.kind && options.kind !== 1 ? 0 : options.direction || 2;
  bytes[11] =
    options.originalLength && options.originalLength !== payload.length ? 1 : 0;
  bytes.writeUInt32LE(options.pid || 123, 12);
  bytes.writeUInt32LE(456, 16);
  bytes.writeBigUInt64LE(BigInt(id), 24);
  bytes.writeBigUInt64LE(epoch + BigInt(id) * 10000n, 32);
  bytes.writeBigUInt64LE(9007199254740993n + BigInt(id), 48);
  bytes.writeBigUInt64LE(BigInt(options.originalLength || payload.length), 56);
  bytes.writeUInt32LE(payload.length, 64);
  bytes.writeUInt32LE(cmd.length, 68);
  bytes.writeBigUInt64LE(BigInt(options.dropped || 0), 72);
  return Buffer.concat([bytes, cmd, payload]);
}

export async function seed(directory) {
  const root = resolve(directory);
  await mkdir(root, { recursive: true });
  const ownerPath = join(root, "owner.json");
  try {
    const owner = JSON.parse(await readFile(ownerPath, "utf8"));
    if (owner.project !== "qscope-webui-synthetic-tests")
      throw new Error("Foreign test directory");
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
  }
  const captures = join(root, "captures");
  const imports = join(root, "imports");
  await mkdir(captures, { recursive: true });
  await mkdir(imports, { recursive: true });
  const pb = Buffer.from(
    "089601120268691d785634122101000000000000802b08012c0802",
    "hex",
  );
  const records = [header()];
  for (let id = 1; id <= 1200; id++) {
    let command = `Test.Command.${id % 20}`;
    let payload = pb;
    if (id === 17) command = "Test.SearchTarget";
    if (id === 18) {
      command = "Test.TLV";
      payload = Buffer.from("010600020801", "hex");
    }
    if (id === 19) {
      command = "Test.JCE";
      payload = Buffer.from("00011603616263", "hex");
    }
    if (id === 20) {
      command = "Test.NonPB";
      payload = Buffer.from("ff", "hex");
    }
    records.push(
      record(id, command, payload, {
        direction: id % 2 ? 2 : 1,
        dropped: id >= 100 ? 3 : 0,
        originalLength: id === 21 ? 99 : undefined,
      }),
    );
  }
  records.push(
    record(1201, "controller_stopped", Buffer.alloc(0), {
      kind: 4,
      dropped: 3,
    }),
  );
  const closed = Buffer.concat(records);
  await writeFile(join(captures, "closed.qscap"), closed);
  await writeFile(
    join(captures, "live.qscap"),
    Buffer.concat([
      header(123, epoch + 864000000000n),
      record(1, "Live.Ready"),
    ]),
  );
  await writeFile(
    join(root, "upload.qscap"),
    Buffer.concat([
      header(321),
      record(1, "Imported.Packet", Buffer.from([8, 42]), { pid: 321 }),
      record(2, "controller_stopped", Buffer.alloc(0), { kind: 4, pid: 321 }),
    ]),
  );
  await writeFile(
    join(root, "large.qscap"),
    Buffer.concat([
      header(789),
      record(1, "Large.Payload", Buffer.alloc(1048576, 0x5a), { pid: 789 }),
    ]),
  );
  // Obviously fake accounts: two launches of 10001 and one of 10002.
  const launch = (pid, started, uin, prefix, count) =>
    Buffer.concat([
      header(pid, started),
      record(1, `account_observed type=1 uin=${uin}`, Buffer.alloc(0), {
        kind: 2,
        pid,
      }),
      ...Array.from({ length: count }, (_, i) =>
        record(i + 2, `${prefix}.Command.${i % 3}`, pb, {
          pid,
          direction: i % 2 ? 1 : 2,
        }),
      ),
      record(count + 2, "controller_stopped", Buffer.alloc(0), {
        kind: 4,
        pid,
      }),
    ]);
  await writeFile(
    join(captures, "account-first.qscap"),
    launch(301, epoch + 36000000000n, "10001", "First", 6),
  );
  await writeFile(
    join(captures, "account-second.qscap"),
    launch(302, epoch + 72000000000n, "10001", "Second", 4),
  );
  await writeFile(
    join(captures, "account-other.qscap"),
    launch(303, epoch + 90000000000n, "10002", "Other", 3),
  );
  const text = Buffer.from(longText);
  const longPayload = Buffer.concat([
    Buffer.from([10]),
    varint(text.length),
    text,
  ]);
  const repeat = Buffer.from([
    8, 1, 8, 2, 16, 255, 255, 255, 255, 255, 255, 255, 255, 255, 1,
  ]);
  // 1500 top-level varints, then a message candidate of 600 fields: the second field page lands inside it.
  const inner = Buffer.from(Array(600).fill([8, 0x80, 1]).flat());
  const pages = Buffer.concat([
    Buffer.from(Array(1500).fill([8, 1]).flat()),
    Buffer.from([0x12]),
    varint(inner.length),
    inner,
  ]);
  // A string field holding JSON, with an integer beyond 2^53 that must stay exact.
  const json = Buffer.from(
    '{"id":"demo","seq":18446744073709551615,"ok":true,"none":null,"list":[1,-2.5e3,{"name":' +
      JSON.stringify('示例 "引号"') +
      ',"tags":[]}],"empty":{}}',
  );
  const jsonPayload = Buffer.concat([
    Buffer.from([10]),
    varint(json.length),
    json,
  ]);
  await writeFile(
    join(captures, "text.qscap"),
    Buffer.concat([
      header(654),
      record(1, "Long.Text", longPayload, { pid: 654 }),
      record(2, "Repeated.Fields", repeat, { pid: 654 }),
      record(3, "Nested.Pages", pages, { pid: 654 }),
      record(4, "Json.Text", jsonPayload, { pid: 654 }),
      record(5, "controller_stopped", Buffer.alloc(0), { kind: 4, pid: 654 }),
    ]),
  );
  const manifest = {
    project: "qscope-webui-synthetic-tests",
    source: "generated, no QQ/account data",
    time_utc: new Date().toISOString(),
    root,
    closed_sha256: createHash("sha256").update(closed).digest("hex"),
    packets: 1200,
  };
  await writeFile(ownerPath, JSON.stringify(manifest, null, 2));
  return { root, captures, imports };
}
