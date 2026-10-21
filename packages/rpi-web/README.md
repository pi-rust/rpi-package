# rpi-web

rpi-web 是原生 `rpi` 编程智能体的 Web 工作台，提供项目与会话侧栏、流式对话、工具执行与交互、Markdown、文件预览和 Git 差异查看，支持简体中文 / English 和浅色 / 深色主题。

运行关系：`浏览器 → rpi-web → rpi --mode rpc → 模型与项目工具`。每个打开的会话对应一个原生宿主进程。项目文件、模型凭据和会话记录都由运行服务的电脑或服务器保存。

## 安装

本指南使用原生宿主 **rpi 0.7.0**；Web 请安装下面链接中的新发行版。宿主和 Web 需要分别安装；rpi-web 是独立程序，不使用 `rpi install rpi-web` 安装。

1. 从 [宿主 v0.7.0](https://github.com/bigfish1913/pi-rust/releases/tag/v0.7.0) 下载并解压对应平台的 `rpi`。
2. 从 [Web 发行包](https://github.com/pi-rust/rpi-package/releases?q=rpi-web-v) 下载并解压 `rpi-web`。支持 Windows x64、Linux x64 / ARM64、macOS Intel / Apple Silicon。
3. 保留 Web 压缩包中的整个 `assets/` 目录。可以将宿主程序放到同一目录，或用 `--rpi-bin` 指定其绝对路径。发行包中的 `rpi-web-release.json` 提供文件校验信息。

例如，Windows 解压后的目录可以是：

```text
C:/Tools/rpi-web/
├── rpi-web.exe
├── rpi.exe
└── assets/
    ├── index.html
    ├── favicon.svg
    └── assets/
```

Linux / macOS 的程序名为 `rpi-web` 和 `rpi`，没有 `.exe` 后缀。预编译发行包无需 Rust 或 Node.js；使用 Git 功能时，需要让服务进程能在 PATH 中找到 Git。

也可通过 Cargo 安装这两个程序：

```bash
cargo install rpi-cli --version 0.7.0 --locked
cargo install rpi-web --locked
rpi --version
rpi-web --version
```

Cargo 安装后，仍建议从 Web 发行包保存完整 `assets/`，启动时用 `--assets` 指定其绝对路径，避免依赖 Cargo 源码缓存中的前端文件。

## 推荐：把启动配置放进 .rpi

旧版 `0.1.0` 仅支持下文的命令行启动方式；使用配置文件前请更新 Web 程序。

在项目目录内执行一次：

```bash
rpi-web --init
```

会创建 `.rpi/web.json`，已有文件不会被覆盖。默认内容是：

```json
{
  "project": "..",
  "rpiBin": "rpi",
  "port": 3030
}
```

将 `rpi` 加入 PATH，并将完整 `assets/` 放在 Web 程序旁边后，以后在这个项目目录直接运行：

```bash
rpi-web
```

需要自定义时，编辑配置文件即可，例如 Windows：

```json
{
  "project": "..",
  "projects": ["D:/Projects/another-project"],
  "projectsRoot": "D:/Projects/web-projects",
  "rpiBin": "C:/Tools/rpi-web/rpi.exe",
  "assets": "C:/Tools/rpi-web/assets",
  "bind": "127.0.0.1",
  "port": 3030
}
```

所有字段都可省略。JSON 路径相对于**该配置文件所在目录**解析，所以项目 `.rpi/web.json` 中的 `"project": ".."` 指向项目根目录；`rpiBin` 为裸程序名时从 PATH 查找，带目录时按路径解析。使用绝对路径时，Windows 推荐写成 `D:/Projects/...`，避免反斜杠转义。

配置读取顺序：内置默认值 → `~/.rpi/web.json` → 启动项目的 `.rpi/web.json` → 命令行参数。默认启动项目是当前目录，指定 `--project PATH` 时从该目录查找项目配置，不会向父目录递归搜索。全局配置适合固定管理多个项目的服务；项目配置适合在项目目录启动。

同名字段覆盖前一层；项目配置中的 `projects` 数组替换全局列表，命令行 `--add-project` 再追加项目。临时改端口只需 `rpi-web --port 3031`。如果配置了 `allowedHosts`，改端口时也要更新相应的 Host 条目。

指定配置文件时，只加载这个文件，再应用命令行覆盖：

```bash
rpi-web --config /path/to/.rpi/web.json
# 在指定位置生成配置。
rpi-web --config /path/to/.rpi/web.json --init
```

另外可配置 `sessionDir`、`registry` 和 `allowedHosts`。不设置它们时保留原有默认值，历史会话位置不变。JSON 为严格格式，拼错字段或无效内容会在启动时明确报错。

Web 访问密码与模型 API Key 仍由环境变量或宿主凭据配置提供，不写入 Web JSON。`RPI_WEB_ALLOWED_HOSTS` 环境变量优先于 JSON 中的 `allowedHosts`。

## 命令行启动（兼容 0.1.0）

先准备一个已经存在的项目目录，并为服务进程配置模型凭据。下面使用 Anthropic 环境变量；也可使用 `OPENAI_API_KEY`，然后在 Web 中选择对应模型。已有 CLI 配置时，Web 启动的宿主会使用同一运行用户的配置。

**Windows / PowerShell**（程序按上面的目录布局解压）：

```powershell
$env:ANTHROPIC_API_KEY = 'your-api-key'
C:/Tools/rpi-web/rpi-web.exe --project D:/Projects/my-project --rpi-bin C:/Tools/rpi-web/rpi.exe --assets C:/Tools/rpi-web/assets --port 3030
```

**Linux / macOS**（在解压后的 Web 目录运行）：

```bash
export ANTHROPIC_API_KEY='your-api-key'
./rpi-web --project /path/to/my-project --rpi-bin /path/to/rpi --assets "$PWD/assets" --port 3030
```

打开 [http://127.0.0.1:3030/](http://127.0.0.1:3030/)。终端中的服务进程需要保持运行；按 Ctrl+C 停止。默认只监听本机，未设置访问密码时直接进入工作台。

模型 API Key 和 Web 登录密码用途不同：API Key 用于访问模型，`RPI_WEB_PASSWORD` 用于登录工作台。自定义模型、代理网关及凭据配置见 [宿主配置指南](https://github.com/bigfish1913/pi-rust/blob/main/crates/rpi-cli/README.md)。自定义模型通常配置在 `~/.rpi/agent/models.json`。

## 第一次使用

1. 在左侧选择项目，点击该项目旁的 **新建会话**，进入对话。
2. 点击顶部模型名称或打开运行时面板，选择模型、思考等级和可用工具。**服务商授权**可配置 API Key；OAuth 操作只会在宿主支持时显示。
3. 输入需求并按 Enter 发送，Shift+Enter 换行。回复、思考过程和工具结果会流式更新；需要交互的工具会显示相应对话框。
4. 执行中可用停止按钮取消任务；继续发送的普通消息会进入后续队列。可在队列面板查看和取消待处理消息。
5. 点击文件按钮查看项目文件及 Git 变更。文件和差异面板提供只读预览；智能体执行工具时可以按请求修改项目文件。
6. 在设置中切换简体中文 / English、浅色 / 深色主题；登录页也可以切换语言。

### 新建项目与添加已有项目

点击侧栏 **添加项目**：

- **新建项目**：输入名称，可选择初始化 Git。服务会在 `--projects-root` 下创建文件夹。
- **打开已有目录**：填写该根目录内的现有子目录，相对路径按 `--projects-root` 解析，也可填写根目录内的绝对路径。

远程访问时，这里选择的是**服务器目录**。需要添加根目录外的现有项目时，在启动命令中使用可重复的 `--add-project`：

```bash
rpi-web --project /work/project-a --add-project /work/project-b --add-project /work/project-c --projects-root /work/new-projects --rpi-bin /path/to/rpi --assets /path/to/web/assets
```

每个项目有独立的会话分组；新会话使用所选项目作为工具工作目录，分叉与克隆保留来源项目。会话菜单可重命名、置顶、归档和恢复；归档保留历史记录，也不会取消正在执行的任务。侧栏搜索可按项目名或会话标题过滤。

### 常用命令与快捷键

| 操作 | 输入或快捷键 |
| --- | --- |
| 查看命令帮助 | `/help` |
| 选择模型 / 思考等级 / 工具 | `/model`、`/thinking`、`/tools` |
| 新建会话 / 停止执行 | `/new`、`/abort` |
| 压缩上下文 | `/compact` 或 `/compact 压缩时需要保留的重点` |
| 恢复被中断的运行 | `/continue` 或中断提示中的继续按钮 |
| 新建会话 | Ctrl / Cmd + N |
| 搜索会话 | Ctrl / Cmd + K |
| 展开 / 收起侧栏 | Ctrl / Cmd + B |
| 打开设置 | Ctrl / Cmd + , |

操作会根据宿主返回的能力显示；本版本尚未覆盖 Codex 的全部功能。

## 启动参数

可用 `rpi-web --help` 查看参数。

| 参数 | 默认值与用途 |
| --- | --- |
| `--init` | 生成项目 `.rpi/web.json`；与 `--config` 合用时生成到指定位置，不覆盖已有文件 |
| `--config PATH` | 只读取指定 JSON 配置，跳过全局 / 项目自动发现 |
| `--project PATH` | 当前工作目录；启动时必须已经存在，作为默认项目，也用于查找项目配置 |
| `--add-project PATH` | 无；可重复添加其他已存在的项目 |
| `--projects-root PATH` | 注册表同级的 `projects/`；浏览器中新建或打开项目的根目录 |
| `--rpi-bin PATH` | `rpi`；从 PATH 查找宿主，推荐指定绝对路径 |
| `--assets PATH` | 自动查找程序旁或源码中的前端；部署时推荐指定完整 `assets/` 的绝对路径 |
| `--bind IP` | `127.0.0.1`；需要局域网或容器访问时使用 `0.0.0.0` |
| `--port N` | `3030`；命令行覆盖配置文件 |
| `--registry PATH` | `~/.rpi-web/sessions.json`；保存会话索引、标题和组织信息 |
| `--session-dir PATH` | 使用宿主默认会话存储；可显式指定原生会话记录目录 |
| `--version` / `-V` | 打印 Web 版本 |

## 局域网或服务器访问

下面以服务器地址 `192.168.1.10`、端口 `3030` 为例；请替换为实际地址。除了调整监听地址，还需设置允许的 Host 和访问密码。

**Linux / macOS：**

```bash
export RPI_WEB_PASSWORD='replace-with-a-long-password'
export RPI_WEB_ALLOWED_HOSTS='192.168.1.10:3030,127.0.0.1:3030,localhost:3030'
rpi-web --project /work/my-project --rpi-bin /path/to/rpi --assets /path/to/web/assets --bind 0.0.0.0 --port 3030
```

**Windows / PowerShell：**

```powershell
$env:RPI_WEB_PASSWORD = 'replace-with-a-long-password'
$env:RPI_WEB_ALLOWED_HOSTS = '192.168.1.10:3030,127.0.0.1:3030,localhost:3030'
C:/Tools/rpi-web/rpi-web.exe --project D:/Projects/my-project --rpi-bin C:/Tools/rpi-web/rpi.exe --assets C:/Tools/rpi-web/assets --bind 0.0.0.0 --port 3030
```

在其他设备访问 `http://192.168.1.10:3030/`，使用设置的密码登录。`RPI_WEB_ALLOWED_HOSTS` 是逗号分隔的 Host 列表，填写主机名或 IP 与实际端口，不带 `http://`、路径或结尾斜杠；默认只允许 `127.0.0.1:端口` 和 `localhost:端口`。

使用 HTTPS 反向代理时，把实际域名（例如 `rpi-web.example.com`）加入允许列表，代理需保留浏览器的 Host、设置 `X-Forwarded-Proto: https`，并关闭 SSE 响应缓冲。若代理在另一台机器或容器，还需让它能够连接 Web 的监听地址。公网部署应使用 HTTPS 并设置访问密码。当前登录方式是共享访问密码；访问者共用服务端的项目和会话。

环境变量在启动时读取，修改后需重启服务；程序不会自动加载 `.env` 文件，请由 shell 或进程管理器注入。API Key 等宿主环境变量也要传给同一个服务进程。

## 数据保存与升级

- `--registry` 指定的文件保存会话索引、标题、置顶和归档状态；同级 `projects.json` 保存通过浏览器添加的项目。
- 项目源码保存在各项目目录中。原生对话记录由宿主保存；设置 `--session-dir` 时会使用指定目录，注册表不能代替这些记录。
- 模型配置、凭据和扩展由运行服务的用户保存与加载；容器部署时需持久化相应目录。
- 重启服务后，历史会话可能显示为离线，点击会话即可重新打开。刷新浏览器会从宿主恢复对话。
- 升级时等待当前任务结束，备份以上数据，停止 Web，替换程序和配套的完整 `assets/`，保持原来的项目、注册表、会话目录和运行用户，再启动并检查 `--version`。

## 常见问题

| 现象 | 检查方法 |
| --- | --- |
| 提示无法启动 `rpi` | 检查 `rpi --version`，使用 `--rpi-bin` 指定宿主程序的绝对路径 |
| 页面空白或 JS / CSS 404 | 检查 `--assets` 指向的目录是否包含 `index.html` 与内层 `assets/`；整套更新前端后刷新页面 |
| 局域网访问被拒绝 | 检查 `--bind`、端口连通性及 `RPI_WEB_ALLOWED_HOSTS` 是否包含浏览器使用的 Host 与端口 |
| 登录成功但模型调用失败 | Web 密码与模型授权分别配置；检查服务用户的模型凭据，确认所选模型与凭据对应 |
| 模型列表或配置没有更新 | 配置需位于服务器的服务用户目录；修改宿主启动配置后重新打开会话，环境变量变更后重启 Web |
| 重启后找不到历史 | 检查是否使用同一注册表、原生会话目录、项目路径和运行用户 |
| 无法打开已有项目目录 | 浏览器只能添加 `--projects-root` 内的子目录；其他现有目录使用 `--add-project` |

## 开发与 API 参考

以下保留协议、持久化和前端开发说明。普通使用只需完成上面的安装和启动步骤。

### 从源码运行

在 `rpi-package` 仓库根目录执行，先安装或编译好原生宿主：

```bash
cargo run --locked -p rpi-web -- --project /path/to/project --rpi-bin /path/to/rpi --assets packages/rpi-web/assets --port 3030
```

仓库已提交构建后的前端资源；只有修改前端时才需要按下文安装 Node.js 依赖并重新构建。

### API 与能力发现

The server binds to loopback by default and exposes:

- `GET /api/v1/health`
- `GET /api/v1/capabilities`
- `GET/POST/DELETE /api/v1/auth` for the optional password-protected browser
  session; when `RPI_WEB_PASSWORD` is set, all other API and SSE routes require
  the HttpOnly session cookie
- OAuth login is exposed as the capability-gated `oauth` RPC command: send
  `begin`, display the returned authorization URL/instructions, then send
  `exchange` with `{ "input": { "value": "..." } }`; credential material is
  saved natively and never returned to the browser
- `POST /api/v1/sessions` / `GET /api/v1/sessions` (list)
- `GET /api/v1/sessions/:id` / `DELETE /api/v1/sessions/:id`
- `POST /api/v1/sessions/:id/open` (reopen a remembered session)
- `POST /api/v1/sessions/:id/commands`
- `POST /api/v1/sessions/:id/fork` with optional `{ "entryId": "...", "title": "..." }`
- `POST /api/v1/sessions/:id/clone` with optional `{ "title": "..." }`
- `POST /api/v1/sessions/:id/rename` with `{ "title": "..." }`
- `GET /api/v1/sessions/:id/snapshot` (native transcript + caught-up events)
- `GET /api/v1/sessions/:id/events` (SSE, `?after=<seq>` replay)

Open `http://127.0.0.1:3030/` for the workspace UI.

The bridge can be protected for public deployment with `RPI_WEB_PASSWORD`.
Set `RPI_WEB_ALLOWED_HOSTS` to a comma-separated list of accepted Host headers;
when the password is unset, local development remains unauthenticated.

This bridge now exposes operations/history, queue, and stats surfaces, plus
provider authentication controls, fork/clone, and append-only branch navigation.
It deliberately leaves history deletion/editing and full parity unfinished.
`capabilities.parityComplete` stays `false`, and the UI only renders what
`GET /api/v1/capabilities` advertises:

- create a session (one native `rpi --mode rpc` child per session)
- send a prompt and watch `agent_start` → `message_*` → `agent_end` →
  `agent_settled`
- abort the active run
- follow tool execution events
- read a transcript projected into ordered rows: user/assistant text, collapsible
  thinking, tool calls with their input and result inline, and injected notices
- continue a run that recovery left interrupted (`/continue`, or the banner
  action), which spends the tokens that run never did
- compact the conversation (`/compact [instructions]`) and see the compaction
  boundary in the transcript, before and after a reload
- run browser-side slash commands (`/model`, `/thinking`, `/tools`, `/continue`,
  `/abort`, `/new`, `/help`) that call the same RPC commands as the Runtime panel
- watch context usage against the compaction threshold, from the same projection
  the next provider call will send
- pick a model, thinking level, and active tool set via `get_state`,
  `get_available_*` and `set_*`
- switch between live sessions in the sidebar, close them, and restore a
  transcript after a page reload
- inspect durable operation history in an independently scrollable timeline;
  expand records and open-operation payloads with credential fields redacted;
  use sequence pagination, inspect branch/leaf ids and token/cost stats, and keep
  settlement outcomes visible after reload
- view operation counts by type plus the active provider/model in the Stats panel
- queue follow-ups, steer messages, and next-turn messages; inspect pending or
  completed queue entries with lifecycle status, then cancel individual entries
  or clear the pending queue
- fork or clone a session with a custom name; the sidebar persists parent/child
  relation metadata as a compact branch tree, and sessions can be renamed
- configure provider API keys without returning or logging secret material; run
  the capability-gated OAuth begin/exchange flow through the extension provider,
  with credentials saved natively; refresh existing OAuth credentials through
  the native auth extension

### Persistence and reopen

Native hosts are spawned with `--session-id <uuid>`, so transcripts live in the
project's session store and survive a bridge restart. A tiny registry
(`~/.rpi-web/sessions.json` by default) remembers the ids and titles; `GET
/api/v1/sessions` reports remembered-but-not-running sessions as `offline`, and
`POST /api/v1/sessions/:id/open` brings them back (including a session whose
child exited). `--registry PATH` moves the registry file, `--session-dir PATH`
overrides the native session store.

### Snapshot, replay, and reload

History comes from the native host, not from browser state. `GET /snapshot`
issues a read-only `get_messages` command, then returns the transcript together
with any events published while the snapshot was being taken:

```json
{ "messages": [...], "lastSeq": 11, "events": [{ "seq": 12, "event": {...} }] }
```

The client renders `messages`, applies `events` in order, and then opens SSE with
`?after=<lastSeq>`. There is no gap and no overlap, and nothing is lost if events
land mid-snapshot. `leafId`, `pendingResume`, and the paging cursor fields ride
along so the UI can point at the branch tip, flag an interrupted run, and load
older transcript pages when the native host returned a truncated tail.

The native `get_messages` response keeps the newest `limit` entries by default
and returns `firstId`, `lastId`, and `hasMore`. Sending `beforeId: firstId` asks
for the previous page. Each projected message carries its session `entryId`, so
rows remain stable across reloads and pages; the UI uses that cursor for its
"Load earlier messages" action. `total` remains the full branch projection
count, while `truncated` is retained as a compatibility alias for `hasMore`.

Sessions also keep the last 512 projected events in memory with a monotonic
`seq`; SSE frames carry `id: <seq>`. That buffer is what makes `?after=` work, and
it is the fallback path for a host that does not advertise `get_messages`.
Reconnects honor `Last-Event-ID`. If the replay window was evicted, the host
sequence restarted, or a consumer fell behind, `resync_required` tells the browser
to reload the native snapshot before subscribing again. SSE sends keepalives.

The browser does not edit or delete historical records. Navigation only changes
which existing branch entry is the active leaf; the durable operation records and
settlements remain available for inspection.

## Smoke test

```bash
# Uses a separate bridge child, registry, and session directory.
RPI_BIN=/path/to/rpi RPI_WEB_BIN=/path/to/rpi-web \
  packages/rpi-web/scripts/smoke-dev-web.sh

# Optional provider-backed prompt. HTTP 429/quota is reported as SKIP (exit 0).
SMOKE_PROMPT=1 packages/rpi-web/scripts/smoke-dev-web.sh
```

The smoke script never targets or kills the current agent session; cleanup only
terminates the bridge process it started.

## Frontend

The React/Vite sources live in `frontend/`; the built bundle in `assets/` is what
the Rust server serves. `GET /` re-reads `assets/index.html` on every request, so
a frontend rebuild does not require recompiling the server.

```bash
cd packages/rpi-web/frontend
npm ci
npm run build        # typecheck + bundle into ../assets
npm run dev          # Vite on :3181, proxying /api to :3030
npm run e2e:production # builds and runs browser regression against the actual production bundle
```

`assets/` is generated output and is committed so the binary can be served
without a Node toolchain. Rebuild and commit it whenever `frontend/src` changes.

## 集成验证

以下脚本启动独立的 Web、宿主及临时数据目录。配置、项目、授权与事件验证不调用模型；聊天验证使用本地模型模拟服务，不消耗真实服务商额度。Windows 请提供 `.exe` 的绝对路径。

```bash
python packages/rpi-web/scripts/verify-config.py --web /path/to/rpi-web --rpi /path/to/rpi
python packages/rpi-web/scripts/verify-projects.py --web /path/to/rpi-web --rpi /path/to/rpi --project /path/to/project
python packages/rpi-web/scripts/verify-auth.py --web /path/to/rpi-web --rpi /path/to/rpi
# Linux / WSL：合成 RPC 故障测试。
python3 packages/rpi-web/scripts/verify-events.py --web /path/to/rpi-web
# 加 --browser 需先在 frontend 中安装 npm 依赖和 Playwright Chromium。
python packages/rpi-web/scripts/verify-chat.py --web /path/to/rpi-web --rpi /path/to/rpi --browser
```
