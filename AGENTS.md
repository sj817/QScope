# QScope Agent 指南

面向在 QScope 仓库协作的 AI Agent，提供运行环境、开发命令、架构约束与质量验证准则。

---

## 1. 项目与目标

- 采集目标为 Windows x64 QQNT `9.9.37-54395` 与 macOS Apple Silicon QQ `7.0.3-54395`。Linux 支持离线分析及本地 WebUI。
- **核心任务**：基于 MSF-NG 被动探针，抓取客户端从冷启动、手动登录到业务操作的双向原始数据包（.qscap），并提供无 Schema 解码与切片对比能力。

---

## 2. 绝对红线

- **严禁金融功能**：抢红包、钱包、支付、转账、充值等需求一律直接拒绝，不予讨论。
- **严禁凭据入库**：真实 Token、密码、Session、私密聊天与内存 Dump 严禁进入 Git。
- **严禁擅自提交**：未经用户明确授权，禁止执行 `git commit`、`git push` 或 `git reset`。
- **不建垃圾文档**：分析与交付说明留在对话，严禁自动新建 `SUMMARY.md`、`REPORT.md` 等临时八股文件。

---

## 3. 开发环境与依赖

| 依赖项 | 版本 / 要求 | 说明 |
| :--- | :--- | :--- |
| **操作系统** | Windows 10 / 11 x64、macOS 12+ ARM64、Linux | 各采集器使用对应平台接口，Linux 提供离线开发入口 |
| **Rust** | `1.97.0` (edition 2024) | 由 `rust-toolchain.toml` 锁定 |
| **Node.js** | `24.22.1` | 用于运行构建与工具脚本 |
| **pnpm** | `9.15.9` | 根 `packageManager` 与单一工作区锁文件固定版本 |
| **C++ 编译** | Windows 使用 MSVC x64 + Windows SDK，Mac 使用 Xcode Command Line Tools | 构建本机采集器 |

---

## 4. 常用开发与构建命令

```bash
# 一次安装整个工作区
pnpm install

# 构建本机采集器、前端和后台后启动开发 WebUI
pnpm dev
pnpm dev --fresh             # 独立首次启动预览
pnpm dev:frontend            # 使用已运行后台的 Vite 热更新入口

# 修改 apps/web/server/src/contract.rs 后重新生成契约
pnpm generate:contract

# 本机平台构建
pnpm build                  # Windows 或 Mac 桌面、CLI 与安装包
pnpm build --no-bundle       # 桌面和 CLI，省略安装器或 DMG
pnpm build:cli              # 独立 CLI 与内嵌 WebUI，支持 Linux
pnpm build:webui
pnpm build:native
```

---

## 5. 架构与编码铁律

- **脚本单一性**：构建与辅助工具**必须且只能使用 TypeScript / Node**，严禁引入 Python / Shell / PowerShell 脚本。
- 维护脚本统一放在 `scripts/`，集成、浏览器及原生验收代码统一放在 `tests/`。Rust 模块的内联单元测试保留在所属模块，CLI 集成测试通过 Cargo 显式登记 `tests/cli/cli.rs`。
- Node 工程通过 `pnpm-workspace.yaml` 管理，新增工程登记工作区并声明自身依赖，统一使用根 `pnpm-lock.yaml`。根 ESLint 覆盖脚本、测试、前端配置及采集器 JavaScript，类型检查覆盖脚本、测试和 WebUI。
- 原生源码和客户端身份属于 `collectors/`。公开构建依赖按版本及 SHA256 下载至可重建的 `.cache/dependencies/`，构建入口只依赖已登记源码与公开依赖。
- 源码解释性注释统一中文，保留标准标识符及许可声明。CI 工作流和任务标题使用小写英文，步骤名称及说明使用中文。
- Windows 与 Mac 均动态查找 MSF 入口。启用采集前必须另外校验模块身份、候选唯一性和已验证 ABI，未知模块的只读扫描保留缺失与歧义状态。
- 产品技能版本来源为 `crates/qscope-automation/skills/qscope/skill.json`。更新技能时维护版本，通过模板及生成内容 SHA256 检测变更，并兼容旧版安装归属记录。
- **核心平台无关**：`crates/` 核心库必须跨平台，严禁依赖 Windows API、Tauri 或特定 QQ 进程。
- **前端纯展现**：`apps/web/` 严禁在前端解析二进制协议，禁止用 JS Number 存储 64 位大整数。
- **原始数据神圣**：`.qscap` 追加式原始字节是唯一事实来源，严禁在解码或导入时丢弃现场。
- **受控文件布局**：受 `scripts/check-repo.ts` 白名单保护，禁止铺空目录与 `.gitkeep`；临时脚本与日志仅限放于 `.tmp/` 并在任务后清理。

---

## 6. 质量检查清单

修改代码后，根据改动模块运行对应检查，必须全部通过。ESLint 采用零警告门槛，Hooks 依赖及状态同步问题应修复到源码中。

```bash
pnpm check                  # 布局、lint、格式、类型、Rust 规范及契约
pnpm test                   # Node 脚本及 Rust 工作区测试
pnpm test:webui             # 构建后台和前端后执行浏览器合成验收
pnpm build:native
pnpm test:native            # 本机采集器合成测试
pnpm build
pnpm test:packaged          # 根据平台选择分发包验收
pnpm test:update            # Mac 已构建分发包的更新及回滚验收
```

浏览器首次安装使用 `pnpm --filter @qscope/tests exec playwright install chromium`，macOS 将浏览器参数改为 `webkit`。Windows 默认使用 Edge。测试只使用合成数据，真实 QQ 登录及多端发布验收分别记录结果。
