import { copyFile, mkdir } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { pnpm, sha256 } from "./build-utils.ts";

const repo = resolve(dirname(fileURLToPath(import.meta.url)), "..");

export async function buildIcons(): Promise<void> {
  const source = join(repo, "apps/web/public/favicon.svg");
  const output = join(repo, "build/icons", (await sha256(source)).slice(0, 12));
  const destination = join(repo, "apps/desktop/icons");
  await mkdir(output, { recursive: true });
  await pnpm(
    [
      "--dir",
      "apps/desktop",
      "exec",
      "tauri",
      "icon",
      source,
      "--output",
      output,
    ],
    repo,
  );
  await mkdir(destination, { recursive: true });
  for (const name of [
    "icon.ico",
    "32x32.png",
    "128x128.png",
    ...(process.platform === "darwin" ? ["icon.icns"] : []),
  ])
    await copyFile(join(output, name), join(destination, name));
  console.log(`App icons generated from ${source}`);
}

if (
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  if (process.argv.slice(2).includes("--help"))
    console.log(
      "node tools/build-icons.ts\nGenerate desktop PNG and ICO assets from the WebUI favicon SVG",
    );
  else if (process.argv.length > 2)
    throw new Error("Unknown icon build option");
  else await buildIcons();
}
