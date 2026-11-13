# rpi-web preview 实机测试与验收记录

- 日期：2026-11-13
- 源码：`pi-rust/rpi-package` `main` @ `3d08c25`（PR #274 合并点）；另含未提交的两处诊断修复（见下）
- 本地安装：`~/.rpi/agent/extensions/librpi_web_extension-v0.1.53-68e43e702a4147be8725f1ba91473cb7.so`
  - 由 `cargo build -p rpi-web --lib --release` 产出（19,510,464 bytes，含缓存修复与版本徽标）
  - sha256 `f4e674afc90fb2469072d47c94f00abee05d8cc73886433d4257ad79ba7d2b70`（初装 `80ba4db9…`，PR #274 后 `d62f634d…`）
  - registry `~/.rpi/agent/native-packages.json` 标记 `version: 0.1.54-preview`、`source_commit: f90cde37…`（PR #274 的修复）
- 原正式版备份：`~/.rpi/agent/preview-backups/`（`.so` + `native-packages.json`，时间戳 `20261112-225458`）
- 服务：本机 `rpi --webserver`，`http://127.0.0.1:3997`（`web.json` 端口 3997）
- 前端资源：`packages/rpi-web/assets/` 由 `npm run build` 产出并嵌入 cdylib

## 适用范围（重要）

本报告的所有实机检查都针对 **`http://127.0.0.1:3997`**，即本机 `rpi --webserver` 加载的 `.so`。
`https://work-dev.bigfisharc.cn` 是另一套环境（公共 Vite 源码服务 + 独立后端），
按 `AGENTS.md` 应服务 `rpi-package-dev` worktree（`D:/Projects/rpi-package-dev`，不在本机）。
本轮改动**没有同步到那个 worktree**，因此该页面仍为旧版；重启本机 `rpi --webserver` 不会影响它。

## 重启后看不到变化的原因与修复（2026-11-13 追加）

用户反馈"重启后依旧没有任何变化"。复查发现三个叠加原因：

1. **新增 UI 不可见**：本轮唯一新增的可见界面是 Resources 面板，位于 **Inspector 最底部**；
   默认 `steer`、保活、隐藏路径、抖动修复都是行为变化，界面上零标志。
2. **HTML 无缓存控制头**：`index()` 直接返回 `Html(html)`，浏览器重启服务后可能继续用缓存的
   旧 HTML（指向旧 hash 的 JS）。已改为 `Cache-Control: no-store, must-revalidate`
   （`packages/rpi-web/src/server.rs`）。
3. **无法自证版本**：已加侧栏绿点徽标 `rpi-web preview`（旧后端显示 `rpi-web legacy`），
   title 为 `Preview build · session idle 1800`，依据 `/capabilities` 是否含
   `sessionIdleTimeoutSeconds`。截图 `/tmp/verify-badge.png` 可见。
4. **Inspector 入口不可辨 + 新 UI 藏得太深**（2026-11-13 二次反馈）：#272 把 Inspector 图标
   从 `Settings2` 换成 `SlidersHorizontal`，17px 下与相邻的 timeline `ListOrdered` 都是「三横线」，
   肉眼无法区分；且 Resources 面板放在 Inspector 最底部，需滚过整屏诊断信息。
   已改为 `PanelRight`（明确的右侧面板轮廓）+ `aria-pressed` 状态，并把 Resources 移到
   Inspector 顶部（距顶部 68px）。截图 `/tmp/final-inspector.png`。

以上修复已构建进本地 preview，服务 11:37 重启后验证：`curl -I /` 返回
`cache-control: no-store, must-revalidate`，全新 Chrome profile 加载 `index-CKWLsAd9.js`，
侧栏显示 `rpi-web preview`，Inspector 图标为 `lucide-panel-right`，打开后 Resources 立即可见。

## 自动化实机结果（已执行并落盘）

