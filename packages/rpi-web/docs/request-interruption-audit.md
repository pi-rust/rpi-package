# 中断路径审计：会话/任务为何断，哪里能安全重试（#301）

> 范围：`packages/rpi-web/frontend/src`（浏览器侧）。基线 `main@85fb0bd`。
> 本文只记现状与结论，不改行为；最后给出一个已经落地的最小代码改动。

## 请求面：一个函数管所有 HTTP

`src/api.ts:468` 的 `request<T>()`：

- 直接 `fetch('/api/v1'+path)`，**无连接/响应超时**（只有调用方显式传 `AbortSignal` 才会中断）；
- **无自动重试**；非 2xx 抛 `ApiError(status, code)`；
- 唯一的例外路径：`commandData()`（`api.ts:586` 起）对 404（host 已退休）做
  「重开 host + 重放一次」，且只对 `REPLAY_SAFE_COMMANDS` 白名单生效。

全部流量（读 GET + 写 POST/DELETE）都走它；SSE 不走它（浏览器 `EventSource`
自带重连，见下）。

## 中断路径清单与幂等性结论

| # | 路径 | 触发 | 现有恢复机制 | 可否自动重试 |
|---|---|---|---|---|
| 1 | 命令 POST 瞬时网络失败（连接重置/代理 5xx/超时） | 弱网、代理抖动 | 无。`commandData` 只处理 404 | **POST 不可盲目重试**（会重复入队/重复执行）；GET 类可 |
| 2 | 命令 POST 返回 404（host 退休/idle 回收） | bridge 30 分钟回收空闲 host | `commandData`：`reopenHost()` + 白名单命令重放一次（`api.ts:520-548,586`） | 已处理，白名单外不重放 |
| 3 | SSE 断流（idle EOF、代理断开） | 长时间无事件 | `EventSource` 自动重连；`App.tsx:2226` 起 onerror 探测 host 是否真退休再决定 UI 状态 | 浏览器已重试，无需应用层做 |
| 4 | SSE 404（host 被退休后流再也打不开） | 退休 + 探测 | `App.tsx:2273-2282`：先 `hostIsRetired()` 探测，必要时重开 | 已处理 |
| 5 | 上传文件失败 | 弱网 | 无自动重试；UI 报错 | 不可（FormData 重传代价大且非幂等） |
| 6 | 页面刷新后恢复 | 用户刷新 | URL 状态 + 会话恢复流程，非重试范畴 | 不适用 |
| 7 | 登录 401 | 凭据过期 | 跳登录页 | 不可 |

## 已落地的最小改动

对**只读 GET** 加超时 + 有限退避重试；写操作一律不重试（保持上表结论）。
具体见同分支 `src/api.ts` 的 `requestWithRetry`：仅当 `init?.method` 缺省
（即 GET）时启用，最多 3 次尝试，300ms 起指数退避；`ApiError`（服务器明确
应答）不重试，只对网络层异常（`TypeError: Failed to fetch` / `AbortError`）
重试；尊重调用方自己的 `AbortSignal`。

> 会改变状态的 POST（`prompt`、`queue_message` 等）刻意不动：host 侧没有
> 请求去重键，盲目重试会产生重复 prompt/重复入队，比「报错让用户重发」更糟。

## 未验证 / 边界

- 本文行号对应 `main@85fb0bd`，后续 main 演进后需复核。
- `TypeError: Failed to fetch` 也可能由 CORS/服务宕机引起；对 GET 重试 2 次
  是无害的（最多多花 900ms），但不区分根因。
- SSE 的重连节奏由浏览器内置策略控制（Chrome 默认约 3s 起），本审计未建议
  在应用层再包一层重试——那会造成双重重连。
