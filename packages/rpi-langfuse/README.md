# rpi-langfuse

Langfuse 集成插件，为 rpi 提供 LLM 可观测性追踪。

## 功能

- **自动追踪**：监听 agent 生命周期事件，自动上报到 Langfuse
  - Session 开始/结束 → trace（含 userId/sessionId/metadata/trace name/tags）
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
- **trace 名称 / tag / source 全部由宿主派生**：宿主在 `BeforeAgentStart` payload 里带
  `host: {name, version}`，插件据此生成 trace name（`"{host} Turn"` → `rpi Turn`）、
  trace tag（`["rpi"]`）、`langfuse.trace.metadata.source` 和 OTLP 的 `service.name`。
  改名只需要改宿主一处，插件不会留下过期的品牌名；老宿主（没有 `host` 字段）退回内置常量。
  `service.version` 是**宿主**版本，`telemetry.sdk.version` 才是插件版本。
- **trace 级属性下发到每一个 span**：`langfuse.trace.name` / `langfuse.session.id` /
  `langfuse.user.id` / `langfuse.trace.tags` / `langfuse.version` 不只写在根 span 上，
  而是写在该 trace 的**所有** span 上。Langfuse 官方文档明确要求这样（OTEL →
  "Propagating Trace Attributes to All Spans"）：Langfuse 只在 span 级属性上做 session/user/tag
  的筛选与聚合，Langfuse SDK 用 `propagate_attributes`/baggage 实现同一件事。
  另外每个 span 同时带 `langfuse.observation.metadata.*` 和 `langfuse.trace.metadata.*`，
  兼容只从根 span 读 trace metadata 的旧版本。
- **只导出已结束的 span**：中途 flush（一轮里还有 tool call）只发送已经结束的生成/工具 span；
  一轮结束（助手消息不含 tool call）时才收尾根 span 并整体导出。
- **根 span 判定**：`parentSpanId` 为空且没有继承的父 span 才算根；子代理自己的
  `Subagent Turn` 会挂在父轮根 span 上，不再被误当成第二个 trace 根。
- **trace metadata 只在根 span 播种一次**：`langfuse.trace.metadata.*` 取自轮次根 span 的
  metadata，因此每个 span 上报的是同一套；子 span 自己的字段（`tool_name`、`stop_reason`
  等）只出现在 `langfuse.observation.metadata.*`，不会被当成 trace 级事实。
- **v4 读接口**：`langfuse_trace` 用 `/api/public/v2/observations?traceId=...`（旧的 `/api/public/traces` 已 404），
  `langfuse_score` 的 list 用 `/api/public/v3/scores`（旧 `/api/public/scores`、`/v2/scores` 已 404）。
  `langfuse_trace.update` 在 v4 不可用（会返回明确报错）。

### sessionId 怎么来的

Langfuse 的 session 用 `langfuse.session.id` 标识。宿主在三条通道上给出同一个值，
插件**取最新鲜的那一条**（过期风险不同，所以顺序有讲究）：

| 序 | 通道 | 说明 |
|---|---|---|
| 1 | `BeforeAgentStart` payload 的 `sessionId` | 宿主每轮重算，能跟住 session 切换；不可能被父进程环境变量污染 |
| 2 | `__rpi.sessionId`（插件工具调用参数） | 宿主每次工具调用都注入，模型看不到也改不了 |
| 3 | `RPI_SESSION_ID`（环境变量） | 宿主只写一次，而**环境变量会被子进程继承**（rpi 拉起 rpi 时会串），所以排最后 |
| 末 | `pid:<进程号>` | 宿主没有上面任何一条时的兜底，一个进程 = 一个 Langfuse session |

实现要点：

- `__rpi` 是在 `ToolExecutionStart` 里从**原始** argv 摘出来锁存的（redact 之前），
  锁存值"后到覆盖先到"，所以一个进程内换 session 能自愈；
- 拿到更新的 id 后，把**当前 trace 里所有还没导出的 span**（含更早建好的根 span）
  的 trace 上下文整体改写，导出发生在轮次结束，来得及；
- `__rpi` 属于宿主的路由信封（`cwd` + `sessionId`），不是模型输入，
  所以上报 tool input 前会被 `strip_host_tool_context` 剔掉。

> 0.2.4 之前始终回落到字面量 `"default"`，所有项目、所有会话都挤在一个 Langfuse session 里。

配套的 pi-rust 改动（commit `733f80d`，随下一个版本发布）：宿主在会话建好后写 `RPI_SESSION_ID`，并在
`BeforeAgentStart` 的 payload 里带上 `sessionId`——后者是让**第一轮**（还没发生过任何
工具调用）就能报出真实 id 的关键。

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
