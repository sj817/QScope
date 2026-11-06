// Synthetic captures for README screenshots. Accounts 10001 and 10003 are obviously fake, every
// payload is generated here, and field layouts are illustrative rather than decoded QQ schemas.
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { header } from "./fixture.mjs";

const TICKS_PER_SECOND = 10000000n;
const EPOCH_OFFSET = 11644473600n;
// 2026-11-06 06:30:00 UTC as a Windows FILETIME.
export const started =
  (BigInt(Date.UTC(2026, 10, 6, 6, 30, 0) / 1000) + EPOCH_OFFSET) *
  TICKS_PER_SECOND;

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
const tag = (field, wire) => varint((field << 3) | wire);
const int = (field, value) => Buffer.concat([tag(field, 0), varint(value)]);
const bytes = (field, value) =>
  Buffer.concat([tag(field, 2), varint(value.length), value]);
const text = (field, value) => bytes(field, Buffer.from(value));
const message = (field, ...parts) => bytes(field, Buffer.concat(parts));
const pb = (...parts) => Buffer.concat(parts);

// Deterministic filler so repeated runs render the same screenshots.
function noise(length, seed) {
  const out = Buffer.alloc(length);
  let state = seed >>> 0;
  for (let i = 0; i < length; i++) {
    state = (Math.imul(state, 1664525) + 1013904223) >>> 0;
    out[i] = state >>> 24;
  }
  return out;
}

function tlv(type, value) {
  const head = Buffer.alloc(4);
  head.writeUInt16BE(type, 0);
  head.writeUInt16BE(value.length, 2);
  return Buffer.concat([head, value]);
}

// Records are queued and written in time order; a request and its reply share one sequence.
class Capture {
  constructor(pid, start) {
    this.pid = pid;
    this.start = start;
    this.queue = [];
    this.pairs = 0;
  }
  push(at, command, payload, options = {}) {
    this.queue.push({ at, command, payload, options });
  }
  call(at, command, request, reply, latency = 0.04) {
    const pair = ++this.pairs;
    this.push(at, command, request, { direction: 2, pair });
    this.push(at + latency, command, reply, { direction: 1, pair });
  }
  inbound(at, command, payload) {
    this.push(at, command, payload, { direction: 1 });
  }
  status(at, text) {
    this.push(at, text, Buffer.alloc(0), { kind: 2 });
  }
  end(at) {
    this.push(at, "controller_stopped", Buffer.alloc(0), { kind: 4 });
    const parts = [header(this.pid, this.start)];
    const sequences = new Map();
    let seq = 31000 + this.pid * 7;
    this.queue
      .sort((left, right) => left.at - right.at)
      .forEach(({ at, command, payload, options }, index) => {
        const kind = options.kind || 1;
        if (kind === 1 && !(options.pair && sequences.has(options.pair)))
          sequences.set(options.pair || `single-${index}`, ++seq);
        const sequence =
          kind === 1 ? sequences.get(options.pair || `single-${index}`) : 0;
        parts.push(
          this.record(
            index + 1,
            at,
            command,
            payload,
            kind,
            options.direction,
            sequence,
          ),
        );
      });
    return Buffer.concat(parts);
  }
  record(id, at, command, payload, kind, direction, sequence) {
    const cmd = Buffer.from(command);
    const record = Buffer.alloc(80);
    record.write("QSR1");
    record.writeUInt32LE(80 + cmd.length + payload.length, 4);
    record.writeUInt16LE(kind, 8);
    record[10] = kind === 1 ? direction : 0;
    record.writeUInt32LE(this.pid, 12);
    record.writeUInt32LE(this.pid + 4096, 16);
    record.writeBigUInt64LE(BigInt(id), 24);
    record.writeBigUInt64LE(
      this.start + BigInt(Math.round(at * 1000)) * 10000n,
      32,
    );
    record.writeBigUInt64LE(BigInt(Math.round(at * 1e7)), 40);
    record.writeBigUInt64LE(BigInt(sequence), 48);
    record.writeBigUInt64LE(BigInt(payload.length), 56);
    record.writeUInt32LE(payload.length, 64);
    record.writeUInt32LE(cmd.length, 68);
    return Buffer.concat([record, cmd, payload]);
  }
}

