# rpi-office 宿主能力需求

本文是 `packages/rpi-office`（pi-for-office 桥的 Rust 移植）在移植过程中撞到的**宿主侧能力缺口**清单，用于驱动后续宿主/SDK 变更。它是需求文档，不是“已实现”声明：除标注“已验证”的项外，条目的现状都按代码和测试填写。

范围只覆盖**插件 ABI 与宿主自身**。加载项（Office.js task pane）侧不需要任何改动；`rpi-web` 的文档面板属于 Web 前端工作，不在此列。

## 复核基准（第二轮）

首版清单是按 **本仓库 pin 的 `rpi-plugin-sdk = "=0.3.20"`** 写的，而 0.3.20 只是 SDK 的**发布线快照**，不等于宿主能力。第二轮复核直接读宿主与其 SDK 源码，结论如下：

- 宿主源码：`pi-rust` main，workspace version `0.25.0`；交叉核对了 `v0.24.0`、`v0.25.0` tag（本机真机验证用的就是 `rpi 0.24.0`）。
- 发布 SDK：从 crates.io 下载 `rpi-plugin-sdk-0.25.0.crate` 解包核对（不只是 tag）；同时核对了 registry 里已解包的 `0.3.20` 与 `0.8.3`。
- 判定规则：**“宿主已提供、但 pin 的 SDK 没有槽位/枚举”不算宿主缺口**，记为“依赖升级”。

复核结论：

| 原编号 | 首版判定 | 复核判定 | 处置 |
| --- | --- | --- | --- |
| H1 | P0 宿主缺 abort action | **不成立**：宿主 `v0.24.0` 起已有 `RuntimeActionId::Abort = 27` | **已实现**（`5da3f47`） |
| H2 | P0 宿主缺 unload 契约 | **降级**：宿主已有 `PluginKeepalive` + `register_plugin_lifecycle` 钩子，reload 路径确实会调用 | **已实现**（`5da3f47`；宿主文档由 pi-rust#366 补） |
| H3 | P1 缺草稿文本 | **已由宿主实现（未发版）**：`GetEditorText = 32` + 补全 provider 槽位（pi-rust#366） | 改本扩展 |
| H4 | P1 需升 SDK | **不成立**：`register_tool_prompt` 已随发布 SDK 0.25.0 提供 | **已实现**（`5da3f47`） |
| H5 | P2 缺工具作用域 | **仍缺**：`0.25.0` 无 `SetToolEnabled`，`SetActiveTools(5)` 仍是全局 | 保持 |
| H6 | P2 `__rpi` 缺工具名 | **仍缺**：`__rpi` 仍只有 `cwd`/`sessionId` | 保持 |
| H7 | P3 缺命令参数补全 | **已由宿主实现（未发版）**：`register_command_completions`（pi-rust#366） | 改本扩展 |
| H8 | P3 缺前台标识/服务注册表 | **仍缺**：无 `RPI_SURFACE`，无回环服务注册表 | 保持 |
| — | — | **新增**：宿主已有 4 项桥未使用的能力（`N1`–`N4`） | 见文末 |

一句话：**首版把“SDK pin 落后”误记为“宿主缺口”的占了 2 条（H1/H4），另有 1 条（H2）宿主机制早已就位。** 真正要向宿主提的只剩 5 条。

### 版本事实（可直接引用）

| 能力 | pin 的 SDK 0.3.20 | 已发布 SDK 0.8.3 | 已发布 SDK 0.25.0 | 宿主 `v0.24.0` / `main` |
| --- | --- | --- | --- | --- |
| `RuntimeActionId` 上限 | `SideQuestion = 25` | `SideQuestion = 25` | `ReloadModels = 31` | 同 0.25.0 |
| `Abort = 27` | 无 | 无 | 有 | 有（`actions.rs` 已分发） |
| `StopRound = 29` | 无 | 无 | 有 | 有 |
| `RelayUiDialog = 28` | 无 | 无 | 有 | 有 |
| `ListCommands = 26` | 无 | 无 | 有 | 有 |
| `register_plugin_lifecycle` 槽位 | **无** | 有 | 有 | 有（reload 时调用） |
| `register_tool_prompt` 槽位 | 无 | 无 | 有 | 有 |
| `register_tool_with_wake` 槽位 | 无 | 有 | 有 | 有 |
| `EditorChange` 带文本 | 无 | 无 | 无 | 无 |
| `GetEditorText = 32` | 无 | 无 | 无 | 有（pi-rust#366） |
| `register_autocomplete_provider` 槽位 | 无 | 无 | 无 | 有（pi-rust#366） |
| `register_command_completions` 槽位 | 无 | 无 | 无 | 有（pi-rust#366） |

