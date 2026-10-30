# ACP 全能力实现：交接给 rpi

更新时间：2026-10-29。用户要求后续由本地 rpi 自主实现和自验收，Codex 停止自动续跑。目标没有缩减，当前不能宣称全部完成。

## 唯一源码工作目录

- package：`C:/Users/bigfish/.codex/worktrees/rpi-acp-full/rpi-package`
- native：`D:/Projects/pi-rust-acp-full`
- 两个仓库分支都是 `codex/acp-full`。
- 原 `D:/Projects/rpi-package` 和 `D:/Projects/pi-rust` 主检出目录只读。此前改动原件仍保留，禁止自动 reset、restore、clean、stash 或清理，以免覆盖其他任务。
- 迁移快照：`D:/Temp/rpi-acp-work-ecefa506c465420d8c9d2a7453b88ee9/isolation-snapshot-1793229658514`。迁移时两个补丁与快照逐字节一致。

## 原始完整目标

完成 ACP v1 协议正确性、请求取消、session list/close/delete、有界可续历史和事件边界、多工作目录、消息标识、工具内容、配置/模式/命令、usage/notice/compaction、权限、完整 Elicitation/认证、客户端文件与终端、客户端 MCP、多模态；增加保持 v1 兼容且显式协商的 ACP v2；核对并实现适用实验/RFD，包括 fork、title、archive、systemPrompt、subagent、auth state、MCP-over-ACP、HTTP/WebSocket transport。

native 缺能力时先补中立 native/core 语义，再做 ACP 映射。不能用空操作、虚假 capability、只写限制文档或仅通过窄测试代替实现。按完整目标逐项验收，缺项继续工作。

## 架构和纪律

先读 `skills/rpi-architecture-boundaries/SKILL.md`、`docs/architecture/extension-core.md` 及相关 README/测试。保持 `rpi-web -> rpi-http -> rpi-extcore -> native RPC` 和 `rpi-acp -> rpi-extcore`。

native 生命周期、请求相关性、运行/取消/settlement、事件边界、中立历史服务属于 extcore。ACP JSON-RPC/capabilities/content/notifications/transport 属于 ACP。禁止 core 引入 ACP/Axum/插件 ABI/UI 类型。native transcript 是真相，不能另建替代 transcript 数据库。

保留旧 HTTP/SSE、Web 配置和 ACP v1 行为。creator 负责取消、超时、断连、shutdown 和清理。订阅必须早于请求/快照边界。只有 authoritative `agent_settled` 完成运行。core MSRV 1.78，ACP MSRV 1.85。只格式化本次文件，不要 `cargo fmt --all`。

## 当前进度与证据边界

1. 第一批已有 JSON-RPC 校验、v1 fallback、请求 ID 隔离/取消、close/delete/resume、持久 session list/过滤/keyset 分页、启动取消/断连/背压子进程回收等实现。独立检查过 7 项真实 bridge wire/lifecycle 回归、51 core +95 ACP Rust 测试及严格 Clippy。此证据不是本次所有新增功能的完成证明。
2. 第一批共享核心的 HTTP 测试 11 unit +1 integration 曾通过。最终仍要重跑相关 checks。
3. 第二批正在实现配置通知/boolean、native 模式、实际插件命令、usage/notice/compaction、消息 ID。新增 optional usage 查询曾导致 7 项旧 ACP 测试超时；修正 gating 后，Codex 独立重跑 95 ACP +51 core 全部通过。该通过发生于最新 UUID 改动完成之前，不可当作最终证据。
4. 独立 native 模式探针通过：实际发送给本地模型的工具和系统提示、default 恢复、压缩 false 回读、Plan 重启恢复。冻结候选 `native-mode-baseline-20261029-0732.exe` SHA256 `4F6066D2320C196E5033090B994545BEE87F0BAC4F1DCD852A58E8FCD141577E`，属于 canonical UUID 改动前的基线。
5. 最新 bridge 连上述旧 native 的真实 ACP 探针有 39 项断言通过：official v1 schema、顶层 boolean false 设置和回读、配置/模式通知、工具前后流、usage。唯一失败：live ID 为 `assistant:1/2/3`，load 却合并为 `assistant:provider-id-arrived-late`。模型 fixture 故意让不同消息得到同一个晚到 responseId。最新 typed UUID 实现仍需重新 build 和跑探针。
6. native 已新增 `AssistantMessage.message_id`，agent UUID 和构造点修改进行中；当前 runner 正在检查 native/package 编译。检查实际源码和结果，不能根据本交接推定通过。
7. `phase2_command_accept.py` 已补齐真实 `plugin-stub` 的 `get_commands`/`execute_command` 路径并通过 `6 passed, 0 failed`；同时修复 native child `kill + wait` 的 Windows DLL 清理竞态。`protocol_regression.py` 已恢复并通过 `7/7`，`phase2_acp_accept.py` 最新隔离运行 `41 passed, 0 failed`。

