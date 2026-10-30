# 真实 Zed 1.25.3 × rpi ACP：验收与菜单差异

**2026-10-31 · 四项定向缺陷已修复，并在新冻结工件的真实 Zed run12 验收通过；未部署。原全能力目标仍 PAUSED / INCOMPLETE。**

最新进展为下方run12。run11的compact失败和图片重载未验是旧907工件的历史结果，保留而不改写；不能迁移旧native/GUI PASS认证新工件。全部ACP能力、外部账号、子链优雅退出等未因此完成。

## run12：新工件四项真实 GUI 验收通过 — 2026-10-31 00:03–00:12 +08:00

**根因已确认并修复，不再只是时序嫌疑。** 固定Zed commit `8dd255c3eafb5231056fe4e6f601cedd011f1c93` 的 `crates/agent_servers/src/acp.rs`：`new_session`等待成功响应后才注册session，`handle_session_notification`忽略未知session通知。旧907在响应前公告commands，客户端丢弃，符合run11未知命令。`protocol.rs`现由同一个受监管request task准备inventory，再先写成功response、后写commands；new/load/resume共用，不sleep、不另建detached任务。load历史/v2状态同步仍在响应前，失败/取消/输出失败不公告；native/extension/v2门控不变。ACP仅修协议输出时序，无native源码变更或第二套history。

新冻结 **`C:/Users/bigfish/.cargo/target/acp-zed-commands-01/bin-1/rpi-acp-bridge.exe`**，SHA256 **`66ecf4ce0c56d7bec76691f7909e493bccca211689d912a8646dc69d1dc927e4`**；native仍`acp-reviewed-final-bin/rpi.exe`的b1b。新隔离根`acp-zed-real-1253-06/`，tag12，Zed1.25.3，`rpi-reviewed`实际permission gate、**非bypass**。旧907/harness PINS未覆盖。56个实际ACP frames、8个本地模型HTTP请求、0fixture failures、5条canonical user messages。

| 项目 | 新工件真实结果 | 证据与限定 |
| --- | --- | --- |
| Default/Plan | **PASS，菜单及实际权限约束** | Plan实际schema无write/edit/bash，fixture强制write后界面`Tool write not found`，转Default前canary不存在。Default同参数write，实际GUI Yes单次批准，wire按allow_once kind关联optionId0，内容严格落盘。`12-10`–`12-12`、`12-plan-no-file.json`。 |
| `/compact` | **PASS，补全、选择和真实执行** | `/`显示compact及说明，Tab选择/Enter提交，Zed显示**Context Compacted**；真实wire收到`/compact`prompt、相同compactionId的in_progress→completed，native只持久compaction摘要`ZED_COMPACTION_SUMMARY_12: controlled GUI history summarized.`，不插入普通compact用户消息。`12-04-slash-inventory.png`、`12-05-compact-complete.png`。new/load成功响应均先于恰好一次commands公告。 |
| 图片历史重载 | **PASS，真实整桥Reload Agent与原生存档保真** | GUI Image对话框发送PNG；通过真实菜单Reload Agent启动新proxy/bridge，Zed发送同session的`session/load`，图片chunk和文字按原顺序在load响应前回放，不调用模型。原native ID`01a18d90-d22a-7201-9bbb-75fc6b01a77e`映射为`history:<原ID>`；wire/native/replay/model均73bytes、image/png，SHA256`18f4fa5bba0f3aee92c9c6c7b85367a2247e976a9ff46056a06338685487cfc1`。`12-07`、`12-09-agent-reloaded.png`的generic Image chip证明附件恢复，**不等于像素渲染质量验收**。源PNG4ff6…进入ACP前被Zed重编码，未声称源文件原字节保持。 |
| 目录完整URI | **PASS，真实picker→native/model** | `@attachment-dir-12` Tab选目录；完整`file:///C:/Users/bigfish/.cargo/target/acp-zed-real-1253-06/workspace/attachment-dir-12/`保留到native用户消息和模型，无目录canary内容。`12-13-directory-sent.png`。 |

**验收与清理证据：** `run12-feature-evidence.json` **24/24**严格断言通过，真实GUI不是模拟ACP客户端。`parent-run12-verified-02.json` **13/13**补充父核验通过；`job-lifecycle-12.json`持有句柄确认17个真实Job成员全退出/句柄关闭。实际菜单`zed: quit`正常退出，Zed exit0、owner无force、Job关闭无额外强杀，57400监听关闭、未下载HTML。**但桥/native/proxy含exit1，proxyOwnersSettled=false且proxy状态仍running；不能称子链graceful/joined。** 固定Zed的AcpConnection Drop会kill子进程。owner脚本按此状态返回失败，不把资源均已退出当作成功join。

**保留一项父验证失败及历史安装漂移更正。** `parent-run12-verified.json`首次核验错误使用旧安装版rpi的b4e…作为本轮baseline，检查失败、exit1完整保留；四项feature的24条断言未失败。实际`runs/12/manifest.json`启动前已记录安装版`aafabde461f6d7d3bed7b98bcbb4304c846e0fe02921db1e62f872c57fd33e0d`，收尾相同；run09/10/11启动manifest同样已是aafab…。文件LastWriteUtc为2026-10-30T14:19:51.528269Z，**历史变化来源未知**。补充父核验严格比较本轮真实before/after，并单列相对b4e的历史漂移，不覆盖首次FAIL，不重新写入/恢复安装版。正式settings仍b208…、ACP DLL782c…、全部旧新冻结运行工件和gate hash匹配。本报告此前“安装未变”应理解为各次运行前后，不应继续把b4e写成run09–12的当前安装身份。

### 本轮源码和分层回归

证据根`C:/Users/bigfish/.cargo/target/acp-zed-commands-01/`：

- `src/protocol_command_order_tests.rs`三项：成功响应先行且一次公告、失败/取消抑制、BrokenPipe不公告；通过。`tests/command_ordering.py`对旧907严格失败（red-907），新66ec三项通过（green-order），覆盖v1 new/load/resume、513条load replay在响应前、旧native无假公告/v2无本地compact shortcut。`history_rpc`仅opt-in公告compact测试能力，不伪装执行。
- ACP193单测通过。继承环境的整包命令两次在remote_http两项BrokenPipe/Reset失败，`packages.log`、`packages-serial-02.log`保留；**不能称原整包命令成功**。同一HTTP测试二进制在正向环境allowlist下2/2通过；ACP/extcore其余test binaries在同一clean launcher下全部exit0，`clean-suite.log`、`clean-suite-results.json`。本次不是四包489项全量重新认证。
- scoped ACP/extcore all-target strict Clippy（test-support，`-D warnings`）通过。新冻结工件history_protocol **6/6**、真实native兼容回归 **31/31**（含实际compact、mode、媒体重载），均非GUI替代；history旧harness ResourceWarning日志保留。frontend未改变，此前typecheck通过，本轮未重跑。

