// 在线更新签名以平台清单和 Ed25519 签名为依据，公钥定义在 crates/qscope-automation/src/update.rs 的 RELEASE_KEY。
// 通过 pnpm release:sign keygen --out PATH 生成密钥，通过 pnpm release:sign sign --dir RELEASE_DIR --version VERSION 签名。
// Windows 使用 qscope-update.json，Mac 使用 qscope-update-macos-arm64.json。
// 私钥通过 --key、QSCOPE_RELEASE_SIGNING_KEY 或 QSCOPE_RELEASE_SIGNING_KEY_FILE 提供。
import {
  createPrivateKey,
  createPublicKey,
  generateKeyPairSync,
  sign,
  verify,
  type KeyObject,
} from "node:crypto";
import { lstat, readFile, rm, stat, writeFile } from "node:fs/promises";
import { dirname, isAbsolute, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";
import { sha256 } from "./build-utils.ts";

const repo = resolve(dirname(fileURLToPath(import.meta.url)), "..");
export const repository = "sj817/QScope";
export type ReleasePlatform = "windows-x64" | "macos-arm64";
export const platforms: readonly ReleasePlatform[] = [
  "windows-x64",
  "macos-arm64",
];
const packages = {
  "windows-x64": [
    ["installer", "windows-x64-setup.exe"],
    ["portable", "windows-x64.zip"],
    ["cli", "windows-x64-cli-webui.zip"],
  ],
  "macos-arm64": [
    ["app", "macos-arm64.zip"],
    ["cli", "macos-arm64-cli-webui.zip"],
  ],
} as const;

export function manifestName(platform: ReleasePlatform): string {
  return platform === "windows-x64"
    ? "qscope-update.json"
    : "qscope-update-macos-arm64.json";
}

export function packageNames(version: string, platform: ReleasePlatform) {
  if (!/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(version))
    throw new Error("Invalid release version");
  return packages[platform].map(([kind, suffix]) => ({
    kind,
    name: `QScope-${suffix}`,
  }));
}

function rawPublicKey(key: KeyObject): string {
  const der = (key.type === "public" ? key : createPublicKey(key)).export({
    format: "der",
    type: "spki",
  });
  return der.subarray(der.length - 32).toString("base64");
}

async function trustedKey(): Promise<string> {
  const source = await readFile(
    join(repo, "crates/qscope-automation/src/update.rs"),
    "utf8",
  );
  return /pub const RELEASE_KEY: &str = "([^"]*)";/.exec(source)?.[1] ?? "";
}

async function privateKey(path?: string): Promise<KeyObject | undefined> {
  const file = path ?? process.env.QSCOPE_RELEASE_SIGNING_KEY_FILE;
  const pem = file
    ? await readFile(file, "utf8")
    : process.env.QSCOPE_RELEASE_SIGNING_KEY;
  if (!pem) return undefined;
  const key = createPrivateKey(pem);
  if (key.asymmetricKeyType !== "ed25519")
    throw new Error("The release signing key must be Ed25519");
  return key;
}

// 根据 directory 中的分发包生成清单，提供密钥时同时写入签名。
export async function signRelease(
  directory: string,
  version: string,
  keyPath?: string,
  platform: ReleasePlatform = "windows-x64",
): Promise<{ manifest: string; signed: boolean }> {
  if (!/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(version))
    throw new Error("Invalid release version");
  const assets = [];
  for (const { kind, name } of packageNames(version, platform)) {
    const path = join(directory, name);
    const info = await lstat(path).catch((error: NodeJS.ErrnoException) => {
      if (error.code === "ENOENT") return undefined;
      throw error;
    });
    if (info) {
      if (
        !info.isFile() ||
        info.size <= 0 ||
        info.size > 1024 ** 3 ||
        name.length > 128
      )
        throw new Error(`Invalid release package ${name}`);
      assets.push({
        kind,
        name,
        bytes: info.size,
        sha256: await sha256(path),
      });
    }
  }
  if (!assets.length)
    throw new Error(`No release packages for ${version} in ${directory}`);
  const manifest = {
    schema_version: 1,
    product: "qscope",
    version,
    notes_url: `https://github.com/${repository}/releases/tag/v${version}`,
    assets,
  };
  const bytes = Buffer.from(JSON.stringify(manifest, null, 2) + "\n");
  const manifestPath = join(directory, manifestName(platform));
  const key = await privateKey(keyPath);
  if (!key) {
    await writeFile(manifestPath, bytes);
    // 新清单缺少签名时必须清除旧签名，避免形成错误配对。
    await rm(manifestPath + ".sig", { force: true });
    return { manifest: manifestPath, signed: false };
  }
  const trusted = await trustedKey();
  if (!trusted || trusted !== rawPublicKey(key))
    throw new Error("The signing key does not match RELEASE_KEY in update.rs");
  const signature = sign(null, bytes, key);
  if (!verify(null, bytes, createPublicKey(key), signature))
    throw new Error("Release signature self-check failed");
  await writeFile(manifestPath, bytes);
  await writeFile(manifestPath + ".sig", signature.toString("base64") + "\n");
  return { manifest: manifestPath, signed: true };
}

if (
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  const [action, ...rest] = process.argv.slice(2);
  const { values } = parseArgs({
    args: rest,
    options: {
      out: { type: "string" },
      dir: { type: "string" },
      version: { type: "string" },
      key: { type: "string" },
      platform: { type: "string", default: "windows-x64" },
    },
  });
  if (action === "keygen") {
    if (!values.out)
      throw new Error("Use keygen --out PATH outside the repository");
    const out = resolve(values.out);
    const inside = relative(repo, out);
    if (!inside.startsWith("..") && !isAbsolute(inside))
      throw new Error("Keep the private release key outside the repository");
    if (await stat(out).catch(() => undefined))
      throw new Error(`${out} already exists`);
    const { privateKey: secret, publicKey } = generateKeyPairSync("ed25519");
    await writeFile(out, secret.export({ format: "pem", type: "pkcs8" }), {
      mode: 0o600,
      flag: "wx",
    });
    console.log(`Private key: ${out}`);
    console.log(
      "Store its PEM text as the GitHub Actions secret QSCOPE_RELEASE_SIGNING_KEY and keep a private backup.",
    );
    console.log(
      `Public key for RELEASE_KEY in crates/qscope-automation/src/update.rs:\n${rawPublicKey(publicKey)}`,
    );
  } else if (action === "sign") {
    if (!values.dir || !values.version)
      throw new Error("Use sign --dir RELEASE_DIR --version VERSION");
    const selected =
      values.platform === "all"
        ? platforms
        : platforms.filter((platform) => platform === values.platform);
    if (!selected.length)
      throw new Error("Use --platform windows-x64, macos-arm64 or all");
    for (const platform of selected) {
      const result = await signRelease(
        resolve(values.dir),
        values.version,
        values.key,
        platform,
      );
      if (!result.signed)
        throw new Error(
          "No signing key: set QSCOPE_RELEASE_SIGNING_KEY or pass --key",
        );
      console.log(`Signed ${result.manifest}`);
    }
  } else {
    console.log(
      "pnpm release:sign keygen --out PATH\npnpm release:sign sign --dir RELEASE_DIR --version VERSION [--key PATH] [--platform windows-x64|macos-arm64|all]",
    );
    process.exit(action ? 1 : 0);
  }
}
