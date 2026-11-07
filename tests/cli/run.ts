import { join } from "node:path";
import { command } from "../../scripts/build-utils.ts";

if (!["win32", "darwin", "linux"].includes(process.platform)) {
  throw new Error("分发包验收支持 Windows x64、macOS ARM64 与 Linux");
}
const standalone =
  process.platform === "linux" || process.argv.includes("--cli-only");
await command(process.execPath, [
  join(
    import.meta.dirname,
    standalone
      ? "standalone.ts"
      : process.platform === "darwin"
        ? "packaged-macos.ts"
        : "packaged.ts",
  ),
  ...process.argv.slice(2),
]);