四项定向缺陷现已具备源码、严格回归及真实Zed的交叉证据；**不部署、不提交**。run12已stopped，无live owner可续接。外部AI/账号、完整Skills/MCP/子代理矩阵、子链优雅关闭和原全能力目标继续保持未完成。下文是逐轮历史快照。


本报告中的 GUI 结果来自实际 Windows Zed 进程和鼠标/键盘操作，不是 ACP 测试客户端、截图 mock 或浏览器路由 fixture。模型服务仍是本地可控 HTTP fixture，不能据此宣称外部 AI、真实账号或日常 MCP 已验收。

## run11：模式/目录通过，compact仍未知命令，图片重载未验 — 23:10–23:34 +08:00

恢复桌面后先续接run10，真正选择`rpi-reviewed`，907bridge完成Zed initialize/new，Default控件可见；6个ACP frames、0模型调用。run10按23:06期限结束，12个真实Job成员持有句柄均退出并关闭；root强制fallback、Zed/native exit1、bridge exit0，不称正常quit。父证据`parent-run10-verified.json`与`runs/10/`已保存，旧“仍live”暂停记录只代表当时状态。

核验终态后启动新tag11（1800秒owner）。仍为**90745044… bridge / b1b… native / Zed1.25.3**，`rpi-reviewed`使用隔离permission gate，**不是bypass**。实际GUI→ACP→native→本地HTTP fixture，37个ACP frames、6模型请求、0fixture failures，4条canonical user message。本轮只改target测试harness和报告，没有修改/构建runtime源码或部署。

| 项目 | 真实结果 | 证据与边界 |
| --- | --- | --- |
| Default/Plan | **PASS，本轮真实菜单+约束** | `11-02`可见Default/Plan；GUI选择Plan ACK，模型schema只有read，fixture仍强制write，界面Tool write not found，转Default前明确无canary。Default同一参数write可用，Zed显示Allow write?，点击Yes一次，wire由allow_once kind映射optionId`0`，`fs/write_text_file`执行，实际文件内容逐字匹配。`11-plan-pre-default.json`、`11-default-write-result.json`、`11-03`至`11-05`。持久化/重启模式仍只引用旧native层验证。 |
| `/compact` | **FAIL，不能交付** | 真实键盘`/`及`/co`都无命令补全，提交完整`/compact`后Zed报“not a recognized command”；`11-08-compact-execution.png`。wire确实在new响应之前发送available_commands_update（frame index3/response4）；这是**时序嫌疑，不是已确定根因**。Zed未转发该prompt，无模型压缩请求、无普通compact canonical消息；不能用旧native31项替代GUI失败。后续先读Zed命令注册路径/实际adapter调度，添加严格时序回归，再修复、冻结新hash重验。 |
| 目录URI | **PASS，本轮GUI选择目录→完整URI入模** | `@attachment-dir-09` picker Tab选择目录，发送`ZED_DIRECTORY_11`，`11-13`/`11-14`。wire resource_link URI为`file:///C:/Users/bigfish/.cargo/target/acp-zed-real-1253-05/workspace/attachment-dir-09/`，模型文本完整包含同一URI及补充name。未含`ZED_DIRECTORY_CANARY_09`，未把目录内容嵌入prompt。此处不是同名不同URI或query/fragment GUI验证，后者仍在单测/native层。 |
| 图片历史 | **输入链PASS；GUI历史重载NOT TESTED** | 真实Image对话框选择PNG、发送`ZED_IMAGE_11`，`11-12`有图片attachment chip。ACP wire/native canonical/model均73bytes，PNG MIME相同，SHA256 **18f4fa5bba0f3aee92c9c6c7b85367a2247e976a9ff46056a06338685487cfc1**。源文件SHA2564ff6…与wire不同，表示**进入ACP前已重新编码**，不能称源文件原字节；ACP之后严格相等。原消息ID`01a18d68-f0df-7fc0-b82e-cf93166eb4c3`。开始找历史重载入口时OpenInputDesktop error5立即停止，未执行整桥/线程重载，不能当作图片回放已验。 |

GUI helper仅在target内补physical-DPI坐标（早期点击未命中记录保留）、owned Unicode文本（含owned native dialog），每次输入检查PID+创建时间/foreground，未切系统语言、读写剪贴板或绕过桌面。首个图片对话框SendKeys受IME影响截断路径，原dialog截图`11-11`保留；修正为受保护Unicode输入后确实发送图片。原始`11-10`是黑图，不作成功证据。一次PowerShell日志读取缺省ANSI导致JSON解析失败；后续Python严格UTF8读取全部37帧成功，未改写日志。

**严格证据脚本第一次失败必须保留。** `run11-feature-evidence.json`错误地把allow_once kind当成optionId，且要求源PNG文件与Zed发送bytes相同，两个断言失败，其预先写出的PASS label无效。依据实际permission options和wire/canonical字节诊断后，更正测试契约为kind→optionId关联、源文件重编码单独记录，仍严格断言wire/native/model三者bytes+MIME一致。权威`run11-feature-evidence-02.json` **15/15**证据断言通过，**不含compact成功或图片重载断言**；不是四项GUI全绿。

出现error5后不再GUI，发送owner STOP-11，WM_CLOSE等待失败，只由原owner对自有Popen强制fallback。23:33:58最终**stopped /root forcedKill=true /Zed/native exit1**；10个真实Job成员均在持有句柄下确认退出并关闭，Job关闭无需另杀，proxy owners settled，本地61239监听关闭。正式settings b208…、安装rpi/DLL、冻结PINS全未变；**正常quit未通过**。父级`parent-run11-verified.json`10项检查通过但结果仍PARTIAL_GUI_WITH_COMPACT_FAILURE。归档`runs/11/`，源码/native旧失败不删除。

**下一步不是继续点击旧窗口：run10/11均已退出。** 先定位修复compact客户端注册时序/契约；新工件再用tag12+。图片回放需新的合法前台GUI线程/整桥重载，与既有native exact-byte回归分层计证据。不提交、不部署。

## run10：已取得前台并进入 Agent 菜单，随后桌面 error5（当时暂停记录，终态见run11节）

用户再次要求继续后，22:46:31 +08:00 在同一隔离 profile 启动新 tag **10**（未覆盖run09归档），再次校验冻结PINS。只读等待实际确认前台PID属于本轮Zed，并由窗口PID+创建时间保护全部输入。实际操作已确认测试项目、打开命令面板和外部Agent菜单；截图 `10-01-foreground.png` 至 `10-04-agent-menu.png` 保留。首次文本输入被中文IME组合，原截图保留；仅target下GUI helper新增逐字符检查owned foreground的Unicode SendInput，英语命令在真实菜单正确显示，没有切换系统语言/剪贴板或修改runtime。

准备点击 `rpi-reviewed` 时窗口失去前台，保护在点击之前拒绝；随后只读/owned-window入口直接返回 **OpenInputDesktop error5**，立即停止全部GUI操作，未绕过。尚未选择agent，**四项修复GUI仍全部NOT TESTED**，不把外部Agent菜单或冻结hash配置当成907处理过真实Zed ACP请求。