证据位置（宿主 `main`）：`crates/rpi-plugin-sdk/src/lib.rs` 的 `RuntimeActionId`/`EventTag`/`PluginApi`；`crates/rpi-extensions/src/actions.rs` 的 action 分发；`crates/rpi-extensions/src/loader.rs` 的 `PluginKeepalive`/`begin_shutdown`；`crates/rpi-cli/src/session.rs` 的 reload 路径（`old_session.begin_shutdown()`）。

## 第三轮：宿主侧 H3/H7 已实现（未发版）

`pi-rust` PR #366（合并提交 `bd08af3`）把上游 Pi 的 `ui.getEditorText()`、`ui.addAutocompleteProvider()`、`RegisteredCommand.getArgumentCompletions` 对应能力合入宿主 `main`：`RuntimeActionId::GetEditorText = 32`、`PluginApi::register_autocomplete_provider`、`PluginApi::register_command_completions`，全部追加在 vtable 尾部、ABI 版本不变。同一 PR 还补了 `register_plugin_lifecycle` 的文档（H2 的答案）。

因此剩余的真实宿主缺口收缩到 **3 条**：H6（`__rpi.toolName`）、H5（单工具作用域）、H8（surface / 回环服务注册表）。H1/H2/H3/H4/H7 都已变成「升 pin + 改本扩展」的工作。

> 阻塞点：`rpi-plugin-sdk` 尚未发布含新槽位的版本（0.3.20 / 0.8.3 / 0.25.0 都没有 `GetEditorText` 与两个 completion 槽位），本扩展要动 H3/H7 得先等一次 SDK 发布。

## 第四轮：真机探针实测（`rpi 0.24.0` + SDK 0.25.0）

前三轮是源码核对；这一轮用**一次性探针插件**（不入库，`/tmp/probe`，`cdylib`，依赖 crates.io `rpi-plugin-sdk 0.25.0`）在真机跑了一遍：`register` 阶段记录 `abi_version`/`struct_size` 与每个槽位是否 `Some`，`SessionStart`（空闲态）与 `BeforeAgentStart`（run 已存在）各跑一次相同 action 电池。

要分两种模式跑，因为 **`SessionStart` 在 `--print` / `--list-models` 下不会派发**（那里只有 `AgentStart`/`TurnStart`/`BeforeAgentStart`）：

```sh
# 空闲态：RPC 模式会派发 BeforeTuiStart + SessionStart，且不需要真实模型
printf '{"type":"get_state","id":"1"}\n' | rpi --local-only --extensions-dir /tmp/probe-ext \
    --mode rpc --provider openai --model gpt-6-astra --api-key dummy \
    --base-url http://127.0.0.1:9 --no-session

# run 活动态：--print 会派发 BeforeAgentStart；provider 打不通不影响插件侧调用
rpi --local-only --extensions-dir /tmp/probe-ext --provider openai --model gpt-6-astra \
    --api-key dummy --base-url http://127.0.0.1:9 --timeout 3 --print "x"
# → extensions: loaded 1 plugin(s) (0 tool(s))
```

### 实测结果

| 项 | 空闲（`SessionStart`） | run 活动态（`BeforeAgentStart`） |
| --- | --- | --- |
| `abi_version` / `struct_size` | `4` / **`192`**，与 SDK 0.25.0 的 `size_of::<PluginApi>()` **完全相等** | 同上（注册期即固定） |
| 可选槽位 | `register_plugin_lifecycle`、`register_tool_prompt`、`register_tool_with_wake`、`register_streaming_provider`、`register_tool_result`、`register_oauth_provider_cancellable` **全部 `Some`** | — |
| `Abort(27)` | `{"aborted":false,"runId":null,"steer":[],"followUp":[]}` | `{"aborted":true,"runId":"01a1f307-c54b-…","steer":[],"followUp":[]}`；**连调两次得到同一 runId 且都为 `true`** |
| `StopRound(29)` | `{"requested":true}` | 同上 |
| `ListCommands(26)` | 36 条（35 builtin + 探针注册的 `/probe-cmd`） | 同上 |
| `GetExecutionContext(23)` | `sessionId:"ephemeral"`（`--no-session`）、`cwd` 真实、`childAgent:true` | `sessionId` 是真实 UUID |
| `ToolProgram(30)` | `rc=1`，`"ToolProgram requires an authenticated executing invocation"` | 同上 |
| `RPI_SURFACE` / `RPI_DESKTOP_SESSION_ID` / `PI_DESKTOP_SESSION_ID` | 全部未设置 | 全部未设置 |

