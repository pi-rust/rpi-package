# rpi-providers

一个可安装的原生扩展包，统一维护 DeepSeek、OpenRouter、llama.cpp 和 Proxy。
只有一个 Cargo package、一个动态库和一个插件入口；供应商实现分别放在模块中。
核心继续保留 OpenAI / Anthropic 通用协议，CLI 不依赖供应商包。

| provider ID | 默认模型 | 认证声明 |
| --- | --- | --- |
| `deepseek` | `deepseek-v4-pro`、`deepseek-flash` | 必须认证，`DEEPSEEK_API_KEY` |
| `openrouter` | `openrouter/auto` | 必须认证，`OPENROUTER_API_KEY` |
| `llama-cpp` | `local` | 可匿名，可选 `LLAMA_CPP_API_KEY` |
| `proxy` | `default` | 可匿名，可选 `PROXY_API_KEY` |

包名 `rpi-providers` 与 provider ID 不同。一次安装注册四份目录，认证仍按 provider ID
隔离，不能借用 `OPENAI_API_KEY` 或其他供应商的 key。已有 models.json、auth.json 的
provider ID 不变，无需合并凭据。匿名目录就绪不代表本地服务已经启动，使用时指定 provider。

## 安装、升级与迁移

当前使用 crates.io 发布的 `rpi-plugin-sdk` 0.6.3，并显式启用可选的 `provider`
feature；`rpi-ai` 与集成测试使用的 `rpi-extensions` 同样固定为 0.6.3。
使用 rpi 0.6.3 宿主，无需从指定 Git 提交构建。本包目前仍设置 `publish = false`，
通过 GitHub Release 分发预编译包；其他扩展继续使用工作区固定的 SDK 0.3.20。

```toml
rpi-plugin-sdk = { version = "=0.6.3", features = ["provider"] }
rpi-ai = { version = "=0.6.3", features = ["providers"] }
```

`provider` 默认关闭；普通工具扩展无需启用，也不会因此引入 AI / HTTP 依赖。

安装预编译包（需要上述兼容宿主）：

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
本包不注册 OAuth 登录流程；宿主已有的有效 OAuth access token 可作为 Bearer 凭据使用。

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
8,192 token；后者是保守默认值，可通过 models.json 或请求 options 覆盖。
目录价格采用 2026-10-16 的峰时美元 / 百万 token 估值，非峰时账单可能较低，可覆盖 cost。

模型名、限制与价格依据 [首次调用](https://api-docs.deepseek.com/)、
[模型说明](https://api-docs.deepseek.com/quick_start/pricing/)。供应商调整时应同步更新本包目录。

`--thinking off` 显式发送 `thinking.type=disabled`；minimal / low 映射 low，
medium / high / xhigh 映射 high，max 映射 max。遵循宿主 options 约定：`None` 与 `Off`
均关闭思考；CLI 的默认 medium 映射 high，不依赖供应商隐含默认值。
工具请求保留所有历史 assistant 思考块，包括上一轮没有调用工具的回答，避免续轮 400。
规则依据 [思考模式与工具调用](https://api-docs.deepseek.com/guides/thinking_mode/)。

用户端点和请求头由宿主私有执行绑定提供，本包不读取进程 key 或改回固定运行时端点。
涉及包的 `/reload` 会重新绑定；移除当前包时保留旧会话并报错，先切换模型再卸载。

## 离线验证

在仓库根目录执行；必须先 build 真实动态库，缺少产物会失败，不静默跳过：

```sh
cargo build -p rpi-providers
cargo test -p rpi-providers
```

测试实际加载 DLL / so / dylib，覆盖：首个正文增量到达后服务端才继续发送的 SSE 门控、
中文 UTF-8 跨块、thinking 与 usage、响应未结束时取消、HTTP 401、唯一终态、思考开关，
以及工具调用 → 工具结果 → 回答 → 新用户轮次的完整历史回传。

宿主命令与凭据检查另外需要安装或构建 rpi 0.6.3 CLI，再设置路径显式执行：

```powershell
$env:RPI_TEST_CLI = 'C:\path\to\pi-rust\target\debug\rpi.exe'
cargo test -p rpi-providers --test host -- --ignored --nocapture
```

该测试使用隔离的临时配置与本地 SSE 服务，验证目录、禁用扩展、默认模型、认证来源、
OpenAI key 隔离、端点 / 请求头覆盖和实际 CLI 推理，不访问 DeepSeek 服务。

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