| # | 检查 | 方式 | 结果 |
|---|---|---|---|
| 1 | 服务可用 | `GET /api/v1/health` | PASS `{"ok":true,"service":"rpi-web","protocol":"rpi"}` |
| 2 | preview 已加载（#260） | `GET /api/v1/capabilities` | PASS `sessionIdleTimeoutSeconds: 1800`（旧版无此字段） |
| 3 | 全局资源可见（#262） | `GET /projects/{id}/resources?scope=global` | PASS `success=true`, 2 条：`librpi_web_extension…`(extension, in use)、`playwright-agentd`(skill, in use) |
| 4 | 项目资源可见（#262） | `?scope=project` | PASS `success=true`, 0 条（当前项目确实没有项目级资源，面板显示 "No resources found."） |
| 5 | 非法 scope 拒绝 | `?scope=bogus` | PASS HTTP 400 `scope must be project or global` |
| 6 | 隐藏路径边界未放宽（#261） | `/file?path=.rpi/uploads/a.md`、`node_modules/…` | PASS HTTP 403 `path is outside the browsable project` |
| 7 | 生产 bundle 含新代码 | `GET /` + 主 JS 字符串扫描 | PASS `resourcesRestartHint`、`/resources?`、`Queue type`、`useState("steer")` |
| 8 | Inspector 资源面板（真实 Chrome） | Playwright 直连 3997，打开 Inspector | PASS 同时出现 Global / Project 标题、`playwright-agentd`、restart 提示；两个 scope 的 `/resources?` 请求均发出 |
| 9 | 插话默认 `steer`（#259） | 真实会话 `4e683613…` 发起 run 后插话，抓 `POST /commands` | PASS `{"message":"interjection: ignore the rest","deliverAs":"steer"}` |
| 10 | 前端隐藏路径不生成按钮（#261） | e2e `message-links.spec.ts`（5 项） | PASS（改前失败、改后通过，已验证） |
| 11 | 保活/重开/抖动/重入队等 | e2e + `cargo test` | PASS：`cargo test -p rpi-extcore -p rpi-httpcore --lib` 40 passed；tsc 通过；相关 Playwright 用例通过 |
| 12 | 长资源名不撑破面板 | 真实 Chrome 量测 `.resource-item-head strong` 右边界 vs 面板右边界 | PASS（加 `min-width:0` 后 name right `1196` ≤ panel right `1268`）；修复见 PR #274 |

| 13 | HTML 不被缓存 | `curl -I /` | PASS `cache-control: no-store, must-revalidate` |
| 14 | 版本可自证 | 全新 Chrome profile 直连 3997 | PASS 侧栏 `rpi-web preview`（title `Preview build · session idle 1800`），加载 `index-CKWLsAd9.js` |
| 15 | Inspector 入口可辨 | 读取按钮 `svg` class + `aria-pressed` | PASS `lucide-panel-right`，点击后 `aria-pressed=true`（旧为 `lucide-sliders-horizontal`，与 timeline 图标混淆） |
| 16 | 新 UI 无需滚动即可见 | 量测 `.resource-panel` 与 `aside.inspector` 顶部距离 | PASS delta `68px`（原来在整屏诊断信息之后） |

截图：`/tmp/preview-resources.png`、`/tmp/preview-resources-scrolled.png`、`/tmp/preview-resources-fixed.png`、`/tmp/diag2.png`、`/tmp/verify-badge.png`（版本徽标）、`/tmp/final-inspector.png`（Inspector 顶部直接显示 Resources）。
> 注：截图为临时路径，未纳入仓库；如需归档请复制到 `docs/`。

## 需要真人验收（未自动判定，请逐项确认）

以下项目涉及主观视觉、真实模型交互或生产环境差异，自动化只能证明"没有报错"，**不能代替真人判断**。请在本地 `http://127.0.0.1:3997` 打开页面逐项勾选，并把结论写回本文件。

- [ ] **A. 资源面板可读性**：打开右上 Inspector（`SlidersHorizontal` 图标），滚动到底部 "Resources"。
      - Global 列表是否清晰展示 kind / 版本 / in-use / 路径 / diagnostics？
      - 长文件名是否正确换行、不撑破面板？（已由自动量测 #12 覆盖，请真人确认视觉上仍易读）
      - "Changing skills, prompts or context files requires restarting the native host" 提示是否位置合适、不误导？
      - 中文界面（`Settings → 简体中文`）下 "全局 / 项目 / 资源 / 刷新资源" 文案是否准确？
