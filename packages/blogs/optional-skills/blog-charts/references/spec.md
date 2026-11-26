# 图表输入规格

使用 UTF-8 JSON 对象；路径相对于规格文件目录解析。运行：

```text
python render_chart.py --spec article.json --output article-assets --name comparison
```

`--name` 只支持英文字母、数字、下划线、连字符。同名输出会覆盖。

## 通用字段

| 字段 | 说明 |
| --- | --- |
| `type` | `line`、`bar`、`barh`、`compare` |
| `title` | 必填，图内标题 |
| `source` | 非示意图必填，真实来源说明，可含 URL/日期/版本 |
| `illustrative` | 默认 false；true 时图内强制标“示意数据” |
| `note` | 测试条件、统计方法或指标方向等补充说明 |
| `alt` | 图片替代文本，默认采用标题 |
| `width` / `height` | 像素，默认 1600/900；范围 640–4096 / 400–4096 |
| `font` | 可选，本地中文字体文件路径；默认自动查找中文字体 |

默认输出 PNG、SVG、可重绘 JSON、Markdown。来源和说明过长时用较大的 height 或拆短文字；交付前检查图片。

## 数值图

```json
{
  "type": "bar",
  "title": "不同实现的请求延迟",
  "illustrative": true,
  "note": "仅展示格式；延迟越低越好，不代表真实测试结果",
  "x": ["实现 A", "实现 B", "实现 C"],
  "ylabel": "请求延迟",
  "unit": "ms",
  "series": [
    {"name": "中位数", "values": [18, 12, 9]},
    {"name": "P95", "values": [35, 26, 19]}
  ],
  "show_values": true
}
```

- `x`：非空数组。各系列 values 长度与它相同；最多 8 个系列。
- `series`：对象数组，每个包含 `name`、有限数值数组 `values`、可选十六进制 `color`。
- `xlabel`、`ylabel`、`unit`：坐标和单位。水平柱状图中 `ylabel` 指数值指标。
- `show_values`：柱状图显示数值，默认 true；折线图默认 false。
- 折线图可使用 `null` 表示缺失值（断线）；柱状图不接受缺失值，先明确处理方法。
- `x_type`：折线图默认 `category`（等间距），可设 `number` 或 `date`。后两者必须严格递增，日期用 ISO 8601 且时区口径一致。分类顺序按输入保留。
- 不同指标/单位不要放在同一数值轴，脚本不生成双轴或 3D 图。
- `ylim`：仅折线图可选 `[最小值, 最大值]`，必须包含所有数据值；图中会标注该范围。默认折线图纵轴包含零。

折线图示例（间隔与日期对应，缺测保持断点）：

```json
{
  "type": "line",
  "title": "每日访问量趋势",
  "illustrative": true,
  "x_type": "date",
  "x": ["2026-10-01", "2026-10-02", "2026-10-06", "2026-10-10"],
  "ylabel": "访问量",
  "unit": "次",
  "series": [{"name": "博客", "values": [120, null, 180, 210]}]
}
```

## 图片前后对比

```json
{
  "type": "compare",
  "title": "页面优化前后",
  "source": "用户提供的同尺寸页面截图",
  "note": "原图等比展示，未修改截图内容",
  "images": [
    {"label": "优化前", "path": "before.png"},
    {"label": "优化后", "path": "after.png"}
  ]
}
```

`images` 支持 2–4 张本地图片，不自动下载网络资源。原图不拉伸或裁切，统一画布留白使展示比例一致，4 张使用 2×2 布局。SVG 内嵌的是截图位图；数值图 SVG 是矢量图。图片会复制到 `<name>-assets/`，输出 JSON 使用这些相对路径，可连同资产目录移到别处重新生成。
