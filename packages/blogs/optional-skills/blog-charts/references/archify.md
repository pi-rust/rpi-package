# 与 Archify 配合

Archify 是结构与关系图的生成器；blog-charts 是数值图与真实截图对比的生成器。同一篇文章可以同时使用，单张图只选择负责它的技能，不在二者之间重复生成同一内容。

| 读者的问题 | 使用方式 |
| --- | --- |
| 哪种方案更快/更便宜，趋势如何 | blog-charts，真实数据与来源，折线或柱状图 |
| 页面前后有什么变化 | blog-charts 的 compare，输入真实截图 |
| 系统由什么组成，服务如何连接 | Archify architecture |
| 操作有哪些步骤、分支与异常 | Archify workflow |
| 谁先调用谁，响应/异步如何返回 | Archify sequence |
| 数据从哪里来、经过哪些处理、去哪里 | Archify dataflow |
| 草稿、等待、重试和结束如何转换 | Archify lifecycle |
| 数据库表结构、主外键与字段 | Archify erd（v3.1 起） |
| 类/接口的字段与方法契约 | Archify class（v3.1 起） |
| 模块或组织层级 | Archify tree（v3.1 起） |
| 事件沿时间轴发生（无依赖） | Archify timeline（v3.1 起） |
| 一次请求内部各阶段耗时叠加 | Archify waterfall（v3.1 起） |
| 功能与优缺点比较 | Markdown 表格；只有需要视觉卡片时另作设计 |

## 一篇文章中的分工

例如缓存优化文章：Archify 解释“请求 → 缓存 → 缺失时查询数据库”，blog-charts 绘制用户提供的 P50/P95 对比。架构箭头不证明性能提升，性能结果不能代替架构证据。保持产品名称、术语和系列颜色一致；不要虚构服务、接口、测试数据或版本。

由 write-router 进入 `skills/visualize/SKILL.md`，需要关系图时才读取默认技能 `skills/archify/SKILL.md`。加载 Archify 自己要求的 schema 和参考资料，不把整套文档复制进 blog-charts。保留上游文件及许可证，来源固定在 `skills/archify.lock.json`。

从项目内运行：

```powershell
node skills/archify/bin/archify.mjs finalize architecture "candidate.json" "article-architecture.html" --quality showcase --json
```

`meta.output` 必须与输出 HTML 对应，`meta.quality_profile` 与 `--quality` 一致（showcase 或 standard，不匹配时 `deliver` 会失败）。遵守 Archify 的 `finalize`、来源证据和修复流程。对真实仓库图，引用锁定提交中的文件证据；组合示例只是工作方案，不声称分析了真实程序实现。

v3.1 起支持十种图型：architecture、workflow、sequence、dataflow、lifecycle、erd、class、tree、timeline、waterfall。字段级 ERD 表多（约 8 张表或 50 个字段以上）时首轮用 `standard`，排版仍可读再升 showcase。

## 交付给博客

1. 保留 Archify 的 JSON、验证回执和完整 HTML，中文内容设置 `meta.locale: "zh-CN"`。
2. 检查生成图；按需要用 `visual-check` 获取截图并实际查看。自动检查通过不能代替视觉检查。
3. 在生成 HTML 的 Export 菜单导出完整图的 PNG/SVG，正文默认用浅色 PNG。不能把带工具栏的浏览器截图当作正式导出图，也不要用交互 HTML 替代正文图片。未实际导出时明确记录待导出。
4. 数值图继续使用 blog-charts 的 PNG/SVG 与数据 JSON。正文图放在 `static/images/<article-slug>/`，使用 `/images/<article-slug>/<name>.png` 引用；交互 HTML 可另存为静态文件并给出配套链接。保留 JSON、回执和可重绘资源，正文不引用技能目录或本机绝对路径。
5. 在 `task dev` 的实际文章页面检查正文和放大视图。图表完成后回到 write-router 和 blog-qa，分别记录机器检查、事实审校和视觉检查；文章仍为草稿。

需要跨平台同步时另按用户授权使用同步工具，正文用 PNG，交互 HTML 不直接注入微信公众号等富文本编辑器。本组合不增加自动发布或全局工具依赖。
