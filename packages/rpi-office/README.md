# rpi-office

让 rpi 直接读写正在打开的 Excel、Word、PowerPoint：本扩展在 rpi 进程内起一个**仅回环**的 WebSocket 服务，[pi-for-office](https://github.com/dieuluucanh/pi-for-office) 任务窗格加载项连上来，模型的 `office_*` 工具调用被代理到窗格，由 Office.js 真正执行。

这是 Pi 原生扩展 `@dieulc/pi-office-bridge`（MIT）的 Rust 移植：**加载项一行不改**，协议仍是 wire v3。

## 安装

```sh
rpi install rpi-office
```

安装后重启 rpi 让扩展加载。Office 侧仍需按 pi-for-office 的方式部署/旁加载加载项（加载项不在本仓库，必须 HTTPS 托管或本地旁加载）。

## 用法

桥默认是**关的**，只有你明确连上时才占用 38617，因此多个会话不会互相抢占端口。

```text
/office                   # 等同于 /office status
/office status            # 状态、端口、已连接的应用与文档
/office connect [port]    # 启动桥（默认 38617）
/office close             # 关闭桥并断开加载项
/office restart [port]    # 先关再开（可换端口）
/office help              # 命令列表
/office-tools             # 列出 32 个目录工具 + 发现工具，并标注是否激活
```

在加载项里打开 Settings → Connections → Local Pi agent，填入同一个端口即可连接；`curl http://127.0.0.1:38617/health` 可确认桥和已连文档。

### 配置

| 来源 | 端口 | 自动启动 | `/health` 额外允许来源 |
| --- | --- | --- | --- |
| CLI | `--office-bridge-port <port>` | `--office-bridge-autostart` | — |
| 环境变量 | `RPI_OFFICE_BRIDGE_PORT`（其次 `PI_OFFICE_BRIDGE_PORT`） | `RPI_OFFICE_BRIDGE_AUTOSTART`（其次 `PI_...`） | `RPI_OFFICE_BRIDGE_ALLOWED_ORIGINS`（其次 `PI_...`） |
| 默认 | `38617` | 关 | 上游开发源与 GitHub Pages 源 |

环境变量前缀保留上游 `PI_OFFICE_BRIDGE_*`，已有配置无需改动；`RPI_*` 优先。

## 工具

工具目录与加载项共用同一份声明式目录（[`catalog/office-catalog.json`](catalog/office-catalog.json)，由上游 TypeBox 目录生成），共 32 个：

- **Excel（15）**：`get_overview`、`read_range`、`write_cells`、`fill_formula`、`search_workbook`、`modify_structure`、`format_cells`、`conditional_format`、`charts`、`trace_dependencies`、`explain_formula`、`view_settings`、`comments`、`workbook_history`、`get_capabilities`
- **Word（11）**：`get_overview`、`get_capabilities`、`read_document`、`insert_text`、`replace_text`、`format_range`、`insert_blocks`、`insert_table`、`insert_page_break`、`insert_image`、`insert_hyperlink`
- **PowerPoint（6）**：`get_overview`、`read_slide`、`add_slide`、`add_text_box`、`format_slide`、`get_capabilities`

外加跨应用的发现工具 `office_list_documents`（id、名称、宿主、是否聚焦、窗口数）。

每个工具都可以带 `document` 参数：文档 id、唯一文档名或 `"active"`。多份文档同时打开且没有聚焦时，缺省目标会**报错并列出候选**，绝不猜测。该参数由桥剥离，不会传进加载项。

工具激活跟随生命周期：桥关闭时整族工具从活动集合移除；桥打开且窗格连上后，只激活该窗格**实际声明支持**的 op（旧版 0.2.x 客户端只允许 v1 op 集）。

## 与上游 Pi 扩展的差异

| 能力 | 状态 |
| --- | --- |
| 工具代理（29 个 op → 32 个工具） | 一致 |
| 协议 v3（`hello`/`welcome`、`pane_state`、`tool_call`/`tool_result`、`agent_message` delta+final、`tool_activity`、`/health`） | 一致 |
| 多文档路由、`#name` 选择器、op 能力握手 | 一致 |
| `/office` 命令族、手动生命周期、自动启动开关 | 一致 |
| `before_agent_start` 注入已连文档与使用规则 | 一致 |
| `client-abort`（窗格 Stop 真正中止当前 run） | 一致：调用宿主 `RuntimeActionId::Abort`（27）；中止后不把迟到的 final 回推窗格 |
| reload 安全（桥开着时 `/reload`） | 一致：`register_plugin_lifecycle` 的 `begin_shutdown` 非阻塞停桥，`finish_shutdown` join 桥线程与 worker 后才返回 0 |
| 工具 `promptSnippet` / `promptGuidelines` | 一致：`register_tool_prompt`（元数据取自共享目录） |
| TUI 输入框里 `#` 文档补全 | 一致：`register_autocomplete_provider` 把已连文档作为候选，接受后插入 `#<名称>`（同名不同文档时插入 `#<id>`）。宿主 vtable 没有该槽位时（如 `rpi 0.24.0`）自动跳过，退回 `office_list_documents` 或 `document` 参数 |
| `/office <Tab>` 参数补全 | 一致：`register_command_completions` 补 `status/connect/close/restart/help` |
| Pi Desktop 的 Documents rail | 不在扩展范围（属于宿主 UI） |

另外，本扩展是**进程内**的 Rust 动态库：桥的生命周期绑定当前进程的会话，`session_shutdown` 一定关桥并释放端口。

## 初始化时机

`SessionStart` 和 `AgentStart` 共享同一次初始化（解析 flag、对账活动工具、按
`--office-bridge-autostart` 起桥），只会执行一次：非交互的 `--print` 会话**不会**
派发 `SessionStart`（在 `rpi 0.24.0` 上实测，只派发 `AgentStart`/`TurnStart`/
`BeforeAgentStart`），所以两条路径都要覆盖。

## 宿主版本要求

本扩展依赖 `rpi-plugin-sdk 0.26`（ABI 4）。槽位按宿主 vtable 长度逐个可用：

| 槽位 | 需要宿主 |
| --- | --- |
| `Abort`(27)、`register_plugin_lifecycle`、`register_tool_prompt` | ≥ 0.24.0 |
| `register_autocomplete_provider`、`register_command_completions` | 含 0.26 槽位的宿主（`pi-rust#366`） |

vtable 更短时 SDK 把这些槽位报成 `None`（注册 helper 返回 `-1`），本扩展跳过对应
功能：不宣告 `client-abort`、不注册工具摘要、不做 reload join、没有 `#` 与参数补全。
其余功能不受影响——在 `rpi 0.24.0` 上实测仍能加载 33 个工具。

## 关于 `unsafe`

除 `src/abi.rs` 外全crate `#![deny(unsafe_code)]`。那个模块只有一处 `unsafe`：把
runtime action 的 `out` 交给 SDK 的 `OwnedString` 读取。宿主仓库已有对应的
`call_action!` / `stb_text!` / `stb_take!` 宏，但尚未随 SDK 发版；等它们发布后删除
`src/abi.rs` 并直接调用宏，即可回到完全无 `unsafe`。

## 安全模型

- 只监听 `127.0.0.1`，不做外部绑定，没有鉴权（与上游一致：回环 + 能力握手）。
- 服务端对 op 目录**权威**：窗格只能在 `hello.ops` 里缩小可执行集合，不能注入工具名、schema 或描述。
- `tool_result` 只接受发起该调用的连接的回答；单个文本上限 50,000 字符，`details` 上限 1 MiB。
- `/health` 只暴露桥元数据，CORS 仅放行加载项来源（可配置追加）。
- 工具调用 120 秒超时；窗格断线立即失败并在错误里说明。

## 开发

```sh
cargo test -p rpi-office          # 单元 + 协议互操作 + 端到端 + 注册流程
cargo clippy -p rpi-office --all-targets --no-deps -- -D warnings
cargo fmt --check -p rpi-office
```

测试分三层：

1. `src/**` 单元测试：协议校验、文档路由、能力门、生命周期、命令语法、目录一致性。
2. `tests/bridge_interop.rs`：真实 WebSocket 客户端 ↔ 真实桥（hello/welcome、工具往返、多文档聚焦路由、断线/取消、`/health` 与 CORS、坏帧）。
3. `tests/end_to_end.rs`、`tests/registration.rs`：用假宿主驱动真实 `rpi_plugin_register`，覆盖注册面、`/office` 全流程、工具 execute/poll 往返、窗格提问 → 注入会话 → delta/final 回推，以及终端回合不外泄。

目录更新见 [`catalog/README.md`](catalog/README.md)。

## 来源与许可

- 协议、目录、路由规则、错误文案与用户可见行为移植自 [dieuluucanh/pi-for-office](https://github.com/dieuluucanh/pi-for-office) 的 `packages/protocol` 与 `packages/bridge-extension`（MIT）。
- 加载项（`packages/add-in`）仍由上游提供，本仓库不包含。
- 本扩展：MIT，与仓库其余包一致。
