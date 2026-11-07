import { createHash, randomUUID } from "node:crypto";
import {
  copyFile,
  mkdir,
  readFile,
  readdir,
  rename,
  writeFile,
} from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { command, download, json, sha256, writeJson } from "./build-utils.ts";

const repo = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const collector = join(repo, "collectors/qqnt-windows");

interface Profile {
  schema_version: number;
  build: string;
  module_sha256: string;
  send_account_offset: string;
  send_account_type_offset: string;
  send_prefix: string;
  recv_prefix: string;
}
interface Client {
  build: string;
  modules: { path: string; sha256: string }[];
}
interface Minhook {
  repository: string;
  commit: string;
  files: Record<string, string>;
}
export interface NativeManifest {
  build_id: string;
  outputs: { path: string; sha256: string }[];
}

function versionOrder(left: string, right: string): number {
  const a = left.split(".").map(Number),
    b = right.split(".").map(Number);
  for (let index = 0; index < Math.max(a.length, b.length); index++) {
    const difference = (a[index] || 0) - (b[index] || 0);
    if (difference) return difference;
  }
  return 0;
}

async function latestDirectory(path: string): Promise<string> {
  const names = (await readdir(path, { withFileTypes: true }))
    .filter((entry) => entry.isDirectory() && /^\d+(\.\d+)+$/.test(entry.name))
    .map((entry) => entry.name);
  names.sort(versionOrder);
  if (!names.length)
    throw new Error(`Build toolchain directory is empty (${path})`);
  return names.at(-1)!;
}

