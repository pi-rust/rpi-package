# 不用把代码说给云端听：rpi-voice 让本地语音 Coding 真正跑起来

> 对着麦克风说需求、报错和重构想法，rpi 直接在本机接着写代码。
> 语音转写支持完全离线，插件直接加载进 rpi TUI，不需要另起一个语音助手。

## 一句话介绍

`rpi-voice` 是 rpi Rust coding agent 的语音扩展。它把麦克风、语音转写和回复朗读接进正在运行的 rpi 终端：你可以说“定位这个编译错误”“把这个函数改成异步”，转写文本会进入正常的 rpi 对话流程，代码上下文、工具调用和历史记录都不用切换。

它的重点不是把聊天窗口换成一个麦克风按钮，而是让语音成为 coding workflow 的一部分：

- **本地语音输入**：编译 `local-stt` 版本后，SenseVoice 通过 sherpa-onnx 在本机完成中英日韩粤转写，音频不上传。
- **可编辑草稿**：识别结果先放入输入框，默认等待 2 秒自动发送；发现识别错了，直接打字修改即可。
- **按住说话**：`/voice ptt` 把空格变成 push-to-talk，松开后提交，适合连续描述问题。
- **免手连续对话**：`/voice auto` 让 rpi 说完回复后自动重新监听，适合边看代码边追问。
- **回复朗读**：Edge TTS 不需要 API Key，支持切换中英文音色；开始打字或按下 PTT 时会立即停止朗读。

## 本地语音 Coding 是什么体验

打开 rpi 后输入：

```text
/voice ptt
```

按住空格说：“检查这个模块为什么在 Windows 构建失败，先不要改代码，告诉我最可能的原因。”松开后，rpi-voice 会录音、转写，并把结果交给 rpi 的正常输入链路。你可以在发送前改词，也可以用：

```text
/voice output send
```

让识别结果直接发送给 agent。

长任务中可以使用：

```text
/voice auto
```

rpi 朗读完回复后才会重新打开麦克风，避免把自己的声音录回去。你开始打字，连续模式会暂停；`/voice auto off` 可以随时退出。

## 为什么适合敏感代码

完全离线模式下，录音和转写都发生在本机。SenseVoice 模型约 240 MB，放在：

```text
~/.rpi/agent/models/sense-voice/
```

首次使用可以自动下载，也可以提前执行 `/voice model download`。离线模式不需要 API Key，也不需要运行服务器。

需要说明的是：默认的回复朗读使用 Microsoft Edge TTS，它不需要 API Key，但需要访问网络。若要求整条语音链路都不出机器，请关闭自动朗读；语音转写本身仍可保持本地运行。

## 部署方式

### 方案一：最快体验，API 转写

适合先试用功能，不想安装本地语音编译依赖的用户。先安装 rpi，再安装扩展：

```bash
# 安装 rpi
curl -fsSL https://raw.githubusercontent.com/bigfish1913/pi-rust/main/scripts/install.sh | sh

# 安装 rpi-voice（API 版本）
rpi install rpi-voice
```

Windows 也可以用 Scoop 安装 rpi：

```powershell
scoop bucket add bigfish1913 https://github.com/bigfish1913/scoop-bucket
scoop install rpi
rpi install rpi-voice
```

如果本机已有 Rust，也可以执行 `cargo install rpi-cli`，再运行同样的
`rpi install rpi-voice`。

然后配置 OpenAI 兼容的语音接口：

```bash
export OPENAI_API_KEY=sk-xxx
export RPI_STT_ENGINE=api
export RPI_STT_MODEL=whisper-1
```

Windows PowerShell：

```powershell
$env:OPENAI_API_KEY = "sk-xxx"
$env:RPI_STT_ENGINE = "api"
$env:RPI_STT_MODEL = "whisper-1"
```

启动 `rpi`，输入 `/voice status` 检查状态，再用 `/voice` 开始说话。

### 方案二：推荐，编译完全本地转写版本

适合不希望把麦克风音频发往第三方服务的 coding 场景。需要 Rust、CMake 和 libclang；这些只在编译时使用，运行时不需要额外 DLL。

```bash
git clone https://github.com/pi-rust/rpi-package.git
cd rpi-package

# Windows PowerShell
pwsh packages/rpi-voice/build-offline.ps1

# Linux / macOS
export LIBCLANG_PATH="$(python -c 'import clang,os;print(os.path.dirname(clang.__file__))')/native"
export RUSTFLAGS="-C relocation-model=dynamic-no-pic"
cargo build -p rpi-voice --release --features local-stt
```

Windows 如果还没有 libclang，先执行：

```powershell
pip install libclang
pwsh packages/rpi-voice/build-offline.ps1
```

把编译出的扩展安装到 rpi：

```bash
cargo run --release -p rpi-package-install -- install --all
```

启动后预取模型：

```text
/voice model download
/voice model
/voice ptt
```

`/voice model` 会显示当前 STT 引擎和模型状态。模型下载完成后，即使没有 STT API Key，也可以继续进行本地语音 coding。

### 方案三：不编译插件，用本地 Whisper 服务

如果希望转写留在本机，但不想处理 sherpa-onnx 的编译依赖，可以运行本地 OpenAI 兼容服务：

```bash
docker run -p 8000:8000 ghcr.io/speaches-ai/speaches:latest-cpu

export RPI_STT_ENGINE=api
export RPI_STT_API_BASE=http://localhost:8000/v1
export RPI_STT_MODEL=Systran/faster-whisper-small
```

这种方式的音频只发给本机的 Whisper 服务，不需要云端 Key。Windows 也可以使用同一条 Docker 命令，再在 PowerShell 中设置对应环境变量。

## 常用命令

| 命令 | 用途 |
| --- | --- |
| `/voice` | 录音、转写，放入可编辑输入框 |
| `/voice ptt` | 开启按住空格说话 |
| `/voice auto` | 回复后自动重新监听 |
| `/voice output draft` | 识别结果先进入草稿，默认行为 |
| `/voice output send` | 松开后直接发送给 agent |
| `/voice status` | 查看麦克风、引擎、音色和播放状态 |
| `/voice model download` | 提前下载本地 SenseVoice 模型 |
| `/voice stop` | 停止当前朗读 |
| `/voice set zh-CN-YunxiNeural` | 切换中文男声音色 |

## 适合哪些场景

- 手上不方便打字，想快速描述一个 bug 或重构目标。
- 阅读大型 diff 时，用语音连续追问实现细节。
- 写代码时口述测试思路、提交说明和待办事项。
- 处理不适合上传到第三方转写服务的仓库。
- 在 Windows、Linux 或 macOS 上使用同一套 rpi 终端工作流。

## 现在就试试

```text
rpi
/voice model download
/voice ptt
```

一句话总结：**把想法说出来，把上下文留在 rpi，把转写留在本机。**

项目地址：<https://github.com/pi-rust/rpi-package>

插件源码和完整环境变量：[`packages/rpi-voice/`](../packages/rpi-voice/)

## 可选标题

- **你的代码，不必先经过云端：rpi-voice 开启本地语音 Coding**
- **从“打字写需求”到“开口改代码”：rpi-voice + rpi 实战**
- **开口就能让 Agent 改代码：一套支持离线转写的 rpi 语音插件**
