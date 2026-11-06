# QScope 仓库规范

本文件是 Codex 在本仓库的工作约定。当前优先目标是纯抓包：真实 QQ 从启动、用户手动登录到业务操作，记录双向原始协议数据；包体不限于 PB。已有 Tauri 2 桌面端、Rust 核心/CLI、本地 WebUI、PB/TLV16-BE/JCE 查看、固定 Windows x64 54395 的原生 MSF-NG 收发探针和便携运行入口，并已实际观察登录前后流量。schema 编辑及协议导出属于后续能力。技术方案见本机笔记 `artifacts/notes/architecture.md`（Git 忽略，不随仓库发布）；不要把建议、单次观察或结构候选写成完整覆盖或已还原协议。

## 工作方式

- 开始先读 `README.md`，检查 `git status --short --branch`；按任务需要读架构或对应协议主题，不通读所有资料。
- 先查已有实现、结论和失败条件，再动手；保留其他会话的修改和正在使用的进程、数据库。
- 创建或迁移远端仓库时，使用用户明确指定的账号或组织；参考项目的归属不能作为发布目标的依据。
- 在当前授权范围内完成实现及验证。常规检查、可逆修改不反复确认；新需求、技术选型建议不等于所有功能已经获准实施。
- 不把另一仓库的旧协议结论、账号状态、采集目录、特定任务限制直接当作本项目事实。
- 对任何金融相关开发、逆向、抓包、分析或自动操作任务直接拒绝。抢红包、红包协议、钱包、支付、转账、充值、提现及其他金融功能均在禁用范围内。具体技能规则以 `crates/qscope-automation/skills/qscope/SKILL.md` 的金融禁令为准。
- 完成后在聊天里说明结果、验证和未验边界。默认不新建总结文档，不自动提交或推送，不执行整树清理或重置。

## 文件与目录

只在有实际内容时创建目录，不铺空目录和 `.gitkeep`。根文件、正式文档和顶层目录由 `tools/check-repo.ts` 的白名单管理；新增入口需同时说明用途并更新白名单。

| 内容 | 唯一归属 | 生命周期 |
| --- | --- | --- |
| 使用入口、当前阶段 | `README.md` | 只维护当前事实和入口 |
| README 截图 | `.github/screenshots/` | 真实界面配 `apps/web/tests/showcase.mjs` 合成数据，由 `pnpm --dir apps/web run screenshots` 重新生成，不放真实抓包 |
| AI 工作规则 | `AGENTS.md` | 本文件，不再复制一套规则 |
| 桌面界面及 Tauri 壳 | `apps/desktop/` | 产品源码 |
| 本地 WebUI 及 HTTP 会话服务 | `apps/web/`、`apps/web/server/` | 复用核心，不在前端解析协议 |
| 独立 CLI | `apps/cli/` | 产品源码，复用核心 |
| Rust 解码、模型、存储等核心 | `crates/` | 不依赖 UI 或特定 QQ 版本 |
| 采集宿主、注入探针、版本适配 | `collectors/` | 与核心通过有版本的协议通信 |
| `.proto`、描述符构建入口、字段来源 | `schemas/` | 正式协议定义及机器可读来源 |
| 脱敏最小回归样本 | `tests/fixtures/<case>/` | 进入 Git，附 `manifest.json` |
| 持续维护的检查、构建、研究辅助工具 | `tools/` | 有明确消费者与调用说明 |
| 架构与协议笔记 | `artifacts/notes/<topic>.md` | Git 忽略，不随开源仓库发布；按主题更新，不按会话堆积 |
| 一次性脚本、草稿、试验、验证日志 | `.tmp/<task>/` | Git 忽略，任务后清理自己的文件 |
| 原包副本、解包、抓包、dump、IDA/TTD 数据 | `artifacts/<topic>/<run-id>/` | Git 忽略；有价值证据保留 manifest |
| 编译、打包产物 | `target/`、`build/`、`dist/` 或工具约定的构建目录 | Git 忽略，可重建 |
| 产品运行数据库、账号状态、本机设置 | 用户数据目录，Windows 默认 `%LOCALAPPDATA%/QScope/` | 不与研究临时目录混用 |
| 本机 AI 工具配置与记录（Codex MCP 等） | `.codex/`、`.claude/`、`.agents/`、`.cursor/` | 整个目录 Git 忽略，不提交模板或本机记录 |

