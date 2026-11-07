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
  ".npmrc",
  ".env.example",
]);
const allowedDirectories = new Set([
  ".github",
  "apps",
  "crates",
  "collectors",
  "schemas",
  "tests",
  "scripts",
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
const skillMetadata = JSON.parse(
  await readFile(
    join(root, "crates/qscope-automation/skills/qscope/skill.json"),
    "utf8",
  ),
) as { version?: unknown };
if (
  typeof skillMetadata.version !== "string" ||
  !/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(skillMetadata.version)
)
  failures.push("产品技能必须声明有效的语义版本号");
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
  if (path.endsWith("pnpm-lock.yaml") && path !== "pnpm-lock.yaml")
    failures.push(`子项目必须使用根工作区锁文件 ${path}`);
  if (parts[0] !== "tests" && parts.slice(1).includes("tests"))
    failures.push(`集成测试及验收脚本统一放入 tests/ ${path}`);
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
  "pnpm-workspace.yaml",
  "pnpm-lock.yaml",
  "tests/package.json",
  "tests/tsconfig.json",
  "eslint.config.js",
  "scripts/check-repo.ts",
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
  // 检查 Markdown 链接及 README 截图使用的 HTML 图片路径。
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
