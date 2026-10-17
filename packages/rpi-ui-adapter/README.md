# rpi-ui-adapter

安装 GitHub 预编译包：

```bash
rpi install rpi-ui-adapter --github pi-rust/rpi-package
```

使用 `rpi update` 升级、`rpi uninstall rpi-ui-adapter` 卸载。

通过 `rpi --webui=<UI>` 接入现有 Web UI。首个适配器是 Open WebUI 0.12.2。
**每个 UI 使用自己的配置文件和参数结构**，统一放在当前项目的 `.rpi/webui/`：

```text
.rpi/webui/
  openwebui.json          # 手工填写的 Open WebUI 配置
  .state/openwebui.json   # 自动生成的 token / 安装状态，不要手工编辑
```

启动命令只选择适配器：

```bash
rpi --webui=openwebui
```

后续 UI 各自新增 `<ui>.json` 和自己的配置解析器；目前只实现 Open WebUI。
核心、`rpi-acp` 和 Open WebUI 源码无需修改。

## Open WebUI 配置

在启动 rpi 的项目目录创建 `.rpi/webui/openwebui.json`。
如果文件缺失，首次运行会生成模板并退出，填写后再次启动；不会直接连接模板地址。
同机直接运行 Open WebUI 的最小配置：

```json
{
  "url": "http://127.0.0.1:8080",
  "token": "<Open WebUI 管理员 token>"
}
```

也可以不把管理员 token 写入文件，通过 `tokenEnv` 指定环境变量：

```json
{
  "url": "http://127.0.0.1:8080",
  "tokenEnv": "OPENWEBUI_TOKEN"
}
```

未指定 `tokenEnv` 时默认读取 `OPENWEBUI_TOKEN`；`token` 存在时优先使用文件中的值。
管理员 token 用于自动安装/更新托管 Pipe，支持管理员登录 JWT；使用 API key 时，
实例还需允许该 key 调用 Functions 管理 API。同名但非本扩展托管的 Function 不会被覆盖。

首次安装、更新版本或更改连接/项目/权限配置时需要管理员 token。
安装成功后，配置不变且没有提供管理员 token 时，启动直接复用 `.state` 中的状态。
运行状态不会包含从环境变量读取的管理员 token，也不会覆盖或重排手工配置。
新建模板和状态文件在 Unix 上使用 `0600`；包含 token 的项目配置应保持私有，不要提交。

## Open WebUI 在 Docker，rpi 在容器外

配置示例：Open WebUI 映射宿主机 5233 端口，rpi 提供 5234 端口。

```json
{
  "url": "http://127.0.0.1:5233",
  "tokenEnv": "OPENWEBUI_TOKEN",
  "adapter": {
    "bind": "0.0.0.0:5234",
    "url": "http://host.docker.internal:5234"
  },
  "permissions": "bypass"
}
```

然后仍然只运行 `rpi --webui=openwebui`。Linux Docker 需给 Open WebUI 容器添加
`--add-host=host.docker.internal:host-gateway`，或 Compose 的
`extra_hosts: ["host.docker.internal:host-gateway"]`。
也可将 `adapter.bind` 绑定在宿主的 Docker 网桥地址，仅对容器提供访问。

| 字段 | 默认值 / 用途 |
| --- | --- |
| `url` | 必填，rpi 能访问到的 Open WebUI 实例地址 |
| `token` | 可选，管理员 token |
| `tokenEnv` | `OPENWEBUI_TOKEN`，管理员 token 的环境变量名；空字符串关闭环境变量读取 |
| `adapter.bind` | `127.0.0.1:5234`，rpi 监听地址 |
| `adapter.url` | 默认由监听地址生成；Open WebUI 后端能访问到的 rpi 地址 |
| `permissions` | `ask`、`bypass`、`deny`，默认 `ask` |