本次先暂停并保留有期限的owner，方便恢复桌面后续接：`acp-zed-real-1253-05/pause-run10.json`、`status.json`、`job-observed-live.json`；截止时间 **2026-10-30 23:06:31 +08:00**，到期owner自动启动清理。暂停时2个已验证Job进程成员仍存活，**退出、最终句柄回收与normal quit尚未验证，不是清理PASS**；正式settings hash当前仍b208…，无部署。恢复前必须先检查status/runTag及deadline：若tag10仍running且桌面/前台归属均合法才续接；若已stopped，先检查 `job-lifecycle-10.json`、`runs/10/` 与终态hash，再开tag11+，不覆盖失败记录。原全能力目标保持暂停。

## run09：桌面可访问，但测试窗口无法取得前台 — 2026-10-30 22:23–22:35 +08:00

本次按用户“继续”实际重试 GUI，不是重复运行既有 native fixture。22:23 预检 `OpenInputDesktop` 成功，error0、foreground 非零；重新准备 `C:/Users/bigfish/.cargo/target/acp-zed-real-1253-05/`，明确校验 bridge **90745044…** / native **b1b…** / Zed 1.25.3 / gate 冻结 hash，创建新隔离 profile、Desktop、测试文件和本地模型服务，使用新 run-tag **09**。未修改 runtime 源码、已安装程序或正式设置。

Zed 实际启动，截图 `01-start.png` 显示新测试 workspace 的 Unrecognized Project / Restricted Mode 信任对话框。但 `SetForegroundWindow` 未能将测试窗口置于前台，owned-input guard 在发送第一个 Enter **之前**拒绝；UIAutomation 对同一已验证窗口的 SetFocus 也失败。两次只读复检仍是可访问桌面、非零但**不属于测试 Zed**的前台 PID，不能将其误记成 error5/锁屏。请求用户配合前台后的交互等待超时，最终仍非 owned foreground；未使用全局按键、线程输入绑定或桌面绕过。

**结果：BLOCKED_BEFORE_ACP_INITIALIZE。0 ACP frames、0 模型请求、四项 GUI 子项全部 NOT TESTED。** 907 bridge 已校验并配置，但没有启动外部 agent，不能声称该 bridge 已实际处理 Zed 请求。没有模式菜单、slash、媒体重载或目录 URI 的新增通过证据。

发送 owner STOP 后，WM_CLOSE 的10秒等待未退出，owner 仅对原 Popen 的 Zed 执行强制 fallback，**forcedKill=true、Zed exit1，正常 quit 不通过**。本轮使用真正 Windows Job 成员验证而非 PPID 猜测，提前持有2个 job-member进程句柄；最终均已退出、句柄关闭，job关闭无需额外强杀。proxy 集合为空，本地模型 listener 已关闭，无 HTML 下载；正式设置 hash 仍 b208…，安装版及全部旧/新冻结工件 hash 均未变。失败、等待超时、强杀和初始化前阻塞都保留，不归因于 ACP 修复缺陷，也不记成 GUI PASS。

父验证：`acp-zed-real-1253-05/parent-run09-verified.json`；归档 `runs/09/`；退出证据 `job-lifecycle-09.json`；前台证据 `focus-refusal-preflight.json`、`post-user-wait-preflight.json`。下一次需新 tag（10+），并在开始时配合把**隔离测试 Zed**留在前台；run09 已安全回收，不继续复用正在运行的实例。下节源码/native结果仍有效，但不替代此 GUI 步骤。

## 定向修复与验证 — 2026-10-30，未部署 / GUI 回归受阻

用户授权从验收转为修复后，实施以下最小接线与投影修复：

| 缺陷 | 本次实现 | 当前验证层级 |
| --- | --- | --- |
| Default / Plan 不显示 | 将 native `get_modes` 投影为 mode select；要求 native 同时公告 get/set 且当前值有效；config setter 与 legacy setter 同步配置，v1 保留 current_mode_update，v2 不发送 legacy 更新 | 真实 native v1/v2 ACK、非法值拒绝、close/reopen 和整桥重启后的模式保持；Plan 实际 schema 无 write/edit/bash，强制 write 不落盘，Default 同一工具真实写入 |
| `/compact` 不可达 | v1 new/load/resume 公告内置 compact，不依赖扩展目录；保留显式启用且可执行的扩展命令，compact 去重且内置 handler 优先；v2 不虚假公告 v1 shortcut | 真实 native 压缩请求、持久化模型摘要、对应 ACP 进行中/完成更新、无普通 `/compact` 用户消息；空历史 declined 及时返回且下一次 native prompt 正常 |
| 图片历史丢失 | 共享有界历史投影补 image/audio，保留原顺序、data、mimeType、messageId；不另存 ACP transcript | 真实 PNG/WAV 入模；整个 bridge 重启后 v1 load/v2 resume(start) 与原生 entry 逐块、逐字节、原 messageId 相同，回放不调用模型 |
| resource_link 丢 URI | name 仅作为补充标签，完整 URI 始终保留；URI-only embedded resource 同样保留 | 单测区分同名不同 URI；真实 native 模型收到含 query/fragment 的完整目录 URI，不扫描目录或获取 URL |

真实 native 测试还找出此前未到达的压缩执行缺陷：native 返回异步 `accepted:true`，旧桥误当同步结果而报 `Native compaction declined`。修复置于 **extcore**：复用前台 lease，发送前订阅、按 ACK/compactionId 关联，等待 `agent_settled` 后才释放；保留旧同步 outcome。总 deadline 120 秒，提交后的取消/caller drop 最多 drain 3 秒，无法确认结算时终止 backend；未入队取消直接释放，不误杀健康 backend；合法 declined 也必须结算。新增 **22 个** core 生命周期单测。ACP 仍仅映射状态，无第二套运行时/历史库。

### 工件与实际检查

全部新产物在 `C:/Users/bigfish/.cargo/target/acp-zed-fixes-01/`：

- **新 bridge**：`bin-3/rpi-acp-bridge.exe`，SHA256 `90745044a44a7d8562ca614f3e87b19b7cc4b907c0a09aaba81ee76e05d8e302`。
- **native 未变**：原 b1b 冻结工件；旧 759 bridge、安装版 rpi/DLL 和正式 Zed 设置均未替换。
- 四包 ACP/extcore/MCP core/MCP adapter（含 extcore test-support），串行测试分两条完成：**489 top-level PASS、0 failed、3 ignored，另 1 个 nested TLS subprocess PASS**。`packages-final-acp-core-adapter.log`、`packages-final-mcp-core.log`；不是 whole-native/workspace 检查。
- 同范围 all-target strict Clippy、frontend `npm run typecheck` 通过：`clippy-final.log`、`frontend-typecheck.log`。
- 最终冻结 bridge 的有界历史协议测试 **9/9**、canonical insertion/cancellation **5/5**：`history-protocol-final.log`、`prompt-correctness-final.log`。
- `tests/zed_compat_native.py` 的最终 `native-05/result.json`：**31/31**，含诊断/清理子项，**17 个真实 native → 本地模型 HTTP 请求**；不是 31 项 GUI 操作。另有 7 个 Windows 归属判定安全自测。

