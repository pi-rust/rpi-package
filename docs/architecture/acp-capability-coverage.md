# ACP 能力覆盖矩阵

**总体状态：PAUSED / INCOMPLETE（原全能力目标已暂停，未完成）。** 本表按当前工作树源码、实际日志和固定协议依据跟踪 `rpi-acp`，不是完整 ACP、完整 OAuth 或部署验收声明。源码实现、受控协议测试、真实 native + 本地模型/网络 fixture、第三方服务互操作是不同证据层级，不能相互替代；构建通过也不表示已安装或替换运行中的服务。

## 最新真实 GUI run12 — 四项定向修复通过；未部署，完整目标仍未完成

2026-10-31 00:03–00:12 +08:00，新冻结bridge **66ecf4ce0c56d7bec76691f7909e493bccca211689d912a8646dc69d1dc927e4**（`acp-zed-commands-01/bin-1/`）/原b1b native/Zed1.25.3，真实reviewed gate非bypass。固定Zed源码证实new响应后才注册session，未知session通知被丢弃；ACP现同request task先成功响应再commands公告，new/load/resume共用，无sleep/detached任务，load replay/v2同步仍在响应前。旧907严格时序回归失败、新工件通过，保留red工件。

**四项真实GUI PASS：** Default/Plan实际切换，Plan强制write失败/无文件，Default同参数write经Yes单次批准落盘；slash compact补全/选择执行，Context Compacted、关联终态、native摘要持久且无普通compact用户消息；Image输入后真实Reload Agent整桥重启、同session/load保留原ID/顺序/73bytes/PNG MIME及SHA18f4…，GUI Image chip恢复；目录picker完整file URI保留至native/model，无canary内容。56ACP frames/8本地模型HTTP请求/0fixture failures，`acp-zed-real-1253-06/run12-feature-evidence.json`24/24严格证据断言。源PNG仍在ACP前重编码，非源文件原字节或像素质量声明。

实际zed:quit给Zed exit0、无owner force；17真实Job成员持有句柄均退出/关闭，监听关闭。但bridge/native/proxy含exit1、proxyOwnersSettled=false，**子链graceful/joined仍未通过**。`parent-run12-verified-02.json`13/13补充父核验；第一次`parent-run12-verified.json`保留FAIL：误把历史安装rpi的b4e当本轮before。run09/10/11/12启动manifest实际均已aafab…，本轮前后相同，历史变更来源未知；无重新写入/恢复/部署。正式settings/DLL/旧新冻结工件hash匹配。

本轮ACP193单测、strict scoped Clippy、新时序3/3/history6/6/native31/31通过；继承环境下两次整包命令的remote_http两项socket失败保留，同一二进制clean allowlist环境2/2通过，其余ACP/extcore test binaries同环境全exit0；不是原整包命令或所有历史suite全绿。详见[验收报告](acp-zed-1253-acceptance.md)。run12已stopped，无live owner。**只完成四项定向缺陷，完整ACP目标/外部账号/子链优雅关闭仍未完成。**

## 历史真实 GUI run11 — 模式/目录PASS，compact FAIL，图片回放未验；已清理未部署

恢复桌面后run10终于实际连上907bridge，随后按期限结束；父验证12个Job成员已退出/句柄关闭，root forcedKill/native exit1，非正常quit。新tag11使用同一907/b1b/Zed1.25.3与真实隔离gate（非bypass）：**Default/Plan菜单可见，Plan schema只有read，强制write失败且无文件；切Default后同参数write经GUI Yes一次批准成功落盘。目录picker真实选目录，模型收到resource_link的完整file URI且不含目录canary内容。** 图片输入wire/native/model73bytes/MIME/SHA18f4…相等，但源PNG为4ff6…，不能称源文件未重编码；**GUI图片历史重载尚未验**。

**`/compact`真实GUI仍FAIL：无slash补全，提交后Zed报未知命令。** wire在new响应前公告compact，时序只是根因嫌疑，尚未修；无转发prompt/压缩请求，原native31项不代表GUI通过。权威`acp-zed-real-1253-05/run11-feature-evidence-02.json`15项已有证据断言通过，不含compact执行/图片重载。第一次脚本将allow_once kind误当optionId并错误要求源图与wire完全一致，2断言失败；原失败JSON与错误预写PASS label保留并明确无效。

再次OpenInputDesktop error5立即停GUI，STOP-11后10个真实Job成员持有句柄确认退出/关闭、proxy settled、监听关闭，正式settings/安装/冻结hash不变；root forcedKill=true，Zed/native exit1，正常quit不通过。run10/11均stopped，不能续接旧窗口。`parent-run10-verified.json`、`parent-run11-verified.json`、`runs/10/`、`runs/11/`均保留。下一步定位修复compact客户端时序/契约，冻结新hash再tag12+，并补图片GUI重载；详见[详细报告](acp-zed-1253-acceptance.md)。本轮仅target harness/文档，无runtime改动/部署，原全能力目标仍暂停。

## 历史 GUI 重试 run10 — 当时暂停快照，终态已在上节核验

22:46:31 +08:00启动tag10，实际owned foreground确认后操作到外部Agent菜单，保存 `10-01` 至 `10-04` 截图；中文IME输入问题只在target GUI helper中用owned逐字符Unicode SendInput解决，不切系统语言/剪贴板。点击reviewed之前前台丢失，输入保护拒绝，随后OpenInputDesktop error5立即停止；尚未选择agent，四项功能仍NOT TESTED、未部署。`acp-zed-real-1253-05/pause-run10.json` 记录暂停及 **23:06:31 +08:00** owner自动清理期限；暂停时2个Job成员仍存活，**最终清理/退出尚待核验**。恢复先检查status/期限；仍running才在合法owned前台续接，已stopped则先核验终态和hash，再用tag11+。见[详细报告](acp-zed-1253-acceptance.md)。

## 历史 GUI 重试 run09 — 2026-10-30 22:23–22:35 +08:00，初始化前 BLOCKED

实际预检桌面已可访问（error0、非零 foreground），新隔离 `acp-zed-real-1253-05/` 校验907/b1b/Zed/gate后启动Zed，截图保留 Unrecognized Project 信任对话框。但正常切前台及 owned-window UIAutomation focus 均失败，输入保护在首个 Enter 之前拒绝；前台一直不是测试Zed。请求用户配合等待超时后停止，不绕过前台保护。**0 ACP frames /0 模型请求，四项修复GUI均NOT TESTED**，仅配置新bridge，未实际启动agent。

owner STOP 的正常关闭等待失败，保留 **forcedKill=true、Zed exit1**，不称正常quit通过。用Windows Job成员验证并提前持有的2个进程句柄均已退出/关闭，无剩余job成员/model监听/HTML下载；正式设置及安装版/冻结工件hash未变。本轮仅target测试harness和文档，无runtime改动/部署。证据 `acp-zed-real-1253-05/parent-run09-verified.json`、`runs/09/`；详见 [详细报告](acp-zed-1253-acceptance.md)。下一次新tag10+需在开始时将测试窗口保持前台，不能把“继续”解释为允许向其他窗口输入。原31项native结果不是新GUI通过。

