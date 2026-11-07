import { command } from "./build-utils.ts";
import { buildNative } from "./build-native.ts";
import { fileURLToPath } from "node:url";

const target = `${process.platform}-${process.arch}`;
if (process.argv.includes("--help")) {
  console.log(
    "pnpm build:native\n支持 Windows x64 与 macOS ARM64，本机工具链构建。",
  );
} else if (process.argv.length > 2) {
  throw new Error("采集器构建不接受额外参数");
} else if (target === "win32-x64") {
  console.log(await buildNative());
} else if (target === "darwin-arm64") {
  await command(
    process.execPath,
    [fileURLToPath(new URL("./build-native-macos.ts", import.meta.url))],
    {
      cwd: import.meta.dirname,
    },
  );
} else {
  throw new Error(`当前平台尚无采集器适配 ${target}`);
}