测试模型/会话/环境均隔离且使用正向环境 allowlist，无真实凭据或外部账号。此 native 回归显式使用 `RPI_ACP_NO_PERMISSION_GATE=1`，仅验证 native mode 工具策略，**不属于审批验收**。5 个 root 均 exit0、读取线程及 HTTP owner 已 join；10 个 native 在 EOF 前按精确冻结路径、创建时间/父链验证并持有句柄，实际均退出、code **1**。没有 force fallback，不把 native exit1 记作优雅退出/exit0。旧 PPID-only 归属算法会因 PID 重用误计旧进程，现已修正；不终止未验证候选。

### 失败保留与结论边界

- `native-01` 保留真正的 compact ACK/终态处理失败。`native-02` 已完成压缩，但初版测试误要求 ACP 状态行等于模型摘要；按 native `compaction_json` 的实际契约改为**同时精确检查 canonical 摘要和独立状态行**，不是改 runtime 迎合测试。
- `native-04` 的父检查保留 PPID-only 归属产生的 error5/unknown，不宣称这些 PID 是 owned 或已退出。`native-05-attempt01` 保留新增 native-exit0 预期失败；最终持有句柄记录实际 code1，退出不等于 graceful。
- 历史协议 fixture 补齐真实 v2 canonical insertion，并断言只响应一次、取消后 idle/cancelled；remote fixture 补齐它所广告命令的 execute 能力。没有降低生产协议约束。
- 旧 `protocol_regression.py` 广泛套件仍 **2 PASS /6 FAIL /1 SKIP**；未修复的 PID 日志/过期 v2 MCP、replay 断言在旧 759 bridge 对照中得到**同样失败集合**。两份日志保留，不宣称所有历史测试全绿。
- 首次并行包测试保留两项 Windows socket WouldBlock 失败；未改 MCP 实现/断言，串行完整回归通过。一次总命令超过工具 120 秒上限的中断日志也保留，确认无遗留 cargo/provider 进程后分组重跑。
- 新 GUI 预检 `desktop-preflight.json` 返回 **OpenInputDesktop error5、foreground0**；立即停止，未启动新 Zed、未输入、未绕过桌面。因此 **Default/Plan 菜单可见、Zed slash 选择、真实 GUI 图片重载和目录附件四项新工件回归仍待桌面可交互后完成**。旧 CRLF 等失败不改写。

下文各轮 GUI 结果是旧工件历史快照，不能迁移成新工件 PASS。正式设置及安装工件的 SHA256 检查、受控 listener 关闭和持有句柄退出证据见 target 下的最终父验证文件。

## 工件与隔离

| 对象 | 实际身份 |
| --- | --- |
| Zed | Scoop `1.25.3`，commit `8dd255c3eafb5231056fe4e6f601cedd011f1c93`；exe SHA256 `9e8902cf9ee5e6a25367120e1e1a95d9185fbaf18c1ba9e84a0f93a8e13cc6a9` |
| 实测 bridge | `acp-wire-final-bin/rpi-acp-bridge.exe`，SHA256 `759eaf88e486ffd624c8f3d764666eeb819b03786692faf54fb144559c852e6a` |
| 实测 native | `acp-reviewed-final-bin/rpi.exe`，SHA256 `b1b371966a3dfe6227e863e8a90655059dea86218d49d8bda4ef9564eb241888` |
| 实测插件 | 仅冻结 permission gate，SHA256 `526214d2079104ba7773d0076959af08b144f6b492da19f87caf62e7fed2870b`；本轮没有装载 ask_user/subagents 插件 |
| 已安装 native | `C:/Users/bigfish/.cargo/bin/rpi.exe`，`--version` 为 `rpi 0.11.0`，SHA256 `b4e652670830a6e54ea0ccd15e443ea74d18dd55394ac1ff11439c696029294f`；后续 runs 02–05 已完成限定 GUI 对照，见末节 |
| 已安装 ACP DLL | SHA256 `782c71bed31640600888d2e486fcfd6cfa46262bb40150a71e7da478f87e0970`；后续安装版入口实际加载，不等同于冻结 bridge |

以上 target 相对路径均位于 `C:/Users/bigfish/.cargo/target/`。注册表另有 Zed 1.26.2 卸载记录，但对应 exe 路径不存在，不能把它当作本机实际测试版本。

实际运行目录：`C:/Users/bigfish/.cargo/target/acp-zed-real-1253-03/`。使用真实 Zed `--user-data-dir <root>/data <root>/workspace`；该精确版本的路径实现确认配置位于 `<root>/data/config/settings.json`。HOME、用户资料、agent、会话和 TEMP 均隔离在该 C 盘目录，无用户密钥继承；`RPI_ACP_NO_AUTO_REGISTER_ZED=1`。

正式 Zed 设置只做脱敏检查和前后 SHA256 比对，**未修改**。没有更新 Zed、替换已安装 rpi/DLL、部署或提交。须单独披露一个隔离副作用：Zed 首次启动按默认 `auto_install_extensions.html=true` 自动下载了 HTML 扩展，位置仅在测试数据目录 `data/extensions/installed/html`，并非正式扩展目录。没有执行依赖安装命令；后续启动必须显式设置 `auto_install_extensions: {"html": false}` 和 `suggest_extensions: false`，不能再笼统声称“零安装副作用”。既有测试目录/文件均保留。

两个早期隔离启动目录 `…-01`、`…-02` 保留了启动失败证据：自建 Windows profile 缺少规范 `AppData/Local` 路径，Zed 在 `paths.rs:204` panic。创建规范隔离目录后 `…-03` 启动成功。这是 harness 隔离问题，不归因于 rpi ACP。

## 已确认的菜单缺口：Default / Plan 消失

**优先修复项：ACP 配置投影与 Zed 选择器优先级不一致。**

真实 `session/new` 成功响应同时返回：

- `modes.currentModeId = default`，`availableModes = [Default, Plan]`；
- 非空 `configOptions`，仅包含 model、thinking、auto_compaction，**没有 mode 项**。

真实截图 `04-reviewed-open.png` 的底栏确实只有模型、Thinking 和自动压缩，没有模式选择器。对应 trace 第 5 行保存完整响应。

精确 Zed 源码解释了该现象：