### 结论

1. **`struct_size` 相等**：`rpi 0.24.0` 宿主的 vtable 与 `0.25.0` SDK 完全一致，不存在“0.24.0 是残缺宿主”。H1/H2/H4 只要把 pin 升到 `=0.25.0` 就能直接用；`GetExecutionContext(23)` 这类“文档没写给插件用”的 action 也能用注册期 `user_data` 正常调用。
2. **H1 契约成立，但要区分两种状态**（本轮初稿只在 `SessionStart` 探过一次，把“run 已存在”误读成“空闲也返回 `true`”）：
   - 真正空闲（无 run）→ `{"aborted":false,"runId":null}`，符合宿主文档；
   - run 已存在 → `{"aborted":true,"runId":…}`，且对该 run **幂等**（重复调用同一 runId、仍 `true`），直到 run 结算。
   - 所以窗格 Stop 可以放心用 `aborted`/`runId` 判断“宿主是否真的取消了某个 run”；本地回复绑定仍以 pane 自己的活动 prompt 为准，两者不冲突。
3. **`ListCommands(26)` 是真实的命令发现面**：它把插件通过 `register_command` 注册的命令一起列出（带 `source`），`rpi-acp` / `rpi-web` 想枚举 `/office`、`/office-tools` 不必再读注册表。
4. **H8 部分收窄**：`/health` 的 `owner.sessionId` / `owner.cwd` 今天就能填（ephemeral 会话的 `"ephemeral"` 需归一化为 `null`）；真正缺的只剩 **surface 标识**与**回环服务注册表**。

### SDK 人机工程缺口（本轮副产品）

写这个探针本身暴露了一个 SDK 层面的缺口：`StbString` / `StepResult` 的**读取与释放全是 unsafe API**，因此在 0.25.0 上不可能写出“零 unsafe”的 out-param 处理器。

| 形状 | 必须写的 unsafe |
| --- | --- |
| 读入参 `StbString`/`StbStringRef` | `StbString::to_string_lossy()`、`StbStringRef::as_str()`、`OwnedString::from_raw()` |
| 写 out 参数（command / 补全 handler） | `callbacks::Output::from_raw(*mut StbString)`（之后 `write`/`clear` 是安全的） |
| 返回工具步骤 | `callbacks::take_step_result()` / `take_step_result_with()`、`StepResult::pending_payload()` |

事件侧有 `event_handler!` 宏把 unsafe 收进 SDK，其余三种形状没有对应宏——于是每个插件都要手写并由自己承担 SAFETY 契约。建议 SDK 补 `command_handler!` / `step_fn!` 一类的宏（或提供接受原始指针的安全构造器），并把每种形状的 SAFETY 说明写进文档。

本扩展的过渡做法（`5da3f47`）：crate 级 `#![deny(unsafe_code)]`，只在确实要碰 ABI 字符串/步骤返回的函数上 `#[allow(unsafe_code)]` 并逐处写 SAFETY 注释。这样新增的 unsafe 必须显式落在带注释的函数上，而不是整个 crate 静默放开。

## 已验证的现状

- 本扩展作为 Rust cdylib 加载进真实 `rpi 0.24.0` 成功：33 个工具（32 目录 op + 发现工具）、2 个 slash 命令、8 个事件、`before_agent_start` 与 2 个 flag 全部注册返回 0，进程无告警退出（`rpi --local-only --extensions-dir <dir> --list-models`）。
- 该真机加载验证抓到过一个真实缺陷（注册后自释放 `StableToolSchema` 字符串 → libmalloc 双释放 abort），说明“注册期字符串所有权”目前只能靠既有插件的写法约定，值得在 SDK 文档里写死（见文末“文档缺口”）。
- 插件侧已具备：自管线程 + tokio 运行时、回环 TCP/WebSocket、同步 HTTP 响应、后台 runtime action 调用、跨线程事件回推。这些**不再需要宿主新增能力**。
- 第二轮补验（未跑真机，仅源码级）：`0.24.0` 的宿主插件桥已能分发 `Abort`/`StopRound`/`RelayUiDialog`/`ListCommands`，且 reload 会在换 session 之前调用插件的 shutdown 钩子。

