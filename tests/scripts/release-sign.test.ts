import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { createPublicKey, generateKeyPairSync, verify } from "node:crypto";
import {
  access,
  copyFile,
  mkdir,
  mkdtemp,
  readFile,
  realpath,
  rm,
  symlink,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import test, { type TestContext } from "node:test";
import { promisify } from "node:util";
import { sha256 } from "../../scripts/build-utils.ts";
import {
  manifestName,
  packageNames,
  platforms,
  type ReleasePlatform,
} from "../../scripts/release-sign.ts";
import {
  assembleRelease,
  collectRelease,
  finalChecksums,
  releaseNames,
} from "../../scripts/release-workflow.ts";

const execute = promisify(execFile);
const version = "0.2.0";

test("release filenames stay fixed across versions on both platforms", () => {
  const expected = [
    "QScope-windows-x64-setup.exe",
    "QScope-windows-x64.zip",
    "QScope-windows-x64-cli-webui.zip",
    "QScope-macos-arm64.zip",
    "QScope-macos-arm64-cli-webui.zip",
    "QScope-macos-arm64.dmg",
  ];
  for (const value of ["0.2.0", "1.0.0-beta.1"])
    assert.deepEqual(
      platforms.flatMap((platform) => releaseNames(value, platform)),
      expected,
    );
});

async function fixture(t: TestContext) {
  const root = await mkdtemp(
    join(await realpath(tmpdir()), "qscope-release-test-"),
  );
  t.after(() => rm(root, { recursive: true, force: true }));
  const tools = join(root, "scripts");
  await mkdir(tools);
  for (const name of ["release-sign.ts", "build-utils.ts"])
    await copyFile(
      join(import.meta.dirname, "../../scripts", name),
      join(tools, name),
    );
  await writeFile(
    join(root, "package.json"),
    JSON.stringify({ type: "module" }),
  );
  const { privateKey, publicKey } = generateKeyPairSync("ed25519");
  const key = join(root, "synthetic-private.pem");
  await writeFile(key, privateKey.export({ format: "pem", type: "pkcs8" }), {
    mode: 0o600,
  });
  const source = join(root, "crates/qscope-automation/src/update.rs");
  await mkdir(dirname(source), { recursive: true });
  const publicBytes = publicKey
    .export({ format: "der", type: "spki" })
    .subarray(-32)
    .toString("base64");
  await writeFile(source, `pub const RELEASE_KEY: &str = "${publicBytes}";\n`);
  const directory = join(root, "release");
  await mkdir(directory);
  const env = { ...process.env };
  delete env.QSCOPE_RELEASE_SIGNING_KEY;
  delete env.QSCOPE_RELEASE_SIGNING_KEY_FILE;
  return {
    root,
    key,
    publicKey,
    source,
    directory,
    sign: async (platform: string, keyFile: string | null = key) => {
      return execute(
        process.execPath,
        [
          join(tools, "release-sign.ts"),
          "sign",
          "--dir",
          directory,
          "--version",
          version,
          "--platform",
          platform,
          ...(keyFile ? ["--key", keyFile] : []),
        ],
        { cwd: root, env, timeout: 10000 },
      );
    },
  };
}

async function packages(directory: string, platform: ReleasePlatform) {
  for (const name of releaseNames(version, platform))
    await writeFile(join(directory, name), `synthetic ${name}\n`);
}

test("Windows manifest bytes stay v1-compatible while Mac app/CLI assets are independently signed", async (t) => {
  const f = await fixture(t);
  for (const platform of platforms) await packages(f.directory, platform);
  await f.sign("all");
  for (const platform of platforms) {
    const name = manifestName(platform);
    const bytes = await readFile(join(f.directory, name));
    const signature = Buffer.from(
      (await readFile(join(f.directory, name + ".sig"), "utf8")).trim(),
      "base64",
    );
    assert.equal(signature.length, 64);
    assert.ok(verify(null, bytes, f.publicKey, signature));
    const manifest = JSON.parse(bytes.toString());
    const assets = [];
    for (const asset of packageNames(version, platform)) {
      const file = await readFile(join(f.directory, asset.name));
      assets.push({
        ...asset,
        bytes: file.length,
        sha256: await sha256(join(f.directory, asset.name)),
      });
    }
    const expected = {
      schema_version: 1,
      product: "qscope",
      version,
      notes_url: `https://github.com/sj817/QScope/releases/tag/v${version}`,
      assets,
    };
    assert.equal(bytes.toString(), JSON.stringify(expected, null, 2) + "\n");
    assert.deepEqual(
      manifest.assets.map((asset: { kind: string }) => asset.kind),
      platform === "windows-x64"
        ? ["installer", "portable", "cli"]
        : ["app", "cli"],
    );
    assert.ok(
      !manifest.assets.some((asset: { name: string }) =>
        asset.name.endsWith(".dmg"),
      ),
    );
    assert.ok(
      !verify(
        null,
        Buffer.concat([bytes, Buffer.from(" ")]),
        f.publicKey,
        signature,
      ),
    );
  }
});

test("a mismatched key fails before replacing either manifest or signature", async (t) => {
  const f = await fixture(t);
  await packages(f.directory, "macos-arm64");
  const name = manifestName("macos-arm64");
  await writeFile(join(f.directory, name), "previous manifest");
  await writeFile(join(f.directory, name + ".sig"), "previous signature");
  const wrong = generateKeyPairSync("ed25519").privateKey;
  const key = join(f.root, "wrong.pem");
  await writeFile(key, wrong.export({ format: "pem", type: "pkcs8" }), {
    mode: 0o600,
  });
  await assert.rejects(
    f.sign("macos-arm64", key),
    /does not match RELEASE_KEY/,
  );
  assert.equal(
    await readFile(join(f.directory, name), "utf8"),
    "previous manifest",
  );
  assert.equal(
    await readFile(join(f.directory, name + ".sig"), "utf8"),
    "previous signature",
  );
});

test("unsigned output removes a stale signature and never borrows the other platform signature", async (t) => {
  const f = await fixture(t);
  for (const platform of platforms) await packages(f.directory, platform);
  await f.sign("all");
  await assert.rejects(f.sign("windows-x64", null), /No signing key/);
  await assert.rejects(access(join(f.directory, "qscope-update.json.sig")), {
    code: "ENOENT",
  });
  const signature = Buffer.from(
    (
      await readFile(
        join(f.directory, "qscope-update-macos-arm64.json.sig"),
        "utf8",
      )
    ).trim(),
    "base64",
  );
  assert.ok(
    verify(
      null,
      await readFile(join(f.directory, "qscope-update-macos-arm64.json")),
      createPublicKey(await readFile(f.key)),
      signature,
    ),
  );
});

test("signing rejects empty assets and linked files", async (t) => {
  const f = await fixture(t);
  const name = packageNames(version, "macos-arm64")[0].name;
  await writeFile(join(f.directory, name), "");
  await assert.rejects(f.sign("macos-arm64"), /Invalid release package/);
  if (process.platform !== "win32") {
    await rm(join(f.directory, name));
    const external = join(f.root, "external");
    await writeFile(external, "synthetic");
    await symlink(external, join(f.directory, name));
    await assert.rejects(f.sign("macos-arm64"), /Invalid release package/);
  }
});

test("workflow collection verifies both platform builds and produces one complete signed release", async (t) => {
  const f = await fixture(t);
  const input = join(f.root, "input");
  await mkdir(join(f.root, "build/desktop"), { recursive: true });
  for (const platform of platforms) {
    const build = join(f.root, "build", platform);
    await mkdir(build);
    await packages(build, platform);
    const assets = [];
    for (const name of releaseNames(version, platform)) {
      const path = join(build, name);
      assets.push({
        path,
        bytes: (await readFile(path)).length,
        sha256: await sha256(path),
      });
    }
    await writeFile(
      join(build, "result.json"),
      JSON.stringify({
        version,
        target: platform === "macos-arm64" ? "darwin-arm64" : "win32-x64",
        assets,
      }),
    );
    await writeFile(
      join(f.root, "build/desktop/current.json"),
      JSON.stringify({ manifest: join(build, "manifest.json") }),
    );
    await collectRelease(
      f.root,
      join(input, `qscope-release-${platform}`),
      platform,
    );
  }
  await assembleRelease(input, f.directory, version);
  await assert.rejects(
    assembleRelease(input, join(f.root, "wrong-version"), "0.3.0"),
    /version mismatch/,
  );
  await assert.rejects(access(join(f.root, "wrong-version")), {
    code: "ENOENT",
  });
  await f.sign("all");
  await finalChecksums(f.directory, version);
  const hashes = (await readFile(join(f.directory, "SHA256SUMS.txt"), "utf8"))
    .trim()
    .split("\n");
  assert.equal(hashes.length, 10);
  for (const line of hashes) {
    const [hash, name] = line.split("  ");
    assert.equal(await sha256(join(f.directory, name)), hash);
  }
  await writeFile(
    join(
      input,
      "qscope-release-macos-arm64",
      releaseNames(version, "macos-arm64")[0],
    ),
    "changed",
  );
  await assert.rejects(
    assembleRelease(input, join(f.root, "rejected"), version),
    /hash mismatch/,
  );
  await assert.rejects(access(join(f.root, "rejected")), { code: "ENOENT" });
});
