# rpi Agent Packages

每个 `packages/<name>/` 是一个独立发布的 Agent。启动时，当前目录是工作区。宿主从已安装的包读取 extensions、Skills 和常驻指令，会话按包名、版本和工作目录保存。

## `blogs` 示例包

资源来自相邻 `blogs` 项目的 `36ac7055fd7489db2aae17d1e1fc41ddec51fd54`，用同一套博客写作和验收流程。`review` 是为验证指令接口加的入口，调用原有的写作路由和审稿方法。

| 包内路径 | 内容 |
| --- | --- |
| `instructions/SYSTEM.md` | 原项目 `.rpi/SYSTEM.md`，逐字节保留 |
| `skills/` | 11 个默认 Skills，含 `write-router` 的 6 个按需题材模式及配套文件 |
| `optional-skills/` | 13 个按需 Skills 及配套文件 |
| `extensions.lock.json` | MCP Adapter、Voice、WebFetch、WebSearch 的正式发布版本与归档和库文件校验值 |
| `extensions/` | 构建时按目标平台装入上面四个扩展的原生库 |
| `commands/review.md` | 将 `review` 指令接到原有博客审稿流程 |

上游正式预编译库替代 `blogs/.rpi/extensions` 里现存的 Windows DLL。新版 MCP adapter 的工具名是 `mcp_list`、`mcp_call`、`mcp_resource` 和 `mcp_request`，包沿用其余原项目默认工具。创作和验收所需的文章、Zola 站点、项目脚本以及用户的 MCP 配置由工作区和运行环境提供。`skills/.ignore` 保留 `skills/README.md` 的索引用途，使精确发现只注册实际 Skills。

## 构建与本地验证

```sh
python3 scripts/build_package.py blogs --out dist
cargo build --locked --manifest-path ../pi-rust-wt-package-demo/Cargo.toml -p rpi-cli --bin rpi
python3 scripts/smoke_blogs.py \
  --rpi ../pi-rust-wt-package-demo/target/debug/rpi \
  --blogs ../../blogs
```

构建输出 `release.json` 和两份 ZIP，分别供 Apple Silicon macOS 和 Windows x64 使用。脚本逐字节核对两份归档里的 Skills 和系统提示词，校验 4 个平台扩展的 SHA-256。运行测试通过本机 HTTP 安装，使用测试模型驱动宿主真实的 `read` 读取已安装的写作路由、审稿规则和 `blogs` 工作目录中的文章，并验证会话恢复。

GitHub Actions 手动运行生成归档，`blogs-v0.1.0` tag 触发 GitHub Release。发布后可按名称安装，也可用 `--version` 或 `--manifest URL` 指定版本或发布清单。

```sh
rpi install package blogs
cd ~/work/blogs
rpi run package blogs
```

原型宿主通过 JSONL RPC 提供 `list_package_commands` 和 `invoke_package_command`。终端示例在 `blogs` 目录运行。

```sh
python3 /path/to/rpi-package/scripts/package_command.py \
  --rpi /path/to/demo/rpi blogs review \
  --args '{"path":"content/blog/ddd-architecture.md"}'
```
