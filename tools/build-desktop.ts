import { createHash, randomUUID } from "node:crypto";
import {
  cp,
  copyFile,
  mkdir,
  readdir,
  stat,
  writeFile,
} from "node:fs/promises";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { buildNative, type NativeManifest } from "./build-native.ts";
import { buildIcons } from "./build-icons.ts";
import { packResources, writeZip } from "./pack-resources.ts";
import { buildInstaller } from "./build-installer.ts";
import { signRelease } from "./release-sign.ts";
import { command, json, pnpm, sha256, writeJson } from "./build-utils.ts";

const repo = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const target = `${process.platform}-${process.arch}`;
const options = new Set(process.argv.slice(2));
if (options.has("--help")) {
  console.log(
    "node tools/build-desktop.ts [--prepare | --no-bundle]\nAvailable collector target win32-x64",
  );
  process.exit(0);
}
for (const option of options)
  if (!["--prepare", "--no-bundle"].includes(option))
    throw new Error(`Unknown desktop build option ${option}`);
if (target !== "win32-x64")
  throw new Error(`Desktop collector adapter is unavailable for ${target}`);
if (options.has("--prepare") && options.has("--no-bundle"))
  throw new Error("Choose either --prepare or --no-bundle");
const configurationSource = await json<{
  identifier: string;
  mainBinaryName: string;
  bundle: { publisher: string };
}>(join(repo, "apps/desktop/tauri.conf.json"));
const metadata: { packages: { name: string; version: string }[] } = JSON.parse(
  await command(
    "cargo",
    ["metadata", "--no-deps", "--format-version", "1", "--locked"],
    { cwd: repo, capture: true },
  ),
);
const version = metadata.packages.find(
  (entry) => entry.name === "qscope-desktop",
)?.version;
if (!version || !/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(version))
  throw new Error("Invalid desktop package version");
for (const entry of metadata.packages)
  if (entry.version !== version)
    throw new Error(`Workspace version mismatch in ${entry.name}`);
for (const path of [
  "apps/desktop/tauri.conf.json",
  "apps/desktop/package.json",
  "apps/web/package.json",
]) {
  if ((await json<{ version?: string }>(join(repo, path))).version !== version)
    throw new Error(`Release version mismatch in ${path}`);
}

const releaseEnv = {
  RUSTFLAGS: [process.env.RUSTFLAGS, "-C target-feature=+crt-static"]
    .filter(Boolean)
    .join(" "),
};
const native = await buildNative();
const nativeManifest = await json<NativeManifest>(
  join(native, "manifest.json"),
);
const build = join(repo, "build/desktop", randomUUID());
const payload = join(build, "payload");
await mkdir(payload, { recursive: true });
await writeFile(join(payload, ".owner"), "qscope-desktop-payload");
await pnpm(["--dir", "apps/web", "install", "--frozen-lockfile"], repo);
await command(
  "cargo",
  ["run", "-p", "qscope-web", "--locked", "--", "types", "--check"],
  { cwd: repo },
);
await pnpm(["--dir", "apps/web", "run", "build"], repo);
await pnpm(["--dir", "apps/desktop", "install", "--frozen-lockfile"], repo);
await buildIcons();
await cp(join(repo, "apps/web/dist"), join(payload, "ui"), { recursive: true });
await mkdir(join(payload, "native"));
for (const file of nativeManifest.outputs.filter(
  (file) => !file.path.endsWith(".exe"),
)) {
  if (file.path !== file.path.split(/[\\/]/).at(-1))
    throw new Error("Invalid native artifact filename");
  if ((await sha256(join(native, file.path))) !== file.sha256)
    throw new Error("Native build output hash changed");
  await copyFile(join(native, file.path), join(payload, "native", file.path));
}
await writeJson(join(payload, "native/manifest.json"), {
  ...nativeManifest,
  outputs: nativeManifest.outputs.filter((file) => !file.path.endsWith(".exe")),
});
await mkdir(join(payload, "collectors/qqnt-windows"), { recursive: true });
await copyFile(
  join(repo, "tools/qq-portable/client.json"),
  join(payload, "collectors/qqnt-windows/client.json"),
);
await copyFile(
  join(repo, "collectors/qqnt-windows/profile.json"),
  join(payload, "collectors/qqnt-windows/profile.json"),
);
const devtools = join(payload, "collectors/devtools");
await mkdir(devtools, { recursive: true });
await pnpm(
  [
    "--dir",
    "collectors/devtools",
    "install",
    "--prod",
    "--frozen-lockfile",
    "--ignore-scripts",
  ],
  repo,
);
const chii = join(repo, "collectors/devtools/node_modules/chii");
await cp(join(chii, "public"), join(devtools, "node_modules/chii/public"), {
  recursive: true,
  dereference: true,
});
await copyFile(
  join(chii, "LICENSE"),
  join(devtools, "node_modules/chii/LICENSE"),
);

const licenses = join(payload, "licenses");
await mkdir(licenses);
const files: { path: string; bytes: number; sha256: string }[] = [];
async function inventory(directory: string): Promise<void> {
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isSymbolicLink())
      throw new Error("Desktop payload must contain physical files");
    if (entry.isDirectory()) await inventory(path);
    else
      files.push({
        path: relative(payload, path).replaceAll("\\", "/"),
        bytes: (await stat(path)).size,
        sha256: await sha256(path),
      });
  }
}
await inventory(payload);
files.sort((left, right) =>
  left.path < right.path ? -1 : left.path > right.path ? 1 : 0,
);
const payloadId = createHash("sha256")
  .update(JSON.stringify(files))
  .digest("hex")
  .slice(0, 12);
