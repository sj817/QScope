// Release signing for online updates. The desktop and CLI trust a release only through
// qscope-update.json and its Ed25519 signature, checked against RELEASE_KEY in
// crates/qscope-automation/src/update.rs.
//
//   node tools/release-sign.ts keygen --out PATH      create a key pair; prints the public key
//   node tools/release-sign.ts sign --dir RELEASE_DIR --version VERSION [--key PATH]
//
// `sign` writes qscope-update.json and qscope-update.json.sig beside the release packages.
// The private key comes from --key, QSCOPE_RELEASE_SIGNING_KEY (PEM text) or
// QSCOPE_RELEASE_SIGNING_KEY_FILE.
import {
  createPrivateKey,
  createPublicKey,
  generateKeyPairSync,
  sign,
  verify,
  type KeyObject,
} from "node:crypto";
import { readFile, stat, writeFile } from "node:fs/promises";
import { dirname, isAbsolute, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseArgs } from "node:util";
import { sha256 } from "./build-utils.ts";

const repo = resolve(dirname(fileURLToPath(import.meta.url)), "..");
export const repository = "sj817/QScope";
const packages = [
  ["installer", (version: string) => `QScope-${version}-windows-x64-setup.exe`],
  [
    "portable",
    (version: string) => `QScope-${version}-windows-x64-portable.zip`,
  ],
  ["cli", (version: string) => `QScope-${version}-windows-x64-cli-webui.zip`],
] as const;

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

// Writes the manifest for the packages present in `directory`, and its signature when a key is available.
export async function signRelease(
  directory: string,
  version: string,
  keyPath?: string,
): Promise<{ manifest: string; signed: boolean }> {
  if (!/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(version))
    throw new Error("Invalid release version");
  const assets = [];
  for (const [kind, name] of packages) {
    const path = join(directory, name(version));
    const info = await stat(path).catch(() => undefined);
    if (info?.isFile())
      assets.push({
        kind,
        name: name(version),
        bytes: info.size,
        sha256: await sha256(path),
      });
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
  const manifestPath = join(directory, "qscope-update.json");
  await writeFile(manifestPath, bytes);
  const key = await privateKey(keyPath);
  if (!key) return { manifest: manifestPath, signed: false };
  const trusted = await trustedKey();
  if (trusted && trusted !== rawPublicKey(key))
    throw new Error("The signing key does not match RELEASE_KEY in update.rs");
  const signature = sign(null, bytes, key);
  if (!verify(null, bytes, createPublicKey(key), signature))
    throw new Error("Release signature self-check failed");
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
    const result = await signRelease(
      resolve(values.dir),
      values.version,
      values.key,
    );
    if (!result.signed)
      throw new Error(
        "No signing key: set QSCOPE_RELEASE_SIGNING_KEY or pass --key",
      );
    console.log(`Signed ${result.manifest}`);
  } else {
    console.log(
      "node tools/release-sign.ts keygen --out PATH\nnode tools/release-sign.ts sign --dir RELEASE_DIR --version VERSION [--key PATH]",
    );
    process.exit(action ? 1 : 0);
  }
}