const HEARTBEAT = "trpc.qq_new_tech.status_svc.StatusService.SsoHeartBeat";
const PUSH = "trpc.msg.olpush.OlPushService.MsgPush";

// 16383 encodes as ff 7f, which is never UTF-8, so nested messages are not mistaken for text.
const MARK = int(15, 16383);
const words = (value) => message(2, message(1, text(1, value), MARK));
const face = (id) => message(2, message(2, int(1, id), MARK));

function chat(at, from, to, seq, elements) {
  const time = 1793946600 + Math.round(at);
  return pb(
    message(
      1,
      int(1, from),
      text(2, `u_demo_${from}`),
      int(5, to),
      text(6, `u_demo_${to}`),
      message(8, text(4, `示例好友 ${from}`), MARK),
    ),
    message(
      2,
      int(1, 166),
      int(2, 0),
      int(5, seq),
      int(6, time),
      int(7, 1),
      int(12, 7205759403792793600n + BigInt(seq)),
    ),
    message(
      3,
      message(
        1,
        message(
          1,
          int(1, 0),
          int(2, time),
          int(3, 2147483647),
          text(9, "微软雅黑"),
          MARK,
        ),
        ...elements,
      ),
    ),
  );
}

function account(uin, pid, start, minutes) {
  const capture = new Capture(pid, start);
  capture.call(
    0.4,
    "wtlogin.trans_emp",
    pb(
      tlv(0x0016, noise(22, 7)),
      tlv(0x001b, noise(12, 8)),
      tlv(0x001d, noise(8, 9)),
      tlv(0x0033, Buffer.from("QScope-Demo-Device")),
      tlv(0x0035, Buffer.from([0, 0, 0, 8])),
    ),
    pb(tlv(0x0017, noise(40, 10)), tlv(0x00d1, noise(16, 11))),
  );
  capture.status(1.2, `account_observed type=1 uin=${uin}`);
  capture.call(
    1.4,
    "trpc.qq_new_tech.status_svc.StatusService.Register",
    pb(
      int(1, 1),
      text(2, "QScope-Demo-Device"),
      message(
        3,
        text(1, "Windows"),
        text(2, "10.0.26300"),
        text(3, "9.9.37-54395"),
      ),
      int(4, 0),
      int(5, 11),
    ),
    pb(text(2, "register success"), int(3, 1793946601), int(5, 0)),
  );
  capture.call(
    1.9,
    "trpc.msg.register_proxy.RegisterProxy.SsoInfoSync",
    pb(int(1, 735), int(2, 1793946600), message(3, int(1, 0), int(2, 1))),
    pb(
      message(
        3,
        ...[20001, 20002].map((group) =>
          message(2, int(1, group), int(2, 1024 + group), int(3, 0)),
        ),
      ),
      message(4, int(1, 1793946602), int(3, 1)),
    ),
    0.12,
  );
  const friends = [
    [10002, "示例好友 · A"],
    [10004, "示例好友 · B"],
    [10005, "QScope 测试号"],
    [10006, "Demo Friend"],
  ];
  capture.call(
    2.6,
    "OidbSvcTrpcTcp.0xfd4_1",
    pb(
      int(1, 4052),
      int(2, 1),
      message(4, int(2, 300), int(3, 0), message(10001, int(1, 1)), int(12, 1)),
    ),
    pb(
      int(1, 4052),
      int(2, 1),
      int(3, 0),
      message(
        4,
        int(3, friends.length),
        ...friends.map(([friend, nick], index) =>
          message(
            101,
            text(1, `u_demo_${friend}`),
            int(3, friend),
            int(4, index % 2),
            message(
              10001,
              message(2, text(1, nick), text(2, "合成签名")),
              int(1, 1),
            ),
          ),
        ),
      ),
    ),
    0.18,
  );
  capture.call(
    3.1,
    "OidbSvcTrpcTcp.0xfe5_2",
    pb(int(1, 4069), int(2, 2), message(4, message(1, int(1, 1), int(2, 1)))),
    pb(
      int(1, 4069),
      int(2, 2),
      message(
        4,
        message(
          2,
          int(3, 20001),
          message(4, text(1, "QScope 演示群"), int(5, 42), int(6, 500)),
        ),
        message(
          2,
          int(3, 20002),
          message(4, text(1, "合成数据交流群"), int(5, 18), int(6, 200)),
        ),
      ),
    ),
    0.16,
  );

  let seq = 900;
  const end = minutes * 60;
  // Activity gathers in the last two minutes, which is what a freshly opened capture shows first.
  const late = end - 120;
  for (let at = 8; at < end; at += 60)
    capture.call(
      at,
      HEARTBEAT,
      int(1, 1),
      pb(int(1, 0), int(2, 1793946600 + at)),
      0.03,
    );
  const pushes = [
    [late + 2, 10004, [words("QScope 可以按命令、方向和时间筛选抓包")]],
    [late + 14, 10002, [words("字段树保留字段号、wire type 与原始字节")]],
    [
      late + 31,
      10002,
      [
        words("这是一条合成示例消息，仅用于界面展示 "),
        face(14),
        words(" 收到请回复"),
      ],
    ],
    [late + 70, 10005, [words("Hello from a synthetic capture 👋")]],
    [late + 96, 10006, [words("64 位整数在前端始终按字符串保存")]],
  ];
  for (const [at, from, elements] of pushes)
    capture.inbound(at, PUSH, chat(at, from, uin, ++seq, elements));
  // An outgoing message and the server acknowledgement.
  capture.call(
    late + 22,
    "MessageSvc.PbSendMsg",
    pb(
      message(1, message(1, int(1, 10002), text(2, "u_demo_10002"))),
      message(2, int(1, 1), int(2, 0), int(3, 0)),
      message(
        3,
        message(
          1,
          message(2, message(1, text(1, "你好，这是 QScope 发出的合成消息"))),
        ),
      ),
      int(4, 48213),
      int(5, 2873441069),
    ),
    pb(
      int(1, 0),
      text(2, ""),
      int(3, 1793946652),
      int(10, 18446744073709551615n),
    ),
    0.09,
  );
  // A structured card whose string field holds JSON, so the value viewer has something to format.
  const card = JSON.stringify({
    app: "com.example.qscope.demo",
    view: "news",
    ver: "1.0.0.1",
    prompt: "[分享] QScope 合成示例卡片",
    meta: {
      news: {
        title: "QScope 合成示例",
        desc: "仅用于 README 截图的虚构卡片，不包含真实数据",
        tag: "QScope",
        jumpUrl: "https://example.com/qscope",
      },
    },
    config: { ctime: 1793946680, forward: true, token: "0000-demo" },
  });
  capture.inbound(
    late + 44,
    PUSH,
    chat(late + 44, 10005, uin, seq + 1, [
      message(2, message(51, text(1, card), MARK)),
    ]),
  );
  capture.call(
    late + 57,
    "trpc.group_pro.synclogic.SyncLogic.SyncFirstView",
    pb(int(1, 0), int(2, 0), int(3, 1), int(8, 0)),
    pb(
      message(1, int(1, 0), int(2, 1793946720)),
      bytes(6, noise(96, 42)),
      int(9, 1793946720),
    ),
    0.21,
  );
  capture.inbound(
    late + 84,
    "ConfigPushSvc.PushReq",
    Buffer.from(
      "10022c3c4c56124e6f746966794f6e6c696e6543666766da000102030405060708090a0b",
      "hex",
    ),
  );
  return capture.end(end);
}

export async function seedShowcase(directory) {
  const root = resolve(directory);
  await mkdir(root, { recursive: true });
  const ownerPath = join(root, "owner.json");
  try {
    const owner = JSON.parse(await readFile(ownerPath, "utf8"));
    if (owner.project !== "qscope-readme-showcase")
      throw new Error("Foreign showcase directory");
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
  }
  const captures = join(root, "captures");
  const imports = join(root, "imports");
  await mkdir(captures, { recursive: true });
  await mkdir(imports, { recursive: true });
  await writeFile(
    join(captures, "demo-10001.qscap"),
    account("10001", 4101, started + 3600n * TICKS_PER_SECOND, 6),
  );
  await writeFile(
    join(captures, "demo-10003.qscap"),
    account("10003", 4102, started, 3),
  );
  await writeFile(
    ownerPath,
    JSON.stringify(
      {
        project: "qscope-readme-showcase",
        source: "generated, no QQ/account data",
      },
      null,
      2,
    ),
  );
  return { root, captures, imports };
}
