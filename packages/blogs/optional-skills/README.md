# 可选技能库

此目录不加入默认发现路径。任务需要时读取精确路径，文章任务仍先经过 `skills/write-router/SKILL.md`。

| 路径 | 使用条件 |
|---|---|
| ai-seo/SKILL.md | 明确讨论 AI 搜索可见性 |
| copywriting/SKILL.md | 明确要写营销转化文案 |
| copy-editing/SKILL.md | 明确要编辑营销文案 |
| video/SKILL.md | 明确要视频脚本或制作方案 |
| community-marketing/SKILL.md | 明确要社区运营 |
| public-relations/SKILL.md | 明确要公关材料 |
| competitors/SKILL.md | 明确要产品对比或替代页 |
| competitor-profiling/SKILL.md | 明确要竞品资料研究 |
| customer-research/SKILL.md | 明确要用户研究 |
| blog-charts/SKILL.md | visualize 按需调用，数值图与真实截图对比 |
| mermaid/SKILL.md | visualize 按需调用，正文内 ```mermaid 流程图、时序、状态图；写入前校验渲染 |
| drawio/SKILL.md | visualize 按需调用，从源码/规格抽取模块与调用关系，产出可编辑 .drawio |
| diagram-design/SKILL.md | visualize 按需调用，需要编辑级设计感的流程、架构、时间线、状态图 HTML/SVG/PNG |

## 图表类技能分工

| 读者要理解什么 | 用哪个 | 交付 |
|---|---|---|
| 数值趋势与对比、真实截图前后 | blog-charts | PNG/SVG + 数据 JSON |
| 架构、请求时序、数据流、状态机（精细、可交互） | skills/archify（默认发现） | 校验过的 HTML + JSON，另导出 PNG/SVG |
| 正文里贴进 Markdown 的流程图、时序图 | mermaid | 校验通过的 ```mermaid 源码，需要时导 SVG/PNG |
| 从仓库源码/清单得到模块关系、调用边 | drawio | 可编辑 .drawio、Diagram IR、结构校验回执 |
| 排版讲究的架构/流程/时间线长图 | diagram-design | 单文件 HTML（内联 SVG），可导出 PNG |

mermaid、drawio、diagram-design 的上游包、许可证和提交哈希分别固定在 `mermaid.lock.json`、`drawio.lock.json`、`diagram-design.lock.json`；校验与调用命令见 `docs/writing/visual-skills.md`。运行时依赖 Node.js（Mermaid 校验）和 Python 标准库（drawio 导入器）；Graphviz `dot`、draw.io 桌面版缺失时不影响 XML 生成和 Mermaid 校验，但自动布局与原生导出不可用。

保留上游方法与参考资料，不代表已连接其中的 API、MCP 或第三方账号。资料中的已删除技能名称只作为上游关联能力介绍，不安装、不加载，不能用它们当本次必经步骤。营销素材缺失时不虚构产品、客户数据或案例。

Archify 已移入默认发现目录 `skills/archify/`，来源固定在 `skills/archify.lock.json`，不再属于本可选库；blog-charts 通过项目 Python 环境执行。均不加载全局技能，图表入口与检查方式见 `docs/writing/visual-skills.md`。