## 需求矩阵

“状态”列：`缺` = 宿主确实没有；`升级即可` = 宿主有、pin 的 SDK 没有；`插件实现` = 宿主能力就位、本扩展未接。

| 编号 | 优先级 | 缺口 | 状态 | 影响 |
| --- | --- | --- | --- | --- |
| H1 | P0 | 中止当前 run 的 runtime action | 已实现（`5da3f47`） | 窗格 Stop 无效；`client-abort` 无法宣告 |
| H2 | P0 | 插件卸载/重载契约（unload 钩子 + DLL 保活） | 已实现（`5da3f47`） | 桥开着时 reload 可能 dlclose 后线程仍在跑 |
| H3 | P1 | 输入框草稿文本（或补全 provider 槽位） | 宿主已实现（未发版） | 无法实现 `#文档` 补全 |
| H4 | P1 | `register_tool_prompt` | 已实现（`5da3f47`） | 33 个工具只有 description，无 snippet/guidelines |
| H5 | P2 | per-tool / per-plugin 活动工具作用域 | 缺 | `SetActiveTools` 全局替换，与 plan-mode 等有竞态 |
| H6 | P2 | `__rpi.toolName` | 缺 | 目录驱动插件必须为每个工具生成一个 trampoline |
| H7 | P3 | 命令参数补全回调 | 宿主已实现（未发版） | `/office con<Tab>` 无法补全 |
| H8 | P3 | 回环服务注册表 / 前台标识 | 缺 | rpi-web 无法发现桥；`/health` 的 owner 无法区分界面 |

---

## H1（P0）中止当前 run —— 已实现（`5da3f47`）

**复核判定**：宿主 `RuntimeActionId::Abort = 27` 早在 `v0.24.0` tag 就存在并已分发（`crates/rpi-extensions/src/actions.rs`）。首版说“没有中止 run 的 action”是照着 pin 的 SDK 0.3.20 枚举写的，而 0.3.20 的 `RuntimeActionId` 停在 `SideQuestion = 25`。**这不是宿主缺口，是依赖版本缺口。**

**真实契约**（与首版建议形状不同，以 SDK 文档为准）：

```text
Abort = 27,   args: {}
  有 run 在跑 → {"aborted": true, "runId": "…", "steer": [...], "followUp": [...]}
  lane 空闲   → {"aborted": false, "steer": [], "followUp": []}   // 幂等，不是错误
```

要点：
- **协作式**：宿主在下一个 await 点观察取消，action 返回后仍可能有零星流事件到达；
- 用户在这次 run 里排队（steer/follow-up）的消息在 abort 时被**回收并随结果返回**，远程队列可据此恢复而不是吞掉；
- 空闲态返回 `aborted: false`，Stop 按钮天然幂等；
- 同一 run lane 的取消路径与 ACP/HTTP 共用（host trait 方法 `abort`），插件没有第二套撤销逻辑。

**`StopRound = 29`（伴生能力）**：`{"requested": true}`，只阻止游标进入下一轮，不打断正在生成的回答——对应“这轮答完再停”，是窗格 Stop 之外的第二个按钮语义。首版没提到它。

**补完后本扩展的改动**：
1. `Cargo.toml` 的 `rpi-plugin-sdk` 从 `=0.3.20` 升到 `=0.25.0`（顺带解锁 H2/H4 的槽位）；
2. `on_abort_request` 调用 `RuntimeActionId::Abort`，用返回的 `aborted`/`runId` 记录“宿主是否真的取消了某个 run”（空闲时是 `false`+`null`，见第四轮实测），本地回复绑定仍按活动 pane prompt 决定，把 `steer`/`followUp` 按需回灌；
3. `protocol::CAPABILITIES` 加回 `client-abort`；
4. 可选：窗格双按钮（Stop / 答完停），后者走 `StopRound`；
5. `tests/bridge_interop.rs` 的能力断言与 README 差异表同步更新。

**真机实测**（见第四轮）：`Abort(27)`、`StopRound(29)` 在 `0.24.0` 上都能调用并返回预期体；连续两次 `Abort` 返回同一 `runId` 且都是 `aborted: true`，所以**不要用 `aborted` 字段做逻辑分支**，按本地是否存在活动 pane prompt 决定清理即可。

**验收**：窗格按 Stop 后正在流式输出的回合停止，宿主发出 `AgentSettled{outcome:"aborted"}`；pending 的 `office_*` 工具调用收到 `cancel` 并被拒绝；窗格不再收到该回合的 delta/final；空闲时按 Stop 不报错。

