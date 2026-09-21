# rpi-server

TCP JSONL server for rpi with streaming subscriptions — 基于 **Transport trait** 抽象，支持多种传输协议。

## 特性

- ✅ **TCP JSONL 协议** — 简单、高效、易于调试
- ✅ **Transport trait 抽象** — 可扩展 WebSocket/Unix Socket 等传输方式
- ✅ **流式订阅** — 通过 `subscribe` 方法订阅实时事件流
- ✅ **会话管理** — 支持多个 rpi 会话并行
- ✅ **连接管理** — 内置重试、心跳、延迟检测、在线状态追踪
- ✅ **标准化** — JSON-RPC 2.0 消息格式，跨语言兼容

## 使用方式

### 通过 rpi CLI 启动（推荐）

```bash
# 启动 rpi 并自动启动 TCP 服务器（默认 127.0.0.1:9800）
rpi --server

# 指定端口
rpi --server --port 8080

# 指定绑定地址
rpi --server --bind 0.0.0.0 --port 9800
```

rpi-server 作为扩展加载，检测到 `--server` flag 后自动启动 TCP 服务器。

### 在 rpi 会话中管理

```json
// 启动服务器
{
  "action": "start",
  "port": 9800
}

// 响应
{
  "id": "rpi-rpc-1",
  "address": "127.0.0.1:9800",
  "token": "a1b2c3d4...",
  "state": "running",
  "protocol": "jsonl",
  "transport": "tcp"
}

// 查看状态
{
  "action": "status",
  "id": "rpi-rpc-1"
}

// 列出所有服务器
{
  "action": "list"
}

// 停止服务器
{
  "action": "stop",
  "id": "rpi-rpc-1"
}
```

### 客户端连接

#### 方式一：使用 nc 测试

```bash
# 连接服务器
nc 127.0.0.1 9800

# 发送启动会话请求（每行一个 JSON）
{"jsonrpc":"2.0","id":1,"method":"start_session","params":{}}

# 收到响应
{"jsonrpc":"2.0","id":1,"result":{"sessionId":"session-1","session":{...}}}

# 发送命令
{"jsonrpc":"2.0","id":2,"method":"send","params":{"sessionId":"session-1","command":{"type":"prompt","content":"hello"}}}

# 订阅事件流
{"jsonrpc":"2.0","id":3,"method":"subscribe","params":{"sessionId":"session-1"}}

# 收到事件推送
{"jsonrpc":"2.0","method":"event","params":{"subscriptionId":1,"event":{"type":"delta","content":"Hello"}}}
```

#### 方式二：使用 rpi 的 rpc_client 工具

```json
// 启动会话
{
  "serverId": "rpi-rpc-1",
  "method": "start_session",
  "params": {}
}

// 发送命令
{
  "serverId": "rpi-rpc-1",
  "method": "send",
  "params": {
    "sessionId": "session-1",
    "command": {"type": "prompt", "content": "hello"}
  }
}

// 订阅事件流
{
  "serverId": "rpi-rpc-1",
  "method": "subscribe",
  "params": {"sessionId": "session-1"},
  "subscribe": true,
  "timeoutSeconds": 30
}
```

#### 方式三：使用 ManagedConnection（Rust 代码）

```rust
use std::sync::Arc;
use rpi_server::transport::{ManagedConnection, TcpTransport};
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 创建连接管理器
    let transport = Arc::new(TcpTransport::new());
    let conn = ManagedConnection::new(transport);
    
    // 连接（自动重试 10 次）
    conn.connect_with_retry("127.0.0.1:9800").await?;
    
    // 发送请求
    conn.send(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "start_session",
        "params": {}
    })).await?;
    
    // 接收响应
    if let Some(response) = conn.recv().await? {
        println!("response: {}", response);
    }
    
    // 启动心跳（独立 task）
    let conn_clone = conn.clone();
    tokio::spawn(async move {
        let _ = conn_clone.start_heartbeat().await;
    });
    
    // 查看连接状态
    let status = conn.status().await;
    println!("connected: {}", status.connected);
    println!("latency: {}ms", status.latency_ms);
    
    Ok(())
}
```

