#!/usr/bin/env python3
"""Render reproducible blog charts from a JSON specification."""
import argparse
from datetime import datetime
import json
import math
from pathlib import Path
import re
import sys
import textwrap

import matplotlib
matplotlib.use("Agg")
from matplotlib import dates as mdates, font_manager, pyplot as plt
from matplotlib.ft2font import FT2Font
from PIL import Image, ImageOps

COLORS = ["#2563EB", "#D97706", "#059669", "#9333EA", "#DC2626", "#0891B2", "#475569", "#BE185D"]


def number(value):
    if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value):
        raise ValueError(f"Expected a finite number, got {value!r}")
    return value


def required_text(obj, key):
    value = obj.get(key)
    if not isinstance(value, str) or not value.strip():
        raise ValueError(f"{key} must be a nonempty string")
    return value.strip()


def wrap(value, width=64):
    return "\n".join(textwrap.wrap(str(value), width=width))


def validate(spec, base):
    if not isinstance(spec, dict):
        raise ValueError("Specification must be a JSON object")
    kind = spec.get("type")
    if kind not in {"line", "bar", "barh", "compare"}:
        raise ValueError("type must be line, bar, barh, or compare")
    required_text(spec, "title")
    if not isinstance(spec.get("illustrative", False), bool):
        raise ValueError("illustrative must be true or false")
    if not spec.get("illustrative", False):
        required_text(spec, "source")
    for key in ("source", "note", "alt", "xlabel", "ylabel", "unit"):
        if key in spec and not isinstance(spec[key], str):
            raise ValueError(f"{key} must be a string")
    for key, default, low in (("width", 1600, 640), ("height", 900, 400)):
        value = spec.get(key, default)
        if isinstance(value, bool) or not isinstance(value, int) or not low <= value <= 4096:
            raise ValueError(f"{key} must be an integer between {low} and 4096")
    if "show_values" in spec and not isinstance(spec["show_values"], bool):
        raise ValueError("show_values must be true or false")
    if kind == "compare":
        images = spec.get("images")
        if not isinstance(images, list) or not 2 <= len(images) <= 4:
            raise ValueError("compare requires 2 to 4 images")
        for item in images:
            required_text(item, "label")
            path = (base / required_text(item, "path")).resolve()
            if not path.is_file():
                raise ValueError(f"Image not found: {path}")
        return None
    xs, series = spec.get("x"), spec.get("series")
    if not isinstance(xs, list) or not xs:
        raise ValueError("x must be a nonempty array")
    if not isinstance(series, list) or not 1 <= len(series) <= 8:
        raise ValueError("series must contain 1 to 8 series")
    for item in series:
        required_text(item, "name")
        values = item.get("values")
        if not isinstance(values, list) or len(values) != len(xs):
            raise ValueError("Each values array must have the same length as x")
        present = [v for v in values if v is not None]
        if not present:
            raise ValueError("Each series needs at least one numeric value")
        for value in present:
            number(value)
        if kind != "line" and len(present) != len(values):
            raise ValueError("Bars do not accept missing values; specify their treatment first")
        if "color" in item and not re.fullmatch(r"#[0-9a-fA-F]{6}", str(item["color"])):
            raise ValueError("color must be a six-digit hex color")
    if "ylim" in spec:
        limits = spec["ylim"]
        if kind != "line" or not isinstance(limits, list) or len(limits) != 2:
            raise ValueError("ylim is supported only for lines as [minimum, maximum]")
        lo, hi = map(number, limits)
        values = [v for item in series for v in item["values"] if v is not None]
        if lo >= hi or min(values) < lo or max(values) > hi:
            raise ValueError("ylim must increase and contain all data values")
    xtype = spec.get("x_type", "category")
    if kind != "line" and xtype != "category":
        raise ValueError("Bar charts use category x coordinates")
    if xtype == "number":
        coordinates = [number(v) for v in xs]
    elif xtype == "date":
        times = [datetime.fromisoformat(str(v).replace("Z", "+00:00")) for v in xs]
        if len({t.utcoffset() is None for t in times}) > 1:
            raise ValueError("Do not mix timezone-aware and naive dates")
        coordinates = list(mdates.date2num(times))
    elif xtype == "category":
        if any(not isinstance(v, (str, int, float)) or isinstance(v, bool) for v in xs):
            raise ValueError("Category labels must be strings or numbers")
        coordinates = list(range(len(xs)))
    else:
        raise ValueError("x_type must be category, number, or date")
    if xtype != "category" and any(a >= b for a, b in zip(coordinates, coordinates[1:])):
        raise ValueError("Numeric and date x coordinates must strictly increase")
    return coordinates