---

## H2（P0）插件卸载/重载契约 —— 已实现（`5da3f47`）

**复核判定（首版判断有误）**：宿主有两块机制，且 `v0.24.0` 就有：

1. **DLL 保活**：`crates/rpi-extensions/src/loader.rs::PluginKeepalive` 持有 `Library`，每个 `PluginToolAdapter` 与 `ExtensionEmitter` 各持一份 `Arc` 克隆，`Library` 只在最后一个持有者释放时卸载——即“工具 vec（最后一次可能调用）先于 DLL 消失”。
2. **卸载钩子对**：vtable 槽位 `PluginApi::register_plugin_lifecycle(PluginBeginShutdownFn, PluginFinishShutdownFn, user_data)`：
   - `begin` 必须幂等、非阻塞：置停止标志、取消在飞请求、拒绝新工作；
   - `finish` 在宿主的 blocking 清理线程上调用，**返回非 0 表示“还有代码可能被执行”**，宿主就**永久保留**该 library 与 runtime 上下文——不会“超时后照样卸载活代码”；
   - reload 路径（`crates/rpi-cli/src/session.rs`，换 session 之前）确实调用 `old_session.begin_shutdown()`，即**纯插件 reload 也覆盖**，不只是 `SessionShutdown`。

**剩下的真缺口**：
- 槽位在 pin 的 SDK 0.3.20 里**不存在**（0.8.3 起才有），所以本扩展目前既无法注册钩子，也没有保活之外的保证；
- 宿主侧文档没写：`docs/extensions/lifecycle.md` 通篇没有 `register_plugin_lifecycle`，`docs/authoring.md` 也没说 reload 会走 shutdown 钩子——首版把这段空白读成了“机制不存在”。

**建议形状（修订）**：不再要求新增 `EventTag::BeforePluginUnload`，也不需要单独的 `DllKeepAlive` action；把上面两块机制写进 `docs/extensions/lifecycle.md`（reload/new/shutdown 三条路径、`finish` 返回非 0 的后果、禁止在 `finish` 里回调宿主），并在 SDK 文档里给出最小骨架。

**补完后本扩展的改动**：注册 lifecycle 钩子——`begin` 里 `Server::stop()` 置停止标志、拒绝新的连接/帧；`finish` 里 `close()` + join 桥线程后返回 0。`on_session_shutdown` 现状保留（两条路径幂等）。

**验收**：桥开启状态下触发 `/reload`，进程不崩溃、端口被释放并可重新 `/office connect`；`finish` 人为返回非 0 时宿主保留 library（不 abort）；`cargo test -p rpi-office` 既有用例不受影响。

---

## H3（P1）草稿文本 / 补全 provider —— 宿主已实现，等发版

**现状（第三轮，pi-rust#366 已合入 main）**：宿主按上游 Pi 的形状补齐了两条通道。`EventTag::EditorChange` 仍然**不携带**草稿文本，这是有意的，不是缺口。

- `RuntimeActionId::GetEditorText = 32`：`{}` → `{"text":"...","cursor":N}`（`cursor` 为字节偏移）。永不报错：无编辑器的宿主返回空草稿。TUI 每个 tick 发布一次草稿（含仅移动光标），所以「调用时现在的草稿」是准确的。
- `PluginApi::register_autocomplete_provider`：handler 收到 `{"text","cursor","line","linePrefix"}`，返回 `{"items":[{"text","label"?,"description"?,"insertSpace"?,"isDirectory"?}]}`；返回 `1`、空列表或非法 JSON 都表示「没有意见」，宿主继续问下一个插件并最终回落内置 file/slash provider。插件 provider 排在内置**之前**，单次最多 50 条；`out` 由注册时传入的 `plugin_free_string` 回收。
- 插入侧仍是 `RuntimeActionId::SetEditorText = 19`（`mode: append|replace`）。

两个槽位都追加在 vtable 尾部、ABI 版本不变；旧宿主留 `None` 时插件跳过即可。

**阻塞点**：`rpi-plugin-sdk` 还没发布含这些槽位的版本（0.3.20 / 0.8.3 / 0.25.0 都没有 `GetEditorText` 和两个 completion 槽位），本扩展现在拿不到。

**后果**：上游的 `#` 文档选择器（多文档时还能按 id 区分同文档多窗口）无法实现。本扩展的降级：改用 `office_list_documents` 工具或 `document` 参数，并在 README 标注。