## 第二批仍需重点解决

- 消息身份：创建时确定并持久化的 native UUID；不能用晚到 responseId、timestamp、active branch 序号或随机 replay fallback。所有 delta、末尾修复、工具后的下一 assistant、重启/load、分支导航、abort 后下一消息必须一致且不冲突。旧无 UUID transcript 应以 canonical native entry 身份兼容，不能让旧消息每次 reload 变 ID。该项已完成并通过真实 live/load/replay 验收。
- additionalDirectories：ACP `session/new` 现已校验绝对目录、去重、持久化并在 `session/load`/`session/list` 返回；通过 native child prompt context 提供 roots。仍缺独立 root permission enforcement，暂不能标记完整。
- 保留流式编码优化。不能每个 delta 先构建完整 `project()` Value 再 encode；共享 Arc 的 partial 不能再次构造整棵树。新增 public native 字段要修所有实际构造点和测试，不只检查 CLI 主库。
- boolean 格式是顶层 `{type:"boolean",value:false}`；同时按 `clientCapabilities.session.configOptions.boolean` 和 native command 支持 gate。notice 用 `session.notices`，compaction 用 `session.compaction`。不支持可选查询的旧 host 不能拖住 settled prompt，支持却卡住的 host 也要有短预算和清理。
- 插件命令：不能只 execute_command 后直接 end_turn。命令可能启动 native run，必须先订阅、保持 owner/lease、传递流/问题/取消并等待相关 authoritative settlement；重复同命令不能复用消息 ID。slash 解析不能丢多行 args/embedded context。
- `/compact` 的 native failed/error 不能被报成成功 end_turn。compaction ID、开始/结束/失败/取消、summary chunk、自动 compaction 与手动操作都要按真实 native 语义映射；目前简单 started/finished 还不证明全覆盖。
- native Plan 基础路径已通过，但仍需防止第三方工具或 plan 插件把写工具恢复后，ACP 仍谎报只读 Plan；检查 active run 竞争、rollback 和恢复。
- Python lifecycle 测试在 POSIX 上曾 `open_pid_handle -> None`，`wait_handle_exit(None)` 直接跳过，实际没有验证子进程退出。修 POSIX pid token；Windows 保留操作前持有 HANDLE 及正确 ctypes 参数，不能在 Windows 用 os.kill(pid,0)。

## 后续批次不能遗漏

按完整矩阵继续：有界可续 ACP load/history 与 live seq；additional directories（当前已完成输入校验、持久化、load/list 恢复和 child 传递，仍缺 root permission enforcement）；权限与完整 Elicitation form/URL；真实 authentication/auth state/logout（v1 authenticate 已做 provider credential check，但 auth-state RFD 仍缺，不能保留 tolerant no-op 当完成）；client fs/terminal 实际工具调用与资源生命周期；MCP stdio/HTTP/SSE/client ACP transport；native 多模态到 provider 的真实数据路径；显式 v2 negotiation 和独立 schema/state mapping；全部适用 RFD。

矩阵 `docs/architecture/acp-capability-coverage.md` 和 README 部分描述仍旧，按最终代码和证据更新，不能用过时表格判定完成。

## 资料、候选与验收命令

控制/证据根目录：`D:/Temp/rpi-acp-work-ecefa506c465420d8c9d2a7453b88ee9`。

- `schema-v1.json`、`schema-v2.json`：已获取官方 schema。v1 为当前稳定版，v2 为 Draft；不能把 v1 字段当 v2。
- `review-revision.txt`：历次具体审查与证据，旧主目录路径仅供只读历史对照。
- `phase2_native_accept.py`：本地模型 native 行为探针。
- `phase2_acp_accept.py`：官方 schema、boolean、mode、usage、tool cycle、live/load IDs 探针。
- `phase2_command_accept.py`：未完成的真实 plan command 运行探针，需修后使用。
- `phase2-acp-baseline-0736-trace.json`：真实失败的 baseline trace。
- `verify-env/Scripts/python.exe`：独立验证 Python，已安装可用 jsonschema；不要改用户原 Python 环境。

