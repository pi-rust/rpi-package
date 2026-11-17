# 「明细」按钮对齐核查（#306）

> 诉求：会话 status 行的「明细」按键应右对齐，与上下文 / CH 等指标同一水平线。
> 基线：`main@85fb0bd`。结论：**基线已实现该诉求**，本分支只记录核查结果，
> 未改行为（改动会破坏现状）。

## 现状

status 行有两个渲染变体，均已在同一 flex 行内右对齐「明细」入口：

1. 精简版（`App.tsx` 的 `composer-metrics slim-metrics`，指标为 cache / context，
   末尾是 `<button class="slim-metrics-more">{t("details")}</button>`）：
   `ui-slim.css` 的 `.slim-metrics-more { margin-left: auto; }` 把按钮推到行尾，
   容器 `.slim-metrics { display: flex; align-items: center; }` 保证同一水平线。

2. workspace 完整版（`<details class="composer-metrics">`，summary 内 4 个
   `composer-metric` + `<span class="composer-metrics-more">{t("details")}</span>`）：
   `desktop.css` 的 `.workspace .composer-metrics-summary { display: flex;
   flex-wrap: nowrap; align-items: center; }` 与
   `.workspace .composer-metrics-more { margin-left: auto; }` 达成同样布局。

根容器始终带 `workspace` 类（`App.tsx` 的 `className="workspace…"`），
所以完整版的规则在任何视口都命中；窄屏媒体查询只收紧 gap，不改变对齐，
`flex-wrap: nowrap` 防止换行错位。

## 若仍复现「不右对齐」

- 先确认页面构建自 `main@85fb0bd` 之后（`margin-left: auto` 是后来加入的）；
- 浏览器扩展注入的旧 CSS 缓存（HTML 已带 `no-store`，但扩展级缓存不走该头）；
- 若是别的 metrics 变体（移动端 sheet 等）缺规则，按上面两处同样的
  `margin-left: auto` 模式补齐即可。

## 验证方式（无 node 环境下的人工路径）

打开一个有 token 统计的会话，观察 composer 底部 status 行：
「明细 / Details」应位于行尾右端，与 cache / context 指标同一行。
typecheck 未执行：云机无 node/npm。