**本扩展要做的改动**（SDK 发版后）：新增 `src/mentions.rs`，注册一个 autocomplete provider，把 pane 列表（host/name/id/focused/windows）作为候选；只看 `linePrefix` 里最后一个 `#` 之后的片段，插入文本为 `#<name>` 或 `#<id>` 且带 `insertSpace:false`。需要按需读草稿时用 `GetEditorText`，不必再考虑给 `EditorChange` 加尾串的方案。

**验收**：输入 `#` 时列出已连文档并可用 Tab/Enter 选中；无窗格时不弹候选；补全不修改既有输入文本。

---

## H4（P1）`register_tool_prompt` —— 已实现（`5da3f47`）

**复核判定**：`PluginApi::register_tool_prompt` 槽位在已发布的 `rpi-plugin-sdk-0.25.0`（crates.io 实包）里就有，追加在 `register_plugin_lifecycle` 之后，不动 `StableToolSchema`、不 bump ABI。首版说的“只差 SDK 版本”方向正确，但要补两点：**只升到 0.8.3 不够**（0.8.3 仍无该槽位），且宿主 `struct_size` 需覆盖新槽位（`0.24.0` 已覆盖）。

契约：`register_tool_prompt(name, metadata_json)`，`metadata_json` 可含 `promptSnippet`（字符串）与 `promptGuidelines`（字符串数组），参数在调用返回前被复制；返回 `0` 成功 / `1` 未知工具 / `2` 元数据非法 / `-1` 不在注册期。宿主槽位为 `None` 时直接跳过。

**补完后本扩展的改动**：`src/tools.rs::register` 里为 32 个 op 注册 `catalog/office-catalog.json` 里已有的 `promptSnippet`/`promptGuidelines`；旧宿主跳过不影响注册。

**验收**：系统提示词的工具区只显示 snippet（完整 description 不进提示词）；`promptGuidelines` 去重后出现；卸载/停用工具后摘要不残留（宿主行为，需宿主测试覆盖）。

---

## H5（P2）活动工具的作用域 —— 仍缺

**现状（0.25.0 复核未变）**：`GetActiveTools` / `SetActiveTools(5)` 操作**整个进程**的工具集，SDK 0.25.0 里没有 `SetToolEnabled` 之类的单工具 action。本扩展与 `rpi-plan-mode` 都采用“读当前集 → 过滤 → 写回”，交错执行会互相覆盖。

**建议形状**：`SetToolEnabled { name, enabled }`（单工具、幂等），或让 `SetActiveTools` 支持 plugin 作用域（只影响本插件注册的工具）。

**补完后本扩展的改动**：`src/active_tools.rs` 保留纯函数对账逻辑，写入路径改为逐工具 enable/disable；与 plan-mode 的竞态消失。

**验收**：plan-mode 与 office 同时变更工具集时，双方意图都保留；并发 100 次对账后集合稳定收敛。

---

## H6（P2）`execute` 缺少工具名 —— 仍缺

**现状（0.25.0 复核未变）**：`ToolExecuteFn(tool_call_id, params, free_params)` 不含工具名，宿主注入的 `__rpi`（`crates/rpi-extensions/src/tool.rs`，常量 `HOST_CONTEXT_KEY`）只有 `cwd`/`sessionId`，且会覆写插件伪造的同名字段。目录驱动的插件只能为每个工具生成一个独立的 `extern "C"` trampoline——本扩展的 `src/tools.rs` 用宏硬编码了 33 个函数，并额外用一个测试保证它与目录一致。

**建议形状**：在 `__rpi` 里追加 `{"toolName":"office_excel_read_range","toolCallId":"…"}`。纯 JSON 扩展，不 bump ABI，旧插件忽略即可（注意沿用现有“宿主值优先、插件伪造被覆写”的语义）。

**补完后本扩展的改动**：删除 33 个 trampoline 与 `registered_names_match_catalog` 之外的样板，改为单个 `execute` 从 `__rpi` 取名字；目录更新不再需要改代码。

**验收**：新增/重命名 op 无需改 Rust 代码即可生效；`__rpi` 仍不出现在模型可见参数与事件里。

---

## H7（P3）命令参数补全 —— 宿主已实现，等发版

