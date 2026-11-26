---
name: image
description: 为博客处理封面、插画与真实截图：确定信息用途，裁剪脱敏，压缩并检查正文和分享预览效果。由项目系统提示词按需选择；实测图表使用 visualize。
---

# 博客图片

涉及文章时先读取 `skills/write-router/SKILL.md`。图片先说明用途，再决定制作方式；没有信息价值不配图。

## 选择素材

- 操作、报错和实测证据使用真实截图，保留必要上下文及版本。生成图片不得冒充产品界面或测量结果。
- 封面与插画可以使用生成图片或授权素材。检查项目实际可调用的工具；没有生成工具时提供构图方案，不声称已生成，不固定模型名称、价格或退休日期。
- 图表和机制图由 `skills/visualize/SKILL.md` 处理。
- 修改原素材前留存原图，输出另存到文章关联资源目录。已有文章沿用原资源约定；新文章优先 `static/images/<article-slug>/`。

## 项目图床与 CDN 约定

博客图片资源默认使用独立的 RustFS `assets` 桶，不把博客正文桶当作图床，也不把业务域名泛解析直接切到 CDN。

- 资源访问域名：`assets.bigfisharc.cn`
- CDN 源站域名：`origin-assets.bigfisharc.cn`
- 阿里云 DNS：`origin-assets.bigfisharc.cn` 解析到 Caddy 公网入口；它不是 CDN CNAME，避免回源循环。
- 回源链路：七牛 CDN → `origin-assets.bigfisharc.cn` → Caddy → RustFS `assets` 桶。
- 对外路径 `/images/...` 映射到 RustFS 对象路径 `/assets/images/...`；博客文章中使用 `https://assets.bigfisharc.cn/images/...`。
- 七牛回源协议使用 HTTPS；回源 Host 和 SNI 选择“源站域名”。边缘证书由七牛托管，源站证书由 Caddy 管理。
- `assets` 桶只放公开静态资源；私密文件不能放入此桶，应使用私有桶和签名 URL。
- 图片上传后优先使用不可变文件名或内容 hash，CDN 可设置长缓存；HTML、API、登录和 WebSocket 不走这个图床桶。
- 新文章上传图片前先确认对象已进入 `assets` 桶并能通过源站路径验证，再配置七牛 CDN；不要把图床配置写成博客 CDN 或直接写 RustFS 内网地址。

这套约定只适用于博客图片和公开静态资源；具体 CDN 切换仍需按当前七牛控制台状态核验。

## 制作与检查

1. 确定图要说明的内容、正文引用位置、来源与授权。
2. 裁剪无关区域，给账号、密钥等加实色遮挡，不修改实验数字和错误信息。
3. 按正文显示宽度缩放。截图优先 PNG，照片可用 WebP；不以压缩体积为由牺牲文字可读性。
4. 图注说明重点、结论及事实边界。图片 alt 描述内容，不堆关键词。
5. 使用 `image_view` 查看结果，实际页面检查正文和放大视图；分享图另外检查 OG 引用及裁剪。不能用文件存在或尺寸正确代替视觉验收。

## 本地工具

通过项目 MCP 的 `blog-tools` server 调用 `image_view`、`image_process`；或使用 CLI：

```text
python scripts/blog_runtime.py image-info static/images/example/cover.png
python scripts/blog_runtime.py image-process static/images/example/raw.png static/images/example/cover.webp --max-width 1200
```

`image_process` 支持裁剪、实色脱敏、缩放和压缩，禁止覆盖输入和已有输出。需要 Pillow；依赖配置见 `docs/writing/tools.md`。处理真实截图与生成图片分开，不把示意图当成证据。

完成后交回系统提示词决定后续阶段。社交任务的方法见 `skills/social/SKILL.md`；视频只在系统提示词确认本次明确需要时读取 `optional-skills/video/SKILL.md`。
