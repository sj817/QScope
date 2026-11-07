/* eslint-disable @typescript-eslint/no-require-imports -- This test executes an Electron CommonJS package.main with a mocked loader. */
"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const test = require("node:test");
const vm = require("node:vm");

const source = fs.readFileSync(path.join(__dirname, "bootstrap.cjs"), "utf8");
const birth = 134_000_000_123_456_789n;

function fixture(t) {
  const root = fs.mkdtempSync(
    path.join(fs.realpathSync(os.tmpdir()), "qscope-bootstrap-"),
  );
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const appDirectory = path.join(root, "app");
  const profile = path.join(root, "profile");
  fs.mkdirSync(appDirectory);
  fs.mkdirSync(profile);
  const capture = path.join(profile, "capture.qscap");
  const header = Buffer.alloc(64);
  header.write("QSCAP01\0");
  header.writeUInt32LE(1, 8);
  header.writeUInt32LE(64, 12);
  header.writeUInt32LE(7319, 16);
  header.writeBigUInt64LE(birth, 24);
  fs.writeFileSync(capture, Buffer.concat([header, Buffer.alloc(8192, 0xa5)]));
  fs.writeFileSync(
    path.join(appDirectory, "package.json"),
    JSON.stringify({
      main: "./qscope-bootstrap.cjs",
      version: "synthetic-build",
    }),
  );
  fs.writeFileSync(
    path.join(appDirectory, "qscope-automation.cjs"),
    "// mock\n",
  );
  const defaultEntry = path.join(
    appDirectory,
    "application.asar",
    "app_launcher",
    "index.js",
  );
  fs.mkdirSync(path.dirname(defaultEntry), { recursive: true });
  fs.writeFileSync(defaultEntry, "// synthetic entry\n");
  const env = {
    HOME: path.join(profile, "home"),
    CFFIXED_USER_HOME: path.join(profile, "home"),
    TMPDIR: path.join(profile, "tmp") + path.sep,
    QSCOPE_RESEARCH_PROFILE: profile,
    QSCOPE_PROBE_OUTPUT: capture,
    QSCOPE_DEVTOOLS_PORT: "31415",
  };
  const events = [];
  const paths = {};
  const reads = [];
  const descriptors = new Map();
  let ready = false;
  const app = {
    isReady: () => ready,
    setName: (name) => events.push(["name", name]),
    setPath: (name, filename) => {
      paths[name] = filename;
      events.push(["path", name]);
    },
    getPath: (name) => paths[name],
  };
  const trackedFs = Object.create(fs);
  trackedFs.openSync = (filename, ...args) => {
    const descriptor = fs.openSync(filename, ...args);
    descriptors.set(descriptor, filename);
    return descriptor;
  };
  trackedFs.readSync = (descriptor, buffer, offset, length, position) => {
    if (descriptors.get(descriptor) === capture)
      reads.push({ length, position });
    return fs.readSync(descriptor, buffer, offset, length, position);
  };
  trackedFs.closeSync = (descriptor) => {
    descriptors.delete(descriptor);
    return fs.closeSync(descriptor);
  };
  let context;
  const processMock = { type: "browser", pid: 7319, env };
  const load = (name) => {
    if (name === "node:fs") return trackedFs;
    if (name === "node:path") return path;
    if (name === "electron") return { app };
    if (name === path.join(appDirectory, "qscope-automation.cjs")) {
      assert.equal(env.QSCOPE_STATE_ROOT, profile);
      assert.equal(context.__qscopeProcessIdentity.pid, processMock.pid);
      assert.equal(
        context.__qscopeProcessIdentity.birth_filetime,
        birth.toString(),
      );
      assert.equal(paths.downloads, path.join(profile, "files"));
      assert.ok(fs.statSync(path.join(profile, ".qscope")).isDirectory());
      events.push(["bridge"]);
      return {};
    }
    assert.ok(
      path.isAbsolute(name),
      "original entry must resolve inside the app",
    );
    assert.ok(fs.statSync(name).isFile());
    events.push(["original", name]);
    return {};
  };
  context = vm.createContext({
    Buffer,
    process: processMock,
    require: load,
    __dirname: appDirectory,
    __filename: path.join(appDirectory, "qscope-bootstrap.cjs"),
  });
  return {
    root,
    appDirectory,
    profile,
    capture,
    header,
    defaultEntry,
    env,
    events,
    paths,
    reads,
    context,
    processMock,
    app,
    ready: () => (ready = true),
    run: () => vm.runInContext(source, context, { filename: "bootstrap.cjs" }),
    config: (value) =>
      fs.writeFileSync(
        path.join(appDirectory, "package.json"),
        JSON.stringify(value),
      ),
    original: (value) =>
      fs.writeFileSync(
        path.join(appDirectory, "qscope-original-main.json"),
        JSON.stringify(value),
      ),
  };
}