## 最新定向修复 — 2026-10-30，源码/native 已验证，GUI BLOCKED，未部署

用户授权后实施四项修复：native modes → mode config select，config/legacy setter 同步；支持 compact 的 v1 new/load/resume 内置命令公告（不依赖扩展环境，v2 不虚假公告）；有界历史 image/audio 原字节/顺序/messageId 回放；resource_link 始终保留完整 URI。真实验收进一步发现 compact 异步 ACK 被当终态的问题，已在 extcore 现有 foreground lease 中补齐 ACK/compactionId/settlement、120s deadline、提交后取消/drop 最多3s drain、必要关闭，以及 declined/提交前取消边界；22 个生命周期测试通过。无 native 源码修改、第二份 ACP transcript 或 URL/目录读取。

新冻结 bridge `acp-zed-fixes-01/bin-3/rpi-acp-bridge.exe`，SHA256 **90745044a44a7d8562ca614f3e87b19b7cc4b907c0a09aaba81ee76e05d8e302**，native 仍 b1b。四包串行分组 **489 top-level PASS /0 failed /3 ignored +1 nested TLS subprocess PASS**；strict all-target Clippy、frontend typecheck 通过。新工件 history protocol **9/9**、canonical insertion/cancel **5/5**；最终真实 native `native-05/result.json` **31/31**（含诊断/清理）、17 次本地模型 HTTP 请求，另7项 Windows ownership 自测。Plan 强制 write 不落盘、Default 同工具真实写入、压缩持久化及空会话 declined 后继续、整桥重启媒体/原ID/目录 URI 均有证据。测试启用受控 no-permission-gate，非审批/外部AI验收。

5个 root exit0且线程join；10个 native 按冻结路径+创建时间+父链验证、EOF前持有句柄，实际exit1，均关闭，无强杀，**不称 graceful/native exit0**。修正并保留先前测试 harness PPID 重用误归属和 exit0 预期失败。旧广泛 protocol suite **2 PASS /6 FAIL /1 SKIP**，旧759对照同样失败；并行 Windows socket 失败/120s工具中断及重跑日志均保留，不能称全部历史测试全绿。完整详情及测试契约修正依据见 [详细报告](acp-zed-1253-acceptance.md)。

新 GUI 预检再次 error5 / foreground0，立即停止且未启动新 Zed，四项修复的 **真实 Zed GUI 回归仍未通过/待解锁**；安装版/正式设置/旧工件 hash 不变，原全能力 goal 不恢复。下方“未修 runtime”等均为旧 GUI 批次快照。

## 最新真实 Zed 附件续验 — 2026-10-30 18:43–18:55 +08:00，PARTIAL / INCOMPLETE

同版冻结工件，`acp-zed-real-1253-04/runs/06`–`08` 新增 **70 个实际 ACP frames、8 个 native HTTP 请求、26 张截图**。图像实际输入通过：Zed 将 121-byte PNG 重编码为 290 bytes，32×32 像素全等，ACP→模型 HTTP 字节 hash 一致；文件 resource 正文也真实到达模型。**新增两项实际 FAIL：native 存档图片字节完整，但 session/load 没有 image chunk，UI 图片消失；目录 resource_link 的完整 URI 在 native prompt/transcript 中被缩成 @display-name。** 对应当前 history.rs 忽略媒体、session.rs 的 resource_display_name 投影；未修 runtime。安装版 Image 项实际灰色禁用，与 image=false 广告一致，不能将“菜单有此项”当成能使用。

导入命令实际执行但返回 No new threads found to import，仍非成功导入；Threads picker 无可选条目，未发送；Open Thread as Markdown 实际可打开，但不代表媒体/导入往返保真。runs06/07/08 最终 Zed exit0、无 owner force；07/08 观察 15/22 个 owned 后代全退出，bridge/native 仍 exit1。**新增 pinned Zed 源码依据：AcpConnection::drop 显式 child.kill()，故非零子进程退出本身不能归因于 native bug，也不证明 graceful join。** 本批正常完成并清理，无新增桌面阻塞。正式设置 hash 未变，无已知进程/模型监听/HTML 下载；隔离 HOME 不是 Windows shell/recent-folder 沙箱。验证器 **30 个证据断言 PASS、2 个保真验收 FAIL，exit1**。详见 [详细报告](acp-zed-1253-acceptance.md)；首轮 CRLF FAIL 和模式/slash 缺口保留，原全能力目标仍暂停。

## 真实 Zed 首轮 — 2026-10-30，PARTIAL / BLOCKED

见 [Zed 1.25.3 真实验收与菜单差异](acp-zed-1253-acceptance.md)。父进程实际启动隔离 Zed 1.25.3（8dd255c3），以当前 759/b1b 工件捕获 68 个真实 ACP frames；模型/Thinking/自动压缩配置 ACK、文本、读文件、写入允许/拒绝、terminal、取消及后续对话已有实证。**确认模式菜单缺口：非空 configOptions 抑制 legacy modes，而 rpi 未投影 mode 配置项。** 首轮 18 个证据断言通过，但严格 LF 字节断言失败（Zed 实际写 CRLF），正常 Zed quit 未通过（owner 约 10.28 秒后强制 fallback，exit 1）。桌面变为不可访问（error 5、foreground 0），余下菜单、安装版对比、历史恢复、MCP/账号未测；不把源码审查当 GUI PASS。测试 owner/bridge/监听均回收，正式设置 hash 未变。首次 Zed 启动自动下载 HTML 扩展到**隔离**数据目录，须披露此副作用并在后续禁用。原全能力 goal 不因此恢复；以下 ACTIVE 字样属于先前历史检查点。无 runtime 修改、重建或安装版替换。

## 解锁后续验 — 2026-10-30 18:15–18:35 +08:00，仍 PARTIAL / BLOCKED

实际同版 Zed 在 `acp-zed-real-1253-04/runs/02`–`05` 补验：安装版启动/对话，选区以 resource 真正发送，正常 quit 后同 session/load 恢复并继续对话，Zed 本地 rename/archive/reopen，bypass 短 Bash 无审批且 terminal exit 0，本地 stdio MCP 从真实设置界面添加到 native tools/call/结果回传。**安装版只有 model/thinking，reviewed 另有自动压缩；相同 MCP 配置下安装版模型无 MCP schema。新确认 `/compact` 在 Zed 客户端被空命令公告拦截，未到达后端。** 附件恢复被展平成 `<file>` 文本、MCP 卡片显示哈希也有截图/wire 证据。详见上方链接报告新增章节。

