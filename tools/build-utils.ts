import { spawn } from "node:child_process";
import { createHash, randomUUID } from "node:crypto";
import { createReadStream, createWriteStream } from "node:fs";
import { mkdir, readFile, rename, rm, stat, writeFile } from "node:fs/promises";
import { delimiter, dirname, join } from "node:path";
import { Readable } from "node:stream";
import { finished, pipeline } from "node:stream/promises";

export async function sha256(path: string): Promise<string> {
  const hash = createHash("sha256");
  for await (const bytes of createReadStream(path)) hash.update(bytes);
  return hash.digest("hex");
}

export async function json<T>(path: string): Promise<T> {
  return JSON.parse(await readFile(path, "utf8")) as T;
}

export async function writeJson(path: string, value: unknown): Promise<void> {
  await mkdir(dirname(path), { recursive: true });
  const temporary = path + "." + randomUUID() + ".writing";
  try {
    await writeFile(temporary, JSON.stringify(value, null, 2));
    await rename(temporary, path);
  } finally {
    await rm(temporary, { force: true });
  }
}

interface CommandOptions {
  cwd?: string;
  env?: Record<string, string>;
  capture?: boolean;
  stdoutFile?: string;
  timeout?: number;
}

export async function command(
  program: string,
  args: string[],
  options: CommandOptions = {},
): Promise<string> {
  if (options.stdoutFile)
    await mkdir(dirname(options.stdoutFile), { recursive: true });
  const output = options.stdoutFile
    ? createWriteStream(options.stdoutFile, { flags: "wx" })
    : undefined;
  try {
    return await new Promise<string>((done, reject) => {
      const child = spawn(program, args, {
        cwd: options.cwd,
        env: { ...process.env, ...options.env },
        windowsHide: true,
        detached: process.platform !== "win32",
        stdio: [
          "ignore",
          options.capture || output ? "pipe" : "inherit",
          options.capture ? "pipe" : "inherit",
        ],
      });
      let stdout = "",
        stderr = "";
      const terminate = () => {
        if (child.exitCode !== null || child.signalCode !== null || !child.pid)
          return;
        if (process.platform === "win32") {
          const killer = spawn(
            join(
              process.env.SystemRoot || "C:/Windows",
              "System32/taskkill.exe",
            ),
            ["/PID", String(child.pid), "/T", "/F"],
            { windowsHide: true, stdio: "ignore" },
          );
          killer.once("error", () => child.kill());
        } else {
          try {
            process.kill(-child.pid, "SIGKILL");
          } catch (error) {
            if ((error as NodeJS.ErrnoException).code !== "ESRCH") throw error;
          }
        }
      };
      const timer = setTimeout(() => {
        terminate();
        reject(new Error(`${program} exceeded its build deadline`));
      }, options.timeout || 900_000);
      output?.once("error", (error) => {
        clearTimeout(timer);
        terminate();
        reject(error);
      });
      if (output) child.stdout!.pipe(output);
      else
        child.stdout?.on("data", (bytes) => {
          stdout += bytes.toString();
          if (stdout.length > 65536) terminate();
        });
      child.stderr?.on("data", (bytes) => {
        stderr += bytes.toString();
        if (stderr.length > 65536) terminate();
      });
      child.once("error", (error) => {
        clearTimeout(timer);
        reject(error);
      });
      child.once("close", (code, signal) => {
        clearTimeout(timer);
        if (code === 0) done(stdout.trim());
        else
          reject(
            new Error(`${program} exited with ${code ?? signal}\n${stderr}`),
          );
      });
    });
  } finally {
    if (output) {
      output.end();
      await finished(output);
    }
  }
}

export async function download(
  url: string,
  path: string,
  expectedHash: string,
): Promise<void> {
  try {
    if ((await sha256(path)) !== expectedHash)
      throw new Error(`Cached build dependency hash changed (${path})`);
    return;
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
  }
  await mkdir(dirname(path), { recursive: true });
  const temporary = path + "." + randomUUID() + ".partial";
  try {
    const response = await fetch(url, { signal: AbortSignal.timeout(600_000) });
    if (!response.ok || !response.body)
      throw new Error(
        `Build dependency download failed with HTTP ${response.status}`,
      );
    await pipeline(
      Readable.fromWeb(response.body),
      createWriteStream(temporary, { flags: "wx" }),
    );
    if ((await sha256(temporary)) !== expectedHash)
      throw new Error("Build dependency SHA256 mismatch");
    await rename(temporary, path);
  } finally {
    await rm(temporary, { force: true });
  }
}

export async function pnpm(
  args: string[],
  cwd: string,
  env: Record<string, string> = {},
): Promise<void> {
  if (process.platform !== "win32") {
    await command("pnpm", args, { cwd, env });
    return;
  }
  for (const directory of (process.env.PATH || "").split(delimiter)) {
    for (const entry of [
      "pnpm.exe",
      "node_modules/pnpm/bin/pnpm.cjs",
      "node_modules/corepack/dist/pnpm.js",
    ]) {
      const path = join(directory, entry);
      try {
        if (!(await stat(path)).isFile()) continue;
      } catch (error) {
        if (
          ["ENOENT", "EISDIR"].includes(
            (error as NodeJS.ErrnoException).code || "",
          )
        )
          continue;
        throw error;
      }
      await command(
        entry.endsWith(".exe") ? path : process.execPath,
        entry.endsWith(".exe") ? args : [path, ...args],
        { cwd, env },
      );
      return;
    }
  }
  throw new Error(
    "Install the pinned pnpm 9.15.9 build tool before packaging the desktop app",
  );
}