def configure_font(spec, base):
    if spec.get("font"):
        path = (base / spec["font"]).resolve()
        font_manager.fontManager.addfont(str(path))
        family = font_manager.FontProperties(fname=str(path)).get_name()
    else:
        installed = {f.name: f.fname for f in font_manager.fontManager.ttflist}
        family = next((n for n in ["Microsoft YaHei", "Noto Sans CJK SC", "SimHei", "PingFang SC", "WenQuanYi Zen Hei"] if n in installed), "DejaVu Sans")
        path = Path(font_manager.findfont(family))
    charmap = FT2Font(str(path)).get_charmap()
    labels = [spec["title"], spec.get("source", ""), spec.get("note", ""), "示意数据" if spec.get("illustrative") else "来源", spec.get("xlabel", ""), spec.get("ylabel", ""), spec.get("unit", "")]
    labels += [str(v) for v in spec.get("x", [])]
    labels += [i["name"] for i in spec.get("series", [])]
    labels += [i["label"] for i in spec.get("images", [])]
    missing = sorted({c for c in "".join(labels) if not c.isspace() and ord(c) not in charmap})
    if missing:
        raise ValueError(f"Font {family} lacks characters {''.join(missing)}; set font to a suitable font file")
    plt.rcParams.update({"font.family": family, "axes.unicode_minus": False, "font.size": 12, "svg.fonttype": "path", "text.parse_math": False})
    return family