配置中的 `adapter.bind` 使用 `0.0.0.0` 或 `::` 时，必须明确设置一个可达的 `adapter.url`。
`url` 与 `adapter.url` 表示相反的连接方向；容器里的 `127.0.0.1` 是容器自己。
域名、DNS、HTTPS 和反向代理由部署配置管理，UI Adapter 不负责创建域名。
跨机器连接使用支持 WebSocket Upgrade 的 TLS 反向代理，避免公网明文传递 token。

旧的 `--webui-url`、`--webui-token`、`--webui-bind`、`--webui-adapter-url`、
`--webui-permissions` 已移入配置文件；传入时会提示迁移。
旧的 `RPI_WEBUI_*` 环境变量和全局 `ui-adapter` 配置目录不再作为配置来源。
`RPI_CODING_AGENT_DIR` 仍可控制 rpi 的模型/扩展配置目录，但不改变项目 `.rpi/webui` 的位置。

## 工作方式

```text
浏览器 → Open WebUI + 自动安装的 Pipe
                     ↓ UI 事件 ↔ ACP JSON-RPC
              rpi-ui-adapter 的鉴权 WebSocket
                     ↓ 原样转发 ACP / stdio
                  rpi --acp → rpi --mode rpc → 模型和工具
```

Open WebUI 容器无需 rpi、项目挂载或模型密钥，也不需要 Computer。
WebSocket 是本扩展的传输封装，每帧一个 ACP JSON 对象；不宣称是标准 ACP 远程传输。
工具由 rpi 执行，UI 展示原生状态和只读工具卡片，不会重复执行工具。
当前工作目录决定项目和会话 `cwd`；一个实例服务一个项目，同一个 Open WebUI 当前注册一个
`rpi` 模型（ID `rpi_ui_adapter`）。

`ask` 把实际的 ACP 权限请求映射到确认框，断线、拒绝和超时均取消请求；`bypass` 自动选择
允许选项，`deny` 取消所有权限请求。权限请求由 agent/permission-gate 产生，未产生请求的
工具不会被适配器额外拦截。普通 `ask_user` 问题扩展暂未协商支持。

通用 rpi 参数 `--model`、`--provider`、`--extensions-dir` 仍转交 ACP 子进程。
同一进程不能同时使用 `--webui` 和 `--acp`；适配器自行启动 ACP 进程。

## 会话行为

- 正文、思考流式输出，等待和工具进度使用原生 `status`，结果使用可展开工具卡片。
- 停止发送 `session/cancel`；断开连接时取消未完成请求并通过 stdio EOF 回收 ACP/RPC 子进程。
- 用户、聊天、完整对话前缀匹配时恢复 ACP 会话，历史回放不会混入新回答。
- 编辑、重试、停止或失败后新建会话，将可见分支作为文本上下文带入，不重复执行历史工具。
  该路径不复制旧会话隐藏的工具上下文。
- Open WebUI 索引保存在 `$DATA_DIR/ui-adapter/sessions.sqlite3`，rpi 侧仍使用 ACP 的持久历史。
  两者都应保留；历史丢失时明确报错。
- 一个聊天串行执行；当前只支持文本。图像/音频明确拒绝，自动标题等辅助任务不触发 agent。

## 构建和验证

本扩展尚未发布，先从源码构建；已有配置模型的 rpi，并安装 `rpi-acp`。

```bash
cargo build --release --workspace
cargo run --release -p rpi-package-install -- install
cargo test -p rpi-ui-adapter --locked
cargo clippy -p rpi-ui-adapter --all-targets --locked -- -D warnings
# 使用含 Open WebUI 依赖的 Python 环境，不调用模型
python -m unittest discover -s packages/rpi-ui-adapter/tests -v
```

设置 `RPI_TEST_HOST`、`RPI_TEST_UI_PLUGIN`、`RPI_TEST_ACP_PLUGIN` 可启用隔离的真实宿主测试，
验证配置模板、自动安装、鉴权、ACP initialize、失败重试与无管理员 token 重启。

参考：[Open WebUI Pipe](https://docs.openwebui.com/features/extensibility/plugin/functions/pipe/)、
[Events](https://docs.openwebui.com/features/extensibility/plugin/development/events/)。