export async function buildNative(): Promise<string> {
  if (process.platform !== "win32" || process.arch !== "x64") {
    throw new Error(
      `QQ collector adapter is unavailable for ${process.platform}-${process.arch}`,
    );
  }
  const profile = await json<Profile>(join(collector, "profile.json"));
  const client = await json<Client>(join(collector, "client.json"));
  const dependency = (
    await json<{ minhook: Minhook }>(join(collector, "dependencies.json"))
  ).minhook;
  if (
    profile.schema_version !== 1 ||
    profile.build !== client.build ||
    profile.module_sha256 !==
      client.modules.find((module) => module.path.endsWith("/wrapper.node"))
        ?.sha256
  ) {
    throw new Error("QQ collector profile does not match its client identity");
  }
  for (const name of [
    "send_account_offset",
    "send_account_type_offset",
  ] as const)
    if (!/^0x[0-9a-f]{1,3}$/.test(profile[name]))
      throw new Error("Invalid collector account offset");
  for (const name of ["send_prefix", "recv_prefix"] as const)
    if (!/^[0-9a-f]{64}$/.test(profile[name]))
      throw new Error("Invalid collector prefix");
  const minhook = join(
    repo,
    ".cache/dependencies/minhook",
    dependency.commit,
    "source",
  );
  async function validDependency(path: string): Promise<boolean> {
    try {
      for (const [name, hash] of Object.entries(dependency.files))
        if ((await sha256(join(path, name))) !== hash) return false;
      return true;
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === "ENOENT") return false;
      throw error;
    }
  }
  if (!(await validDependency(minhook))) {
    for (const [name, hash] of Object.entries(dependency.files))
      await download(
        `https://raw.githubusercontent.com/${dependency.repository}/${dependency.commit}/${name}`,
        join(minhook, name),
        hash,
      );
    if (!(await validDependency(minhook)))
      throw new Error("MinHook 源码校验失败");
  }
  const programFiles =
    process.env["ProgramFiles(x86)"] || "C:/Program Files (x86)";
  const vswhere = join(
    programFiles,
    "Microsoft Visual Studio/Installer/vswhere.exe",
  );
  const visualStudio = await command(
    vswhere,
    [
      "-latest",
      "-products",
      "*",
      "-requires",
      "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
      "-property",
      "installationPath",
    ],
    { capture: true },
  );
  const msvcRoot = join(visualStudio, "VC/Tools/MSVC");
  const msvcVersion = await latestDirectory(msvcRoot);
  const msvc = join(msvcRoot, msvcVersion);
  const sdk = join(programFiles, "Windows Kits/10");
  const sdkVersion = await latestDirectory(join(sdk, "Include"));
  const compiler = join(msvc, "bin/Hostx64/x64/cl.exe");
  const sources: Record<string, string> = {};
  const paths: Record<string, string> = {};
  paths["scripts/build-native.ts"] = fileURLToPath(import.meta.url);
  for (const name of ["portable.cpp", "client.json", "dependencies.json"])
    paths[name] = join(collector, name);
  for (const name of [
    "probe.cpp",
    "probe.hpp",
    "transport.hpp",
    "automation.js",
    "profile.json",
  ])
    paths["collectors/qqnt-windows/" + name] = join(collector, name);
  paths["tests/native/windows/selftest.cpp"] = join(
    repo,
    "tests/native/windows/selftest.cpp",
  );
  for (const name of Object.keys(paths).sort())
    sources[name] = await sha256(paths[name]);
  const compilerHash = await sha256(compiler);
  const buildId = createHash("sha256")
    .update(JSON.stringify({ sources, compilerHash, msvcVersion, sdkVersion }))
    .digest("hex");
  const root = join(repo, "build/native/qqnt-windows-x64");
  const output = join(root, buildId.slice(0, 16));
  try {
    const existing = await json<NativeManifest>(join(output, "manifest.json"));
    if (existing.build_id !== buildId)
      throw new Error("Existing native build has a different identity");
    for (const file of existing.outputs)
      if ((await sha256(join(output, file.path))) !== file.sha256)
        throw new Error("Existing native output hash changed");
    await writeJson(join(root, "current.json"), {
      schema_version: 1,
      build_id: buildId,
    });
    return output;
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
  }
  const staging = join(root, "building-" + randomUUID());
  await mkdir(staging, { recursive: true });
  const prefix = (name: "send_prefix" | "recv_prefix") =>
    profile[name]
      .match(/../g)!
      .map((byte) => "0x" + byte)
      .join(",");
  await writeFile(
    join(staging, "client-profile.hpp"),
    `#pragma once\nnamespace qscope_profile {\nconstexpr unsigned send_account_offset=${profile.send_account_offset};\nconstexpr unsigned send_account_type_offset=${profile.send_account_type_offset};\nconstexpr unsigned char send_prefix[]={${prefix("send_prefix")}};\nconstexpr unsigned char recv_prefix[]={${prefix("recv_prefix")}};\n}\n`,
  );
  const automation = await readFile(join(collector, "automation.js"), "utf8");
  if (automation.includes(')QScopeJS"'))
    throw new Error("Automation script conflicts with its C++ delimiter");
  await writeFile(
    join(staging, "automation-script.hpp"),
    `#pragma once\nconstexpr char qscope_automation_script[] = R"QScopeJS(${automation})QScopeJS";\n`,
  );
  const env = {
    PATH: dirname(compiler) + ";" + process.env.PATH,
    INCLUDE: [
      join(msvc, "include"),
      ...["ucrt", "shared", "um", "winrt"].map((name) =>
        join(sdk, "Include", sdkVersion, name),
      ),
    ].join(";"),
    LIB: [
      join(msvc, "lib/x64"),
      join(sdk, "Lib", sdkVersion, "ucrt/x64"),
      join(sdk, "Lib", sdkVersion, "um/x64"),
    ].join(";"),
    VSLANG: "1033",
  };
  const common = [
    "/nologo",
    "/O2",
    "/EHsc",
    "/MT",
    "/utf-8",
    "/std:c++17",
    "/Fo" + staging + "/",
  ];
  const includes = [
    "/I" + join(minhook, "include"),
    "/I" + collector,
    "/I" + staging,
  ];
  const hooks = [
    "src/buffer.c",
    "src/hook.c",
    "src/trampoline.c",
    "src/hde/hde64.c",
  ].map((name) => join(minhook, name));
  await command(
    compiler,
    [
      ...common,
      "/LD",
      ...includes,
      join(collector, "portable.cpp"),
      join(collector, "probe.cpp"),
      ...hooks,
      "/link",
      "/INCREMENTAL:NO",
      "/OUT:" + join(staging, "qscope-portable.dll"),
      "shell32.lib",
      "ole32.lib",
      "uuid.lib",
      "advapi32.lib",
      "imm32.lib",
    ],
    { env, cwd: repo },
  );
  await command(
    compiler,
    [
      ...common,
      ...includes,
      join(repo, "tests/native/windows/selftest.cpp"),
      ...hooks,
      "/link",
      "/INCREMENTAL:NO",
      "/OUT:" + join(staging, "probe-selftest.exe"),
    ],
    { env, cwd: repo },
  );
  await copyFile(
    join(minhook, "LICENSE.txt"),
    join(staging, "LICENSE.minhook.txt"),
  );
  const outputs = [];
  for (const name of [
    "qscope-portable.dll",
    "probe-selftest.exe",
    "LICENSE.minhook.txt",
  ])
    outputs.push({ path: name, sha256: await sha256(join(staging, name)) });
  for (const [name, path] of Object.entries(paths))
    if ((await sha256(path)) !== sources[name])
      throw new Error("Native source changed during compilation");
  await writeJson(join(staging, "manifest.json"), {
    schema_version: 1,
    build_id: buildId,
    created_utc: new Date().toISOString(),
    sources,
    compiler,
    compiler_sha256: compilerHash,
    compiler_version: msvcVersion,
    sdk_version: sdkVersion,
    minhook_commit: dependency.commit,
    outputs,
  });
  await rename(staging, output);
  await writeJson(join(root, "current.json"), {
    schema_version: 1,
    build_id: buildId,
  });
  return output;
}

if (
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  if (process.argv.slice(2).includes("--help"))
    console.log("pnpm build:native\n本机 Windows x64 采集器构建");
  else if (process.argv.length > 2)
    throw new Error("Unknown native collector build option");
  else console.log(await buildNative());
}