**现状（第三轮，pi-rust#366 已合入 main）**：新增 `PluginApi::register_command_completions(name, handler, user_data)`，把 Pi 的 `getArgumentCompletions` 挂到**已注册**的命令上。handler 收到 `{"argumentPrefix":"con"}`（命令名与首个空格之后的全部文本），返回 `{"items":[{"text","description"?}]}`；返回 `0` 成功、`1` 该命令不属于本插件、`-1` 不在注册期或旧宿主无该槽位。

宿主一次 Tab 的询问顺序是：命令参数 → 草稿 provider → 内置 provider。本扩展仍需先 `register_command` 再挂补全，同样等 SDK 发版。

**验收**：`/office <prefix>` 时列出 `status/connect/close/restart/help`，选中后补齐参数。

---

## H8（P3）服务发现与前台标识 —— 仍缺

**现状（0.25.0 复核未变）**：本扩展的 `GET /health` 已暴露 `service/serverVersion/port/protocolVersion/catalogVersion/owner/panes`，`owner.pid` 真实、`owner.client` 硬编码 `"cli"`、`owner.sessionId` 为 `null`。宿主没有 `RPI_SURFACE` 之类的界面标识，也没有回环服务注册表。`GetExecutionContext(23)` 只回 model/thinkingLevel/tools/cwd/sessionId，不含界面。

**部分已可用**（第四轮实测）：`GetExecutionContext(23)` 现在就能提供 `sessionId`/`cwd`，`/health` 的 owner 不必等宿主；ephemeral 会话返回 `"ephemeral"`，需归一化为 `null`。

**两个小需求**：
1. **前台标识**：宿主提供 `RPI_SURFACE=cli|desktop|web`（或等价 env），让 `/health` 能区分界面；`sessionId`/`cwd` 用 `GetExecutionContext(23)` 已可取得，不需要新能力。
2. **回环服务注册表**（可选）：插件注册 `{purpose:"office-bridge", port, healthPath}`，宿主/`rpi-web` 据此列出“已连接文档”面板。这是上游 Pi Desktop 的 Documents rail 在 rpi 侧的唯一合理落点；不提供的话，只能靠用户自己 `curl /health`。

**验收**：`/health` 的 `owner.client` 与实际界面一致；若实现注册表，`rpi-web` 能枚举并显示已连桥（不带端口猜测）。

---

## 宿主已提供、本扩展尚未使用（复核新增）

这些不是“缺口”，但都是本轮复核才确认可用的能力，价值排序如下。

### N1 `RelayUiDialog = 28` —— 把宿主 UI 弹窗中继给窗格

宿主与 TUI 共用一个对话框邮箱：`{"op":"take"}` 取走尚未被插件保留的请求，`{"op":"respond","requestId","answer"}` / `{"op":"cancel","requestId"}` 回填。当前用户在看 Excel 时，审批/选择弹窗出现在终端里，实际上等于丢失。

落点：桥已有一条到窗格的全双工通道，`tool_activity`/`pane_state` 帧之外再加 `ui_prompt` 帧即可把审批搬到 Excel task pane。这条比 H3（`#` 补全）更贴近“office 场景可用”的目标。

注意：TUI 与插件共享邮箱，桥必须先 `take` 再自行渲染，且退出/超时要 `cancel`（0.24.2 起 `UiDialogMailbox::forget` 支持显式释放）。

### N2 `StopRound = 29` —— “这轮答完再停”

见 H1。窗格 Stop 一个按钮装不下两种意图：硬中断 vs 收尾。

### N3 `ListCommands = 26` —— 从宿主侧枚举命令

返回 `{commands:[{name,description,source}]}`，`source` 为 `builtin|extension|prompt`。对本扩展用处有限（`/office` 自己就知道子命令），主要给 rpi-web/远程客户端做 `/` 补全；但可作为 H7 落地前的**替代路径**：窗格输入 `/` 时向宿主取全量命令列表，至少能做命令名补全。

### N4 `ToolProgram = 30` / `ReloadModels = 31` —— 明确不用

`ToolProgram` 面向 invocation-bound 的嵌套工具执行，桥的 32 个 op 是目录驱动的原子工具，套上去只会更复杂；`ReloadModels` 只重读 `models.json`，本扩展不碰 provider。仅记录“知道有、评估过不用”，免得下一轮复核重复发现。

---

## 明确不需要宿主补的

