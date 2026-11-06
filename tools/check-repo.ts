import { access, readFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { execFileSync } from "node:child_process";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const allowedRootFiles = new Set([
  ".gitattributes",
  ".gitignore",
  ".editorconfig",
  "AGENTS.md",
  "README.md",
  "LICENSE",
  "CHANGELOG.md",
  "Cargo.toml",
  "Cargo.lock",
  "rust-toolchain.toml",
  "rustfmt.toml",
  "package.json",
  "pnpm-lock.yaml",
  "pnpm-workspace.yaml",
  "tsconfig.json",
  "eslint.config.js",
  ".prettierignore",
  ".env.example",
]);
const allowedDirectories = new Set([
  ".github",
  "apps",
  "crates",
  "collectors",
  "schemas",
  "tests",
  "tools",
]);
const markdown = new Set([
  "README.md",
  "AGENTS.md",
  "crates/qscope-automation/skills/qscope/SKILL.md",
]);
const raw = execFileSync(
  "git",
  [
    "-c",
    "core.quotepath=false",
    "ls-files",
    "--cached",
    "--others",
    "--exclude-standard",
    "-z",
  ],
  { cwd: root, encoding: "utf8", maxBuffer: 8 * 1024 ** 2 },
);
const paths = [...new Set(raw.split("\0").filter(Boolean))].sort();
const failures: string[] = [];
const localDirectory =
  /(^|\/)(\.tmp|artifacts|target|build|dist|node_modules|\.venv|__pycache__|coverage|\.cache|\.sites-runtime|\.playwright-cli|test-results|playwright-report)(\/|$)/i;
const binary =
  /\.(exe|dll|node|pdb|dmp|i64|idb|id0|id1|id2|nam|til|pcap|pcapng|qscap|run|trace64|sqlite|sqlite3|db)(-wal|-shm|-journal)?$/i;
for (const path of paths) {
  try {
    await access(join(root, path));
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") continue;
    throw error;
  }
  const parts = path.split("/");
  if (parts.length === 1 && !allowedRootFiles.has(path))
    failures.push(`Unregistered root file ${path}`);
  if (parts.length > 1 && !allowedDirectories.has(parts[0]))
    failures.push(`Unregistered root directory ${path}`);
  if (localDirectory.test(path))
    failures.push(`Generated directory must stay outside Git ${path}`);
  if (binary.test(path))
    failures.push(
      `Local binary, capture or database must stay outside Git ${path}`,
    );
  if (/\.(ps1|bat|cmd|sh|py)$/i.test(path))
    failures.push(
      `Use cross-platform TypeScript for repository scripts (${path})`,
    );
  if (/(^|\/)\.env($|\.)/i.test(path) && !/(^|\/)\.env\.example$/.test(path))
    failures.push(`Local environment file ${path}`);
  if (/^\.(codex|claude|agents|cursor)\//.test(path))
    failures.push(`Local AI tool files stay outside Git ${path}`);
  if (/\.(md|markdown)$/i.test(path) && !markdown.has(path))
    failures.push(`Unregistered Markdown ${path}`);
}
for (const path of [
  ".gitignore",
  ".editorconfig",
  "AGENTS.md",
  "README.md",
  "package.json",
  "eslint.config.js",
  "tools/check-repo.ts",
  ".github/workflows/repository.yml",
]) {
  try {
    await access(join(root, path));
  } catch {
    failures.push(`Missing repository entry point ${path}`);
  }
}
for (const path of markdown) {
  let content: string;
  try {
    content = await readFile(join(root, path), "utf8");
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") continue;
    throw error;
  }
  // Markdown links plus HTML image sources, which README screenshots use.
  const targets = [
    ...content.matchAll(/\[[^\]\r\n]+\]\(([^\s)]+)\)/g),
    ...content.matchAll(/\b(?:src|srcset)="([^"\s]+)"/g),
  ].map((match) => match[1]);
  for (const target of targets) {
    if (/^[a-zA-Z][a-zA-Z0-9+.-]*:/.test(target) || target.startsWith("#"))
      continue;
    const file = resolve(
      dirname(join(root, path)),
      decodeURIComponent(target.split(/[?#]/, 1)[0]),
    );
    try {
      await access(file);
    } catch {
      failures.push(`Broken local link in ${path} (${target})`);
    }
  }
}
if (failures.length) {
  for (const failure of failures) console.error(failure);
  process.exitCode = 1;
} else
  console.log(
    `Repository layout OK (${paths.length} candidate files, ${markdown.size} registered Markdown documents)`,
  );
