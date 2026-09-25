# rpi-langfuse

Langfuse 集成插件，为 rpi 提供 LLM 可观测性追踪。

## 功能

- **自动追踪**：监听 agent 生命周期事件，自动上报到 Langfuse
  - Session 开始/结束 → trace（含 userId/sessionId/metadata）
  - Provider 请求/响应 → generation（含 model、modelParameters、usage、TTFT）
  - Tool 调用/结果 → span
  - Turn 开始/结束 → span
  - Agent 开始/结束 → span
  - Session compact（上下文压缩）→ span
- **手动工具**：
  - `langfuse_score`：为 trace/observation 添加评分
  - `langfuse_prompt`：管理 prompt 版本
  - `langfuse_trace`：查询 trace（`get` 返回该 trace 的所有 observations；`list` 每 trace 一行 root observation）

## 传输通道：OTLP（Langfuse v4 必须）

插件通过 **OpenTelemetry/HTTP JSON** 上报，端点 `POST {baseUrl}/api/public/otel/v1/traces`，
并带上 `x-langfuse-ingestion-version: 4`（不带这个头，v4 会把它当成旧 SDK 走 dual-write，延迟可达 15 分钟）。

> 为什么不用 `/api/public/ingestion`：Langfuse v4 的 `events_only` 部署**只接受 `score-create` / `sdk-log`**，
> `trace-create` / `span-create` / `generation-create` 一律返回
> `400 Event type "..." is not accepted by /api/public/ingestion when LANGFUSE_MIGRATION_V4_WRITE_MODE is events_only`。
> 这也是本插件 0.2.0 的改动：把旧批量事件模型换成「缓冲完整 span、结束时一次性导出」，
> 因为 OTLP 没有 update 语义（一个 span 只能导出一次，结束时间/输出必须在导出前就确定）。

要点：

- **id 必须是 OTLP 格式**：trace id = 32 位十六进制，span id = 16 位十六进制（旧版的 `obs-xxxx` 会被拒）。
- **子代理嵌套**：`LANGFUSE_PI_PARENT_TRACE_ID` / `LANGFUSE_PI_PARENT_SPAN_ID` 现在直接作为
  OTLP 的 traceId / parentSpanId 使用，所以子代理会正确挂在触发它的那一轮下面。
- **只导出已结束的 span**：中途 flush（一轮里还有 tool call）只发送已经结束的生成/工具 span；
  一轮结束（助手消息不含 tool call）时才收尾根 span 并整体导出。
- **v4 读接口**：`langfuse_trace` 用 `/api/public/v2/observations?traceId=...`（旧的 `/api/public/traces` 已 404），
  `langfuse_score` 的 list 用 `/api/public/v3/scores`（旧 `/api/public/scores`、`/v2/scores` 已 404）。
  `langfuse_trace.update` 在 v4 不可用（会返回明确报错）。

### 一次性运行（`rpi -p`）也能上报

print / `--mode json` 模式下宿主**不会**派发 `AgentEnd` / `AgentSettled` / `SessionShutdown`
（实测只派发 `AgentStart` / `BeforeAgentStart` / `BeforeProviderRequest` / `MessageEnd`），
所以本插件在**助手消息结束且没有后续 tool call**时就把该轮收尾并导出——否则 `rpi -p` 会一直缓冲到进程退出、什么都发不出去。

### 错误会出现在哪（不会污染 TUI）

插件的**唯一**用户可见错误通道是宿主状态栏（`SetStatus` runtime action）：

| 时机 | 状态栏 |
|---|---|
| 一轮开始（trace 已建） | `langfuse ✓` |
| 导出成功 | `langfuse ✓ (trace sent)` |
| 导出失败 | `langfuse ✗ (flush failed)` |

**绝不裸写 stderr**：全屏 TUI 下 `eprintln!` 会落在光标处（也就是输入行），把 alt-screen 画乱。
导出失败时 span 会留在缓冲区、下次 flush 重试，不会丢数据。详细错误只在 `RPI_LANGFUSE_DEBUG=1` 时打到 stderr（明确的人工排障开关）。

### 排障

```bash
RPI_LANGFUSE_DEBUG=1 rpi -p "..."     # stderr 打印收到的事件、导出的 span 数、失败原因
```

输出形如：

```
[rpi-langfuse] event: BeforeAgentStart
[rpi-langfuse] event: BeforeProviderRequest
[rpi-langfuse] event: MessageEnd
[rpi-langfuse] exporting 2 span(s) via OTLP
```

### 测试

```bash
cargo test -p rpi-langfuse                 # 单元测试（不含网络）
# 联网回环（需要真实凭据）：
LANGFUSE_BASE_URL=https://langfuse.laofu.online LANGFUSE_PUBLIC_KEY=pk-lf-... LANGFUSE_SECRET_KEY=sk-lf-... cargo test -p rpi-langfuse -- --ignored --nocapture
#   live_otlp_round_trip     —— 手工 span 直接导出 + v2 API 读回
#   live_plugin_event_flow   —— 驱动真实事件处理器跑完整一轮（root + generation）再读回
```

## 配置

支持两种配置方式，环境变量优先级高于配置文件。

### 方式一：环境变量

```bash
export LANGFUSE_BASE_URL="https://your-langfuse-instance.com"
export LANGFUSE_PUBLIC_KEY="pk-xxx"
export LANGFUSE_SECRET_KEY="sk-xxx"
```

### 方式二：配置文件

创建 `.rpi/langfuse.json`（项目级）或 `~/.rpi/agent/langfuse.json`（全局）：

```json
{
  "baseUrl": "https://your-langfuse-instance.com",
  "publicKey": "pk-xxx",
  "secretKey": "sk-xxx"
}
```

或通过环境变量引用：

```json
{
  "baseUrl": "https://your-langfuse-instance.com",
  "publicKeyEnv": "LANGFUSE_PUBLIC_KEY",
  "secretKeyEnv": "LANGFUSE_SECRET_KEY"
}
```

### 配置优先级

1. 环境变量（LANGFUSE_BASE_URL 等）
2. 配置文件中的直接值（baseUrl 等）
3. 配置文件中的环境变量引用（baseUrlEnv 等）
4. 默认值（https://cloud.langfuse.com）

## 安装

```bash
rpi install rpi-langfuse
```

## 使用示例

### 自动追踪

安装后自动生效，无需额外配置。所有 agent 活动会自动上报到 Langfuse。

### 手动评分

```json
{
  "tool": "langfuse_score",
  "params": {
    "name": "accuracy",
    "value": 0.95,
    "traceId": "trace-123",
    "comment": "High accuracy response"
  }
}
```

### 查询 Trace

```json
{
  "tool": "langfuse_trace",
  "params": {
    "action": "get",
    "traceId": "trace-123"
  }
}
```

## 开发

```bash
cargo build --package rpi-langfuse
cargo test --package rpi-langfuse
```

## License

MIT