runs 02/03/04 **Zed exit 0、forcedKill=false**，但 run 04 句柄观察的 31 个 owned 后代虽全部结束，bridge/native exit 1、proxy 状态未正常落 stopped；不宣称子链优雅退出。run 05 再遇 error 5 / foreground 0 后停止输入，owner forcedKill/exit1 清理。最终 known PIDs/模型监听均无、正式设置 hash 未变、无隔离 HTML 下载。109 个实际 JSON-RPC frames、41 张截图（含 run01 启动图）、11 个 native HTTP 请求；25 个**证据断言**通过，其中包括缺口/失败保留断言，非 25 项功能全部通过。无 runtime 修改、重建或部署，原 goal 仍 PAUSED / INCOMPLETE。剩余附件种类、实际 Skills/导入、remote fork/子代理/form/URL、账号以及子链优雅退出未完成。

## 最新检查点 — 2026-10-30 16:25 +08:00 wire-final

仅文档更新：读取实际日志/JSON、独立 SHA-256 验证；未重跑测试或修改 runtime/test/binary。`Ctarget` = `C:/Users/bigfish/.cargo/target/`。**本节优先于下方历史 reviewed-final C1–C6 与 E1–E10，旧 pending 不再代表最新状态；总体 ACTIVE / INCOMPLETE。**

| 当前冻结工件 | 独立复核 SHA-256 |
| --- | --- |
| `acp-wire-final-bin/rpi-acp-bridge.exe` | `759eaf88e486ffd624c8f3d764666eeb819b03786692faf54fb144559c852e6a` |
| `acp-reviewed-final-bin/rpi.exe`（未变） | `b1b371966a3dfe6227e863e8a90655059dea86218d49d8bda4ef9564eb241888` |

- **W1 parent checks：** `acp-wire-current-all-tests.log` 实际重计 **463 top-level PASS /0 failed /3 ignored +1 nested TLS subprocess PASS**；464 pass-result entries 不等于 464 unique tests。同四包 ACP/extcore/MCP core/MCP adapter + extcore test-support；`acp-wire-current-clippy.log` scoped strict **PASS**，非 whole-native/workspace。
- **W2 最新 759/b1b 全部 PASS：** ACP sampling **22/22** `acp-sampling-wire-final.log`；OAuth **16/16** `acp-wire-oauth-final.log`；modalities/roots **34/34** `acp-modalities-wire-final.log`；child 全 **5/5** `acp-wire-child-final-parent-01.log` 和同名目录 `result.json`。实际 **9 child headers、5 settled normal reopens，各 14 messages /0 model calls**；非 ACP restart replay。child phase 1 完成，phase 2 仍缺失。
- **W3 native-only，bridge 无关：** 未变 b1b sampling **13** `acp-sampling-native-reviewed-final.log`、hostile budget **9** `acp-sampling-budget-reviewed-final.log`、backpressure **4** `acp-backpressure-reviewed-final.log` 全 PASS。unsafe opened-root component、ordered/mixed-legacy 两项 P2 的 corrected focused tests **2 +16 +28 PASS**，分别见 `acp-reviewed-native-path-tests-final.log`、`acp-reviewed-native-protocol-tests-final.log`、`acp-reviewed-native-modes-tests.log`。
- **W4 最新实际双向 schema：** `acp-wire-schema-final-parent.json` **427 frames =354 validated +0 failed +73 unsupported；0 unbound replies、4 pending requests、28 controls**。v1 293 frames /272 validated /21 unsupported /4 pending；v2 134 /82 /52 /0。状态 **INCOMPLETE**。pending 是 cancel 的 `terminal/wait_for_exit` 与 EOF 的 `terminal/wait_for_exit`、`terminal/kill`、`terminal/release`，故意不答复，不制造 ack，不把 cleanup emission 当远端成功。
- **W5 保留真实失败：** 先前 `acp-bidirectional-schema-final.md` / `.json` 的 b1b/713 audit 实际 **352 validated /2 FAILED frames /73 unsupported /0 unbound /4 pending，总 427 frames**，不是猜测的 358。agent-only 零失败后，双向 binding 找到两个 v2 `session/new` response 缺 `configId`（四 field errors）的真实 bug。当前源码修复 v2 response + notification `configOptions` 的 **`id` → `configId`**、grouped **`group` → `groupId`**，v1 不变；759 最新 capture 零 validation failure 不改写旧 FAIL。
- **W6 身份分离：** `acp-reviewed-oauth-final.log` **16 PASS**、`acp-reviewed-remote-final.log` **2 PASS（HTTP/2 + WS）** 实际存在，属 **b1b/713**，不再 pending。759/b1b OAuth16 见 W2；parent 随后实际运行 `acp-wire-remote-final.log`，**759/b1b 的 HTTP/2 + WS stream/cancel/EOF/reconnect 两项均 PASS**，并非继承旧身份结果。
- **仍缺失/限制：** native browser E2E、独立 GET reverse listener、durable child phase 2（native list/get pages、extcore bounded child history、ACP parent-restart replay）未实现；child caps `{}`，无 direct load/resume/mutations 或 child external tools。只支持 parent POST-response SSE reverse sampling，不宣称 full MCP/all-provider。真实 native + fake 本地 model/network provider 不等于真实外部 AI；无第三方 OAuth account 验收。下文历史“durable child missing”等仅为旧快照，不否定已完成 phase 1/private correlation。总体 **ACTIVE / INCOMPLETE**。

### W7：新增实际 native provider budget 验收

同一 **b1b** native 的 `acp-provider-budgets-run-parent-final-01.log` / 同名目录 `results.json`：**16 PASS /0 fail /0 blocked**。新增 **Anthropic Messages、OpenAI Responses** 的真实 HTTP/SSE 单大事件与多 chunk 超限测试；每次攻击前后同 provider/model 正常请求成功，固定有界 RPC 错误、上游连接关闭、root messages/JSONL hashes 不变。前期 Anthropic fixture 的 `/v1/v1/messages` 路径错误已改为正确 base URL，未为此修改 runtime；失败工件保留。该证据补充 Completions 9/9，不等同外部真实 AI、全 provider/codec 或完整 MCP 验收。

另外，parent 对当前 **b1b/759** 重跑 session RFD **2/2 PASS**（`acp-wire-rfd-final.log`，每版 107 rows /3 pages）、system-prompt persistence/fork/override **2/2 PASS**（`acp-wire-systemprompt-final.log`）、显式选择的 authoritative native fork **1 PASS**（`acp-wire-fork-final.log`）。fork 的负例 stderr 为预期；零选中测试 target 不增加 PASS，默认 ignore 统计不改写。

## 历史检查点 — 2026-10-30 reviewed-final（已由上节 supersede）

仅刷新文档，读取实际日志/JSON 并独立复核冻结 SHA-256；不重新运行验收、不替换安装版。`Ctarget` 为 `C:/Users/bigfish/.cargo/target`。**C1–C6 优先于下方历史 E1–E10，旧身份 PASS 不迁移到新工件。**

| 当前冻结工件（独立 SHA-256 验证） | SHA-256 |
| --- | --- |
| `acp-reviewed-final-bin/rpi.exe` | `b1b371966a3dfe6227e863e8a90655059dea86218d49d8bda4ef9564eb241888` |
| `acp-schema-final-bin/rpi-acp-bridge.exe` | `713f7ed3af41097fa241e8d4f1a770a23f0a1c6f7e66b6337c6dd4b411b541e1` |

