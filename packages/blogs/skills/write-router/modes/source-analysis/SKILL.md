---
name: writing-source-analysis
parent: write-router
description: Source-code reading, call-chain explanation, framework internals, architecture analysis, and implementation deep dives.
---

# Source Analysis Mode

## 适用

用于源码剖析、框架内部、调用链、函数实现、数据结构、架构设计和实现原理文章。

## 文章目标

带读者从入口读懂一段代码怎样运行、为什么这样设计，以及应该如何继续阅读。

## 推荐结构

```text
读者为什么会卡住
→ 版本与入口
→ 文件和函数地图
→ 一条核心调用链
→ 关键数据结构
→ 关键代码片段
→ 设计取舍
→ 常见误解
→ 怎样继续阅读
```

## 写法

- 开始先说明版本、分支、入口文件和阅读范围。
- 先给调用链地图，再深入局部代码。
- 每个代码片段只回答一个问题，并说明上下文。
- 区分“代码明确做了什么”和“根据命名或结构推测的意图”。
- 解释异常路径、边界条件和生命周期，不只讲 happy path。
- 复杂流程可使用一张 Mermaid flowchart 或 sequenceDiagram。
- 让读者知道下一步该打开哪个文件、搜索哪个函数。

## 准确性边界

- 不混用不同版本的源码。
- 文件路径、函数名和调用关系必须来自实际源码。
- 推测必须标注为推测。
- 不把注释、命名或宣传文档直接当成运行事实。

## 模式自检

- 新读者能否找到入口？
- 调用链是否有清楚的方向和数据流？
- 代码片段是否少而关键？
- 是否解释了设计取舍和容易误读的地方？

当前题材任务完成后，由 `.rpi/SYSTEM.md` 系统提示词决定下一阶段及返工；本模式只提供源码解释的具体写法。
