# rpi-web preview 实机测试与验收记录

- 日期：2026-11-13
- 源码：`pi-rust/rpi-package` `main` @ `3736c9d`（PR #273 合并点），预览分支 HEAD `61ae777`
- 本地安装：`~/.rpi/agent/extensions/librpi_web_extension-v0.1.53-68e43e702a4147be8725f1ba91473cb7.so`
  - 由 `cargo build -p rpi-web --lib --release` 产出（19,509,120 bytes）
  - sha256 `d62f634d899df02747d3838f09ad0bf59496616cb9eba819851b8e043b73bfc1`（含长名称换行修复；初装时为 `80ba4db9…`）
  - registry `~/.rpi/agent/native-packages.json` 标记 `version: 0.1.54-preview`、`source_commit: f90cde37…`（PR #274 的修复）
- 原正式版备份：`~/.rpi/agent/preview-backups/`（`.so` + `native-packages.json`，时间戳 `20261112-225458`）
- 服务：本机 `rpi --webserver`，`http://127.0.0.1:3997`（`web.json` 端口 3997）
- 前端资源：`packages/rpi-web/assets/` 由 `npm run build` 产出并嵌入 cdylib

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
| 12 | 长资源名不撑破面板 | 真实 Chrome 量测 `.resource-item-head strong` 右边界 vs 面板右边界 | PASS（加 `min-width:0` 后 name right `1196` ≤ panel right `1268`）；修复见 PR #274，preview 已重新构建加载（sha256 `d62f634d899df02747d3838f09ad0bf59496616cb9eba819851b8e043b73bfc1`） |

截图：`/tmp/preview-resources.png`（Inspector 打开）、`/tmp/preview-resources-scrolled.png`（资源面板滚动后可见）、`/tmp/preview-resources-fixed.png`（长名称换行修复后）。
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
5. **本地 rustfmt 与 CI 版本差异**：`packages/rpi-voice/src/pet.rs` 在本机 rustfmt 1.10.0 下仍报格式差异（origin/main 同样如此），未改动该文件。

## 回退方式

```bash
DIR=~/.rpi/agent/extensions
BK=~/.rpi/agent/preview-backups
cp "$BK"/librpi_web_extension-v0.1.53-68e43e702a4147be8725f1ba91473cb7.so.bak-preview-20261112-225458 \
   "$DIR"/librpi_web_extension-v0.1.53-68e43e702a4147be8725f1ba91473cb7.so
cp "$BK"/native-packages.json.bak-preview-20261112-225458 ~/.rpi/agent/native-packages.json
# 重启 rpi --webserver
```
