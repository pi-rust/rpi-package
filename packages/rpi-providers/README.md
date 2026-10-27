# rpi-providers

一个可安装的原生扩展包，统一维护 DeepSeek、OpenRouter、llama.cpp、Proxy 和 Antigravity。
只有一个 Cargo package、一个动态库和一个插件入口；供应商实现分别放在模块中。
核心继续保留 OpenAI / Anthropic 通用协议，CLI 不依赖供应商包。

| provider ID | 默认模型 | 认证声明 |
| --- | --- | --- |
| `deepseek` | `deepseek-v4-pro`、`deepseek-flash` | 必须认证，`DEEPSEEK_API_KEY` |
| `openrouter` | `openrouter/auto` | 必须认证，`OPENROUTER_API_KEY` |
| `llama-cpp` | `local` | 可匿名，可选 `LLAMA_CPP_API_KEY` |
| `proxy` | `default` | 可匿名，可选 `PROXY_API_KEY` |
| `antigravity` | `antigravity-gemini-3.8-flash` 等 6 个目录模型 | 必须认证，仅 OAuth 登录，无 API key 环境变量 |

包名 `rpi-providers` 与 provider ID 不同。一次安装注册五份目录，认证仍按 provider ID
隔离，不能借用 `OPENAI_API_KEY` 或其他供应商的 key。已有 models.json、auth.json 的
provider ID 不变，无需合并凭据。匿名目录就绪不代表本地服务已经启动，使用时指定 provider。

## 安装、升级与迁移

本包将 `rpi-ai`、`rpi-plugin-sdk` 和测试使用的 `rpi-extensions` 精确固定到
已发布的 **0.8.3**，避免 SDK 源码接口变化影响未迁移的扩展。其他扩展继续使用
工作区固定的 SDK 0.3.20，不需要随本包一起升级。

宿主 0.8.3 支持插件生命周期时，每个 provider 注册独立复用一个惰性 runtime 和
最多 16 个 HTTP 客户端；不同请求的 key、请求头和 provider 实例仍然独立。
重载先取消请求并拒绝新请求，最后一个调用和字符串释放完成后再清理线程、卸载 DLL。
宿主 0.8.2 缺少这项可选能力时自动退回每请求资源，原有认证和推理功能继续可用。

从 0.1.2 开始，本包发布到 crates.io，并通过 GitHub Release 分发预编译包。
仅更新宿主不会替换旧动态库内部的协议实现，升级时还需要 `rpi update` 更新本扩展。

`provider` 默认关闭；普通工具扩展无需启用，也不会因此引入 AI / HTTP 依赖。

安装（需要上述兼容宿主）：

```sh
rpi install rpi-providers
```

也可以显式指定 GitHub 预编译包：

```sh
rpi install rpi-providers --github pi-rust/rpi-package
```

预编译安装使用 `rpi update` 升级、`rpi uninstall rpi-providers` 卸载。
如果此前通过源码 link 安装，请先 `rpi package unlink providers`，避免重复加载。

开发时也可以在 rpi-package 仓库根目录源码安装：

```sh
rpi package link providers --path packages/rpi-providers --debug
rpi --list-models
rpi auth login --provider deepseek
rpi auth check --provider deepseek --json
rpi --provider deepseek --model deepseek-flash
# 已设置 OPENROUTER_API_KEY 时
rpi --provider openrouter --model openrouter/auto
# 本地服务已运行时
rpi --provider llama-cpp --model local --base-url http://localhost:8080/v1
rpi --provider proxy --model default --base-url http://localhost:8080/v1
```

也可以直接设置相应环境变量。`auth check` 只检查本地就绪状态，不发网络请求。
默认凭据优先级仍由宿主管理：显式 `--api-key` → auth.json → models.json → 专属 env。
仅 `antigravity` 注册 OAuth 登录流程（见下文）；其他供应商不注册 OAuth，宿主已有的
有效 OAuth access token 仍可作为 Bearer 凭据使用。

## Antigravity 行为与登录