- **C1 package/native focused：** `acp-schema-current-all-tests.log` **462 top-level PASS、0 failed、3 ignored**，另有 **1 nested TLS subprocess PASS**，不是 463 unique tests。ACP/extcore/MCP core/MCP adapter + extcore test-support；`acp-schema-current-clippy.log` 同范围 strict **PASS**。无 whole native/workspace strict Clippy 声明，pre-existing telemetry type-complexity 阻塞保留。native 两项 P2 已修：实际 opened root header 的 drive-prefix/Windows reserved IDs 令 factory fail closed；mixed ordered/legacy 在 accepted/model/history 前拒绝。`acp-reviewed-native-path-tests-final.log` **2 PASS**、`acp-reviewed-native-protocol-tests-final.log` **16 PASS**、`acp-reviewed-native-modes-tests.log` **28 PASS**。初始 unsafe-header fixture 把 `createdAt` 写成 string 而非 native i64，修为 `0`；无 runtime workaround。
- **C2 当前 sampling/catalog：** `acp-sampling-native-reviewed-final.log` **13/13 PASS**、`acp-sampling-acp-reviewed-final.log` **22/22 PASS**、`acp-sampling-budget-reviewed-final.log` **9/9 PASS**；JSON 目录分别为 `acp-sampling-native-run-reviewed-final-01`、`acp-sampling-native-run-acp-reviewed-final-01`、`acp-sampling-budget-run-reviewed-final-01`。真实 native + 本地 fake HTTP model/MCP，**非外部 AI**。已有 isolated executor、wire callbacks、opt-in catalog 模型工具；default-off、consent/preapproval/decline、双 roots、catalog/tools-only、共享 budget、UI/model cancel/EOF 与 hostile byte-budget 通过。可信 caller policy 启用，consent/execution 共用缩短后的 absolute deadline，不重置。
- **C3 security/backpressure：** bounded overflow 显式失败、reverse POST 401 exact rejected-lease invalidation、native owner watchdog 修复三项 findings，不是 all-secure。`acp-backpressure-reviewed-final.log` / `acp-backpressure-run-reviewed-final-01/results.json` **4/4 PASS**：drained exact cancel exit **0**；open unread stdout cancel/EOF/command fatal **exit 1**，**2.516 /2.296 /2.516 秒**，不宣称 failed stdout joined ack。watchdog 对**所有 RPC teardown（含 healthy EOF/stop）**设 two-second grace，最终全进程 `exit(1)`，不是只 abort writer thread；不保证 destructors/durable flush。**独立 GET reverse listener 缺失**，仅 parent POST-response SSE。无 byte-budget 能力 provider fail closed；当前 OpenAI Completions/Responses、Anthropic，不外推全部 provider。
- **C4 modalities/roots：** `acp-modalities-run-reviewed-final-01.log` / `result.json` **34/34 PASS**（v1/v2 各 7 positive +10 negative）。PNG/WAV exact provider bytes、混合顺序、malformed base64/blob rejection、additional roots **16 次真实 file/tool operations**（8/version）、非法 root 与 persisted metadata reload。仅受控 OpenAI-completions fixture，非 universal modalities/OS sandbox。
- **C5 child phase 1 DONE / phase 2 ABSENT：** `acp-durable-child-run-reviewed-final-01.log` / `result.json` **5/5 PASS**，含 v1/v2 normal、v1 deny/cancel/EOF。实际 storage **9 children headers**、**5 settled normal reopens** 各 **14 messages /0 model calls**；clean-file reopen 无 child/root bytes 变化，本次复核 recorded hashes。仅将 `ToolResult.details: null` 按 native serde None omission 归一化，其余 substantive/unknown fields exact。native persist/reopen 完成；**native authoritative list/get pages、extcore bounded child history、ACP parent-restart replay 未实现**。child caps `{}`，无 direct load/resume/mutations 或 child external tools。私有 `_meta["rpi/parentToolCallId"]` 取实际 native invocation，不受 model spoof 控制；固定 RFD 无此标准字段。旧 parent-01 **4/5 FAILED** 为 v2 fixture `toolCall` vs `subject.toolCall`；parent-02 与 reviewed-final 全 5 通过，不抹除历史失败。
- **C6 schema/身份：** `acp-schema-runtime-final.json` **640 validated、0 failed、28 controls**，仍有 **51 unsupported /200 unbound replies /114 pending requests**，结论 **INCOMPLETE**。旧 `758d…/8ca1…` 9 bad v2 frames 已在新 `713…` bridge 修复，不改写旧失败；此 audit 属前一 **499/713** captures，**b1b native 当前双向 frame audit 仍 pending**（`acp-bidirectional-schema-final.md` 为 INITIAL）。unsupported definitions、未绑定 originating method 的 replies 不计完整合规。此前 `499…/713…` full matrix 有效，`acp-schema-oauth-final.log` **16 PASS**、`acp-schema-remote-final.log` **2 PASS**；当前 b1b 的 `acp-reviewed-oauth-final.log` / `acp-reviewed-remote-final.log` **尚不存在，保持 pending**。
- **保留限制：** 第三方 OAuth actual accounts 未验收；Chromium dev99/prod99 为 route-mocked backend，非 native-browser E2E。`acp-artifact-recovery.md` 保留 **13:01 +08:00 artifact loss**、未知原因与历史观察；当前 hash 不冒充旧工件恢复。**Full goal ACTIVE / INCOMPLETE。**

## 依据、固定版本与协商门槛

