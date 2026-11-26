# 默认博客技能

默认发现本目录，顶层共 11 个技能。write-router 内 6 个题材模式仍保留，只有被选中的模式正文才读取。

| 技能 | 用途 |
|---|---|
| write-router | 通用材料加工、成文和审稿方法；阶段与技能调用由系统提示词编排 |
| visualize | 数据图、机制图和有必要的交互解释 |
| archify | 架构、流程、时序、数据流、状态、ERD、类图、树、时间线、耗时瀑布图（上游 v3.1.0，随默认发现加载） |
| image | 博客封面、插画、真实截图处理 |
| blog-qa | 可执行终稿检查与验收交接 |
| content-strategy | 基于博客读者和已有文章规划选题 |
| seo-audit | 按需检查搜索、索引和文章元信息 |
| schema | 按需检查结构化数据 |
| site-architecture | 栏目、文章关系与站内链接 |
| social | 按需把文章整理为传播稿 |
| analytics | 基于本站 Umami / 用户导出数据评估文章反馈 |

默认技能不意味着每次全部读取。系统提示词 `.rpi/SYSTEM.md` 决定调用哪些技能、阶段顺序、返工和交付；文章任务先读取 write-router 的通用方法，运营技能只补充本次需要的能力。营销上游提到的销售、产品上下文或未配置工具不自动成为博客前置条件。

archify 是上游运行包（<https://github.com/tt-a1i/archify>），保留上游字节与许可证，来源和入口哈希固定在 `skills/archify.lock.json`；`node skills/archify/bin/archify.mjs doctor` 可检查安装完整性。它随默认发现加载，但只在 visualize 需要结构图时读取正文。

可选库见 `optional-skills/README.md`；本地工具和验证边界见 `docs/writing/tools.md`。
