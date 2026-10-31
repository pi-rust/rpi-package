# ACP capability coverage

本表是 `rpi-acp` 的能力跟踪矩阵，不是“总目标已完成”的声明。状态只按代码、测试和官方协议资料填写；native `rpi --mode rpc` 不具备的能力不会通过 ACP capability 假装支持。

## 依据与版本策略

- 稳定协议：官方 [ACP v1 overview](https://agentclientprotocol.com/protocol/v1/overview)、[initialization](https://agentclientprotocol.com/protocol/v1/initialization)、[session setup](https://agentclientprotocol.com/protocol/v1/session-setup)、[cancellation](https://agentclientprotocol.com/protocol/v1/cancellation) 和官方 `schema/v1/schema.json`（官方 schema 页面：[v1/schema](https://agentclientprotocol.com/protocol/v1/schema)）。
- v2：官方 [v2 overview](https://agentclientprotocol.com/protocol/v2/overview) 和 `schema/v2/schema.json`；官方导航当前将 v2 标为 Draft。v2 不能因 v1 的字段扩展而隐式启用。
- 不稳定：官方 `schema.unstable.json` 及 [RFD index](https://agentclientprotocol.com/rfds/about)。不稳定和 Draft RFD 只记录为实验依赖，不计入稳定 capability。
- 当前实现只选择并返回 **ACP v1**。收到 `protocolVersion: 2` 会返回 `-32602`，不会宣称 v2；未来 v2 必须增加显式版本协商、独立 schema 校验和兼容性测试。

## 稳定 ACP v1

| 能力 | 状态 | 当前实现/依赖/缺口 |
| --- | --- | --- |
| JSON-RPC 2.0 envelope、parse/request/params/method 错误 | **已完成（第一批）** | `rpi-acp/src/protocol.rs` 返回 `-32700/-32600/-32602/-32601`；合法 notification 没有响应；坏 prompt block 不再被 `filter_map` 丢弃。 |
| initialize、v1 版本选择、capability 协商 | **已完成（v1）** | 只选择 v1；v2 明确拒绝而不作隐式声明。 |
| `$/cancel_request` | **已完成（第一批）** | 跟踪 ACP client prompt request 和 agent→client dialog request；取消 native run 后等待 authoritative settlement，不直接 abort Rust task。 |
| `session/new`、绝对 cwd、多工作目录 | **部分完成** | `session/new.cwd` 是绝对存在目录，并由 extcore host 作为 child cwd；additional workspace roots / 多根目录尚未实现，不能声称支持。 |
| `session/prompt`、`session/cancel` | **已完成（现有 v1 + 第一批追踪）** | prompt task 有 owner；cancel 发 native abort 并保留 run lease 直到 settled。 |
| `session/close` | **已完成（第一批）** | 协商 `sessionCapabilities.close`；取消并等待活动 prompt/dialog，再关闭 child/watcher；保留 native history，之后可 load/resume；重复 close 对有效持久会话幂等。 |
| `session/delete` | **已完成（第一批清理统一）** | 与 close 共用活动任务、dialog、watcher、child 的 graceful cleanup，之后删除持久目录；重复 delete 幂等。 |
| `session/load` / `session/resume` | **已完成（现有 v1 + 第一批生命周期）** | native transcript 是 source of truth；load replay active branch，resume 不 replay；close 后可重新 attach。 |
| `session/list`、cwd 过滤、稳定分页/cursor、metadata | **已完成（有界发现 slice）** | extcore 中立 `session_catalog` 只读验证 ACP store 内 native v4 header，固定深度、预算、取消/worker owner、全局 createdAt 降序/UUID 升序冻结分页；ACP 广告 `list: {}`，wire 固定50，扫描空页有 cursor 必须续取。游标绑定连接/规范 cwd，scan 单次、冻结页可重试，60s idle/admission 过期、最多2代可淘汰。只返回 ID/cwd/可选 observed mtime，省略 title，不读 message/tail、不建立 sidecar、不启动 native/MCP。详见 [ACP README](../../packages/rpi-acp/README.md#discovering-saved-sessions)。Rust/CI fixtures 不等于真实 native 或已安装扩展资格；Windows provenance-gated smoke 必须另看执行结果。 |
| 有界、可续的历史/事件边界 | **部分完成** | extcore snapshot/history 已有 bounded read、seq boundary、replay-gap 检测；ACP load replay 尚未暴露分页 cursor。 |
| 消息 ID | **未完成** | native transcript 内部有 ID，但 v1 `session/update` message-id RFD 的完整映射尚未实现。 |
| text / resource link / embedded text resource | **已完成（受限）** | 映射为 native text；绝对 URI/file 语义依赖 native cwd/工具。 |
| image / audio / 其它多模态 | **未完成且明确不支持** | native `--mode rpc` 当前只接收单一文本 prompt；ACP capability 不会虚报 image/audio。 |
| tool call / tool call update | **已完成（native 依赖）** | native event 映射到 ACP；native child 是实际执行者，ACP 只做 presentation mapping。 |
| diff / plan | **已完成（native event 依赖）** | edit/write diff、todo/plan event 已映射；能力取决于 native event 是否提供。 |
| 配置选项通知、boolean、mode | **部分完成** | model/thinking select 和 `set_config_option` 已实现；config update notification、boolean option 和 session mode 尚未实现。 |
| slash commands | **未完成** | native RPC 不展开 prompt templates；不广告虚假的 `available_commands_update`。 |
| usage / notice / compaction | **未完成** | native 尚未提供稳定 ACP usage/notice/compaction 语义，未做空操作映射。 |
| 权限 request | **已完成（受限）** | 明确 `purpose: permission` 的 native dialog 映射到 `session/request_permission`；取消/断连会关闭 pending dialog。 |
| Elicitation form | **已完成（受限）** | 协商 `clientCapabilities.elicitation.form` 后使用 `elicitation/create`，返回值仍由 native dialog schema 校验。 |
| Elicitation URL | **未完成** | 没有 native URL elicitation/完成回调接口，不广告支持。 |
| authentication / terminal auth / logout | **部分完成** | terminal auth 是受 capability gating 的启动外流程；`logout` 子进程隔离 stdin/stdout 且 async dispatch，有 bridge fixture 回归；真实 credential revoke 依赖 native `rpi auth logout`。 |
| client `fs/*` | **未完成且明确不支持** | agent 使用 native read/write/edit；没有声明 client fs capability。 |
| client `terminal/*` | **未完成且明确不支持** | agent 使用 native bash；没有声明 client terminal capability。 |
| client MCP | **已完成（tools-only，native 依赖）** | `session/new/load/resume.mcpServers` 经 ACP 校验映射到 extcore 中立 `ToolServer` / `configure_tools`，支持 stdio 与 Streamable HTTP（POST JSON / response-body SSE），`http: true`、`sse: false`。HTTP 要求显式 `type: "http"`、绝对 HTTP(S) URL 和 `headers` 数组，支持静态 Authorization/custom headers；无 OAuth/resources/prompts/sampling/legacy SSE/GET。URL/header 安全策略、可信客户端出站权限（非 SSRF sandbox）、失败后不自动重连/重查/重放、restore/live equality/cwd、无 secret 持久化与逐连接 best-effort cleanup 详见 [ACP README](../../packages/rpi-acp/README.md#client-provided-mcp-tools-stdio-and-streamable-http)。native transcript 仍为权威；Windows native proof 必须绑定实际 binary/source hashes，不能由 fixture CI 推导，也不代表已安装扩展或 Unix native 资格。 |

## 保持 v1 兼容的 v2 / 实验 RFD

| 项目 | 官方状态/依据 | 状态 | 依赖与说明 |
| --- | --- | --- | --- |
| 显式 v2 negotiation | v2 overview/schema 标为 Draft | **未完成** | 目前只做安全拒绝，不把 v1 当 v2；需要正式 v2 adapter/schema gate。 |
| session fork | [RFD session-fork](https://agentclientprotocol.com/rfds/session-fork)（Draft） | **未完成** | extcore 有 native fork 语义，但 ACP method/capability 尚未映射。 |
| client-set title | [RFD session-set-title](https://agentclientprotocol.com/rfds/session-set-title)（Draft） | **未完成** | registry title 不是 ACP client title wire contract。 |
| archive/unarchive | [RFD session-archive](https://agentclientprotocol.com/rfds/session-archive)（Draft） | **未完成** | 尚无 ACP adapter method/notification。 |
| client-provided `systemPrompt` | [RFD client-system-prompt](https://agentclientprotocol.com/rfds/client-system-prompt)（Active） | **未完成** | native prompt API 没有中立 system prompt 参数。 |
| subagent sessions | [RFD subagents](https://agentclientprotocol.com/rfds/subagents)（Active） | **未完成** | 没有 ACP session tree / permission boundary 的 native contract。 |
| auth state query | [RFD get-auth-state](https://agentclientprotocol.com/rfds/get-auth-state)（Draft） | **未完成** | `auth check` 不是已映射的 ACP auth-state method。 |
| MCP-over-ACP | [RFD mcp-over-acp](https://agentclientprotocol.com/rfds/mcp-over-acp)（Active） | **未完成** | 已交付的 `mcpServers` stdio/Streamable HTTP tools 不是 MCP-over-ACP 隧道；未实现该 RFD。 |
| HTTP/WebSocket transport | [RFD streamable HTTP & WebSocket](https://agentclientprotocol.com/rfds/streamable-http-websocket-transport)（Active） | **未完成** | 本包只实现 stdio；HTTP listener/session ownership 属于未来 ACP transport，不放入 extcore 的 ACP 类型。 |
| fork/title/archive/systemPrompt/subagent/auth/MCP/remote 的 schema.unstable | 官方 `schema.unstable.json` / RFD 页面 | **未完成** | 实验项必须显式协商并按 RFD 生命周期升级；当前不在 `initialize` 稳定 capability 中。 |

## 第一批验证证据

- Rust：`cargo test --locked -p rpi-acp -p rpi-extcore`。
- 静态质量：按改动运行 `cargo clippy --locked -p rpi-acp --all-targets --no-deps -- -D warnings`。
- wire/lifecycle：`tests/protocol_regression.py` 启动真实 `rpi-acp-bridge` 子进程，使用 `examples/fake_rpc.rs` 作为受控 native child，覆盖 envelope 错误、notification 无响应、logout stdout/stdin 隔离、prompt cancel、close 等待、resume 和重复 delete。

这张表不会替代官方 schema；若官方稳定 schema/RFD 状态变化，必须先更新依据和状态，再修改 capability 声明。