test("isolates Electron paths, preserves QQ's name and loads the bridge before QQ", (t) => {
  const f = fixture(t);
  f.env.QSCOPE_CLIENT_BUILD = "explicit-client-build";
  f.run();
  assert.deepEqual(f.events[0], ["name", "QQ"]);
  assert.deepEqual(f.events.slice(-2), [
    ["bridge"],
    ["original", f.defaultEntry],
  ]);
  assert.equal(Object.keys(f.paths).length, 13);
  for (const filename of Object.values(f.paths)) {
    assert.ok(filename.startsWith(f.profile + path.sep));
    assert.ok(fs.statSync(filename).isDirectory());
  }
  assert.equal(
    f.paths.appData,
    path.join(f.profile, "home/Library/Application Support"),
  );
  assert.equal(f.paths.documents, path.join(f.profile, "home/Documents"));
  assert.equal(f.paths.videos, path.join(f.profile, "home/Movies"));
  assert.equal(f.env.QSCOPE_CLIENT_BUILD, "explicit-client-build");
  assert.deepEqual(f.reads, [{ length: 64, position: 0 }]);
  assert.ok(Object.isFrozen(f.context.__qscopeProcessIdentity));
});

test("uses the original package main and version before the fixed ASAR fallback", (t) => {
  const f = fixture(t);
  const entry = path.join(f.appDirectory, "original.cjs");
  fs.writeFileSync(entry, "// original\n");
  f.original({ main: "./original.cjs", version: "original-build" });
  f.config({ main: "./other.cjs", version: "rewritten-version" });
  f.run();
  assert.deepEqual(f.events.at(-1), ["original", entry]);
  assert.equal(f.env.QSCOPE_CLIENT_BUILD, "original-build");
});

test("preserves an unchanged package.main without requiring a backup", (t) => {
  const f = fixture(t);
  f.config({
    main: "application.asar/app_launcher/index.js",
    version: "package-build",
  });
  f.run();
  assert.deepEqual(f.events.at(-1), ["original", f.defaultEntry]);
  assert.equal(f.env.QSCOPE_CLIENT_BUILD, "package-build");
});

test("invalid QSCAP headers and process identities fail before either entry loads", async (t) => {
  const cases = [
    ["truncated", (header) => header.subarray(0, 63)],
    ["magic", (header) => ((header[0] = 0), header)],
    ["version", (header) => (header.writeUInt32LE(2, 8), header)],
    ["size", (header) => (header.writeUInt32LE(128, 12), header)],
    ["pid", (header) => (header.writeUInt32LE(7320, 16), header)],
    ["birth", (header) => (header.writeBigUInt64LE(0n, 24), header)],
  ];
  for (const [name, alter] of cases) {
    await t.test(name, (t) => {
      const f = fixture(t);
      fs.writeFileSync(f.capture, alter(f.header));
      assert.throws(f.run, /QScope research bootstrap:/);
      assert.deepEqual(f.events, []);
      assert.equal(f.env.QSCOPE_STATE_ROOT, undefined);
    });
  }
  await t.test("existing identity", (t) => {
    const f = fixture(t);
    f.context.__qscopeProcessIdentity = { pid: 7319, birth_filetime: "1" };
    assert.throws(f.run, /existing process identity/);
    assert.deepEqual(f.events, []);
  });
});

