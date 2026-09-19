# rpi-server

JSON-RPC 2.0 server for rpi with **streaming subscriptions** — 基于 **jsonrpsee** 和 **WebSocket** 传输。

## 特性

- ✅ **JSON-RPC 2.0 标准协议** — 类型安全、工具友好、跨语言兼容
- ✅ **WebSocket 长连接** — 低延迟、高性能、支持多客户端并发
- ✅ **流式订阅** — 通过 `subscribe` 订阅实时事件流
- ✅ **会话管理** — 支持多个 rpi 会话并行
- ✅ **标准化** — 告别原始 TCP JSONL，拥抱标准协议

## 与旧版对比

| 特性 | 旧版 (TCP JSONL) | 新版 (JSON-RPC 2.0) |
|------|-----------------|-------------------|
| **协议** | 自定义 JSONL | JSON-RPC 2.0 标准 |
| **传输** | 原始 TCP | WebSocket |
| **流式** | 逐行中继 | 订阅模式 |
| **会话管理** | 隐式（连接=会话） | 显式 `start_session`/`stop_session` |
| **类型安全** | ❌ | ✅ (jsonrpsee) |
| **工具支持** | 需要专用工具 | 可用任何 JSON-RPC 客户端 |

## 安装

```bash
cargo build -p rpi-server --release
```

## 使用方式

### 独立运行

```bash
# 启动服务器（默认 ws://127.0.0.1:9800）
rpi-server

# 指定端口
rpi-server --port 8080

# 传递 rpi 参数
rpi-server -- --provider anthropic --model claude-3-sonnet
```

### 作为 rpi 插件

```json
// 启动服务器
{ "action": "start", "port": 9800 }

// 响应
{
  "id": "rpi-rpc-1",
  "address": "ws://127.0.0.1:9800",
  "token": "a1b2c3d4...",
  "state": "running",
  "protocol": "jsonrpc-2.0",
  "transport": "websocket"
}
```

### 客户端调用

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

// 流式订阅（实时接收事件）
{
  "serverId": "rpi-rpc-1",
  "method": "subscribe",
  "params": {"sessionId": "session-1"},
  "subscribe": true
}
```

## JSON-RPC 方法

### `start_session`

启动新的 rpi 会话。

```json
{
  "jsonrpc": "2.0",
  "method": "start_session",
  "params": {},
  "id": 1
}
```

**响应：**
```json
{
  "jsonrpc": "2.0",
  "result": {
    "sessionId": "session-1",
    "session": {
      "id": "session-1",
      "state": "running",
      "pid": 12345,
      "event_count": 0
    }
  },
  "id": 1
}
```

### `send`

向会话发送命令。

```json
{
  "jsonrpc": "2.0",
  "method": "send",
  "params": {
    "sessionId": "session-1",
    "command": {"type": "prompt", "content": "hello"}
  },
  "id": 2
}
```

### `subscribe` (Subscription)

订阅会话的事件流（流式响应）。

```json
{
  "jsonrpc": "2.0",
  "method": "subscribe",
  "params": {"sessionId": "session-1"},
  "id": 3
}
```

**流式事件：**
```json
{"type": "event", "delta": "Hello"}
{"type": "event", "delta": " world"}
{"type": "event", "delta": "!"}
{"type": "session_end"}
```

### `stop_session`

停止会话。

```json
{
  "jsonrpc": "2.0",
  "method": "stop_session",
  "params": {"sessionId": "session-1"},
  "id": 4
}
```

### `list_sessions`

列出所有会话。

```json
{
  "jsonrpc": "2.0",
  "method": "list_sessions",
  "params": {},
  "id": 5
}
```

### `server_status`

获取服务器状态。

```json
{
  "jsonrpc": "2.0",
  "method": "server_status",
  "params": {},
  "id": 6
}
```

## 跨语言客户端

由于使用标准 JSON-RPC 2.0，可以用任何语言调用：

### Python

```python
import websockets
import json

async def main():
    async with websockets.connect("ws://127.0.0.1:9800") as ws:
        # 启动会话
        await ws.send(json.dumps({
            "jsonrpc": "2.0",
            "method": "start_session",
            "params": {},
            "id": 1
        }))
        response = json.loads(await ws.recv())
        session_id = response["result"]["sessionId"]
        
        # 发送命令
        await ws.send(json.dumps({
            "jsonrpc": "2.0",
            "method": "send",
            "params": {
                "sessionId": session_id,
                "command": {"type": "prompt", "content": "hello"}
            },
            "id": 2
        }))
        await ws.recv()
        
        # 订阅流式事件
        await ws.send(json.dumps({
            "jsonrpc": "2.0",
            "method": "subscribe",
            "params": {"sessionId": session_id},
            "id": 3
        }))
        
        # 接收流式事件
        while True:
            event = json.loads(await ws.recv())
            print(event)
            if event.get("type") == "session_end":
                break

asyncio.run(main())
```

### JavaScript/Node.js

```javascript
const WebSocket = require('ws');

const ws = new WebSocket('ws://127.0.0.1:9800');

ws.on('open', () => {
    // 启动会话
    ws.send(JSON.stringify({
        jsonrpc: '2.0',
        method: 'start_session',
        params: {},
        id: 1
    }));
});

ws.on('message', (data) => {
    const msg = JSON.parse(data);
    console.log(msg);
});
```

## 架构

```
Client (ws://) ←→ WebSocket ←→ jsonrpsee Server ←→ Session Manager
                                                          ↓
                                                    Child Process (rpi --mode rpc)
                                                          ↓
                                                    stdout → broadcast → subscribe
```

**关键组件：**
- **jsonrpsee** — JSON-RPC 2.0 实现，提供类型安全和标准化
- **WebSocket** — 长连接传输，低延迟
- **broadcast channel** — tokio 广播通道，支持多订阅者
- **Session** — 管理子进程生命周期和事件流

## 性能

- **WebSocket** 比原始 TCP 多一层帧协议，但开销极小（2-14 bytes/frame）
- **broadcast channel** 支持多个订阅者同时接收事件
- **异步 I/O** — 基于 tokio，高并发性能优秀

## License

MIT
