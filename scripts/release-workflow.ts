// 平台构建和聚合发布共用的 Node 编排。
import {
  appendFile,
  copyFile,
  lstat,
  mkdir,
  readFile,
  readdir,
  writeFile,
} from "node:fs/promises";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";
import { sha256 } from "./build-utils.ts";
import {
  manifestName,
  packageNames,
  platforms,
  type ReleasePlatform,
} from "./release-sign.ts";

const repo = resolve(dirname(fileURLToPath(import.meta.url)), "..");

export function releaseNames(
  version: string,
  platform: ReleasePlatform,
): string[] {
  return [
    ...packageNames(version, platform).map((asset) => asset.name),
    ...(platform === "macos-arm64" ? ["QScope-macos-arm64.dmg"] : []),
  ];
}

async function regular(path: string) {
  const info = await lstat(path);
  if (!info.isFile() || info.size <= 0 || info.size > 1024 ** 3)
    throw new Error(`Invalid release file ${basename(path)}`);
  return info;
}

async function checksumText(
  directory: string,
  names: string[],
): Promise<string> {
  return (
    (
      await Promise.all(
        [...names]
          .sort()
          .map(
            async (name) => `${await sha256(join(directory, name))}  ${name}`,
          ),
      )
    ).join("\n") + "\n"
  );
}

export async function collectRelease(
  root: string,
  destination: string,
  platform: ReleasePlatform,
): Promise<void> {
  const reference = JSON.parse(
    await readFile(join(root, "build/desktop/current.json"), "utf8"),
  ) as { manifest: string };
  const result = JSON.parse(
    await readFile(join(dirname(reference.manifest), "result.json"), "utf8"),
  ) as {
    version: string;
    target: string;
    assets: { path: string; bytes: number; sha256: string }[];
  };
  const expectedTarget =
    platform === "macos-arm64" ? "darwin-arm64" : "win32-x64";
  if (result.target !== expectedTarget)
    throw new Error(`Expected ${expectedTarget} build results`);
  const names = releaseNames(result.version, platform);
  if (result.assets.length !== names.length)
    throw new Error("Incomplete platform release assets");
  for (const name of names) {
    const matches = result.assets.filter(
      (asset) => basename(asset.path) === name,
    );
    if (matches.length !== 1)
      throw new Error(`Missing or duplicate platform asset ${name}`);
    const asset = matches[0];
    const info = await regular(asset.path);
    if (
      info.size !== asset.bytes ||
      (await sha256(asset.path)) !== asset.sha256
    )
      throw new Error(`Build result hash mismatch: ${name}`);
  }
  await mkdir(destination, { recursive: true });
  for (const asset of result.assets)
    await copyFile(asset.path, join(destination, basename(asset.path)));
  await writeFile(
    join(destination, `SHA256SUMS.${platform}.txt`),
    `# qscope ${result.version} ${platform}\n${await checksumText(destination, names)}`,
  );
}

export async function assembleRelease(
  input: string,
  destination: string,
  version: string,
): Promise<void> {
  const files: { source: string; name: string }[] = [];
  for (const platform of platforms) {
    const directory = join(input, `qscope-release-${platform}`);
    const names = releaseNames(version, platform);
    const checksum = `SHA256SUMS.${platform}.txt`;
    const entries = await readdir(directory);
    if (
      entries.length !== names.length + 1 ||
      entries.some((name) => !names.includes(name) && name !== checksum)
    )
      throw new Error(`Unexpected ${platform} artifact contents`);
    await regular(join(directory, checksum));
    const lines = (await readFile(join(directory, checksum), "utf8"))
      .trim()
      .split("\n");
    if (lines.shift() !== `# qscope ${version} ${platform}`)
      throw new Error(`Platform artifact version mismatch: ${platform}`);
    const hashes = new Map<string, string>();
    for (const line of lines) {
      const match = /^([a-f0-9]{64}) {2}([A-Za-z0-9._-]+)$/.exec(line);
      if (!match || hashes.has(match[2]))
        throw new Error(`Invalid ${platform} checksum list`);
      hashes.set(match[2], match[1]);
    }
    if (hashes.size !== names.length)
      throw new Error(`Incomplete ${platform} checksums`);
    for (const name of names) {
      const source = join(directory, name);
      await regular(source);
      if ((await sha256(source)) !== hashes.get(name))
        throw new Error(`Downloaded artifact hash mismatch: ${name}`);
      files.push({ source, name });
    }
  }
  await mkdir(destination, { recursive: true });
  for (const file of files)
    await copyFile(file.source, join(destination, file.name));
}

export async function finalChecksums(
  directory: string,
  version: string,
): Promise<void> {
  const names = platforms.flatMap((platform) => [
    ...releaseNames(version, platform),
    manifestName(platform),
    manifestName(platform) + ".sig",
  ]);
  const actual = await readdir(directory);
  if (actual.some((name) => name !== "SHA256SUMS.txt" && !names.includes(name)))
    throw new Error("Unexpected aggregate release file");
  for (const name of names) await regular(join(directory, name));
  await writeFile(
    join(directory, "SHA256SUMS.txt"),
    await checksumText(directory, names),
  );
}

async function workspaceVersion(): Promise<string> {
  const { version } = JSON.parse(
    await readFile(join(repo, "apps/web/package.json"), "utf8"),
  ) as { version: string };
  packageNames(version, "windows-x64");
  if (
    process.env.GITHUB_REF_TYPE === "tag" &&
    process.env.GITHUB_REF_NAME !== `v${version}`
  )
    throw new Error("Release tag differs from the workspace version");
  return version;
}

if (
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  const [action, ...args] = process.argv.slice(2);
  const { values } = parseArgs({
    args,
    options: {
      out: { type: "string" },
      input: { type: "string" },
      dir: { type: "string" },
      platform: { type: "string" },
    },
  });
  const version = await workspaceVersion();
  if (action === "version") {
    console.log(version);
    if (process.env.GITHUB_OUTPUT)
      await appendFile(process.env.GITHUB_OUTPUT, `version=${version}\n`);
  } else if (
    action === "collect" &&
    values.out &&
    platforms.includes(values.platform as ReleasePlatform)
  )
    await collectRelease(
      repo,
      resolve(values.out),
      values.platform as ReleasePlatform,
    );
  else if (action === "assemble" && values.input && values.out)
    await assembleRelease(resolve(values.input), resolve(values.out), version);
  else if (action === "checksums" && values.dir)
    await finalChecksums(resolve(values.dir), version);
  else
    throw new Error(
      "Use version | collect --platform PLATFORM --out DIR | assemble --input DIR --out DIR | checksums --dir DIR",
    );
}
