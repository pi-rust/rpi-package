---
name: blog-charts
description: 为博客和技术文章制作可复现的折线图、柱状图、数值对比图及截图前后对比；需要中文标注、来源说明和可发布图片时使用。非数值功能比较可用表格，架构比较用流程图。
---

# 博客图表

把文章中的结论转成读者能验证的图表，交付适合博客正文的图片及可复现的数据。

文章任务先经过项目内 `skills/write-router/SKILL.md`，由 `skills/visualize/SKILL.md` 按图表用途读取本技能；完成后返回 visualize 和写作路由。这里只负责图表生成，不另建写作入口。

## 选择表达方式

- 时间、负载等连续变化：折线图。时间间隔不均匀时用真实日期或数值坐标。
- 方案之间的数值比较：分组柱状图；名称较长用水平柱状图。
- 功能、优缺点等非数值比较：优先 Markdown 表格；需要图片时可另做对比卡片。
- 页面或效果前后比较：真实截图并排，保留原图比例并注明标签。
- 架构、流程、时序、数据流、状态关系：需要精细布局或交互 HTML 时读取默认技能 `skills/archify/SKILL.md`；简单流程可直接用 Mermaid。组合交付时读取 [Archify 配合方式](references/archify.md)。

## 数据与呈现

先从用户数据、文件或可核验来源整理图表规格；不要为得到漂亮趋势编造数值。缺少必要数据时询问，或在用户需要示意时明确设置 `illustrative: true`，让图片可见地显示“示意数据”。

记录来源、日期/软件版本/测试条件、单位，以及“越高/越低越好”等适用说明。保留原始精度，单位换算和汇总写明方法；不同单位的指标分别绘图。缺失值使用 `null`，不要填零或自行插值。柱状图包含零基线，避免 3D 和装饰性面积。折线图截断纵轴时在图中注明。

默认白底、中文字体、1600×900 输出；单图只支撑一个主要结论。多系列保持颜色一致，必要时同时用线型区分。标签过多时拆图或增加尺寸，不能牺牲手机阅读效果。

## 使用绘图脚本

先按 [图表输入规格](references/spec.md) 编写 UTF-8 JSON，再运行 `scripts/render_chart.py`。脚本支持 `line`、`bar`、`barh`、`compare`，生成 `.png`、`.svg`、`.json`、`.md`；截图对比还会保存重绘所需的图片资产。

从 blogs 项目根目录运行，使用项目内的 Python 依赖环境：

```powershell
python scripts/blog_runtime.py chart --spec ".rpi/writing/article-slug/chart.json" --output "static/images/article-slug" --name "latency"
```

缺少依赖时运行 `python scripts/blog_runtime.py init`，安装到 `.rpi/blog-tools-venv/`。不依赖全局 skills、Codex 插件缓存或宿主专用运行时。`scripts/requirements-lock.txt` 保存原始绘图环境的精确版本，项目通用环境使用 `scripts/requirements-blog.txt`。相同 `--name` 会覆盖对应输出，改已有图前保留需要的版本。

文章图片放在 `static/images/<article-slug>/`，正文使用 `/images/<article-slug>/<name>.png`。生成的 `.md` 使用相对图片路径，嵌入文章时按上述博客路径调整；图表源数据可以保留在工作底稿目录，不把本机绝对路径写进正文。

## 检查与交付

打开生成 PNG，核对中文、标签、图例、坐标范围、单位、来源和数据点；检查是否裁切或遮挡。折线缺失值应断开，柱高和数值应一致，截图应保持比例。必要时调整规格重新生成。

交付 PNG 供正文与跨平台使用，SVG 供矢量编辑，JSON 和 Markdown 供修改复现。正文包含图注、关键结论和来源，不能只交图片。先在博客实际页面检查正文与放大视图，再返回 write-router 的终稿管线；保留草稿状态，发布按项目现有授权流程执行。