- [ ] **B. 项目级资源存在时的展示**：当前已配置项目均无项目级 skill，只显示了 "No resources found."。
      请在一个项目里放一个 `.rpi/skills/<name>/SKILL.md`，刷新资源，确认 Project 区域出现该条目且 path 正确。
- [ ] **C. 插话时机体感**：跑一个多轮任务，在最终答案前插话，确认 `steer` 让补充指令在下一轮 LLM 请求生效，而不再等到任务结束；再手动切到 `followUp` / `nextTurn` 比较差异，确认默认值符合预期。
- [ ] **D. 会话保活体感**：把 `~/.rpi/agent/web.json` 临时设为 `{"sessionTimeoutSeconds": 8, "port": 3997}` 并重启服务，新建会话后持续输入但**不按回车**超过 12 秒，再发送，确认不再出现 `session not found`。（自动化已用 fake clock 覆盖逻辑，此项验证真实计时器/代理行为。）
- [ ] **E. 抖动主观评估**：发一条包含围栏代码块与反引号的长回复，观察跟随滚动时是否还有上下跳动。本次只消除了 `.working-indicator` 的 34px 部分；Markdown 重排导致的 -24.5px 仍可能存在，请判断残留抖动是否可接受。
- [ ] **F. 设置界面区分**：侧栏底部 "Settings"（齿轮）与 "Runtime settings"（滑杆）图标是否已足够区分？是否需要进一步合并成一个带分区的设置页（需产品决策）？
- [ ] **G. 侧栏密度**：项目全部默认展开、最近会话 5 条、共用一个滚动区，在多个项目各有多条会话时是否仍难用？是否需要紧凑/展开切换（需布局方案）。
- [ ] **H. 回归面**：真实跑一次带附件的消息、Markdown 图片预览（#264 新加的能力）、跨会话切换，确认本次改动没有引入视觉或交互回退。
- [ ] **I. 预览版回退**：确认验收完成后可以用 `~/.rpi/agent/preview-backups/` 里的文件恢复正式版 0.1.53，并重启服务验证。

## 已知问题 / 未修复项（如实记录）

1. **CI 两个既有失败**：`e2e/model-preference.spec.ts` 的 "browser origin isolation…" 与 "cached preference reads…" 在 production preview 下动态 `import("/src/modelPreference.ts")` 失败。在干净的 `origin/main`（PR #264 合并点）上同样失败，属于 #264 引入、与本次改动无关。PR #273 使用 admin merge。
2. **#258 只修一半**：Markdown 流式内容高度非单调（围栏边界重排）仍是 "needs design"，未处理。
3. **#263 只修图标**：两个设置界面合并、侧栏密度均需产品/布局决策，未处理。
4. **#262 为只读视图**：没有启用/禁用/安装入口；`resourceState.enabled=false`（本机 native host 未开启 exact selection），因此面板展示的是 `rpi resource list` 的本地目录观察，不是会话内生效的精确资源绑定。真人验收时请注意这一区别。
5. **PR #281 状态**：`server.rs` 缓存头、版本徽标、`PanelRight` 图标、Resources 置顶均已包含在 PR #281 中。
6. **本地 rustfmt 与 CI 版本差异**：`packages/rpi-voice/src/pet.rs` 在本机 rustfmt 1.10.0 下仍报格式差异（origin/main 同样如此），未改动该文件。

## 回退方式

```bash
DIR=~/.rpi/agent/extensions
BK=~/.rpi/agent/preview-backups
cp "$BK"/librpi_web_extension-v0.1.53-68e43e702a4147be8725f1ba91473cb7.so.bak-preview-20261112-225458 \
   "$DIR"/librpi_web_extension-v0.1.53-68e43e702a4147be8725f1ba91473cb7.so
cp "$BK"/native-packages.json.bak-preview-20261112-225458 ~/.rpi/agent/native-packages.json
# 重启 rpi --webserver
```