- [`agent_servers/src/acp.rs:9436–9450`](https://github.com/zed-industries/zed/blob/8dd255c3eafb5231056fe4e6f601cedd011f1c93/crates/agent_servers/src/acp.rs#L9436)：只要 configOptions 为 Some，就返回 `(None, Some(opts))`，不保留 legacy modes。
- [`agent_ui/src/conversation_view.rs:1511–1527`](https://github.com/zed-industries/zed/blob/8dd255c3eafb5231056fe4e6f601cedd011f1c93/crates/agent_ui/src/conversation_view.rs#L1511)：优先创建 ConfigOptionsView，不创建旧模式/模型选择器。

这不是“Zed 外部 Agent 一律不支持模式”，也不能用 Zed 内置 Ask/Write/自定义 Profile 与 rpi Default/Plan 的语义差异解释掉。最小修复方向：在 ACP configOptions 中投影真实 native modes，处理对应 `session/set_config_option`，复用现有中性 mode setter，并同步旧 `session/set_mode` 的配置更新；保留 v1 旧入口，勿广告 native 不存在的模式。**该 GUI 批次只定位；后续定向源码修复见上节，尚无新工件 GUI PASS。**

## 实际 GUI / wire 验收矩阵（首轮历史快照）

`PASS` 只针对本表明确限定的操作；不代表该功能所有场景均通过。

| 项目 | 本轮结果 | 实际证据 / 限定 |
| --- | --- | --- |
| 外部 Agent 发现、启动、初始化、新线程 | PASS | `03-agent-menu.png`、`04-reviewed-open.png`；真实 Zed 发 ACP v1 initialize，bridge/native 成功建会话 |
| 模型/Provider 菜单 | PASS：展示、分组、切换 ACK | `05-reviewed-model-menu.png`；选择 `local-alt/fixture-reasoning` 后成功响应和 UI 更新；模型服务收到 `fixture-reasoning`。两 provider 共用本地 endpoint，不另宣称已验证外部 Provider 路由 |
| Thinking 菜单 | PASS：展示、切换 ACK | `06-reviewed-thinking-menu.png`；选 High，`session/set_config_option` 与回传 currentValue 均为 high；不据此证明外部模型真实推理质量/effort 字段 |
| 自动压缩布尔开关 | PASS：展示、关闭 ACK | 真实 Zed 发 `configId=auto_compaction,type=boolean,value=false`，成功更新；没有实测上下文达到阈值后的自动压缩 |
| Default / Plan 菜单 | **确认缺口** | native modes 已返回，但 configOptions 没有 mode；真实 UI 缺失，源码优先级吻合 |
| 文本对话与消息显示 | PASS，fixture 限定 | `07-reviewed-chat.png`；真实 session/prompt → native → HTTP SSE fixture → `agent_message_chunk` / end_turn |
| 读文件 | PASS | `08-reviewed-read.png`；Zed `fs/read_text_file` 实际回传 `ZED_NATIVE_SAMPLE\n`，随后 native 接到工具结果 |
| 写文件拒绝 | PASS | `09`–`11` 截图；选 No，native gate 报 blocked；当时文件不存在；trace 在拒绝后没有 write_text_file 调用 |
| 写文件允许 | PASS：文本与落盘；严格 LF 字节检查 FAIL | `11`–`13` 截图；选 Yes 后唯一 write_text_file 调用；实际文件内容正确，但最终磁盘为 CRLF，非模型请求的 LF。不可声称逐字节写入一致，亦未据此认定 ACP 违反规范 |
| 编辑审查入口 | 部分确认 | 实际出现 Edits / Review / Reject / Keep 控件；`15-reviewed-edit-review.png`。未取得展开 diff 编辑器的成功证据，不将截图 `16` 当作 diff 验收 PASS |
| Bash / Zed terminal | PASS，短命令限定 | 允许后 create → wait_for_exit → output → release；实际输出含 `ZED_NATIVE_BASH_OK`、exitCode 0，release 成功；没有长命令/terminal UI 完整矩阵 |
| 取消与继续对话 | PASS | `17`–`19` 截图；真实 session/cancel，fixture 观察连接关闭，stopReason cancelled；同会话随后新 prompt 正常 end_turn |
| 上下文附件入口 | 部分确认 | `20-reviewed-context-menu.png` 显示 Files & Directories、Symbols、Threads、Image、Selection、Branch Diff；文件完成选择与实际发送未完成，图像/选区也未验收 |
| Slash / Skills / MCP | **未完成 GUI 验收** | 实测仅收到 `available_commands_update: []`，隔离环境没有命令插件；这不证明用户日常扩展环境也没有命令。实际 MCP 配置/调用与 skills picker 尚未操作 |
| 历史、导入、load/resume、rename/archive、fork | NOT TESTED | 协议能力与源码支持不替代真实操作；未打开这些菜单 |
| 子代理、表单、URL、OAuth | NOT TESTED | 当前测试未加载相关工具插件；真实 initialize 的能力见下节，不能把能力广告当作 UI 成功 |
| 安装版 / bypass 入口对比 | NOT TESTED | 菜单中可见测试入口，但锁屏前仅启动了无 bypass 的 reviewed 入口；安装版 `--version` 不是 GUI 对照 |
| 普通关闭窗口/退出、重启恢复 | **未通过 / 待复测** | 桌面不可交互后触发 owner STOP；10.28 秒后进程退出码 1，符合强制 fallback 而非正常 quit。不可计为优雅退出 PASS。重启恢复未操作 |
| 测试资源最终清理 | PASS，owner 层限定 | bridge exit 0、proxy liveThreads=[]，已知 owned PIDs 均不存活，无本地模型监听，正式 settings hash 不变；不等同于 Zed 正常退出成功 |

## 配置与协商：不能误判为“ACP 全功能缺失”

正式设置只读检查发现：

1. rpi、rpi-hualong、rpi-writer 都有 `--bypass`。无审批可能是明确配置，不是没实现审批。本轮无 bypass 的真实 UI 已验证允许/拒绝。
2. rpi-hualong 的 args 有单个字符串 `"--provider deepseek"`，而不是 `"--provider", "deepseek"`。这是待修正的 argv 风险；本轮没有启动该正式入口，不能断言它就是用户所有缺失菜单的原因。
3. 正式入口使用安装版 native / ACP 插件，与本次冻结工件不同。不能把本报告直接推广到已安装版本。

真实 Zed initialize（trace 第 1 行）发 `protocolVersion:1`，有 filesystem、terminal、boolean configOptions、compaction、notices、form/URL elicitation 等能力；**没有** rpi 私有 unstable-session revision，也没有 subagents opt-in。与精确源码 [`acp.rs:799–830,1026–1033`](https://github.com/zed-industries/zed/blob/8dd255c3eafb5231056fe4e6f601cedd011f1c93/crates/agent_servers/src/acp.rs#L799) 一致。

因此，普通该版本连接不会自动开放 rpi 私有 fork/title/archive/systemPrompt 等操作或当前 subagents streaming gate。但不能推论所有重命名/归档/MCP/历史菜单都缺失：Zed 本地元数据操作、普通 stdio/HTTP MCP 与远程 RFD 方法不同。Zed 内置 Skills/Profile 的可视化也不等同于 native 运行时是否支持相应能力。完整辅助源码审查保存在 `C:/Users/bigfish/.cargo/target/acp-zed-1253-source-menu-audit.md`；该报告明确不是 GUI 验收。

## 证据与阻塞点

实际运行根目录内：

- `manifest.json`、`installed-version.json`、隔离 `data/config/settings.json`：工件与实际配置；无真实密钥。
- `traces/rpi-reviewed-16104.jsonl`：**68 个实际 ACP frames**，透明双向转发；不是本轮 schema 全量合规计数。
- `00-start.png` 至 `22-focus-state.png`：23 张仅捕获测试 Zed 窗口的截图。
- `model-snapshot.json`：11 个实际 native HTTP 请求记录，含工具 schema 和取消观察，无 provider failure。
- `parent-evidence-verified.json` / `parent-evidence-verification.log`：18 个证据断言 PASS、严格 LF 字节断言 FAIL；验证器 exit 1，失败没有被新断言掩盖。明确文本换行规范化的独立断言不是字节一致性证明。
- `cleanup-verified.json`、`status.json`、proxy status、正式设置前后 hash：资源回收和未修改正式配置的证据。
- `sources/acp.rs`、`sources/conversation_view.rs`：父进程获取的精确 pinned 源码，支撑模式菜单结论。

Windows 在文件附件操作期间变为不可交互：`GetForegroundWindow() == 0`，`OpenInputDesktop` 返回 error 5 / Access Denied。自动化拒绝继续发送按键，没有尝试解锁或绕过输入桌面访问控制。询问用户后未收到回复，因此停止本轮实例并确认资源清理；这是本次完整验收的现实阻塞，不是 rpi 协议错误。

## 继续尝试 — 2026-10-30 18:04–18:10 +08:00

**GUI 仍 BLOCKED，新增 GUI PASS 为 0；以下 native 诊断不是 Zed 菜单验收。**

新隔离目录 `C:/Users/bigfish/.cargo/target/acp-zed-real-1253-04/` 已准备并实际启动同一 pinned Zed。启动前桌面访问成功；到第一次点击前，桌面又变为 error 5 / foreground 0，自动化拒绝发送输入，没有操作或绕过安全桌面。保留 `00-start.png` 的真实测试窗口截图。本次显式禁用 `auto_install_extensions.html` 和扩展建议，检查没有 HTML 扩展目录。脚本支持唯一 run-tag 的 STOP 和运行归档，可在同一隔离数据目录复测重启，无需删除旧 STOP；也明确记录是否 forcedKill。

### 新确认：合并 Provider 参数会被安装版静默忽略

直接运行**实际安装版 native**，仅发送 get_state / stop、使用隔离配置、没有模型 prompt 或账号请求：

| argv | native 实际结果 |
| --- | --- |
| `"--provider local-alt"` 一个参数 | exit 0，但 get_state.provider 仍为默认 `local` |
| `"--provider", "local-alt"` 两个参数 | exit 0，get_state.provider 正确变为 `local-alt` |
| 正式配置同样的 `"--provider deepseek"` 一个参数 | exit 0，但 get_state.provider 仍为默认 `local` |
| `"--provider", "deepseek"` 两个参数 | 隔离环境 exit 1：Unknown provider；隔离 models.json 未注册该 ID |

因此，正式 `rpi-hualong` 的 argv 写法不是仅理论风险：安装版确会忽略这种合并参数，可能实际使用默认 Provider。**拆开参数仍不保证 ID 正确**：正式 models.json 只读检查也没有名为 deepseek 的 provider 项，但未检查正式 provider-package 注册表，不能断言该 ID 在完整日常环境绝对不存在，也没有修改正式入口或验证真实 DeepSeek 账号。

### 新确认：安装版与冻结 native 的能力差异

实际 ready 帧对比，冻结 native 比安装版多出 `get_modes`、`set_mode`、`set_compaction_enabled`、`get_context_usage`、`sample`、`cancel_sample`、`set_system_prompt`、`configure_external_tools`、`external_tool_response`、`client_request`。这证实上一轮新增 native 能力未进入当前安装版；单改 Zed 配置或仅替换 bridge 不能据此保证所有新能力可用。**没有完成安装版 ACP DLL 的真实 Zed UI 对照**，不能把 ready 差异逐项等同为菜单缺失。

证据：`installed-provider-state-check.json`、`exact-deepseek-and-native-ready-check.json`、`parent-continuation-verified.json`。原始 ready 响应保留，不只记录二进制版本号。

本次 owner 最终 **forcedKill=true、Zed exit 1**，正常退出仍未通过。`cleanup-verified.json` 确认 known owned PIDs 不存活、无本地模型监听、正式 settings hash 未变；`runs/01/` 保存该次运行归档。无 runtime 修改、重建、安装版替换或新 GUI 验收声明。

## 解锁后实际继续 — 2026-10-30 18:15–18:35 +08:00，runs 02–05

本节 supersede 上文“未测试”的相应子项，但不改写首轮失败。仍使用同版真实 Zed；冻结工件未重建或替换。隔离目录 `C:/Users/bigfish/.cargo/target/acp-zed-real-1253-04/`，`runs/02/` 至 `runs/05/` 保存各次状态、manifest、HTTP capture 和正式设置 hash。新增 `rpi-reviewed-direct` 入口直接运行冻结 bridge，用于排除透明代理影响；不修改正式注册或正式设置。

### 真实操作与结果

| 项目 | 本次证据与结果 |
| --- | --- |
| 安装版实际启动/对话 | **PASS，受控模型限定**。真实安装版 native + 安装版 ACP DLL 成功新建线程并完成 `ZED_INSTALLED_CHAT`。`traces/rpi-installed-16016.jsonl`、截图 `04`–`06`；不是仅 `--version` 对照。 |
| 安装版 vs reviewed 菜单 | **确认版本差异**。安装版 `configOptions` 仅 model/thinking，reviewed 另有 auto_compaction。安装版未广告 image/audio，reviewed 广告支持，但两者通用附件菜单都显示 Image；**菜单可见不等于实际上传能力通过**。Default/Plan 在两者 UI 都不可见；reviewed 的原因仍是前述投影缺口。 |
| `/compact` / slash | **确认客户端可达性缺口**。截图 `08`–`10`：Zed 拒绝 `/compact`，提示 available commands 为 none；trace 中 available_commands_update 为 `[]`，未产生该 session/prompt。`session.rs` 已有 compact 特殊处理和 native compact 入口，但没有对应公告，Zed 在请求到达前拦截。此结果不表示本轮已执行真实压缩，也不等同所有插件 slash commands 均失效。 |
| Skills / Profiles | **管理界面可见，执行未验收**。Skills 管理可打开（`06`）；Profiles 中 Ask/Minimal/Write、Fork Profile、Configure Built-in/MCP Tools 可见（`41`–`42`）。这些是 Zed 的管理入口，不是已切换 rpi mode 或新增 ACP session/fork 的证据；未安装 skill、未宣称实际技能执行。 |
| 选区附件 | **PASS：真实发送与历史读取**。选中 `sample.txt` 后通过 Agent 添加选区，`session/prompt` 实际携带 resource 文本 `ZED_NATIVE_SAMPLE\n` 和 URI `file:///…/sample.txt#L1:2`，随后正常 end_turn（`15`–`16`；`rpi-reviewed-25308.jsonl`）。不是只打开附件菜单。**恢复后展示退化**：resource 被回放为 `<file uri="…">…</file>` 内联文本，不再是原附件 chip；原文未丢失，但不能声称结构/展示保真。图像、目录、Symbols、Threads、Branch Diff 的实际发送仍 NOT TESTED。 |
| 历史与重启恢复 | **PASS，单工作区/少量历史限定**。run 02 正常退出后 run 03 使用同一数据目录启动，真实 `session/load` 指向原 session `227108f2-c57b-4432-92a4-7c15a594588a`；先 replay 原对话，再发送 `ZED_AFTER_RESTART`，同 session 正常 end_turn。`rpi-reviewed-30664.jsonl`、`18`–`21`。安装版历史切换也有真实 load；不是只观察侧栏。未验收大历史边界/分页/损坏恢复。 |
| rename / archive / reopen | **PASS，仅 Zed 本地行为**。`22` 显示标题 `ZED_RENAMED_02`，`23` 归档后活动列表移除，`24` 全部历史可见，`25` 重开显示原对话；wire 再次真实 session/load。同一 trace 未发私有 set_title/archive/fork，不能升级为 native title/archive 持久化或远端 fork 验收。 |
| Import Threads | **仅入口确认**。`12` 显示实际界面；没有选择外部线程、没有完成导入。 |
| bypass | **PASS，短 Bash 限定**。`rpi-reviewed-bypass-8720.jsonl` 中 `ZED_BASH` 无 session/request_permission，但 terminal create/wait/output/release 全部真实响应，输出含 `ZED_NATIVE_BASH_OK`、exit 0（`27`）。与首轮无 bypass 的审批形成对照。一次初始连接只有 initialize、新进程随后完成该测试；不能据截图假定第一个 proxy 已完成。 |
| Zed 设置 → MCP → native → tool result | **PASS，本地 stdio 单工具限定**。通过真实 UI 添加 `zed-local-fixture`（`28`–`32`），Zed 新建/加载会话实际传 mcpServers。native HTTP 请求含真实 MCP schema；run 05 选择 opaque hashed name，实际 fixture `mcp-34356.jsonl` 记录 tools/call 的 `zed_echo`，返回 `ZED_MCP_ECHO_OK`；ACP tool_call_update completed 携带同结果，下一次 HTTP 请求有一个工具结果（`37`、`rpi-reviewed-4112.jsonl`）。不把单纯 discovery 当调用 PASS；未验证 HTTP MCP、OAuth、sampling/catalog 或外部服务。 |
| MCP 卡片展示 | **确认可用性问题**。native 广告 opaque `mcp_6a19…` 名称，ACP title/name 原样显示长哈希，而非易读 server/tool 名（`37`–`38`）。控制 fixture 起初按 `zed_echo` 匹配不到 hash，run 04 没有调用；仅修 fixture 按唯一控制 description marker 识别，run 05 才实测调用。无 runtime 修改，没有把前一次无调用包装成成功。 |
| 安装版同一 MCP 设置 | **确认安装版没有模型 MCP schema**。`rpi-installed-28840.jsonl` 的 session/new 也实际收到相同 stdio 配置；run 05 对应模型请求仅内置工具，没有 `mcp_*`，没有实际 MCP tool call。不能因 Zed 设置中 Connected 就声称已安装版支持该工具集成。未因失败而修改正式安装。 |
| 普通 quit | **Zed 本身 PASS；子链优雅退出未通过**。runs 02/03/04 经 Alt-F4 得到 Zed exit 0、forcedKill=false。run 04 在退出前保留进程句柄，`direct-quit-run-04.json` 观察 31 个当时 owned 后代全部结束；watcher 没有 kill，但 bridge/native 返回 1，多个 shell/MCP 进程也是 1。既有 proxy status 保持 running，`proxyOwnersSettled=false`。**最终不存活 ≠ proxy joined ≠ bridge/native 优雅退出**；直接 bridge 同样 exit 1，排除“只有代理状态文件”这一解释，但不能据此确定非零退出根因是 Zed、Windows job 还是 runtime。 |

### 再次桌面阻塞与最终清理

run 05 MCP 和安装版对照之后，准备再次 Alt-F4 的前台检查拒绝输入。独立 `OpenInputDesktop` 返回 error 5，foreground 为 0，保存 `desktop-blocked-run-05.json`；没有重试输入或绕过访问控制。owner 收到新建 STOP-05，最终 **forcedKill=true / Zed exit 1**，不计正常退出成功。之前正常 quit 的三次结果保留，不被最后这次强制清理覆盖。

`cleanup-runs02-05.json` 确认已知 owned PIDs、带测试路径的相关进程以及本轮模型端口均无存活/监听；正式设置前后和最终再次 SHA256 均不变，未自动下载 HTML 扩展。代理状态文件缺少 stopped 仍保留，不能以进程消失重写状态或宣称 owner join 成功。没有改 runtime、构建、部署、账号调用或修改正式配置。

### 证据计数（不是全功能通过数）

- `traces/*.jsonl` 共 **109 个真实 ACP JSON-RPC frames**；外层 stderr/生命周期行不计帧。`rpi-reviewed-direct` 不经 trace 代理，不伪造其 wire 记录。
- 根目录 **41 张截图**（包括 run 01 的启动截图；不包含不存在的编号文件），runs 02–05 共 **11 个真实 native HTTP 请求**。
- `verify_runs02_05.py` / `parent-runs02-05-verification.log` / `parent-runs02-05-verified.json`：**25 个证据断言通过**，其中有些断言专门证明缺口、非零退出或失败被保留；**不是 25 项 GUI 功能通过、schema 合规或全面验收成功**。首轮 CRLF 字节 FAIL 仍未消失。

## 附件与恢复续验 — 2026-10-30 18:43–18:55 +08:00，runs 06–08

本批使用相同冻结 Zed/bridge/native，在同一隔离根目录新增 runs 06–08；未改 runtime、构建或部署。为了实测图像，**只将本地模型 fixture 的 input 改成 text/image**，新增受控 PNG 和目录 canary，并记录实际 HTTP 图片 MIME、长度/哈希及限定文本 marker，不记录任意请求内容或密钥。

| 项目 | 实测结果与边界 |
| --- | --- |
| 图像实际发送 | **PASS，32×32 PNG 单例**。`54`–`55` 显示实际图片 chip。`rpi-reviewed-4500.jsonl` 的 `ZED_IMAGE_REAL` 携带 image，native HTTP 请求也含该 PNG 并正常 end_turn。原 PNG 121 字节，Zed 重编码后 290 字节；System.Drawing 逐像素比较全等，**不宣称原文件字节原样保留**。ACP 与模型 HTTP 的 290 字节完全相同，SHA256 `6e24a8b110fe13a34874f3d546fd316820fd65730387114248291d1df52e14af`。 |
| 文件实际发送 | **PASS，文本单文件限定**。使用 Files & Directories 的真实完成选择，`ZED_FILE_REAL` 携带 resource，URI 指向 sample.txt，正文 `ZED_NATIVE_SAMPLE\n`；本地模型捕获同 canary，随后 end_turn。截图 `58`–`60`，不是只输入 `@filesample`。 |
| 目录实际发送 | **部分通过，URI 保真 FAIL**。Zed 的 `ZED_DIRECTORY` 真实 prompt 发送 `resource_link`、name=attachment-dir-06、完整 file URI。但 native 的权威 message 只有 `@attachment-dir-06\n  ZED_DIRECTORY`，**URI 已丢失**；`session.rs:1673–1705` 的 prompt_text/resource_display_name 优先 name，与实际存档吻合。没有自动递归展开目录正文；不能把单纯 end_turn 记为完整目录上下文成功，也不能声称模型拿到了完整目录路径。 |
| 图片历史恢复 | **FAIL，已定位 ACP 投影**。run 07 正常 quit、run 08 同 session `49334293-0688-43c8-9631-e7515e500a87` load。native transcript message `01a18c6b-f890-7d10-a79e-a3808b36c12f` 保留完整 290 字节 PNG、哈希不变；但 `rpi-reviewed-10084.jsonl` 的历史回放没有任何 image chunk，`66` 中原图片 chip 消失，只剩 `ZED_IMAGE_REAL`。当前 `history.rs:108–143` 只投影 text/thinking/toolCall，其余块忽略，与真实 wire 一致。**不是 native 存档删除图片，也没有证据据此声称模型历史图片丢失**；已证实的是 ACP/UI 回放缺失。 |
| 附件恢复后的继续对话 | **PASS，文本限定**。实际 session/load 成功，`ZED_AFTER_MEDIA_RESTART` 在同 session 正常 end_turn。文件正文回放为 `<file>` 文本而非 chip，旧展示退化再次出现；此 PASS 不覆盖媒体回放失败。 |
| 安装版 Image 入口 | **能力 gate 符合广告**。`68` 显示 Image 项存在但**灰色禁用**，不是“可见即能使用”。安装版 initialize 明确 image=false，没有强行发送图片或伪造 prompt。reviewed 可选择并实际发送；因此安装版缺图片能力有实际 GUI 对照，而不是仅 ready/source 推断。 |
| Threads 上下文 | **未完成发送**。`60`–`61` 的 picker 未出现可选择条目，没有将空 picker 的尝试计为功能 PASS；不据此断言所有外部 Agent 的线程附件都不支持。 |
| Import Threads | **实际命令执行，无成功导入**。`63` 选择 `agent: import threads from other channels`，`64` 返回 **No new threads found to import**。没有测试数据可导入；这不是实际导入成功，也不是 native session/list/import 已通过。 |
| Open Thread as Markdown | **视图入口 PASS，非导出往返保真**。`67` 实际打开 Markdown buffer，显示 user/assistant 及已回放的文件文本。未保存到正式目录、未宣称媒体导出或可重新导入；图片已经在 ACP replay 阶段缺失，Markdown 视图不能补回。 |

### 测试助手问题与非运行时副作用

run 06 的隔离 profile 缺 Desktop，原生 Image 文件选择器报“位置不可用”（`46`、`48`）。仅在隔离根目录创建 Desktop 并重启后，run 07 文件对话框正常。这是测试 profile 问题，不归因于 ACP。原输入助手会把焦点移回主编辑器，导致一次发送图片路径文本、一次 `ZED_IMAGE` 纯文本；这些尝试保留为**没有图像**，不计通过。随后加入 `ownedkeys`，仅向当前已确认属于测试 Zed 的前台窗口发送键盘，不再强制激活主窗口；`foregroundshot` 也仅截图 owned dialog，并移除桌面截图 fallback。修的是 C target 下测试助手，不是产品代码。

原生选择器使用 Windows shell 记忆的其他目录作为初始位置（`51`）；仅键入隔离图片绝对路径，未打开其中其他文件。隔离 HOME/user-data-dir **不等同 OS shell/注册表沙箱**，不能宣称零系统 recent-folder 副作用；正式 Zed 设置前后 hash 未改。

### 正常退出：新增来源解释，不把 exit 1 自动归因于 native bug

- runs 06/07/08 最终 **Zed exit 0、forcedKill=false**。run 06 初次 Alt-F4 观察未退出，`direct-quit-run-06.json` 的 false 保留；随后通过实际命令面板 `zed: quit` 正常退出，不能重写第一次观察。
- runs 07/08 使用已在命令面板确认的 Ctrl-Q，持有进程句柄观察退出，当时 owned 后代分别 **15 /22** 个全部结束；bridge/native 仍非零退出，proxy 状态仍未落 stopped。
- 精确 pinned Zed 源码 [`acp.rs:1675–1681`](https://github.com/zed-industries/zed/blob/8dd255c3eafb5231056fe4e6f601cedd011f1c93/crates/agent_servers/src/acp.rs#L1675) 的 `Drop for AcpConnection` **显式调用 child.kill()**。这为正常 Zed quit 时子进程 exit 1 提供客户端生命周期解释；**仅凭 exit 1 不能定性为 native runtime bug**。仍未证明 EOF graceful/join 或调用发生时序，亦不把终止进程当作优雅退出。
- `cleanup-runs06-08.json`：无已知 owned 进程或模型端口监听、无 HTML 自动下载，正式设置 hash 前后及最终再次检查均一致。本批正常完成、主动回收，**没有再次锁屏阻塞**。

证据：`runs/06`–`08`、截图 `43`–`68` **26 张**、新增 **70 个实际 ACP frames /8 个 native HTTP 请求**；`image-06-manifest.json`、`image-07-acp-evidence.json`、`image-07-from-acp.png`、`image-07-pixel-check.json`、`direct-quit-run-07/08.json`。`verify_runs06_08.py` / `parent-runs06-08-verified.json` / 同名前缀 verification.log 有 **30 个证据断言通过，2 个验收断言 FAIL（图片回放、目录 URI 保真）**，验证器 exit 1；最初只发现图片回放的版本另存 `*-initial.*`，没有掩盖失败。原首轮 CRLF FAIL 与此前缺口仍保留。Skills/插件命令、Symbols/Branch Diff、remote fork、subagents/form/URL、Plan 权限、第三方账号仍未通过真实 GUI 验收。

## 继续验收的最短路径

1. 实例已正常回收；如再测 GUI，先确认桌面可访问，使用新 run-tag（如 `09`），勿覆盖旧 runs/STOP。图片输入的单例成功不替代媒体恢复验收。
2. 优先修复已实证的 mode configOptions、`/compact` 公告、图片历史投影与 resource_link URI 丢失；随后以真实 Zed 回归。不能靠隐藏 FAIL 或改弱断言得到“全通过”。当前仍仅验收定位，未修改产品代码、未授权部署。
3. 尚缺实际 Skills/插件命令、成功导入、Threads/Symbols/Branch Diff 内容、remote fork、subagents/form/URL、Plan 约束和外部服务。根据客户端可协商能力与隔离数据条件分开验收，不以源码/能力广告冒充 GUI 成功。
4. 原全能力目标仍 PAUSED / INCOMPLETE；第三方账号另需授权，不自动恢复原实现主线。
