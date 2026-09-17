# 🚀 rpi 飞书智能消息助手上线 — 让 AI 深度融入你的飞书工作流

> 基于 rpi 的飞书/Lark 长连接消息扩展，实现智能对话、自动应答，让 AI 成为群里的"智能成员"

---

## ✨ 功能亮点

### 🤖 智能对话，自动应答

配置 `autoReply: true` 后，飞书群里的每一条文本消息都会实时传递给 rpi Agent。AI 生成的回复会自动发回同一个会话，无需任何手动操作。

让 AI 成为群里的"智能成员"，随时响应问题、提供帮助。

### 🔌 持久长连接，稳定可靠

基于飞书官方 Rust SDK 的 WebSocket 长连接，配合自动重连 (`autoReconnect`) 机制，确保消息不丢失、连接不断线。即使在网络波动时，系统也能自动恢复。

### 🎛️ 灵活配置，多账号支持

支持多 profile 配置，可轻松管理多个飞书机器人：

```json
{
  "defaultProfile": "feishu-main",
  "profiles": {
    "feishu-main": {
      "provider": "feishu",
      "domain": "feishu",
      "appId": "cli_xxx",
      "appSecretEnv": "RPI_FEISHU_APP_SECRET",
      "transport": "long_connection",
      "allowChats": ["oc_xxx"],
      "mentionRequired": true,
      "autoReply": true,
      "autoReconnect": true,
      "maxQueueSize": 256
    }
  }
}
```

### 🖥️ 无头模式，服务端部署友好

```bash
rpi --im-message-server --im-profile feishu-main
```

无需打开 TUI 界面，后台服务直接运行，适合部署在服务器上，实现 7×24 小时在线的智能助手。

### 🔒 安全可控，细粒度权限

- `allowChats` — 白名单限制机器人响应的群组
- `mentionRequired` — 控制是否只在 @机器人 时回复
- 支持环境变量存储密钥，避免敏感信息泄露到配置文件

---

## 📋 使用场景

| 场景 | 描述 |
|------|------|
| **技术支持群** | 自动回复常见技术问题，减少人工客服压力 |
| **团队知识库** | 员工可直接在群里询问流程、政策，AI 实时解答 |
| **运维告警** | 配合其他系统推送告警，AI 可提供初步诊断建议 |
| **产品 FAQ** | 客户群中自动处理常见咨询，提升响应速度 |
| **内部通知** | AI 自动总结、分类群内重要信息 |

---

## 🚀 快速开始

### 步骤 1：创建飞书应用

1. 登录 [飞书开放平台](https://open.feishu.cn/)
2. 创建企业自建应用，启用**机器人**能力
3. 订阅事件：`im.message.receive_v1`
4. 获取 `App ID` 和 `App Secret`

### 步骤 2：配置 im.json

在 `~/.rpi/agent/im.json` 中配置：

```json
{
  "defaultProfile": "default",
  "profiles": {
    "default": {
      "provider": "feishu",
      "appId": "cli_xxx",
      "appSecretEnv": "RPI_FEISHU_APP_SECRET",
      "transport": "long_connection",
      "autoReply": true,
      "autoReconnect": true
    }
  }
}
```

设置环境变量：

```bash
export RPI_FEISHU_APP_SECRET="your-app-secret"
```

### 步骤 3：启动服务

```bash
# 带界面模式
rpi

# 无头服务模式
rpi --im-message-server
```

### 步骤 4：邀请入群

将机器人添加到目标飞书群，开始对话！

---

## 🏗️ 技术架构

```
┌─────────────┐     WebSocket长连接     ┌─────────────────┐
│  飞书服务器  │ ◄──────────────────────► │ rpi-im-message  │
└─────────────┘                         └────────┬────────┘
                                                 │ ABI bridge
                                                 ▼
                                        ┌─────────────────┐
                                        │    rpi Agent    │
                                        └────────┬────────┘
                                                 │
                                                 ▼
                                        ┌─────────────────┐
                                        │    AI 模型推理   │
                                        └─────────────────┘
```

**技术栈：**

- 基于 **Rust** 构建，高性能低延迟
- 支持 **ABI v2** 运行时桥接
- 无头模式下自动回退到 `rpi --print --no-extensions` 子进程
- 消息队列防止并发冲突，保证对话上下文连续性

---

## ⚙️ 配置参考

| 字段 | 类型 | 说明 |
|------|------|------|
| `provider` | string | 消息平台，目前支持 `feishu` |
| `domain` | string | 域名，`feishu` 或 `lark` |
| `appId` | string | 飞书应用 App ID |
| `appSecret` | string | 应用密钥（不推荐写入配置文件） |
| `appSecretEnv` | string | 存储密钥的环境变量名（推荐） |
| `transport` | string | 连接方式：`long_connection` |
| `allowChats` | array | 允许响应的会话 ID 白名单 |
| `mentionRequired` | bool | 是否仅在 @机器人 时回复 |
| `autoReply` | bool | 是否自动回复消息（默认 false） |
| `autoReconnect` | bool | 断线后是否自动重连（默认 true） |
| `maxQueueSize` | int | 消息队列最大容量（默认 256） |

---

## 🎯 为什么选择 rpi-im-message？

| 特性 | 说明 |
|------|------|
| ✅ **零代码集成** | 只需配置 JSON，无需开发 |
| ✅ **企业级稳定** | 自动重连、消息队列、并发控制 |
| ✅ **AI 驱动** | 利用 rpi 的多模型支持，接入任意 AI 后端 |
| ✅ **开源可控** | 完整源代码，可自行定制扩展 |
| ✅ **灵活部署** | 支持本地调试和服务端无头模式 |

---

## 📦 安装与更新

```bash
# 安装扩展
rpi --install-package rpi-im-message

# 查看状态
rpi --im-message-server --action status
```

---

## 📚 相关资源

- **源码仓库**：`packages/rpi-im-message/`
- **飞书 SDK**：`third_party/feishu-sdk/`
- **详细文档**：`packages/rpi-im-message/README.md`

---

## 📝 更新日志

### v0.1.2
- 持久 WebSocket 长连接
- 自动回复与自动重连
- 无头服务模式
- 多 profile 配置支持

---

**立即体验，让你的飞书群变"聪明"！** 🎉

如有问题或建议，欢迎反馈！