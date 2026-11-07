/* eslint-disable @typescript-eslint/no-require-imports -- Electron's package.main loads CommonJS before QQ's original entry. */
"use strict";

// Deploy beside package.json, qscope-original-main.json and the unchanged
// Windows automation.js copied as qscope-automation.cjs. No QQ APIs are replaced.
(() => {
  const fs = require("node:fs");
  const path = require("node:path");
  const { app } = require("electron");
  const fail = (message) => {
    throw new Error(`QScope research bootstrap: ${message}`);
  };
  if (process.type !== "browser" || app.isReady()) {
    fail("requires the Electron main process before app readiness");
  }

  function absolutePath(value, label) {
    if (
      typeof value !== "string" ||
      value.includes("\0") ||
      !path.isAbsolute(value) ||
      value.split(path.sep).includes("..")
    ) {
      fail(`${label} must be an absolute path without parent traversal`);
    }
    return path.resolve(value);
  }

  // Do not canonicalize through links: the launcher sandbox names this exact
  // isolated tree, and following a linked directory could escape it.
  function noLinks(filename, allowMissing = false) {
    const root = path.parse(filename).root;
    const parts = filename.slice(root.length).split(path.sep).filter(Boolean);
    let current = root;
    for (let index = 0; index < parts.length; index += 1) {
      current = path.join(current, parts[index]);
      let stat;
      try {
        stat = fs.lstatSync(current);
      } catch (error) {
        if (allowMissing && error.code === "ENOENT") return;
        throw error;
      }
      if (stat.isSymbolicLink())
        fail(`symbolic link is not allowed: ${current}`);
      if (index < parts.length - 1 && !stat.isDirectory()) {
        fail(`parent path is not a directory: ${current}`);
      }
    }
  }

  function openRegular(filename) {
    noLinks(filename);
    if (!fs.lstatSync(filename).isFile())
      fail(`not a regular file: ${filename}`);
    const descriptor = fs.openSync(
      filename,
      fs.constants.O_RDONLY |
        (fs.constants.O_NOFOLLOW || 0) |
        (fs.constants.O_NONBLOCK || 0),
    );
    try {
      const stat = fs.fstatSync(descriptor);
      if (!stat.isFile()) fail(`not a regular file: ${filename}`);
      return { descriptor, size: stat.size };
    } catch (error) {
      fs.closeSync(descriptor);
      throw error;
    }
  }

  function readExactly(descriptor, size) {
    const bytes = Buffer.alloc(size);
    let offset = 0;
    while (offset < size) {
      const count = fs.readSync(
        descriptor,
        bytes,
        offset,
        size - offset,
        offset,
      );
      if (count === 0) fail("unexpected end of file");
      offset += count;
    }
    return bytes;
  }

  function readJson(filename, optional = false) {
    if (optional && !fs.existsSync(filename)) {
      // existsSync also returns false for a dangling link, which must fail.
      try {
        fs.lstatSync(filename);
      } catch (error) {
        if (error.code === "ENOENT") return undefined;
        throw error;
      }
    }
    const { descriptor, size } = openRegular(filename);
    try {
      if (size === 0 || size > 64 * 1024)
        fail(`invalid config size: ${filename}`);
      const value = JSON.parse(readExactly(descriptor, size).toString("utf8"));
      if (value === null || typeof value !== "object" || Array.isArray(value)) {
        fail(`config must be an object: ${filename}`);
      }
      return value;
    } finally {
      fs.closeSync(descriptor);
    }
  }

  const profile = absolutePath(
    process.env.QSCOPE_RESEARCH_PROFILE,
    "QSCOPE_RESEARCH_PROFILE",
  );
  if (profile === path.parse(profile).root)
    fail("profile cannot be a filesystem root");
  noLinks(profile, true);
  const directories = {
    home: path.join(profile, "home"),
    appData: path.join(profile, "home", "Library", "Application Support"),
    userData: path.join(profile, "userData"),
    sessionData: path.join(profile, "sessionData"),
    temp: path.join(profile, "tmp"),
    logs: path.join(profile, "logs"),
    crashDumps: path.join(profile, "crashDumps"),
    documents: path.join(profile, "home", "Documents"),
    downloads: path.join(profile, "files"),
    desktop: path.join(profile, "home", "Desktop"),
    pictures: path.join(profile, "home", "Pictures"),
    videos: path.join(profile, "home", "Movies"),
    music: path.join(profile, "home", "Music"),
  };
  for (const [name, expected] of Object.entries({
    HOME: directories.home,
    CFFIXED_USER_HOME: directories.home,
    TMPDIR: directories.temp,
  })) {
    if (absolutePath(process.env[name], name) !== expected) {
      fail(`${name} must already point into the research profile at launch`);
    }
  }
  const portText = process.env.QSCOPE_DEVTOOLS_PORT;
  const port = Number(portText);
  if (
    typeof portText !== "string" ||
    !/^[0-9]+$/.test(portText) ||
    !Number.isInteger(port) ||
    port < 1024 ||
    port > 65535
  ) {
    fail("QSCOPE_DEVTOOLS_PORT must be an integer from 1024 through 65535");
  }

  const capture = absolutePath(
    process.env.QSCOPE_PROBE_OUTPUT,
    "QSCOPE_PROBE_OUTPUT",
  );
  const { descriptor, size } = openRegular(capture);
  let identity;
  try {
    if (size < 64) fail("probe output has no complete QSCAP01 header");
    // The capture is still being appended to. Never read packet bodies here.
    const header = readExactly(descriptor, 64);
    if (
      !header.subarray(0, 8).equals(Buffer.from("QSCAP01\0")) ||
      header.readUInt32LE(8) !== 1 ||
      header.readUInt32LE(12) !== 64
    ) {
      fail("invalid QSCAP01 header");
    }
    const pid = header.readUInt32LE(16);
    const birth = header.readBigUInt64LE(24);
    if (pid !== process.pid || birth === 0n) {
      fail("probe process identity does not match this Electron process");
    }
    identity = Object.freeze({ pid, birth_filetime: birth.toString() });
  } finally {
    fs.closeSync(descriptor);
  }
  const existingIdentity = globalThis.__qscopeProcessIdentity;
  if (
    existingIdentity !== undefined &&
    (existingIdentity === null ||
      existingIdentity.pid !== identity.pid ||
      existingIdentity.birth_filetime !== identity.birth_filetime)
  ) {
    fail("existing process identity does not match the probe header");
  }

  const config = readJson(path.join(__dirname, "package.json"));
  const original = readJson(
    path.join(__dirname, "qscope-original-main.json"),
    true,
  );
  const ownEntries = new Set([
    path.resolve(__filename),
    path.join(__dirname, "bootstrap.cjs"),
    path.join(__dirname, "qscope-bootstrap.cjs"),
  ]);
  function resolveEntry(main) {
    if (
      typeof main !== "string" ||
      main.length === 0 ||
      main.includes("\0") ||
      path.isAbsolute(main) ||
      main.split(/[\\/]/).includes("..")
    ) {
      fail("original package.main must be a relative path inside the app");
    }
    const entry = path.resolve(__dirname, main);
    const relative = path.relative(__dirname, entry);
    if (relative === "" || relative.startsWith(`..${path.sep}`)) {
      fail("original package.main escapes the app directory");
    }
    return entry;
  }
  let entry;
  if (original !== undefined) {
    entry = resolveEntry(original.main);
  } else if (config.main !== undefined) {
    const configured = resolveEntry(config.main);
    if (!ownEntries.has(configured)) entry = configured;
  }
  entry ??= path.join(
    __dirname,
    "application.asar",
    "app_launcher",
    "index.js",
  );
  if (
    ownEntries.has(entry) ||
    entry === path.join(__dirname, "qscope-automation.cjs")
  ) {
    fail("original entry cannot load a QScope bootstrap or automation bridge");
  }
  // Electron's fs supports paths inside ASAR archives; Node tests use a directory.
  if (!fs.statSync(entry).isFile())
    fail("original entry is not a regular file");

  const build =
    process.env.QSCOPE_CLIENT_BUILD ?? original?.version ?? config.version;
  if (
    typeof build !== "string" ||
    build.length === 0 ||
    build.length > 128 ||
    Array.from(build).some(
      (character) =>
        character.charCodeAt(0) <= 32 || character.charCodeAt(0) === 127,
    )
  ) {
    fail("QSCOPE_CLIENT_BUILD or the original package version is required");
  }
  const bridge = path.join(__dirname, "qscope-automation.cjs");
  const checkedBridge = openRegular(bridge);
  fs.closeSync(checkedBridge.descriptor);

  const extraDirectories = [
    path.join(profile, ".qscope"),
    path.join(directories.home, "Library", "Caches"),
    path.join(directories.home, "Library", "Preferences"),
  ];
  const allDirectories = [...Object.values(directories), ...extraDirectories];
  // Validate all existing components before creating any directories.
  for (const directory of allDirectories) noLinks(directory, true);
  for (const directory of allDirectories) {
    fs.mkdirSync(directory, { recursive: true, mode: 0o700 });
    noLinks(directory);
    if (!fs.statSync(directory).isDirectory())
      fail(`not a directory: ${directory}`);
  }

  // Preserve the original application name with an isolated bundle ID and HOME.
  app.setName("QQ");
  for (const [name, directory] of Object.entries(directories)) {
    app.setPath(name, directory);
    if (path.resolve(app.getPath(name)) !== directory) {
      fail(`Electron did not accept the isolated ${name} path`);
    }
  }
  process.env.QSCOPE_STATE_ROOT = profile;
  process.env.QSCOPE_DEVTOOLS_PORT = String(port);
  process.env.QSCOPE_CLIENT_BUILD = build;
  if (existingIdentity === undefined) {
    Object.defineProperty(globalThis, "__qscopeProcessIdentity", {
      value: identity,
      writable: false,
      configurable: false,
    });
  }
  require(bridge);
  require(entry);
})();