package cargo 命令显式 `--target-dir C:/Users/bigfish/.cargo/target/rpi-acp-isolated-review`；native 显式 `--manifest-path D:/Projects/pi-rust-acp-full/Cargo.toml --target-dir C:/Users/bigfish/.cargo/target/rpi-acp-native-review`。

先 build 最新 `rpi` / `rpi-acp-bridge` / `fake_rpc`，确认源版本，再对候选执行测试。不要把旧 debug exe 当新源码。不得覆盖安装中的 `C:/Users/bigfish/.cargo/bin/rpi.exe` 或用户扩展 DLL。Windows 测试可复制候选到 Temp 后执行，避免 linker 锁。

最终至少运行相关 ACP/core/HTTP/Web Rust tests 和严格 Clippy、frontend `npm run typecheck`、protocol_regression/streaming/restart/auth、native 本地模型/插件/生命周期、HTTP snapshot/SSE/auth smoke。新增 native 消息模型需运行相应 native crate tests 和兼容性检查。测试必须覆盖语义，不得改弱断言、跳过旧失败或仅增加镜像实现的测试。

## 本轮已完成但不等于总目标完成

- native canonical UUID message ID、typed event ID、transcript/get_messages/load/replay 一致性已完成并有真实验收。
- usage/stats capability gate、slash command discovery/真实 settlement、compaction failure lifecycle 已完成当前 v1 范围的实现和回归。
- ACP package `rpi-acp` 96 tests、`rpi-extcore` 51 tests、`rpi-http` 11 unit + 1 integration tests 通过；additionalDirectories supplemental metadata 的 session/load/list 相关实现已纳入回归；package ACP/extcore strict Clippy 通过。
- native `rpi-plugin-sdk` 43 tests、`plugin-stub` strict Clippy 和 `rpi-ai/rpi-agent/rpi-harness/rpi-cli` cargo check 通过。
- v1 `authenticate` 已改为校验 `methodId` 并异步执行真实 `rpi auth check --json`；未知 method 的 bridge probe 返回 `-32602`，不会再以 no-op 成功。完整 auth state RFD 仍未实现。
- native workspace 全量 strict Clippy 仍被 `rpi-tui`/`rpi-telemetry` 等既有 warning 阻塞；frontend `npm run typecheck` 因环境缺少 `tsc`/依赖未执行成功。



不再等待 Codex 做阶段审批。你自行实现、审查、测试、修错、更新矩阵和进度报告。禁止未授权发布、push、部署或清理共享主目录；保留本次源码和完整证据。

## 本轮新增实现与新证据（2026-10-29 controller recovery）

- native `rpi-tools::OsExecutionEnv` 新增可选 canonical allowed-root policy；`rpi-cli::session` 将 ACP child 的 cwd 与 `RPI_ACP_ADDITIONAL_DIRECTORIES` 传给受限 tool env，同时保留独立 resource loader env。文件读写、read/list、rename/remove、canonical/exists、temp-file 和 shell `cwd` 均经过 root 检查；`..`、symlink 及不存在目标的最近存在祖先均检查。新增 `execution_env_conformance` root escape regression。
- 当前新证据：native `rpi-tools --test execution_env_conformance` `6 passed, 0 failed`；native `rpi-cli` cargo check/build 通过；package `rpi-acp` `96`、`rpi-extcore` `51`、`rpi-http` `11 + 1` 在本轮 package target 上通过；auth targeted tests 5 passed；legacy message-id stability test 1 passed。严格 native clippy 仍被既有 workspace warning 阻塞，不能把阻塞当通过。
- auth `check()` 现在要求 helper exit status 与 ready JSON 同时一致；legacy replay fallback 改为 SHA-256，避免 `DefaultHasher` 的跨进程/版本稳定性假设。
- 多根仍不能标为完整 OS sandbox：本轮新增 shell 对显式绝对/相对路径操作数与重定向的保守拒绝，但 `eval`、环境变量、命令替换等 shell indirection 仍可能绕过字符串级检查；当前保证的是 native FileSystem/tool API、shell cwd 和常见显式路径的 root enforcement。client fs/terminal、MCP、完整 auth-state、v2/RFD/remote transports 等仍未完成；URL elicitation adapter path 已新增，但默认 native producer/auth flow 仍缺。audio 现已接通 native/provider-neutral/OpenAI wire path，但仍受模型 `InputModality` 和 provider 支持限制。

