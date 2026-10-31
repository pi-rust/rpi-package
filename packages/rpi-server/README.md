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

### RPC 消息更新模式

新版 native `rpi --mode rpc` 默认使用 `compact-deltas`：文本、思考和工具参数
的高频 delta 不再携带两份累计消息快照；结构事件和最终 `message_end` 仍然完整。
原始 TCP 事件按子进程输出透传，不由 `rpi-server` 重新补入快照。

`rpc_server` 的启动参数可以显式选择模式：

```json
{"action":"start","rpcMessageUpdates":"compact-deltas"}
```

迁移期需兼容尚未升级的消费者时，显式使用 `"rpcMessageUpdates":"snapshot"`。
仅接受这两个值，其他值（包括 `null`）报错。省略参数时不添加新 CLI flag，使用
所选 native executable 自身的默认值，因此仍可启动不认识此参数的旧 CLI。
这是服务器启动配置，不是 `start_session` 的逐会话选项。

客户端应检查 `ready.semantics.messageUpdates`；字段缺失表示旧 snapshot 模式。
推荐先升级支持两种模式的客户端及 Web/ACP 适配器，再升级 native CLI。
若升级 native CLI 而不升级旧远程 TUI，其依赖累计 `message` 的流式显示可能停止。
旧模式仅作为临时回退，后续计划移除；新集成应直接消费 delta，并在完整边界消息
到达时校准状态。compact 不代表整个事件流完全无快照、严格线性或可无损重放。

### 工具契约、默认值与安全边界

工具名仍为 `rpc_server` / `rpc_client`，使用 TCP JSONL，**不是 HTTP 服务**。
实际注册的 description/schema 与下列说明保持一致；不新增 prompt API 或 hook。

- `rpc_server.action` 可省略，默认 `start`。`stop` / `status` 必须给非空 `id`；
  `list` 不需要参数。只有 `start` 读取监听、令牌与子进程启动字段。
  未把所有字段设为 required，也未关闭顶层额外属性，兼容宿主注入的可选 `__rpi`。
- `start` 默认监听 `127.0.0.1:9800`，创建注册项和后台运行时，不立即启动子进程。
  `port` 范围为 0..65535；现有实现虽然允许 0，但返回/存储地址仍为 `:0`，
  不能据此发现实际端口供 `rpc_client` 使用。建议选择已知可用端口。
- 默认生成令牌；`token` 非空时使用指定值，空串/省略则生成。
  实际支持的 `noToken:true` 会覆盖 `token` 并完全禁用认证，默认 false。
  **start/status/list/server_status 返回值和服务日志包含明文令牌**，勿复制到公开回复、
  工单、仓库或不可信日志中；事件/响应也可能包含敏感内容。生成器输出 32 位十六进制，
  此处不承诺其密码学强度。TCP 无 TLS；令牌认证不提供加密。
  `0.0.0.0` 会暴露所有 IPv4 接口；只使用回环或受保护的可信网络，禁认证时尤其如此。
- `start_session` 才启动当前可执行文件，固定前缀 `--mode rpc`，使用监听启动时保存的参数。
  不经 shell 拼接，但工具、扩展、资源与子进程仍有宿主权限；不是沙箱。
  未传的选项由子进程 CLI/配置决定，并不保证继承当前父会话选择。
  `baseUrl` 是模型提供商端点，不是监听地址；只配置可信端点。
- 字符串启动选项非空白时才转发；省略/null 不发对应 flag。
  `provider/model/baseUrl/systemPrompt/thinking/name/session/sessionId/sessionDir`
  对应 `--provider/--model/--base-url/--system-prompt/--thinking/--name/--session/--session-id/--session-dir`。
  参数值按原样交给原生 CLI，可能出现在进程列表中，不要放密钥。
- 数组省略视为 `[]`，null 非法，最多 128 个非空字符串。
  `tools` / `excludeTools` 非空时以逗号合并为 `--tools` / `--exclude-tools`；
  `appendSystemPrompt/extensionsDir/extension/skill/promptTemplate` 各元素重复传
  `--append-system-prompt/--extensions-dir/--extension/--skill/--prompt-template`。
  本包不校验资源内容或处理发现/优先级冲突，只加载可信资源。