const archivePath = join(build, "resources.zip");
const bundle = await packResources(
  payload,
  files,
  version,
  target,
  archivePath,
);
const sourceFiles = (
  await command(
    "git",
    ["ls-files", "--cached", "--others", "--exclude-standard", "-z"],
    { cwd: repo, capture: true },
  )
)
  .split("\0")
  .filter(
    (path) =>
      (/\.(rs|cpp|h|hpp|toml|lock)$/.test(path) ||
        path === "crates/qscope-automation/skills/qscope/SKILL.md") &&
      !path.startsWith("tests/"),
  );
const sourceHash = createHash("sha256").update(bundle.sha256);
for (const path of [...new Set(sourceFiles)].sort()) {
  if (await stat(join(repo, path)).catch(() => null))
    sourceHash.update(path).update(await sha256(join(repo, path)));
}
const buildId = sourceHash.digest("hex");
const productEnv = { ...releaseEnv, QSCOPE_BUILD_ID: buildId };
const bundledCli = join(build, "qscope-cli.exe");
await command("cargo", ["build", "-p", "qscope-cli", "--release", "--locked"], {
  cwd: repo,
  env: { ...productEnv, QSCOPE_RESOURCES_ARCHIVE: archivePath },
});
await copyFile(join(repo, "target/release/qscope-cli.exe"), bundledCli);
await command(
  bundledCli,
  ["--json", "runtime", "--data-dir", join(build, "self-test")],
  { capture: true },
);
const cliHash = await sha256(bundledCli);
const manifest = {
  schema_version: 1,
  owner: "qscope-desktop-build",
  version,
  build_id: buildId,
  identifier: configurationSource.identifier,
  publisher: configurationSource.bundle.publisher,
  created_utc: new Date().toISOString(),
  target,
  payload_id: payloadId,
  bundle,
  cli_sha256: cliHash,
  native_build: nativeManifest.build_id,
  files,
};
await writeJson(join(build, "manifest.json"), manifest);
const reference = {
  schema_version: 1,
  resources: payload,
  manifest: join(build, "manifest.json"),
};
console.log(`Desktop resources ${payload}`);
if (options.has("--prepare")) {
  await writeJson(join(repo, "build/desktop/prepared.json"), reference);
  process.exit(0);
}
const configuration = join(build, "tauri.bundle.json");
await writeJson(configuration, {
  bundle: { resources: { [bundledCli]: "qscope-cli.exe" } },
});
await pnpm(
  [
    "--dir",
    "apps/desktop",
    "exec",
    "tauri",
    "build",
    "--ci",
    "--no-sign",
    "--config",
    configuration,
    "--no-bundle",
    "--",
    "--locked",
  ],
  repo,
  productEnv,
);
const desktopExecutable = join(
  repo,
  "target/release",
  configurationSource.mainBinaryName + ".exe",
);
const desktopHash = await sha256(desktopExecutable);
const releaseId = createHash("sha256")
  .update(cliHash)
  .update(desktopHash)
  .digest("hex")
  .slice(0, 12);
const portable = join(repo, "dist", `QScope-${version}-${target}-${releaseId}`);
await mkdir(portable, { recursive: true });
await copyFile(desktopExecutable, join(portable, "QScope.exe"));
await copyFile(bundledCli, join(portable, "qscope-cli.exe"));
await writeJson(join(build, "manifest.json"), {
  ...manifest,
  release_id: releaseId,
  desktop_sha256: desktopHash,
});
const releaseAssets: { path: string; bytes: number; sha256: string }[] = [];
const distribution = join(
  repo,
  "dist",
  `QScope-${version}-${target}-${releaseId}-release`,
);
await mkdir(distribution, { recursive: true });
if (!options.has("--no-bundle")) {
  const installer = join(
    distribution,
    `QScope-${version}-windows-x64-setup.exe`,
  );
  const generated = await buildInstaller(repo, portable, distribution, version);
  if (generated !== installer)
    throw new Error("Installer output path differs from the release manifest");
  releaseAssets.push({
    path: installer,
    bytes: (await stat(installer)).size,
    sha256: await sha256(installer),
  });
}
for (const [edition, entries] of [
  ["portable", ["QScope.exe", "qscope-cli.exe"]],
  ["cli-webui", ["qscope-cli.exe"]],
] as const) {
  const archive = join(
    distribution,
    `QScope-${version}-windows-x64-${edition}.zip`,
  );
  const files = await Promise.all(
    entries.map(async (path) => ({
      path,
      bytes: (await stat(join(portable, path))).size,
      sha256: await sha256(join(portable, path)),
    })),
  );
  await writeZip(portable, files, archive);
  releaseAssets.push({
    path: archive,
    bytes: (await stat(archive)).size,
    sha256: await sha256(archive),
  });
}
await writeFile(
  join(distribution, "SHA256SUMS.txt"),
  releaseAssets
    .map((asset) => `${asset.sha256}  ${relative(distribution, asset.path)}`)
    .join("\n") + "\n",
);
// Online updates accept only the signed manifest; an unsigned build still produces the packages.
const update = await signRelease(distribution, version);
console.log(
  update.signed
    ? `Signed update manifest ${update.manifest}`
    : "Update manifest is unsigned: set QSCOPE_RELEASE_SIGNING_KEY before publishing an online update",
);
console.log(`Release assets ${distribution}`);
await writeJson(join(build, "result.json"), {
  schema_version: 1,
  version,
  portable,
  release_id: releaseId,
  desktop_sha256: desktopHash,
  cli_sha256: cliHash,
  assets: releaseAssets,
});
await writeJson(join(repo, "build/desktop/current.json"), reference);
console.log(`Portable desktop ${portable}`);