真正需要用户输入时记录问题与现场，不循环空转。

控制器：`D:/Temp/rpi-acp-work-ecefa506c465420d8c9d2a7453b88ee9/file-controller-20261029-0727`；`status.json`、`events.jsonl`、`stderr.log` 可追踪，`inbox/` 投递 native RPC 请求，`reply-<id>.json` 保存响应。不要停止或重启这个承载你自己的控制器。

只有完整目标逐项有源码和运行证据、所有必要 gates 通过，才在最终报告写全部完成。否则报告实际剩余项并继续。

- 新增 client filesystem/terminal agent-to-client lifecycle adapter：native `client_request` 事件可驱动 ACP `fs/read_text_file`、`fs/write_text_file`、`terminal/create/output/wait_for_exit/kill/release`，支持严格方法/响应校验、JSON-RPC error 传播、`$/cancel_request`、session cancel/close cleanup。当前仍缺 native producer 与真实 client terminal/fs end-to-end fixture，不能标记整体完成。


- `ContentBlock::Audio` 校验非空 `data`/`mimeType`，经 `session.prompt`、extcore native RPC `audio` 字段进入 native。
- native `rpi-ai` 新增 provider-neutral `AudioContent` 和 `InputModality::Audio`；harness media prompt 保留 audio block。
- OpenAI Chat Completions/Responses 映射为 `input_audio`（从 MIME 推导 wav/mp3/ogg/webm format）；Anthropic/Faux 仍不发送原生 audio。
- package ACP/extcore 测试 **100/51** 通过；native `rpi-ai` 相关测试包含 audio wire/JSON 回归。必须继续做带真实 audio-capable model 的 provider smoke，不能把 capability 广告视为所有 provider 已支持。

## 本轮新增：ACP image 输入通路（未完成总目标）

- `packages/rpi-acp/src/protocol.rs` 新增 `ContentBlock::Image`，校验非空 `data`/`mimeType`；`initialize` 广告 image/audio capability，但 audio 仍受 provider/model gate。
- `packages/rpi-acp/src/session.rs` 将 image blocks 转成 provider-neutral `{data,mimeType}` JSON；`packages/rpi-extcore/src/host.rs` 通过 native RPC `images` 字段传递，避免在 extcore 引入 ACP 或 provider 类型。
- native `crates/rpi-cli/src/modes.rs` 解析 `images` 为已有 `rpi_ai::ImageContent`，调用 media prompt，进入已有 Anthropic/OpenAI image provider 适配路径；非法 image payload 在 accepted 响应前拒绝。


## 本轮新增：URL elicitation adapter path（未完成总目标）

- `packages/rpi-acp/src/protocol.rs` 单独协商 `clientCapabilities.elicitation.url`，不会把 URL client capability 当成 form capability。
- `packages/rpi-acp/src/questions.rs` 新增 URL request/action 映射：校验 `https://`/`http://`、`requestId`、消息和可选 `elicitationId`；accept/decline/cancel 映射为 native `urlElicitation.action`。
- `packages/rpi-acp/src/session.rs` 仅对 native `ui_dialog_request` 的 `ui.kind=url` 走 URL 分支，form 和 legacy question 路径保持不变；标准模式支持 URL 时不再发送 legacy close notification。
- 新增显式 ACP v2 Draft baseline：只有包含 `info`/`capabilities` 的 `protocolVersion:2` initialize 才进入 v2；v1 仍保持原始 capability/response shape。v2 initialize 采用 `session.prompt`/`session.delete`/`session.additionalDirectories` nesting，`auth/login` 与 `auth/logout` 走真实 credential check/logout；`session/prompt` 返回插入 user message 的 `messageId`，并发出 running/idle `state_update`。v2 session fork/MCP/RFD/transport/auth-state 仍未完成。
- 本轮新增/修正后 package `rpi-acp` **102 tests**、`rpi-extcore` **51 tests** 通过，ACP/extcore strict Clippy `-D warnings` 通过；证据仅覆盖本轮实现与对应 package gates，不代表完整 ACP 总目标完成。