- 稳定 ACP v1：[overview](https://agentclientprotocol.com/protocol/v1/overview)、[initialization](https://agentclientprotocol.com/protocol/v1/initialization)、[session setup](https://agentclientprotocol.com/protocol/v1/session-setup)、[cancellation](https://agentclientprotocol.com/protocol/v1/cancellation)、官方 `schema/v1/schema.json`。v2 [overview](https://agentclientprotocol.com/protocol/v2/overview) / `schema/v2/schema.json` 仍按 **Draft baseline** 处理；RFD/unstable 不计为稳定能力全部完成。
- session RFD 的精确基线为官方仓库 `1142c83bbfa9407f9f264d1e290ffafcd54eb79e`：见 [固定依据与私有协商说明](acp-session-rfd-evidence.md)，以及该版本的 [unstable schema](https://github.com/agentclientprotocol/agent-client-protocol/blob/1142c83bbfa9407f9f264d1e290ffafcd54eb79e/schema/v1/schema.unstable.json)。当前 `packages/rpi-acp/src/session_extensions.rs` 要求 initialize 中 `_meta["rpi/unstable-session-rfds"]` **等于该完整 revision 字符串**，不是 `true`、对象或任意旧版本。v1 位于 `clientCapabilities._meta`，v2 位于 `capabilities._meta`。这是本 adapter 的**私有 opt-in 策略**，不是官方定义的客户端稳定字段。它门控 fork/title/archive/systemPrompt、`auth/status` 和 MCP-over-ACP 声明；未协商不广告这些实验面。
- v1 `protocolVersion: 1` 保留原有 wire；显式 v2 必须提供 `protocolVersion: 2`、`info` 和 `capabilities`，使用独立 nesting、prompt acknowledgement 和 state update，不把 v1 扩展当作 v2 完整实现。源码：`packages/rpi-acp/src/{protocol,v2}.rs`。
- child 协商独立于上述 revision：v1 `clientCapabilities.subagents: {}`；v2 baseline 启用 live child projection。client tools 还须独立显式协商：v1 `fs.readTextFile` / `fs.writeTextFile` / `terminal` 为 `true`；v2 使用 `capabilities._meta["pi-rust/client-tools"]` 的 `readText` / `writeText` / `terminal`，不从已移除的 v1 字段推断。源码：`packages/rpi-acp/src/{subagents,protocol}.rs`。
- remote 基线：[transport RFD 固定于 `ccff4e7d`](https://github.com/agentclientprotocol/agent-client-protocol/blob/ccff4e7d/docs/rfds/streamable-http-websocket-transport.mdx)。启动开关、wire、安全与限制以 `packages/rpi-acp/README.remote.md`、`src/transport.rs`、`src/transport/` 为准，不声称完整 RFD 一致性。
- 架构不变：`rpi-acp -> rpi-extcore -> native rpi --mode rpc`；`rpi-mcp-adapter -> rpi-mcp-core`，extcore 使用协议中立的 MCP 服务。native 仍拥有 transcript，registry/title/archive 是补充元数据。参见 [extension-core](extension-core.md)。

## 稳定 ACP v1 与现有运行时能力

下表“已实现”仅指列明范围；证据编号见后文。

| 能力 | 状态 | 当前实现、证据与边界 |
| --- | --- | --- |
| JSON-RPC envelope、initialize、错误、notification | **已实现（v1 范围）** | `packages/rpi-acp/src/protocol.rs` 校验请求，返回 `-32700/-32600/-32602/-32601`；合法 notification 不响应；无效 prompt block 不静默丢弃。版本协商与 v2 隔离有 package 回归（E1）。 |
| prompt、`session/cancel`、`$/cancel_request` | **已实现（有 owner 的生命周期）** | 关联 ACP prompt 与 agent→client request；native abort 后等待 authoritative `agent_settled`，不以 `agent_end` 或 acceptance 冒充完成；取消/断连清理 pending dialog。E1、E2、E3。 |
| new、绝对 cwd、additionalDirectories | **部分完成** | 校验、canonicalize、去重、持久化并在 load/list 恢复；extcore 传递 workspace roots。native filesystem、shell cwd/显式路径检查限制越界；shell 间接访问和任意 native plugin **不是 OS sandbox**。child 两个 root 的交互隔离另有 E3，不能据此推断任意插件安全。 |
| close、delete、load、resume | **已实现（持久 root 会话）** | close 取消并等待 prompt/dialog 后释放 child/watcher，保留 native history；delete 再删除持久目录；load replay active branch，resume 默认不 replay。v2 显式 start replay 路径及异常 cursor 有回归。**不适用于 durable child/load/resume。** |
| list、cwd 过滤、稳定分页、metadata | **已实现（当前范围）** | extcore 校验 native session 目录/UUID/头部，提供 bounded keyset 分页；ACP 映射 title/updatedAt/additionalDirectories。非法 cursor 拒绝；实验 archive filter 须单独协商。 |
| bounded native history、cursor、事件边界 | **已实现私有分页；非标准 ACP replay cursor** | `packages/rpi-acp/src/history.rs` 的 `HistoryCursor` 与 `page()`、`protocol.rs` 的 `_rpi/history` 已落地；广告 `_meta["rpi/history"]`，limit 为 1..128，返回 `updates/nextCursor/hasMore` 及 eventBoundary。cursor 绑定 session、instance、native branch anchor 与原子订阅事件边界；attachment/boundary/branch 变化拒绝继续，要求重读。load replay 使用 native bounded history/spool，不应再写“没有有界 native history/cursor”。这不承诺标准 replay 分页或跨重连 live replay。E1、H3。 |
| canonical message ID | **已实现（native 依赖）** | live/RPC/transcript/load 使用 native canonical ID；旧记录 fallback 为稳定 SHA-256，重复相同内容不覆盖有效 entry ID。`history.rs` 回归在 E1；历史真实一致性证据不冒充最新重跑。 |
| text、resource link、embedded text resource | **已实现（文本映射范围）** | 映射为 native text；URI/file 的执行含义仍受 native cwd/tool 约束。 |
| image/audio | **provider-neutral 路径 + 当前 fixture 验收；provider 依赖** | C4 验证 v1/v2 PNG/WAV exact provider bytes、混合顺序和 malformed base64/blob 拒绝；校验后经 ordered RPC media 进入 native。仅受控 OpenAI-completions fixture，不证明所有 provider/model 的 InputModality 或音频支持。 |
| tool call/update、diff、plan | **已实现（native event 依赖）** | ACP 格式化 native tool、edit/write diff、todo/plan；执行与 run semantics 属于 core/native，不能仅靠 mapper 推断所有工具执行成功。E3 验证受限 child 工具路径。 |
| config option、boolean、mode、slash command | **已实现（native capability-gated 范围）** | model/thinking、boolean、config update、mode、`get_commands/execute_command` 及 available commands update；旧 native 可选命令 fallback。真实 plugin 命令验收属历史证据，不宣称任意 command-template RFD 完成。 |
| usage、notice、compaction | **部分完成** | usage/stats 按 native supported command gate 且有超时；compaction started/finished/failed/declined 已映射。完整 notice/RFD 与所有自动 compaction 分支尚未完整验证。 |
| permission、elicitation form/URL | **已实现受限交互路径** | purpose=permission 的 dialog 映射 request_permission；form/URL 按客户端能力映射 elicitation/create，归一化 accept/decline/cancel、校验响应并清理 pending 请求。已有 native producer、auth mapper 与本地 OAuth extension 路径（E3、E4），不再以“无 producer/mapper/callback”否定全部能力；也不推断所有 provider/UI 都可用。 |
| terminal auth、agent auth、logout、auth-state | **部分完成，已有真实 native 子集** | terminal `rpi-credentials` 仍按 capability gate 提供；`auth_flow.rs` probe native descriptor，在 form 且必要时 URL 可用时广告 `rpi-agent-credentials`。v1 authenticate / v2 auth/login 有 native 交互，logout 走真实 native revoke/logout；`auth/status` 受 revision gate，调用 native credential check。`packages/rpi-extcore/src/auth.rs` 管理 AuthProcess/事件/响应与取消；ACP 只做 wire mapping。E4 的 24 例证明本地 extension + 网络 fixture，**不是第三方凭据/生产 OAuth 完成**。 |
| client `fs/*` | **受协商的 native producer + adapter 已实现** | read_text_file/write_text_file 经 native client_request 与 ACP client response 关联、schema 校验、错误保留、取消/断连清理。native producer 已存在，不再标“仅 adapter、无 producer”。E3 覆盖真实 child read/write、拒绝与隔离；不等于任意二进制默认支持。 |
| client `terminal/*` | **受协商的 native producer + 生命周期已验证** | create/wait/output/kill/release 与 pending request owner 关联。E3 成功路径 release，错误/取消路径 kill+release；EOF 后仍尝试输出 kill/release，但 stdin 已关，**无客户端 ack，仅 best effort**。不得承诺远端终端必然释放。 |
| client MCP stdio / HTTP | **已实现工具接入子集** | `packages/rpi-acp/src/mcp.rs` 校验并配置真实 stdio/HTTP server，经 extcore MCP runtime/external-tool forwarding 给 native 模型使用；不是“仅校验 mcpServers”。v1 隐式 stdio、v2 显式 type；HTTP 广告 true、旧 SSE false。H2 有真实 native v1/v2 工具验收；catalog、sampling、OAuth 分别见下表，不能从工具通过推导全部 MCP 完成。 |

## v2 / 实验 RFD / MCP 的实现与缺口

| 项目 | 状态 | 当前源码、门槛与仍未完成的范围 |
| --- | --- | --- |
| 显式 v2 Draft baseline | **部分完成；完整 runtime schema 验收未闭合** | 独立 nesting、messageId acknowledgement、state_update、auth 已实现。v2 command、permission、resource_link 与 configOptions response/notification mapping 修复见 W4–W5；最新 759/b1b 双向 audit 354 validated、0 failed、0 unbound，仍有 73 unsupported /4 pending，INCOMPLETE。历史 C6/E audit 不代表最新状态。 |
| session fork | **revision-gated 子集已实现** | `session/fork` 调用 `packages/rpi-extcore/src/session_fork.rs` 的 native-authoritative fork，持久化并可重新打开；ACP mapper 在 `session_extensions.rs`，H3 有真实 native v1/v2 验收。E1 默认 native fork 测试仍 ignored，不得改记通过。 |
| client title、archive/unarchive | **revision-gated 子集已实现** | `session/set_title`、`session/archive`、`session/unarchive`、title/archive 通知及 list archived filter 已映射，extcore session_metadata 管理补充元数据；H3 验证。不再标“无 mapper”。 |
| client systemPrompt | **revision-gated 文本子集已实现** | `packages/rpi-acp/src/system_prompt.rs` 校验 append/override、仅文本块及聚合 UTF-8 上限；extcore `system_prompt::configure()` 调用 native 中立接口，不再标“无中立参数”。capability 广告 override；H1 的 v1/v2 真实 native 检查通过，不能扩展为任意多模态/system policy 支持。 |
| subagent sessions | **live + phase-1 JSONL storage；总体部分完成** | `subagents.rs` 有界 roster 与 native child_start/event/end；child caps `{}`。native trusted parent/root/invocation 为 authority，association 私有 `_meta["rpi/parentToolCallId"]` 已实现，不是固定 RFD 的标准字段；C5 的 9 次 model spoof 未覆盖实际 tool ID。C5 当前 aggregate 5/5 PASS；9 个真实 child headers、5 次 settled normal storage-only reopen 各 14 messages/0 model calls，非 ACP restart replay。**phase-2 discovery/bounded reads/extcore/ACP replay 未实现；child direct load/resume/mutations/cancel endpoint 未支持。** 普通 runtime actions、external/MCP tools、nested delegation、任意 plugin sandbox 均不因此支持。 |
| auth state / provider callback | **revision-gated 查询与 native 交互子集已实现** | `auth/status`、agent auth mapper、native extension callback/revoke 已存在，E4 实际验证；不再写“无 auth callback”。跨第三方 provider/账号、生产凭据互操作仍未验收，不能标 complete OAuth。 |
| MCP-over-ACP | **revision-gated 子集已实现** | `mcpServers.type: acp` + serverId，连接级注册且禁止重绑；`mcp_acp.rs` 使用 `mcp/message`，保留 inner result/error/opaque 扩展及 null，core 拥有 binding/operation/cancellation，native capability 仍须支持 external tools。协议回归与 E5 支撑此子集，不等于完整 ACP model integration。 |
| MCP MRTR / input-required | **neutral core + ACP policy 已实现并通过协议验收** | `packages/rpi-extcore/src/mcp_external{.rs,/input.rs}` 管理 continuation；`packages/rpi-acp/src/mcp_input.rs` 提供 form/URL/roots policy，保留 decline/cancel、清理 state、限制无效/state-only 请求，处理 pending cancellation 和 sibling isolation。E5 v1/v2 均 PASS；不能把受控协议 peer 当作全部真实模型链路验收。 |
| initialized MCP catalog：prompts / resource templates | **core API + opt-in ACP/native 模型工具已有 fixture 验收** | bounded prompts_list/prompts_get/resource_templates_list、metadata/content、cursor/cancellation 已实现；`RPI_ACP_MCP_CATALOG=1` 由 caller environment 单独 opt-in，C2 catalog/tools-only 真实 native fixture 通过。不是完整 MCP/catalog 全功能验收。initialized core 2025-06-18 与 extcore external 2026-07-28 不混称完整 MCP2026；完整 notifications/subscriptions 等仍未闭合。 |
| MCP sampling | **native executor + wire 已实现；可信 opt-in、受限 text-only fixture 验收** | C2 当前 native13/ACP22 PASS，`mcp_sampling.rs` + extcore NativeSamplingOwner + core callbacks；默认关闭，必须可信 `RPI_ACP_MCP_SAMPLING_POLICY` canonical resource/cwd、budget、native sample/cancel_sample，必要时显式 consent。不能写 always false/仅 fake contract。支持 POST-SSE reverse，不支持独立 GET listener；tool/context sampling 拒绝，无预算能力 provider fail closed。C3 三项修复不代表全安全/全 provider 验收。 |
| managed MCP HTTP credential provider | **provider + ACP opt-in 已实现；本地 fixture 验收，不是完整生产 OAuth** | 既有 CredentialProvider/lease binding、401 至多一次 acquisition/replay、static Authorization 优先和 bounded cleanup 保留（E6）。新增 core network/provider/callback：可信 resource/issuer/client/scopes、PRM/AS metadata、DCR、PKCE/真实 loopback callback、token、full-binding slots、refresh single-flight/scoped memory cache；默认禁用，不自动信任/发现未知 issuer。exact-origin HTTPS、fresh DNS 校验/pin、显式 private-address grants；仅显式 literal-loopback 开发例外允许 HTTP，无 proxy/redirect/cookies。初始 B1 已由 provider 调用 `network.validate_resource_url` 修复，旧 blocking review 仅属旧快照。ACP 环境变量 `RPI_ACP_MCP_OAUTH_POLICY` 指向绝对路径、≤64 KiB 严格 JSON，拒绝未知字段，含可信 resource/issuer/client/scopes/origins/DCR grants；不是模型/server 参数或 E4 native agent-auth identity。最终兼容小补丁让 empty/stdio/staticAuth/no-UI-required 路径 bypass policy；当前 package/Clippy 见 C1（不是旧 E1 的补丁前快照）。E8 真实 native + 本地网络 fixture 16/16 PASS；最新 759/b1b OAuth16 与 prior-pair 713/b1b OAuth16 均有实际 PASS（W2/W6）；第三方账号与完整 security/lifecycle 验收仍未完成。 |
| HTTP/2 / WebSocket transport | **实验 v1 bounded profile，真实 native 测试通过** | `--experimental-http`（HTTP/2 cleartext + WS upgrade）或 `--experimental-remote`（WS），`/acp`、强制 Bearer、Host/Origin、socket-generation identity、预算和 cleanup owner。E2 两个真实 native 测试通过：stream/cancel/EOF/persisted reconnect；不是 fake peer 的结果替代。仍无完整 RFD、一致可靠投递/live replay、v2 durability、TLS、TLS proxy 集成或负载测试结论；不提供多租户 ACL/插件 sandbox。 |

## 先前证据（C:）与历史运行身份

下列 E1–E10 保留原始快照计数及限制，不代表当前冻结身份。当前证据见 C1–C6；特别是 E10 contract-only 状态已经被 C2 supersede，E1 补丁后 strict Clippy pending 已由 C1 的当前日志更新。

以下 `Ctarget` **统一表示 `C:/Users/bigfish/.cargo/target`，不是仓库内的相对目录**。矩阵引用实际运行日志及源码审查；未安装或替换运行中的服务。

- **E1 — 2026-10-30 parent 最新 package/Clippy**：`Ctarget/acp-oauth-current-all-tests.log`：**397 top-level passed、0 failed、3 ignored**；另有 **1 nested TLS proxy subprocess PASS**。直接 regex 累加是 398 pass-result entries，**不是 398 unique tests**。命令范围：`cargo test --locked -p rpi-acp -p rpi-extcore -p rpi-mcp-core -p rpi-mcp-adapter --features rpi-extcore/test-support --target-dir C:/Users/bigfish/.cargo/target/rpi-acp-isolated-review`。core **89 =32 unit +3 catalog +16 oauth_http +9 oauth_network +11 oauth_provider +6 oauth_tls +12 sampling**，另有 nested 1。同四包/features、`--all-targets --no-deps -- -D warnings` 的 `Ctarget/acp-oauth-current-strict-clippy.log` **PASS**，但明确是**最终 empty/stdio/staticAuth/no-UI policy bypass 兼容小补丁之前的 dirty parent 快照**；没有该快照 clean commit/source hash，不声称补丁后 rerun，仍需 parent 最新重跑。保留此前 `Ctarget/acp-final-current-tests.log` **346 passed、0 failed、3 ignored**（349 discovered）及 `acp-final-current-clippy.log` strict PASS；再此前 `Ctarget/acp-full-latest-package.md` / `acp-full-latest-{tests,clippy}.log` **343 passed、3 ignored**（346 discovered），均不得改写为最新。三个默认 ignored 仍是两个 real-native remote 与一个 native fork，不计 passed。
- **E2 — 最新显式 remote native**：`Ctarget/acp-final-current-remote.log`，`http2_real_native_stream_cancel_eof_and_reconnect` 与 `websocket_real_native_stream_cancel_eof_and_reconnect` **2 passed、0 ignored**。这是对 E1 默认跳过项的独立显式运行，不改写 E1 的统计，也不据此宣称 ignored fork 已跑。
- **E3 — 最新 child 真实 native run08**：`Ctarget/acp-child-tools-native-run-08/result.json`，**ALL 5 PASS**：v1-normal / v2-normal / v1-deny / v1-cancel / v1-eof，实际 model_calls 分别 **20 / 20 / 12 / 13 / 3**。normal 两个 root 各有 read/write/bash、form/permission、terminal success/error；deny 拒绝，cancel 目标 child 清理而 sibling 完成全部阶段；EOF 记录 kill/release 发出。stdin 关闭后不能收到回应，故 EOF 只认 best-effort cleanup，非 ack/远端成功保证。fixture 不覆盖 URL elicitation、nested delegation。精确冻结二进制如下（SHA-256 来自 result.json；不声称已替换安装版）：

  | 工件（相对于 Ctarget） | SHA-256 |
  | --- | --- |
  | `acp-eof-cleanup-bin/rpi-acp-bridge.exe` | `5caf2445b5330b5eeb1cbec3291630bfa2566444faecedb195c643e2ae8e9270` |
  | `acp-child-tools-enabled-bin/rpi.exe` | `cbc3911583f72b8834cfcc3fc18d1d8b339eb476ee95789c083beb7b76ba22fa` |
  | `acp-child-tool-bin/rpi_subagents.dll` | `703e5a97f53af03afb01a5fed3ac4a8bf457c8c65b562483cb5cde49c68b7666` |
  | `acp-child-tool-bin/rpi_ask_user.dll` | `f82c6fd2b21972b4bb83b8137a6aa96d921fad7007ef57b1ab87456ea05560e8` |
  | `acp-child-tool-bin/rpi_permission_gate.dll` | `23d2d63b488fbd58e2ca5d8522fb6fbd6001c7b28d40ba672323132ac81882a1` |

  `Ctarget/acp-native-gated-tools-review-final.md` 是**受限 child 设计的只读源码安全审查**：具体 native blocker 通过拒绝或 ownership enforcement 关闭，非任意插件认证。该报告引用的 **run06 总体 FAILED（EOF 缺 kill/release）是历史结论**；现由上述精确 run08 工件补足该验收，不能把 run06 本身改写为通过。
- **E4 — native OAuth**：`Ctarget/acp-oauth-native-report.md`、`Ctarget/acp-oauth-native-results.log`，**24/24 PASS（v1 12 + v2 12）**，真实 native provider extension ABI + synthetic loopback OAuth network fixture。覆盖 success、URL/form decline/cancel、invalid state/code、token error、parent cancel、disconnect、revoke/disk cleanup 和 secret checks。**不是第三方 credentials、真实 IdP/account 互操作或完整 managed-MCP OAuth**。该组使用 `Ctarget/acp-oauth-native-bin/` 的独立冻结工件，哈希以报告为准，不冒充 E3 bridge/native 身份。
- **E5 — MCP MRTR/input protocol**：`Ctarget/acp-mcp-input-protocol.log`，v1/v2 multiround form/URL、roots、verbatim decline/cancel、state clearing、invalid/state-only rejection、pending cancellation、sibling isolation 均 **PASS**。这是 neutral core + ACP caller policy 的协议证据，不是所有 catalog/sampling/模型集成都完成。
- **E6 — MCP core/auth hardening**：`Ctarget/acp-mcp-auth-hardening-final.md`；`acp-mcp-auth-hardening-{focused,tests,tests-repeat,clippy}.log`。完整套件每次 **30 unit + 3 catalog + 16 oauth_http = 49 distinct PASS**，重复运行不是 98 个不同测试；strict Clippy PASS。只证明 Windows loopback/core scope；provider seam、generation retirement、bounded cleanup 的结论和限制以上述报告为准。
- **E7 — HTTP/Web/frontend**：`Ctarget/acp-final-http-web-report.md` 及对应日志：`rpi-http/rpi-web` **24 passed、0 failed**（HTTP 11 unit +1 shared-service integration；Web 12 unit）；`acp-final-http-web-typecheck.log` 的 `tsc --noEmit`、`acp-final-http-web-build.log` 的 Vite production build 均 **PASS**，复用既有 node_modules，未安装依赖或浏览器。**该旧报告当时的 Playwright pending 状态现由 E9 更新**；静态检查和构建本身仍不能替代交互验收。
- **E8 — managed MCP OAuth，独立于 E4 agent auth**：`Ctarget/acp-mcp-oauth-native-final.log` **16/16 PASS**，v1/v2 各 success/cancel/decline/wrongstate/tokenerror/unknownissuer/configdenial/eof。测试 `packages/rpi-acp/tests/mcp_oauth_native.py`；报告实际分散在 `Ctarget/acp-mcp-oauth-native-final/{v1,v2}-{case}/result.json`（**无根目录 aggregate result.json**）。每个 success **两个 roots、两次 token exchanges、两次 tool calls**：native → RS401 → PRM → AS metadata → DCR → ACP URL interaction → **真实 loopback PKCE callback** → token → tools → model。tokens 仅在内存；fixture 检查 native auth.json 未变、transcript 无 OAuth secret。EOF 仅证明 bounded owned lifecycle，不证明第三方生产服务。新冻结 bridge `Ctarget/acp-mcp-oauth-final-bin/rpi-acp-bridge.exe` SHA-256 **`ee9b9e2c1f50d6dcc6561a9e06974e95596dd70b02dd31056f90cefffec1519f`**；native 是旧冻结 `Ctarget/acp-child-tools-enabled-bin/rpi.exe` SHA-256 **`cbc3911583f72b8834cfcc3fc18d1d8b339eb476ee95789c083beb7b76ba22fa`**（本次直接 sha256sum 验证，与 E3 同 native，不覆盖 E3 bridge/hash）。不宣称之后所有源码小补丁都已包含于冻结工件。core network/provider/callback 已实现；旧 `acp-oauth-security-review.md` 初始 B1 non-loopback HTTP blocker 后由 `validate_resource_url` admission 修复，报告的历史行号/哈希不改写。仍非第三方账号/完整 security coverage。
- **E9 — 实际 Chromium browser，MOCK ROUTE BACKEND，非 native E2E**：初始 dev **62 PASS /37 FAIL**（99 total），`Ctarget/acp-browser-dev.log`，exact `/snapshot` fixture 漏 query + remote hardcoded HMR；parent 修复所有 route overrides 为 query-aware（40 bounded fixtures），test-only `VITE_E2E_HMR` 使用 local 4191，保持普通 dev 行为。`Ctarget/acp-browser-dev-final.log` **99 PASS**，但**早于最终 optional-chain fix**，尚无 full post-fix dev rerun（parent 待重跑）。首次 production `Ctarget/acp-browser-production-final.log` **98 PASS /1 FAIL**，login 实际 `TypeError`：App.tsx usingMessageStats 的 `messageStats?.sessionId === sessionId` 可出现 undefined===undefined，随后 `!!messageStats.stats` 解引用 null。parent 单点改成 `!!messageStats?.stats`；最终 `Ctarget/acp-browser-production-fixed.log` **99 PASS**，脚本内 `tsc --noEmit` + Vite build **PASS**。两套均真实 Chromium，但后端为 mocked routes，不能称 fully real native browser；旧 pending 报告不再表示未运行。
- **E10 — 历史 sampling contract 快照（已由 C2 supersede）**：`Ctarget/acp-mcp-sampling-contract.md` 及 `acp-mcp-sampling-{test,core-tests,clippy}.log` 当时只有 12 fake-executor tests；core 89 top-level +1 nested TLS subprocess。当时没有 wire/native acceptance、capability false；**当前已不是该状态**，见 C2 的真实 native13/ACP22 与 opt-in catalog。该历史快照不关闭 GET、phase-2 durable child、full schemas/modality 缺口。

## 历史证据（D:，明确不是最新运行）

以下为已读取的既有真实 native 结果；路径虽在 D:，不能搬写为 C: 最新日志，也不能据其冻结工件推断当前部署等价。

| 编号 / 历史路径 | 已证明的范围 | 边界 |
| --- | --- | --- |
| H1 `D:/Temp/acp-system-native-run1/result.json` | systemPrompt 真实 native v1/v2 PASS，model_calls 8/7，revision `1142c83bbfa9407f9f264d1e290ffafcd54eb79e` | 文本 append/override 子集；非本轮重跑。 |
| H2 `D:/Temp/acp-mcp-native-run4/result.json` | MCP tools 真实 native v1/v2 PASS，各 11 model_calls、8 个 native results 验证，close/abort/EOF cleanup | 工具集成不是全部 prompts/templates/sampling/auth 完成。 |
| H3 `D:/Temp/acp-session-rfd-run4/result.json` | fork/title/archive 真实 native v1/v2 PASS，各 4 model_calls；分页各 107 rows / 3 pages | 使用 revision pin；非全部 session/RFD 或跨重连 cursor 保证。 |

更早的 phase2 41/0、command 6/0、fake lifecycle 7/7、ACP 96/extcore 51 等仅是先前验收快照，不再作为“最新 package/native”计数。历史 native workspace Clippy 阻塞也不能覆盖 E1 当前指定 package strict Clippy PASS。

## 尚未闭合的验收范围

整体继续为 **ACTIVE / INCOMPLETE**：当前 OAuth16（759/b1b）与 prior-pair OAuth16/remote2（713/b1b）均有实际 PASS；最新 759/b1b remote 日志在本次 cutoff 尚不存在。最新双向实际 runtime schema 已完成 capture/binding，354 validated、0 failed/unbound，但 73 unsupported /4 pending 保持 INCOMPLETE（W4）；phase-1 child 5/5 已通过；完整 v2/unstable/RFD、phase-2 native child discovery/bounded reads/extcore/ACP replay/load/resume/mutations、child external tools/普通 runtime actions、任意插件隔离、独立 GET reverse listener、完整 notification/subscription 与 catalog/MCP 集成、sampling 剩余并发/生命周期/hostile-provider 验收、OAuth 第三方账号/security、TLS/proxy/负载、native-backend browser E2E 仍未闭合。**不再将 sampling wire/native executor、opt-in catalog 模型工具、phase-1 child JSONL 或私有 parent correlation 列为完全未实现**；它们也不等于全部 ACP 完成。当前 package/Clippy 与 older-identity 验收分开，mocked-route Chromium 不冒充 native browser。不得因局部通过广告尚未实现的能力；协议版本或 RFD 生命周期变化时，先更新固定依据、协商和兼容测试，再调整 capability。
