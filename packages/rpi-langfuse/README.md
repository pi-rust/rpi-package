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
  - `langfuse_trace`：查询/更新 trace

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
