import { crc32, deflateRawSync } from "node:zlib";
import { open, readFile, rename, rm } from "node:fs/promises";
import { createHash, randomUUID } from "node:crypto";
import { join } from "node:path";
import { sha256 } from "./build-utils.ts";

export interface ResourceFile {
  path: string;
  bytes: number;
  sha256: string;
  mode?: number;
}

// 生成确定性的标准 ZIP，消费端使用 Rust ZIP 读取器。
export async function packResources(
  directory: string,
  files: ResourceFile[],
  version: string,
  target: string,
  destination: string,
) {
  const manifest = Buffer.from(
    JSON.stringify({
      schema_version: 1,
      owner: "qscope-resources",
      version,
      target,
      files,
    }),
  );
  return writeZip(directory, files, destination, manifest);
}

export async function writeZip(
  directory: string,
  files: ResourceFile[],
  destination: string,
  manifest?: Buffer,
) {
  const partial = destination + "." + randomUUID() + ".writing";
  const output = await open(partial, "wx");
  const central: Buffer[] = [];
  let position = 0;
  try {
    async function entry(name: string, bytes: Buffer, mode = 0o100644) {
      const encoded = Buffer.from(name, "utf8");
      const compressed = deflateRawSync(bytes, { level: 9 });
      const crc = crc32(bytes);
      const header = Buffer.alloc(30);
      header.writeUInt32LE(0x04034b50, 0);
      header.writeUInt16LE(20, 4);
      header.writeUInt16LE(0x800, 6);
      header.writeUInt16LE(8, 8);
      header.writeUInt16LE(0x21, 12);
      header.writeUInt32LE(crc, 14);
      header.writeUInt32LE(compressed.length, 18);
      header.writeUInt32LE(bytes.length, 22);
      header.writeUInt16LE(encoded.length, 26);
      const index = Buffer.alloc(46);
      index.writeUInt32LE(0x02014b50, 0);
      index.writeUInt16LE(0x0314, 4);
      header.copy(index, 6, 4, 30);
      index.writeUInt32LE(position, 42);
      index.writeUInt32LE(mode * 0x10000, 38);
      central.push(Buffer.concat([index, encoded]));
      await output.writeFile(header);
      await output.writeFile(encoded);
      await output.writeFile(compressed);
      position += header.length + encoded.length + compressed.length;
    }
    if (manifest) await entry("manifest.json", manifest);
    let expanded = 0;
    for (const file of files) {
      const bytes = await readFile(join(directory, file.path));
      expanded += bytes.length;
      if (
        bytes.length !== file.bytes ||
        bytes.length > 64 * 1024 ** 2 ||
        expanded > 512 * 1024 ** 2 ||
        createHash("sha256").update(bytes).digest("hex") !== file.sha256
      )
        throw new Error("Resource changed during packaging: " + file.path);
      await entry(file.path, bytes, file.mode);
    }
    if (central.length > 20001 || position > 256 * 1024 ** 2)
      throw new Error("Resource archive limit exceeded");
    const index = Buffer.concat(central);
    await output.writeFile(index);
    const end = Buffer.alloc(22);
    end.writeUInt32LE(0x06054b50, 0);
    end.writeUInt16LE(central.length, 8);
    end.writeUInt16LE(central.length, 10);
    end.writeUInt32LE(index.length, 12);
    end.writeUInt32LE(position, 16);
    await output.writeFile(end);
    await output.sync();
    await output.close();
    await rename(partial, destination);
    return {
      schema_version: 1,
      format: "zip",
      bytes: position + index.length + end.length,
      sha256: await sha256(destination),
    };
  } finally {
    await output.close().catch(() => {});
    await rm(partial, { force: true });
  }
}