test("invalid isolation paths never start an unisolated app", async (t) => {
  const cases = [
    ["relative profile", (f) => (f.env.QSCOPE_RESEARCH_PROFILE = "profile")],
    [
      "root profile",
      (f) => (f.env.QSCOPE_RESEARCH_PROFILE = path.parse(f.root).root),
    ],
    ["traversal", (f) => (f.env.QSCOPE_RESEARCH_PROFILE += "/../profile")],
    ["HOME", (f) => (f.env.HOME = f.root)],
    ["CFFIXED_USER_HOME", (f) => delete f.env.CFFIXED_USER_HOME],
    ["TMPDIR", (f) => (f.env.TMPDIR = f.root)],
    ["relative output", (f) => (f.env.QSCOPE_PROBE_OUTPUT = "capture.qscap")],
    ["directory as output", (f) => (f.env.QSCOPE_PROBE_OUTPUT = f.profile)],
    [
      "linked downloads",
      (f) => fs.symlinkSync(f.appDirectory, path.join(f.profile, "files")),
    ],
    [
      "file as downloads",
      (f) => fs.writeFileSync(path.join(f.profile, "files"), "file"),
    ],
    [
      "linked capture",
      (f) => {
        fs.renameSync(f.capture, f.capture + ".real");
        fs.symlinkSync(f.capture + ".real", f.capture);
      },
    ],
    [
      "linked profile",
      (f) => {
        fs.renameSync(f.profile, f.profile + ".real");
        fs.symlinkSync(f.profile + ".real", f.profile);
      },
    ],
  ];
  for (const [name, alter] of cases) {
    await t.test(name, (t) => {
      const f = fixture(t);
      alter(f);
      assert.throws(f.run);
      assert.deepEqual(f.events, []);
    });
  }
});

test("invalid bridge configuration and original entries fail explicitly", async (t) => {
  const cases = [
    ["missing port", (f) => delete f.env.QSCOPE_DEVTOOLS_PORT],
    ["privileged port", (f) => (f.env.QSCOPE_DEVTOOLS_PORT = "1023")],
    ["port overflow", (f) => (f.env.QSCOPE_DEVTOOLS_PORT = "65536")],
    ["nondecimal port", (f) => (f.env.QSCOPE_DEVTOOLS_PORT = "1e4")],
    ["empty build", (f) => (f.env.QSCOPE_CLIENT_BUILD = "")],
    ["missing build", (f) => f.config({ main: "qscope-bootstrap.cjs" })],
    [
      "missing bridge",
      (f) => fs.unlinkSync(path.join(f.appDirectory, "qscope-automation.cjs")),
    ],
    ["escaping main", (f) => f.original({ main: "../outside.cjs" })],
    ["absolute main", (f) => f.original({ main: f.defaultEntry })],
    ["recursive main", (f) => f.original({ main: "./qscope-bootstrap.cjs" })],
    ["bridge as main", (f) => f.original({ main: "./qscope-automation.cjs" })],
    ["missing main", (f) => f.original({ main: "./missing.cjs" })],
    ["invalid backup", (f) => f.original({ version: "backup-without-main" })],
    [
      "dangling backup",
      (f) =>
        fs.symlinkSync(
          "missing.json",
          path.join(f.appDirectory, "qscope-original-main.json"),
        ),
    ],
    ["late execution", (f) => f.ready()],
    ["renderer", (f) => (f.processMock.type = "renderer")],
  ];
  for (const [name, alter] of cases) {
    await t.test(name, (t) => {
      const f = fixture(t);
      alter(f);
      assert.throws(f.run);
      assert.deepEqual(f.events, []);
    });
  }
});

test("an Electron path rejection prevents bridge and original entry loading", (t) => {
  const f = fixture(t);
  f.app.getPath = () => f.root;
  assert.throws(f.run, /Electron did not accept/);
  assert.ok(
    !f.events.some(([event]) => event === "bridge" || event === "original"),
  );
});