def render(spec, base, output, name):
    xs = validate(spec, base)
    family = configure_font(spec, base)
    width, height = spec.get("width", 1600), spec.get("height", 900)
    fig = plt.figure(figsize=(width / 160, height / 160), dpi=160, facecolor="white")
    fig.suptitle(wrap(spec["title"], 48), fontsize=19, fontweight="bold", y=.97)
    footer = []
    if spec.get("illustrative"):
        footer.append("示意数据")
    if spec.get("source"):
        footer.append("来源：" + spec["source"])
    if spec.get("note"):
        footer.append(spec["note"])
    if "ylim" in spec:
        footer.append(f"纵轴范围：{spec['ylim'][0]}–{spec['ylim'][1]}")
    footer_text = "\n".join(wrap(s) for s in footer)
    footer_lines = max(1, len(footer_text.splitlines()))
    bottom = max(.19, .07 + footer_lines * 16 / (height / 160 * 72))
    if bottom > .42:
        raise ValueError("Footer is too long for this height; shorten text or increase height")
    fig.text(.06, .025, footer_text, fontsize=10, color="#475569", va="bottom")
    normalized = json.loads(json.dumps(spec, ensure_ascii=False))
    if spec.get("font"):
        normalized["font"] = str((base / spec["font"]).resolve())
    assets = []
    if spec["type"] == "compare":
        items = spec["images"]
        rows, cols = (2, 2) if len(items) == 4 else (1, len(items))
        axes = fig.subplots(rows, cols, squeeze=False)
        pictures = []
        for item in items:
            with Image.open((base / item["path"]).resolve()) as original:
                picture = ImageOps.exif_transpose(original).convert("RGBA")
                background = Image.new("RGBA", picture.size, "white")
                background.alpha_composite(picture)
                pictures.append(background.convert("RGB"))
        canvas_size = (max(p.width for p in pictures), max(p.height for p in pictures))
        for idx, item in enumerate(items):
            picture = pictures[idx]
            canvas = Image.new("RGB", canvas_size, "white")
            canvas.paste(picture, ((canvas_size[0] - picture.width) // 2, 0))
            ax = axes.flat[idx]
            ax.imshow(canvas)
            ax.set_title(wrap(item["label"], 20), fontsize=13, pad=10)
            ax.axis("off")
            asset_path = Path(f"{name}-assets") / f"{idx + 1}.png"
            normalized["images"][idx]["path"] = asset_path.as_posix()
            assets.append((asset_path, picture))
        fig.subplots_adjust(left=.04, right=.96, bottom=bottom, top=.83, wspace=.12, hspace=.3)
    else:
        ax = fig.add_subplot()
        ax.spines[["top", "right"]].set_visible(False)
        ax.set_axisbelow(True)
        horizontal = spec["type"] == "barh"
        ax.grid(axis="x" if horizontal else "y", color="#E2E8F0", linewidth=.8)
        label = spec.get("ylabel", "")
        if spec.get("unit"):
            label += f" ({spec['unit']})"
        if horizontal:
            ax.set_xlabel(label)
            ax.set_ylabel(spec.get("xlabel", ""))
        else:
            ax.set_ylabel(label)
            ax.set_xlabel(spec.get("xlabel", ""))
        series = spec["series"]
        for idx, item in enumerate(series):
            color = item.get("color", COLORS[idx])
            values = [float("nan") if v is None else v for v in item["values"]]
            if spec["type"] == "line":
                ax.plot(xs, values, label=item["name"], color=color, linewidth=2.4, marker="o", markersize=5, linestyle=["-", "--", "-.", ":"][idx % 4])
                if spec.get("show_values", False):
                    for x, value in zip(xs, values):
                        if math.isfinite(value):
                            ax.annotate(f"{value:g}", (x, value), xytext=(0, 8), textcoords="offset points", ha="center", fontsize=9)
            else:
                barwidth = .8 / len(series)
                positions = [x + (idx - (len(series) - 1) / 2) * barwidth for x in xs]
                bars = ax.barh(positions, values, height=barwidth, label=item["name"], color=color) if horizontal else ax.bar(positions, values, width=barwidth, label=item["name"], color=color)
                if spec.get("show_values", True):
                    ax.bar_label(bars, labels=[f"{v:g}" for v in values], padding=4, fontsize=9)
        if spec.get("x_type", "category") == "category":
            labels = [wrap(v, 16) for v in spec["x"]]
            if horizontal:
                ax.set_yticks(xs, labels)
                ax.invert_yaxis()
            else:
                ax.set_xticks(xs, labels, rotation=25 if len(xs) > 6 else 0)
        elif spec["x_type"] == "date":
            locator = mdates.AutoDateLocator()
            ax.xaxis.set_major_locator(locator)
            ax.xaxis.set_major_formatter(mdates.ConciseDateFormatter(
                locator,
                formats=["%Y", "%m", "%d", "%H:%M", "%H:%M", "%S"],
                zero_formats=["", "%Y", "%m-%d", "%m-%d", "%H:%M", "%H:%M"],
                offset_formats=["", "%Y", "%Y-%m", "%Y-%m-%d", "%Y-%m-%d", "%Y-%m-%d %H:%M"],
            ))
        if "ylim" in spec:
            ax.set_ylim(spec["ylim"])
        elif spec["type"] == "line":
            low, high = ax.get_ylim()
            ax.set_ylim(min(0, low), max(0, high))
        if spec["type"] != "line":
            low, high = ax.get_xlim() if horizontal else ax.get_ylim()
            (ax.set_xlim if horizontal else ax.set_ylim)(min(0, low), max(0, high))
            ax.margins(x=.15 if horizontal else .05, y=.05 if horizontal else .15)
        ax.legend(loc="lower left", bbox_to_anchor=(0, 1.01), ncol=min(4, len(series)), frameon=False, fontsize=10)
        fig.subplots_adjust(left=.19 if horizontal else .12, right=.95, bottom=bottom, top=.79)
    output.mkdir(parents=True, exist_ok=True)
    for asset_path, picture in assets:
        (output / asset_path).parent.mkdir(parents=True, exist_ok=True)
        picture.save(output / asset_path)
    paths = {ext: output / f"{name}.{ext}" for ext in ("png", "svg", "json", "md")}
    try:
        fig.savefig(paths["png"], dpi=160, facecolor="white")
        fig.savefig(paths["svg"], facecolor="white", metadata={"Date": None})
    finally:
        plt.close(fig)
    paths["json"].write_text(json.dumps(normalized, ensure_ascii=False, indent=2, allow_nan=False) + "\n", encoding="utf-8")
    alt = spec.get("alt", spec["title"]).replace("\n", " ").replace("[", "\\[").replace("]", "\\]")
    paths["md"].write_text(f"![{alt}]({name}.png)\n\n{footer_text}\n", encoding="utf-8")
    return {"files": {k: str(v.resolve()) for k, v in paths.items()}, "font": family, "size": [width, height]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--spec", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--name", default="chart")
    args = parser.parse_args()
    if not re.fullmatch(r"[A-Za-z0-9_-]+", args.name):
        parser.error("--name must use letters, digits, underscores, or hyphens")
    try:
        spec = json.loads(args.spec.read_text(encoding="utf-8-sig"))
        result = render(spec, args.spec.resolve().parent, args.output.resolve(), args.name)
    except (ValueError, OSError, TypeError, KeyError, AttributeError) as exc:
        print(f"Chart error: {exc}", file=sys.stderr)
        return 2
    print(json.dumps(result, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
