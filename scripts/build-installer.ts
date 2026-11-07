import { mkdir, stat } from "node:fs/promises";
import { join } from "node:path";
import { command, download, json, sha256 } from "./build-utils.ts";

export async function buildInstaller(
  repo: string,
  product: string,
  output: string,
  version: string,
) {
  if (process.platform !== "win32")
    throw new Error("Inno Setup targets Windows");
  const pins = await json<{
    inno: {
      version: string;
      url: string;
      sha256: string;
      compiler_sha256: string;
    };
    webview2_bootstrapper: { url: string; sha256: string };
  }>(join(repo, "apps/desktop/runtimes.json"));
  const tools = join(repo, ".cache/dependencies/inno-setup", pins.inno.version);
  const archive = join(tools, "compiler.7z");
  await download(pins.inno.url, archive, pins.inno.sha256);
  const extracted = join(tools, "extracted");
  const compiler = join(extracted, "app/ISCC.exe");
  try {
    await stat(compiler);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
    await mkdir(extracted, { recursive: true });
    await command("tar", ["-xf", archive, "-C", extracted]);
  }
  if ((await sha256(compiler)) !== pins.inno.compiler_sha256)
    throw new Error("Inno Setup compiler identity changed");
  const webview = join(
    repo,
    ".cache/dependencies/webview2/MicrosoftEdgeWebview2Setup.exe",
  );
  await download(
    pins.webview2_bootstrapper.url,
    webview,
    pins.webview2_bootstrapper.sha256,
  );
  await command(compiler, [
    "/Qp",
    "/DProductVersion=" + version,
    "/DRepository=" + repo,
    "/DProductDirectory=" + product,
    "/DOutputDirectory=" + output,
    "/DWebViewBootstrapper=" + webview,
    join(repo, "apps/desktop/installer/qscope.iss"),
  ]);
  return join(output, "QScope-windows-x64-setup.exe");
}