- 不往根目录或源码目录倾倒安装包、抓包、生成代码、日志、截图、反编译全文。
- 原始输入已有外部路径时优先只读引用；需要解包时输出到本仓库 `artifacts/`，不执行安装程序来代替解包。
- `.tmp/` 不是知识库：复用脚本移到 `tools/` 或对应采集器；结论提炼到已有主题；有价值的原始证据移到 `artifacts/` 并建立引用。
- 删除只处理本任务拥有且已核对绝对路径的文件；不得根据文件年龄或名字批量删除证据。长期开着的 IDA 数据库不得移动或覆盖。
- 不用全局 `*.bin`、`*.json`、`*.log` 忽略规则掩盖文件归属问题。

## 文档规则

- 仓库内 Markdown 只有 `README.md`、`AGENTS.md`；架构与协议笔记留在本机 `artifacts/notes/`，不进入 Git，README 只写面向使用者和贡献者的入口。
- 产品 skill 源码为 `crates/qscope-automation/skills/qscope/SKILL.md`，由安装器生成各 AI 的本机技能文件，单独登记在检查白名单中。
- 新知识先更新现有主题。仅在出现独立、会持续维护的主题时在 `artifacts/notes/` 增加 `<topic>.md` 或 `protocol/<domain>.md`。
- 不自动生成 `SUMMARY`、`REPORT`、`PLAN`、`TODO`、`WORKLOG`、`STATUS`、`FINAL` 等每轮交付文件，不创建日期目录或 `v2/final/final2` 副本。计划和交付说明默认留在聊天，变更历史交给 Git。
- 不建第二套文档索引、历史文档堆、独立决策日志体系。架构决策写入架构主题，协议结论写入协议主题；未来确有规模需求再调整规则。
- 文档记录稳定知识、使用方法、非显然约束和被反证的方向；不粘贴大段日志、完整反编译或重复代码。
- 文档、代码、注释各有唯一事实来源：字段来源由 `schemas/` 的元数据承载，文档解释方法，避免手写重复字段表。

## 证据与协议定义

- 协议结论绑定客户端版本、平台/架构、相关模块 SHA256、采集层和工具版本。模块地址记录 `模块 SHA256 + RVA`，不只记运行时 VA。
- 所有推断明确标记 `observed`（原始观察）、`hypothesis`（待证假设）、`verified`（有独立验证）、`rejected`（被反证）。`verified` 必须写验证方法及适用范围，不能只靠名称或几个看起来合理的值。
- 保留失败条件和反例；修正结论时写明旧假设为何失效，不创建重复的“最终结论”文件。
- 原始字节不可被解码、脱敏或标注覆盖；派生结果引用输入 SHA256、偏移范围、解码器版本和参数。
- 无 schema 解码保留字段号、wire type、顺序、重复项、原始字节和解析错误。`string/bytes/message/packed` 等只能给候选解释，不把启发式判断升级为真实字段类型。
- 发布 `.proto` 时对可用定义与推断定义做明确区分，未知含义使用中性名称；引入外部定义注明来源、版本和许可，不把其他平台字段直接套到 NT。
- `artifacts/` 每个有效实验的 `manifest.json` 至少记录：问题、UTC 时间、输入哈希、模块身份、工具/命令、实验条件、证据文件及结果边界。一次性失败日志不强制写正式研究文档。
- 每组正式 fixture 的 `manifest.json` 记录来源类型（合成/脱敏）、适用版本、预期行为、文件 SHA256；脱敏后重新计算哈希。真实消息、账号票据、密钥、设备标识和内存转储不进 Git。

## 实现与标准化

- 产品入口固定为 `QScope.exe` 与 `qscope-cli.exe`，全局 `qs` 复用同一个 CLI 产物。桌面通过数据目录发现并连接 CLI 承载的后台，静态界面、Chii 和采集 DLL 由 CLI 内嵌 ZIP 管理。新增运行时、独立宿主 EXE 或第二套后台生命周期需要先讨论架构。
- 同一数据目录只允许一个后台，连接核对实例和构建身份。退出桌面端时关闭从界面启动的研究 QQ 并释放租约；CLI 或 AI 启动的 QQ、准备任务和录制保留后台。在线更新在安装包校验通过后保存录制并关闭全部研究 QQ，再替换程序。SQLite 是可重建元数据索引，原始 `.qscap` 继续作为唯一原包依据。