- 开关 `noSession/noTools/noBuiltinTools/noSkills/noPromptTemplates/noContextFiles/noExtensions/enablePiPackages/offline/headless`
  省略/null/false 不发 flag，true 传对应 kebab-case flag；默认不启用这些开关，
  不代表对原生 CLI 默认加载行为作额外保证。`offline` 不是防火墙，也不关闭 TCP。
  `approve` 是例外：省略不发任何 flag（沿用子 CLI/配置默认），true 传 `--approve`，
  false 传 `--no-approve`，null 非法。自动批准可能放行文件修改/命令执行，请明确评估。
- `session` 与 `sessionId` 只要键存在就互斥；`noSession:true` 与任一键存在冲突，
  **即使其值为 null**。这些原生持久会话选项不同于服务生成的实时 `session-N` 句柄。
- `rpc_client` 必填非空 `serverId` 和 `method`；`serverId` 必须来自当前扩展的运行中注册项，
  不是远程 URL/地址。每次调用新建连接并自动使用保存的令牌认证，不要手动把 token 填入 params。
  `params` 默认 `{}`，各方法条件见下表；任意未知方法会返回 JSON-RPC 错误。
- `subscribe:false` 默认返回 `{serverId,method,result:<完整 JSON-RPC 响应>}`；
  **工具成功不代表远端成功**，须检查 `result.error`。`send` 的 sent 仅表示写入 stdin，非任务完成。
  `subscribe:true` 只是本地接收模式，应配合 `method:"subscribe"` 和 `params.sessionId`；
  返回 `{serverId,method,events,eventCount}`，包含订阅确认和原始通知，不是增量工具输出，
  没有历史回放。广播缓冲为 256 条，落后可能收到 `lagged`。
- `timeoutSeconds` 省略/null 为 30，范围 1..300。外层等待包含连接/重试/认证，
  内层发送后另计接收期限，认证最多等 5 秒；因此不能保证一定拿到部分收集结果。
  事件收集仅限时、无条数/字节上限。客户端仅识别顶层 `session_end`，
  当前服务的结束事件封装于 `params.event`，不保证因此提前退出或结果完整。
- 客户端超时/取消不保证后台 worker 立即退出，也不取消/回滚远端命令或结束子进程。
  不要盲目重试 `start_session` / `send` 等有副作用的方法。
  创建者负责 `stop_session` 或 `rpc_server stop`；宿主 SessionShutdown 也会尝试停止所有注册服务。
  `stop` 先移除注册项，再通知 accept 循环退出、kill/wait 已跟踪子进程；
  不保证已有连接、监听资源或后台运行时线程全部释放。
  启动/停止等待上限为 10/5 秒；超时或取消不回滚已产生的副作用，需检查实际资源状态。

### 客户端连接

#### 方式一：使用 nc 测试

```bash
# 连接服务器
nc 127.0.0.1 9800

# 默认开启认证：先使用 start 返回的 token（此处仅占位，不要公开真实令牌）
{"jsonrpc":"2.0","id":0,"method":"authenticate","params":{"token":"<private-token>"}}

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

| 方法 | 参数 / 说明 |
|------|------|
| `start_session` | 忽略 params；使用服务保存的启动参数启动新子进程，返回实时 `sessionId` |
| `send` | 必须 `sessionId`、`command`；将原生 JSON 命令写到该子进程 stdin；sent 不表示执行完成 |
| `stop_session` | 必须 `sessionId`；从会话表移除并 kill/wait 子进程 |
| `list_sessions` | 忽略 params；列出会话快照，不是历史记录 |
| `server_status` | 忽略 params；服务状态，包含明文 token，必须保密 |
| `subscribe` | 必须有效 `sessionId`；订阅当前连接上的后续事件，工具收集还需 `subscribe:true` |
| `ping` | 忽略 params；返回 pong，不保证模型或子进程健康 |

## Schema 回归检查

`src/lib.rs` 的 `tool_schemas()` 同时供插件实际注册和 Rust 单元测试使用，不维护测试专用 schema。
测试覆盖实际 description/JSON、每个字段说明、默认值、条件必填、可选 `__rpi` 顶层兼容、
令牌/approve/会话冲突、方法参数与结果/超时边界。无网络监听或子进程启动副作用。

运行实际注册 schema 的 Rust 回归测试（无需网络监听）：

```bash
cargo test --locked -p rpi-server registered_schemas
```

如需只检查 Rust 测试编译而不执行，可用 `cargo check -p rpi-server --tests`（需要工作区依赖可用）。

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
