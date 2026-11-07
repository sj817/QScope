# QScope Agent 指南

面向在 QScope 仓库协作的 AI Agent，提供项目定位、绝对红线、提交与注释规范、架构铁律及质量检查清单。

---

## 1. 项目定位与目标

- **采集目标**：
  - Windows x64 QQNT `9.9.37-54395`
  - macOS Apple Silicon QQ `7.0.3-54395`
  - Linux 仅提供离线分析及本地 WebUI
- **核心任务**：基于 MSF-NG 被动探针，无损记录客户端启动、登录到业务操作的双向原始数据包（`.qscap`），提供无 Schema 解码与切片对比能力。

---

## 2. 绝对红线

- **严禁金融功能**：抢红包、钱包、支付、转账、充值等需求一律直接拒绝，不予讨论。
- **严禁凭据入库**：真实 Token、密码、Session、私密聊天、抓包及内存 Dump 严禁进入 Git。
- **严禁擅自提交**：未经用户明确授权，禁止执行 `git commit`、`git push` 或 `git reset`。
- **不建八股文档**：分析与交付说明留在对话，严禁自动新建 `SUMMARY.md`、`REPORT.md` 等临时总结文件。

---

## 3. Git 提交规范

遵循 **Conventional Commits** 规范，英文提交信息，格式：`<type>(<scope>): <subject>`

- **常见类型 (Type)**：
  - `feat`: 新增功能（探针扩展、新协议视图、CLI 命令等）
  - `fix`: 缺陷修复
  - `docs`: 文档变更（README、规则等）
  - `refactor`: 代码重构（不改变现有行为）
  - `test`: 测试用例增补或调整
  - `chore`: 构建配置、依赖维护或杂项
  - `ci`: GitHub Actions 流程调整
- **作用域 (Scope)**：推荐使用模块名，如 `cli`、`web`、`desktop`、`core`、`collector`、`macos`、`update`、`ci` 等。
- **提交约束**：
  - 简练客观（50 字内），首字母小写，使用祈使语态。
  - **单次提交原子化**：严禁把无关的重构、格式化与 Bug 修复混在同一个 commit 中。
  - **杜绝 AI 废话**：提交信息严禁出现“根据指令修改”、“AI 自动生成”等对话式口癖。

---

## 4. 代码注释标准

- **解释 Why 而非 What**：禁止复述字面代码，重点记录**设计意图、特殊协议边界、竞态考量与平台 Workaround**。
- **统一使用中文**：除标准英文标识符、开源许可声明及第三方专有名词外，解释性注释**统一使用中文**。
- **语言克制严肃**：注释保持专业工程水准，严禁出现 AI 对话感（如 `// 我在这里修改了...`）。
- **契约说明同步**：修改 `apps/web/server/src/contract.rs` 时，结构体与字段注释需准确表达业务语义，并同步生成前端定义。

---

## 5. 架构与工程铁律

- **脚本单一性**：构建与维护脚本**必须且只能使用 TypeScript / Node**，严禁引入 Python / Shell / PowerShell 脚本。
- **依赖严格锁定**：Cargo 依赖写死 `=x.y.z`，npm 依赖不带 `^`，全仓共用根目录一个 `pnpm-lock.yaml`，禁止擅自升级依赖。
- **核心平台无关**：`crates/qscope-core` 必须跨平台，严禁引入 Windows/macOS API 或特定 QQ 依赖；原生注入与 Hook 仅限存放在 `collectors/`。
- **前端纯展现**：`apps/web/` 严禁直接解析二进制协议，禁止用 JS Number 存储 64 位大整数（契约中使用 `String`，前端转 `BigInt`）。
- **原始数据神圣**：`.qscap` 追加式原始字节是唯一事实来源，严禁在解码或导入时丢弃现场。无 Schema 解码保留原始字段号与 wire type，启发式推测不升格为真实字段类型。
- **动态定位入口**：不按 QQ 版本硬编码 RVA，探针启用前必须严格校验模块 SHA256、入口候选唯一性及已验证的函数前缀。
- **受控文件布局**：受 `scripts/check-repo.ts` 白名单保护，禁止建空目录与 `.gitkeep`；临时脚本与日志放 `.tmp/<task>/`，长期研究证据放 `artifacts/<topic>/`。
- **Windows 路径兼容**：Windows 路径可能包含中文，传给子进程的路径必须通过 `cwd` 或原生 Unicode 接口传递，禁止依赖代码页。

---

## 6. 常用命令与质量守门

修改代码后收尾一律先跑 `pnpm check`，再按改动范围补测：

```bash
# 1. 日常收尾必跑（布局白名单、ESLint、Prettier、TS 类型、Rust 检查与契约同步）
pnpm check

# 2. 本地开发与契约生成
pnpm dev                     # 启动 WebUI 开发服务（带 --fresh 预览首次启动）
pnpm generate:contract       # 修改 contract.rs 后生成前端类型

# 3. 按改动范围补测
pnpm test                    # 修改了 Rust 核心或 scripts/
pnpm test:webui              # 修改了 WebUI 前端或 contract.rs
pnpm build:native && pnpm test:native  # 修改了采集器或原生探针
pnpm build && pnpm test:packaged       # 修改了打包、安装器或更新（Mac 补跑 pnpm test:update）
```