- 构建、发布与验证脚本只用 TypeScript / Node 编排，不引入 Python、PowerShell 或 shell 脚本；路径、进程和文件操作采用跨平台 API。平台差异集中在对应采集器与工具链适配中，缺少适配时明确报错。产品运行入口使用已构建组件。
- Windows 发布入口命名为 `QScope.exe` 与 `qscope-cli.exe`；桌面与 CLI 通过 `qscope-automation` 共用启动编排，界面通过嵌入的本机服务使用解析核心。
- Windows x64 作为首个采集目标；平台无关解析核心不能依赖 QQ 进程、注入库、Tauri 或 Windows API。
- 解码、采集、存储、展示分层；按真实模块边界拆分，不为预期功能创建大量空 crate、抽象层或插件系统。
- 采集事件、导入/导出格式、数据库和核心/前端 IPC 都有独立版本；不兼容变更必须有明确拒绝或迁移行为。数据约定见本机架构笔记。
- 后端契约生成前端类型；前端不用 JS `number` 保存任意 64 位整数，不在 JS 端另写一套协议解析逻辑。
- WebUI 契约来源是 `apps/web/server/src/contract.rs`，用 `cargo run -p qscope-web --locked -- types` 生成前端类型，`--check` 验证同步。前端检查为 `pnpm --dir apps/web run build`；合成浏览器验收为 `cargo build -p qscope-web --locked` 后运行 `pnpm --dir apps/web test`。不要把真实抓包放入浏览器测试 fixture 或截图。
- 首次初始化各语言工程时固定工具链并提交 lockfile；不自动升级依赖。格式化和 lint 优先采用生态默认配置，避免自行维护大套代码风格规则。
- 公共错误带稳定 code 与上下文，日志走结构化输出；stdout 用作机器协议时日志只进 stderr。
- 捕获回调只做有界复制与入队；队列、包长、解压、递归和存储必须有界，丢包及截断必须可观测，不伪造完整数据。
- 原生 `.qscap` 是追加式现场记录，缺少结束标记和截断尾部必须可查看；不要用严格离线 JSONL 导入的“失败不发布”策略丢掉现场。PB/TLV/JCE 解码显式选择，无法解析的字节照常保留，不改变真实 QQ 的登录流程来迁就解析器。
- 配置工具、MCP 握手、目标附加、完整采集、协议语义确认、真实收发分别报告，不能互相替代。
- 根据实际改动运行检查：仓库布局用 `node tools/check-repo.ts`；Rust 用 `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、`cargo test --workspace --locked`（格式由 `rustfmt.toml` 固定为 2024 风格）；TypeScript、CSS、JSON 与 YAML 先在根目录 `pnpm install --frozen-lockfile`，再用 `pnpm run lint`（ESLint）、`pnpm run format:check`（Prettier 默认风格，`pnpm run format` 写回）和 `pnpm run typecheck`（`tools/` 类型检查）。根目录的 TypeScript 固定为 typescript-eslint 支持的 6.0，`apps/web` 构建继续用自己的 TypeScript 7。解析器变更覆盖截断、非法长度、溢出、重复字段等边界；不要为纯文字改动搭建应用测试。

## Codex 与逆向工具

- MCP 只写本仓库 `.codex/config.toml`（整个 `.codex/` 不进 Git），不创建 `.mcp.json`、Claude 配置或改用户全局配置。项目规则由本文件提供；本机路径和工具配置不进入正式源码。
- IDA/Hex-Rays 使用已安装的官方插件，先列出数据库并核对路径与哈希，再选择目标；不重启、关闭其他任务数据库，不复活参考项目已禁用的旧 HTTP 服务。
- x64dbg 用于交互调试与有界 trace，WinDbg/TTD 用于值流与时间线，DynamoRIO 用于有目的的动态插桩。Frida 是候选采集适配器，具体 QQ 版本上的可靠性必须验证。
- 当前 x64dbg/WinDbg 可复用参考仓库的本机安装，但 cwd、采集、符号缓存和派生证据归属 QScope；不改动参考仓库的源码、工具安装或数据。
- 调试器会影响目标行为。执行真实会话操作前核对本轮范围；不能将配置工具的任务扩展为重启 QQ、登录、发消息或自动采集其他进程。