目录与路由改编自 [cortexkit/antigravity-auth](https://github.com/cortexkit/antigravity-auth)
（MIT，Copyright (c) 2025 Jens），许可文本见 `src/antigravity/LICENSE`。Pi 参考实现的请求
端点为 `https://daily-cloudcode-pa.googleapis.com`，模型上的 `baseUrl` 仍登记为
`https://cloudcode-pa.googleapis.com`；本包发送到 Pi 的实际端点。模型 `baseUrl` 若被覆盖，
请求与项目发现都改用该地址（测试与自建路由使用）。

登录使用 PKCE 授权 URL。宿主打印 URL 后，在浏览器完成授权，把回调地址或 code 粘贴回终端。
授权 URL 只带随机 `state` 与 S256 挑战，PKCE verifier 仅保存在进程内。粘贴的回调地址必须携带
本次登录的 `state`，否则直接拒绝且不发起网络请求；裸 code 使用最近一次发起的登录。每次登录
10 分钟内有效，最多保留 8 次未完成的登录，使用后（无论换取成功与否）立即失效：

```sh
rpi auth login --provider antigravity
rpi auth refresh --provider antigravity
rpi auth logout --provider antigravity
```

OAuth 客户端 ID 与密钥不随源码提供。构建时设置 `RPI_ANTIGRAVITY_OAUTH_CLIENT_ID` 与
`RPI_ANTIGRAVITY_OAUTH_CLIENT_SECRET`；或在运行时环境中设置同名变量（构建期值优先）。
两者缺一或为空时，`login`、`refresh`、`logout` 返回“未配置”错误，其他供应商照常加载。
发布流程从仓库 secrets 读取同名变量，缺失时只输出 `::warning::`，不会失败。

Antigravity 不支持 Gemini API key：Pi 的同一供应商同样只接受 OAuth access token（`stream.ts`
中 `Missing Antigravity OAuth access token`）。请求中的 `apiKey` 即 access token，本包不读取
任何 API key 环境变量。过期凭据不会在请求前自动刷新，需要先执行 `rpi auth refresh`。

本地测试通过 loopback 服务覆盖目录与路由、PKCE、令牌交换与刷新（端点注入只在单元测试内部）、
Gemini SSE 增量、取消、HTTP 错误、生命周期与凭据隔离；运行方式与其他供应商相同：

```sh
cargo test -p rpi-providers --test antigravity
cargo test -p rpi-providers --lib antigravity
```

如果之前分别 link 了旧 DeepSeek / OpenRouter / llama.cpp / Proxy 包，先用
`rpi package unlink <原安装名>` 移除旧安装记录，再 link `providers`。不要同时加载旧包与
新包中的同名 provider：宿主按加载顺序保留第一份实现。unlink 不会删除 models.json 或 auth.json。

以后修改任意供应商，只维护并重新构建 / link `rpi-providers`。卸载统一使用
`rpi package unlink providers`，会一起移除四个 provider；`/provider remove <id>` 只删除对应配置。
已链接包更新后重启或 `/reload` 生效；移除当前使用的包时，重载报错并保留旧会话。

## 开发结构

```text
src/lib.rs             # 唯一 export_plugin! 入口，登记所有模块
src/registration.rs    # 各模块共用 ABI 适配，复用 rpi_plugin_sdk::provider
src/deepseek.rs        # DeepSeek 目录、思考和工具续轮
src/openrouter.rs      # OpenRouter 目录及 HTTP-Referer / X-Title
src/llama_cpp/         # 本地推理、模型查询、服务管理
src/proxy/             # 通用代理推理
src/antigravity/       # Antigravity 目录、Gemini 转换、SSE、项目发现与 OAuth
src/bin/rpi-llama.rs   # 原有本地服务管理程序
```

新增 provider 时添加模块，在 `src/lib.rs` 的登记列表加入它，在本 README 与测试中声明
对应模型 / 认证策略。不新增供应商 crate，不重复导出插件入口，不修改 CLI 的供应商表。
包内模块使用 `register_provider!` 生成请求适配，只有根模块调用一次 `export_plugin!`。
通用协议与 ABI 桥接留在核心库中，供应商策略留在本包。
桥接使用 `rpi-plugin-sdk` 的可选 `provider` feature，不再依赖独立的
`rpi-provider-runtime` crate；多供应商共用 `provider::register` 和 `provider::drive`，
OpenAI-compatible 协议复用 `provider::completions`。

llama.cpp 管理命令保持原有行为，构建入口改为：

```sh
cargo run -p rpi-providers --bin rpi-llama -- status
```

## DeepSeek 行为

默认目录按 `deepseek-v4-pro`、`deepseek-flash` 排序，保持原先默认 Pro 的行为。
端点为 `https://api.deepseek.com/v1`，目录上下文为 1,000,000 token，每次请求输出预设
16,384 token；后者是保守默认值，可通过 models.json 或请求 options 覆盖。
目录价格采用 2026-10-16 的峰时美元 / 百万 token 估值，非峰时账单可能较低，可覆盖 cost。

模型名、限制与价格依据 [首次调用](https://api-docs.deepseek.com/)、
[模型说明](https://api-docs.deepseek.com/quick_start/pricing/)。供应商调整时应同步更新本包目录。

`--thinking off` 显式发送 `thinking.type=disabled`；minimal / low 映射 low，
medium / high / xhigh 映射 high，max 映射 max。遵循宿主 options 约定：`None` 与 `Off`
均关闭思考；CLI 的默认 medium 映射 high，不依赖供应商隐含默认值。
自定义模型未声明 `reasoning` 或显式设为 `false` 时，也会发送关闭思考的字段，
避免服务端默认开启思考。
部分 `compat` 对象会补全 DeepSeek 默认值，并保留用户显式指定的字段和思考等级映射。
`supportsReasoningEffort: false` 会禁止发送 `reasoning_effort`；
`thinkingFormat` 控制思考开关字段。历史思考回传始终保留。
工具请求保留所有历史 assistant 思考块，包括上一轮没有调用工具的回答，避免续轮 400。
规则依据 [思考模式与工具调用](https://api-docs.deepseek.com/guides/thinking_mode/)。

用户端点和请求头由宿主私有执行绑定提供，本包不读取进程 key 或改回固定运行时端点。
请求头名称按不区分大小写的方式合并，覆盖或删除时不会保留旧名称的另一种大小写。
Rust Proxy 调用中，请求级 headers 优先于 ProxyConfig，后者优先于模型默认 headers；
显式 API key 的认证优先级保持不变。普通 metadata 的键仍区分大小写。
涉及包的 `/reload` 会重新绑定；移除当前包时保留旧会话并报错，先切换模型再卸载。

## 流超时与兼容配置

四个 Provider 共用 SDK 的 SSE 实现。总请求超时与流空闲超时独立生效：

- 宿主默认总请求超时为 600 秒，流空闲超时为 300 秒。
- `rpi --stream-idle-timeout 60` 将空闲超时设为 60 秒；`0` 禁用空闲检测。
- `~/.rpi/agent/settings.json` 中可持久化 `"streamIdleTimeout": 60`，命令行优先。
- 心跳和未完整解析的帧，只要收到非空字节就刷新空闲计时；带事件名但无数据的
  心跳和空数据帧会被跳过。卡住后产生可重试错误。
- Rust 调用方使用 `SimpleStreamOptions.stream_idle_timeout`；Proxy 的配置超时只在
  请求未指定 `timeout` 时作为回退。llama.cpp 管理操作的超时与推理超时分别设置。

`models.json` 支持 `thinkingFormat: "deepseek"` 和
`requiresReasoningContentOnAssistantMessages: true`；原生
`requiresReasoningContent` 与别名并存时，以原生字段为准。未知字段和非法类型由宿主
在补齐扩展 API 默认值后报告，避免配置被静默忽略。

## 离线验证

在仓库根目录执行；必须先 build 真实动态库，缺少产物会失败，不静默跳过：

```sh
cargo build -p rpi-providers
cargo test -p rpi-providers
```

测试实际加载 DLL / so / dylib，覆盖：首个正文增量到达后服务端才继续发送的 SSE 门控、
中文 UTF-8 跨块、thinking 与 usage、响应未结束时取消、HTTP 401、唯一终态、思考开关，
以及工具调用 → 工具结果 → 回答 → 新用户轮次的完整历史回传。

宿主联调使用已发布的 0.8.3 CLI（也可检出 `v0.8.3` 构建），再设置路径显式执行：

```powershell
$env:RPI_TEST_CLI = 'C:\path\to\pi-rust\target\debug\rpi.exe'
cargo test -p rpi-providers --test host --locked -- --ignored --nocapture
cargo test -p rpi-providers --test reliability --locked -- --include-ignored
```

该测试使用隔离的临时配置与本地 SSE 服务，验证目录、禁用扩展、默认模型、认证来源、
OpenAI key 隔离、端点 / 请求头覆盖和实际 CLI 推理，不访问 DeepSeek 服务。
`reliability` 另覆盖四个 Provider 的流空闲超时、心跳与分帧、取消、禁用、独立总超时、
兼容字段和思考历史，以及实际宿主通过扩展重试卡住的 SSE 请求。
新增测试通过真实动态库验证四个 Provider 的连接复用、跨请求凭据隔离、生命周期取消、
关闭后拒绝请求和客户端配置错误；旧宿主回退另使用正式发布的 0.8.2 CLI 验证。
PR 工作流在 Linux、Windows、macOS 上执行这些离线检查，并验证宿主重载时将
配置警告返回界面，不直接写入正在使用的终端。

## 可选真实 API 验证

先在当前进程配置自己的 `DEEPSEEK_API_KEY`，再运行：

```sh
cargo test -p rpi-providers --test streaming live_deepseek_extension_stream -- --ignored --exact --nocapture
```

此测试消耗额度，默认 ignored，最多请求 1,024 输出 token、不重试、超时 45 秒，
要求多个正文增量。不要将 key 写入源码、配置示例、日志或快照。

Rust 调用方从原核心入口迁到 `rpi_providers::deepseek::DeepSeekProvider::new(api_key)`；
该构造函数不读取环境变量。接入标准见
[Provider 接入规范](https://github.com/bigfish1913/pi-rust/blob/feature-stream-porider/docs/extensions/provider-integration.md)。