## 可用方法

| 方法 | 说明 |
|------|------|
| `start_session` | 启动新会话 |
| `send` | 发送命令到会话 |
| `stop_session` | 停止会话 |
| `list_sessions` | 列出所有会话 |
| `server_status` | 获取服务器状态 |
| `subscribe` | 订阅事件流 |
| `ping` | 心跳检测 |

## 协议格式

每行一个 JSON，以 `\n` 结尾：

```
请求: {"jsonrpc":"2.0","id":1,"method":"start_session","params":{}}\n
响应: {"jsonrpc":"2.0","id":1,"result":{...}}\n
事件: {"jsonrpc":"2.0","method":"event","params":{"subscriptionId":1,"event":{...}}}\n
```

## 架构

```
Transport trait (抽象层)
    ↓
TcpTransport (已实现) / WebSocketTransport (未来)
    ↓
ManagedConnection (连接管理)
    - 自动重试 (10次)
    - 心跳检测 (30s)
    - 延迟测量
    - 在线状态
    ↓
AppState + Sessions (业务逻辑)
```

## Transport trait 扩展

要添加新的传输协议（如 WebSocket），只需实现 `Transport` 和 `Connection` trait：

```rust
use async_trait::async_trait;
use rpi_server::transport::{Transport, Connection};

pub struct WebSocketTransport {
    // ...
}

#[async_trait]
impl Transport for WebSocketTransport {
    type Connection = WebSocketConnection;
    
    async fn listen(&self, addr: &str) -> Result<(), String> {
        // 实现 WebSocket 监听
    }
    
    async fn accept(&self) -> Result<WebSocketConnection, String> {
        // 实现接受连接
    }
    
    async fn connect(&self, addr: &str) -> Result<WebSocketConnection, String> {
        // 实现客户端连接
    }
    
    async fn shutdown(&self) -> Result<(), String> {
        // 实现关闭
    }
}

pub struct WebSocketConnection {
    // ...
}

#[async_trait]
impl Connection for WebSocketConnection {
    async fn send(&mut self, msg: serde_json::Value) -> Result<(), String> {
        // 实现发送
    }
    
    async fn recv(&mut self) -> Result<Option<serde_json::Value>, String> {
        // 实现接收
    }
    
    fn id(&self) -> &str {
        // 返回连接 ID
    }
    
    async fn close(&mut self) -> Result<(), String> {
        // 实现关闭
    }
}
```

然后在 `run_server` 中使用：

```rust
let transport = Arc::new(WebSocketTransport::new());
run_server(transport, "127.0.0.1", 9800, token, executable, args).await?;
```

## 连接管理

`ManagedConnection` 提供以下功能：

### 自动重试

```rust
// 连接失败时自动重试，最多 10 次，间隔递增
conn.connect_with_retry("127.0.0.1:9800").await?;
```

### 心跳检测

```rust
// 启动心跳任务（30s 间隔）
let conn_clone = conn.clone();
tokio::spawn(async move {
    let _ = conn_clone.start_heartbeat().await;
});
```

### 状态追踪

```rust
let status = conn.status().await;
println!("connected: {}", status.connected);
println!("latency: {}ms", status.latency_ms);
println!("messages sent: {}", status.messages_sent);
println!("messages received: {}", status.messages_received);
```

## 性能

- **TCP JSONL** 比 WebSocket 少一层协议开销，但需要自己处理消息边界（`\n`）
- **broadcast channel** 支持多个订阅者同时接收事件
- **异步 I/O** — 基于 tokio，高并发性能优秀
- **零拷贝** — 消息直接序列化到 TCP 流，无中间缓冲

## 与 WebSocket 对比

| 特性 | TCP JSONL | WebSocket |
|------|-----------|-----------|
| 协议开销 | 低（仅 `\n` 分隔） | 中（帧协议） |
| 浏览器支持 | ❌ 需要代理 | ✅ 原生支持 |
| 调试难度 | 低（nc/telnet） | 中（需要工具） |
| 心跳检测 | 应用层实现 | 协议自带 |
| 适用场景 | 服务间通信 | Web 客户端 |

## License

MIT