- **网络与并发**：回环 TCP/WebSocket、自管 tokio 运行时、同步 HTTP 响应、后台 runtime action 调用均已可用（`rpi-im-message` 是同类先例）。
- **会话与持久化**：不需要 transcript、会话存储或凭据能力；桥不写会话内容。
- **工具协议**：`execute/poll/cancel/destroy` 的非阻塞 poll、`partial` 进度、结果 JSON 形状都够用；Office 侧只回文本 + JSON `details`。`register_tool_with_wake`（0.8.3 起）也够用但非必需。
- **协议层**：wire v3 的所有帧（`hello`/`welcome`/`tool_call`/`tool_result`/`agent_message`/`tool_activity`/`pane_state`/`abort`/`ping`）都能用现有通道表达。
- **加载项**：Office.js task pane 侧零改动。

## 差异对照（本扩展与上游 Pi 扩展）

| 差异 | 对应需求 | 状态 |
| --- | --- | --- |
| 窗格 Stop 中止 run（`client-abort` 已宣告） | H1 | 已实现 |
| reload 与常驻桥线程 | H2 | 已实现（lifecycle 钩子 + join 桥线程） |
| 无 `#文档` 补全 | H3 | 宿主已实现（未发版） |
| 工具 snippet/guidelines | H4 | 已实现（元数据取自共享目录） |
| 33 个 trampoline（因 `execute` 无工具名） | H6 | 宿主仍缺 `__rpi.toolName` |
| `/office` 无参数补全 | H7 | 宿主已实现（未发版） |
| `/health` 的 `owner.client` 恒为 `cli`；无文档面板 | H8 | 宿主仍缺 surface/注册表 |
| 审批弹窗留在终端而不是窗格 | N1 | 宿主已有 `RelayUiDialog`，未接 |
| 窗格只有一个 Stop | N2 | 宿主已有 `StopRound`，未接 |

## 落地顺序（建议）

1. ~~升 pin 到 `=0.25.0`~~ — 已完成（`5da3f47`）。
2. ~~H2 卸载钩子~~ — 已完成（`begin_shutdown` 关窗格后非阻塞退出，`finish_shutdown` join 桥线程与 worker，返回 0 允许卸载）。
3. ~~H1 中止语义~~ — 已完成（`client-abort` 重新宣告；Stop 先解除本地回复绑定再请求宿主 `Abort`，`aborted` 仅记录日志）。
4. ~~H4 提示词元数据~~ — 已完成（33 个工具的 snippet/guidelines 取自共享目录）。
5. **N1 审批中继**：`RelayUiDialog` + 新 `ui_prompt` 帧。
6. **第 2 步之后即可做（宿主已实现，等 SDK 发版）**：H3（`#文档` 补全）、H7（`/office con<Tab>`）。
7. **仍等宿主**：H6（`__rpi.toolName`，收益最大）、H5（单工具作用域）、H8（surface / 服务注册表）。

## 文档缺口（非宿主代码）

- `StableToolSchema` 字符串的**所有权**应写进 SDK 文档：宿主复制并通过 `plugin_free_string` 回收，插件不得自行释放（本次移植按约定才避免了一次 libmalloc 双释放；现有插件只是“碰巧”没释放）。
- `EventTag::EditorChange` 应显式说明“无文本”是有意为之，并指向替代方案（当前只有 `SetChars`-类信息）。否则每个想做补全的插件都会重新发现这一点。
- `docs/extensions/lifecycle.md` 应写 `register_plugin_lifecycle`：`begin`（幂等、非阻塞、取消在飞工作）与 `finish`（blocking 清理线程、返回非 0 = 宿主永久保留 library 与上下文）、reload/new/shutdown 三条路径都会调用、`finish` 内禁止回调宿主。**这一条是本轮复核中唯一被误解成“宿主缺口”的文档空白。**
- `docs/extensions/authoring.md` 应把 `Abort` / `StopRound` / `RelayUiDialog` 三个 action 与它们的返回形状写进 action 清单，并提醒 `abort` 的 `steer`/`followUp` 回收语义——否则停按钮的实现者会重复发明一套本地撤销。
- 建议在 SDK 文档里给一张“能力 → 引入版本”表（本文的“版本事实”表可直接搬用）。当前最大的坑是：**`RuntimeActionId` 的枚举长度取决于插件 pin 的 SDK 版本，而它看起来像宿主能力清单。**
- SDK 应为 out-param 处理器与工具步骤返回补安全包装（见第四轮“SDK 人机工程缺口”）：`StbString` 的读取/释放与 `take_step_result` 目前全是 `unsafe fn`，事件侧有 `event_handler!` 宏兜住，命令/补全/步骤三种形状没有，插件绕不开手写 `unsafe`。
