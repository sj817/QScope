// 构建 ARM64 实验采集器，操作范围限于编译。
import { mkdir, stat, copyFile, readFile } from "node:fs/promises";
import { createHash } from "node:crypto";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { command, download, sha256, writeJson } from "./build-utils.ts";

if (process.platform !== "darwin" || process.arch !== "arm64")
  throw new Error(
    "The native Mac collector requires macOS ARM64 and Xcode command line tools",
  );
const repo = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const deps = join(repo, ".cache/dependencies/macos-native");
const buildRoot = join(repo, "build/macos-native");
const sources = join(repo, "collectors/qqnt-macos/native");
const dependencies = [
  {
    name: "cmake-3.31.6-macos-universal.tar.gz",
    url: "https://github.com/Kitware/CMake/releases/download/v3.31.6/cmake-3.31.6-macos-universal.tar.gz",
    sha256: "330b9514f5112e5ed4fb08b8b05803b776fd9b539a6ae12927d14dcc0ee2ba8d",
    directory: "cmake",
  },
  {
    name: "dobby-5dfc854.tar.gz",
    url: "https://codeload.github.com/jmpews/Dobby/tar.gz/5dfc8546954ce3b3198132ab13fddb89ee92cdd7",
    sha256: "01a417233c40929aa21098c8a9724c00f6f05b5cde3f11abcac84b8e00053748",
    directory: "dobby",
  },
];
const files: Record<string, string> = {};
for (const name of ["probe.cpp", "layout.hpp", "transport.hpp"])
  files[name] = await sha256(join(sources, name));
for (const name of ["layout.cpp", "probe.cpp", "probe-fixture.cpp"])
  files["tests/" + name] = await sha256(join(repo, "tests/native/macos", name));
files["build-native-macos.ts"] = await sha256(fileURLToPath(import.meta.url));
const compiler = await command("/usr/bin/xcrun", ["clang++", "--version"], {
  capture: true,
});
const sdk = await command("/usr/bin/xcrun", ["--show-sdk-version"], {
  capture: true,
});
const identity = createHash("sha256")
  .update(JSON.stringify({ files, dependencies, compiler, sdk }))
  .digest("hex");
const output = join(buildRoot, identity.slice(0, 16));
const marker = { output, build_id: identity };
const manifestText = await readFile(
  join(output, "manifest.json"),
  "utf8",
).catch((error: NodeJS.ErrnoException) => {
  if (error.code === "ENOENT") return null;
  throw error;
});
if (manifestText !== null) {
  const existing = JSON.parse(manifestText) as {
    build_id: string;
    outputs: { path: string; sha256: string }[];
  };
  if (existing.build_id !== identity)
    throw new Error("Native build identity mismatch");
  for (const file of existing.outputs) {
    if (
      !/^[a-zA-Z0-9_.-]+$/.test(file.path) ||
      (await sha256(join(output, file.path))) !== file.sha256
    )
      throw new Error("Native build output integrity mismatch");
  }
  await writeJson(join(buildRoot, "current.json"), marker);
  console.log(JSON.stringify({ ...marker, reused: true }));
  process.exit(0);
}
await mkdir(output, { recursive: true });
for (const dependency of dependencies) {
  const archive = join(deps, dependency.name);
  await download(dependency.url, archive, dependency.sha256);
  const destination = join(deps, dependency.directory);
  if (!(await stat(destination).catch(() => null))) {
    await mkdir(destination, { recursive: true });
    await command("/usr/bin/tar", [
      "-xf",
      archive,
      "--strip-components=1",
      "-C",
      destination,
    ]);
  }
}
const cmake = join(deps, "cmake/CMake.app/Contents/bin/cmake");
await command(cmake, [
  "-S",
  join(deps, "dobby"),
  "-B",
  join(buildRoot, "dobby"),
  "-DCMAKE_BUILD_TYPE=Release",
  "-DCMAKE_OSX_ARCHITECTURES=arm64",
  "-DCMAKE_OSX_DEPLOYMENT_TARGET=12.0",
  "-DFullFloatingPointRegisterPack=ON",
  "-DPlugin.SymbolResolver=ON",
  "-DDOBBY_BUILD_TEST=OFF",
  "-DDOBBY_BUILD_EXAMPLE=OFF",
]);
await command(cmake, [
  "--build",
  join(buildRoot, "dobby"),
  "--target",
  "dobby_static",
  "-j",
  "6",
]);
const flags = [
  "clang++",
  "-std=c++17",
  "-arch",
  "arm64",
  "-mmacosx-version-min=12.0",
  "-I",
  sources,
  "-Wall",
  "-Wextra",
  "-Werror",
];
await command("/usr/bin/xcrun", [
  ...flags,
  "-O2",
  "-dynamiclib",
  "-fvisibility=hidden",
  "-I",
  join(deps, "dobby/include"),
  join(sources, "probe.cpp"),
  join(buildRoot, "dobby/libdobby.a"),
  "-o",
  join(output, "qscope-probe.dylib"),
]);
await command("/usr/bin/xcrun", [
  ...flags,
  "-O2",
  join(repo, "tests/native/macos/layout.cpp"),
  "-o",
  join(output, "layout-test"),
]);
await command(join(output, "layout-test"), []);
await command("/usr/bin/xcrun", [
  ...flags,
  "-O0",
  "-dynamiclib",
  join(repo, "tests/native/macos/probe-fixture.cpp"),
  "-o",
  join(output, "synthetic-wrapper.node"),
]);
await command("/usr/bin/xcrun", [
  ...flags,
  "-O2",
  join(repo, "tests/native/macos/probe.cpp"),
  "-o",
  join(output, "probe-test"),
]);
await command("/usr/bin/codesign", [
  "--force",
  "--sign",
  "-",
  join(output, "qscope-probe.dylib"),
]);
await copyFile(join(deps, "dobby/LICENSE"), join(output, "Dobby-LICENSE.txt"));
await writeJson(join(output, "manifest.json"), {
  schema_version: 1,
  platform: "macos",
  architecture: "arm64",
  build_id: identity,
  dependencies,
  sources: files,
  toolchain: { compiler, sdk },
  outputs: await Promise.all(
    [
      "qscope-probe.dylib",
      "layout-test",
      "probe-test",
      "synthetic-wrapper.node",
      "Dobby-LICENSE.txt",
    ].map(async (path) => ({ path, sha256: await sha256(join(output, path)) })),
  ),
  boundary:
    "Experimental instrumentation and synthetic tests; no QQ version is certified by this build",
});
await writeJson(join(buildRoot, "current.json"), marker);
console.log(JSON.stringify({ output, build_id: identity }));
