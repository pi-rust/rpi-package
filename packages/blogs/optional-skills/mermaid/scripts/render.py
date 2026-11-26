#!/usr/bin/env python3
"""Deterministically render a diagram IR (JSON) to Mermaid source.

Same JSON in, same Mermaid text out. This module does not render. Pipe the output
through `scripts/check.sh` before inserting it into Markdown. `--backend` accepts
only `mermaid`.

Targets are the keys of SERIALIZERS. Schemas: ../reference/ir.md and ../reference/ir-catalog.md.

Usage:
    scripts/render.py diagram.json
    scripts/render.py diagram.json --target flowchart -o diagram.mmd
    scripts/render.py diagram.json --backend mermaid
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path
from typing import Any, Callable

IRError = ValueError


def _require(obj: dict, keys: tuple[str, ...], where: str) -> None:
    missing = [key for key in keys if key not in obj]
    if missing:
        raise IRError(f"{where} missing required field(s): {', '.join(missing)}")


def _safe_id(raw: str, table: dict[str, str]) -> str:
    """Map an arbitrary IR id to a Mermaid-safe bare identifier, memoized in `table`."""
    if raw in table:
        return table[raw]
    if re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", raw):
        safe = raw
    else:
        safe = "n_" + re.sub(r"[^A-Za-z0-9_]", "_", raw)
        if not re.match(r"[A-Za-z_]", safe):
            safe = "n_" + safe
    # Disambiguate collisions created by sanitization (e.g. "1.1" and "1_1").
    base, suffix, candidate = safe, 0, safe
    while candidate in table.values():
        suffix += 1
        candidate = f"{base}_{suffix}"
    table[raw] = candidate
    return candidate


def _quote(text: Any) -> str:
    """Quote a label for Mermaid, escaping embedded quotes and flattening newlines."""
    value = str(text).replace("\r\n", " ").replace("\n", " ").replace('"', "#quot;")
    return f'"{value}"'


def _identifier(raw: str, where: str) -> str:
    if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", raw):
        raise IRError(f"{where} id {raw!r} must be a bare identifier (letters, digits, underscore)")
    return raw


_FLOWCHART_DIRECTIONS = {"TB", "TD", "BT", "LR", "RL"}
_CARDINAL_DIRECTIONS = {"TB", "BT", "LR", "RL"}


def _direction(
    ir: dict, where: str, *, allowed: set[str], default: str | None = None
) -> str | None:
    direction = ir.get("direction", default)
    if direction is not None and direction not in allowed:
        raise IRError(f"{where} has unknown direction {direction!r}; use one of {sorted(allowed)}")
    return direction


# --- graph family -----------------------------------------------------------------------

_FLOWCHART_SHAPES = {
    "terminator": "stadium",
    "process": "rect",
    "decision": "diamond",
    "io": "lean-r",
    "subprocess": "subproc",
    "document": "doc",
    "store": "cyl",
    "connector": "circle",
}
_FLOWCHART_EDGES = {
    "normal": "-->",
    "dependency": "-->",
    "conditional": "-.->",
    "weak": "-.->",
    "strong": "==>",
}
_MINDMAP_SHAPES = {
    "root": ("((", "))"),
    "circle": ("((", "))"),
    "square": ("[", "]"),
    "bang": ("))", "(("),
    "cloud": (")", "("),
}
# Fixed status -> color. `status` is data; only statuses that appear are emitted.
_FLOWCHART_STATUS_STYLES = {
    "done": "fill:#9f9,stroke:#393,color:#000",
    "ready": "fill:#9cf,stroke:#369,color:#000",
    "blocked": "fill:#f99,stroke:#933,color:#000",
    "pending": "fill:#eee,stroke:#999,color:#000",
}


def render_graph_flowchart(ir: dict) -> str:
    _require(ir, ("nodes", "edges"), "graph/flowchart")
    ids: dict[str, str] = {}
    lines = [f"flowchart {_direction(ir, 'graph/flowchart', allowed=_FLOWCHART_DIRECTIONS, default='TD')}"]
    by_group: dict[str | None, list[dict]] = {}
    for node in ir["nodes"]:
        _require(node, ("id", "label"), "graph/flowchart node")
        by_group.setdefault(node.get("group"), []).append(node)

    used_statuses: list[str] = []
    class_lines: list[str] = []

    def emit_node(node: dict, indent: str) -> None:
        node_id = _safe_id(node["id"], ids)
        shape = _FLOWCHART_SHAPES.get(node.get("kind", "process"))
        if shape is None:
            raise IRError(f"graph/flowchart node {node['id']!r} has unknown kind {node['kind']!r}")
        lines.append(f'{indent}{node_id}@{{ shape: {shape}, label: {_quote(node["label"])} }}')
        status = node.get("status")
        if status is not None:
            if status not in _FLOWCHART_STATUS_STYLES:
                raise IRError(f"graph/flowchart node {node['id']!r} has unknown status {status!r}")
            if status not in used_statuses:
                used_statuses.append(status)
            class_lines.append(f"  class {node_id} {status}")

    for group in ir.get("groups", []):
        _require(group, ("id", "label"), "graph/flowchart group")
        group_id = _safe_id(group["id"], ids)
        lines.append(f'  subgraph {group_id}[{_quote(group["label"])}]')
        for node in by_group.pop(group["id"], []):
            emit_node(node, "    ")
        lines.append("  end")
    for node in by_group.pop(None, []):
        emit_node(node, "  ")
    leftover = [group for group in by_group if group is not None]
    if leftover:
        raise IRError(f"graph/flowchart nodes reference undeclared group(s): {leftover}")

    for edge in ir["edges"]:
        _require(edge, ("from", "to"), "graph/flowchart edge")
        arrow = _FLOWCHART_EDGES.get(edge.get("kind", "normal"))
        if arrow is None:
            raise IRError(f"graph/flowchart edge has unknown kind {edge['kind']!r}")
        if edge["from"] not in ids or edge["to"] not in ids:
            raise IRError(f"graph/flowchart edge references undeclared node: {edge}")
        label = f'|{_quote(edge["label"])}|' if edge.get("label") else ""
        lines.append(f'  {ids[edge["from"]]} {arrow}{label} {ids[edge["to"]]}')

    if used_statuses:
        style_lines = [f"  classDef {status} {_FLOWCHART_STATUS_STYLES[status]}" for status in used_statuses]
        lines[1:1] = style_lines  # after the `flowchart <direction>` line, before any node/subgraph
        lines.extend(class_lines)
    return "\n".join(lines) + "\n"


def render_graph_mindmap(ir: dict) -> str:
    _require(ir, ("nodes", "edges", "root"), "graph/mindmap")
    nodes: dict[str, dict] = {}
    for node in ir["nodes"]:
        _require(node, ("id", "label"), "graph/mindmap node")
        if node["id"] in nodes:
            raise IRError(f"graph/mindmap has duplicate node id {node['id']!r}")
        nodes[node["id"]] = node
    if ir["root"] not in nodes:
        raise IRError(f"graph/mindmap root {ir['root']!r} is not a declared node")
    children: dict[str, list[str]] = {}
    for edge in ir["edges"]:
        _require(edge, ("from", "to"), "graph/mindmap edge")
        if edge["from"] not in nodes or edge["to"] not in nodes:
            raise IRError(f"graph/mindmap edge references undeclared node: {edge}")
        children.setdefault(edge["from"], []).append(edge["to"])

    lines = ["mindmap"]
    visited: set[str] = set()

    def emit(node_id: str, depth: int) -> None:
        if node_id in visited:
            raise IRError(
                f"graph/mindmap node {node_id!r} is reached more than once; mindmap requires a tree"
            )
        visited.add(node_id)
        node = nodes.get(node_id)
        if node is None:
            raise IRError(f"graph/mindmap edge references undeclared node {node_id!r}")
        kind = "root" if node_id == ir["root"] else node.get("kind", "default")
        if kind == "default":
            text = str(node["label"])
        else:
            open_, close = _MINDMAP_SHAPES.get(kind, ("", ""))
            if kind not in _MINDMAP_SHAPES:
                raise IRError(f"graph/mindmap node {node_id!r} has unknown kind {kind!r}")
            text = f'{open_}{node["label"]}{close}'
        lines.append("  " * (depth + 1) + text)
        for child in children.get(node_id, []):
            emit(child, depth + 1)

    emit(ir["root"], 0)
    unreachable = [node_id for node_id in nodes if node_id not in visited]
    if unreachable:
        raise IRError(
            f"graph/mindmap node(s) are not reachable from root {ir['root']!r}: {unreachable}"
        )
    return "\n".join(lines) + "\n"


_BLOCK_SHAPES = {
    "process": ("[", "]"),
    "terminator": ("([", "])"),
    "decision": ("{", "}"),
    "subprocess": ("[[", "]]"),
    "store": ("[(", ")]"),
    "connector": ("((", "))"),
    "io": ("[/", "/]"),
}


def render_graph_block(ir: dict) -> str:
    _require(ir, ("nodes", "edges"), "graph/block")
    ids: dict[str, str] = {}
    lines = ["block"]
    columns = ir.get("columns")
    if columns is not None:
        if not isinstance(columns, int) or isinstance(columns, bool) or columns < 1:
            raise IRError(f"graph/block 'columns' must be a positive integer, got {columns!r}")
        lines.append(f"  columns {columns}")
    by_group: dict[str | None, list[dict]] = {}
    for node in ir["nodes"]:
        _require(node, ("id", "label"), "graph/block node")
        by_group.setdefault(node.get("group"), []).append(node)

    def emit_node(node: dict, indent: str) -> None:
        node_id = _safe_id(node["id"], ids)
        shape = _BLOCK_SHAPES.get(node.get("kind", "process"))
        if shape is None:
            raise IRError(f"graph/block node {node['id']!r} has unknown kind {node.get('kind')!r}")
        open_, close = shape
        lines.append(f"{indent}{node_id}{open_}{_quote(node['label'])}{close}")

    for group in ir.get("groups", []):
        _require(group, ("id", "label"), "graph/block group")
        group_id = _safe_id(group["id"], ids)
        lines.append(f'  block:{group_id}[{_quote(group["label"])}]')
        for node in by_group.pop(group["id"], []):
            emit_node(node, "    ")
        lines.append("  end")
    for node in by_group.pop(None, []):
        emit_node(node, "  ")
    leftover = [group for group in by_group if group is not None]
    if leftover:
        raise IRError(f"graph/block nodes reference undeclared group(s): {leftover}")

    for edge in ir["edges"]:
        _require(edge, ("from", "to"), "graph/block edge")
        kind = edge.get("kind", "normal")
        if kind not in ("normal", "dependency"):
            raise IRError(
                f"graph/block edge has unknown kind {kind!r}; block edges are plain arrows only"
            )
        if edge["from"] not in ids or edge["to"] not in ids:
            raise IRError(f"graph/block edge references undeclared node/group: {edge}")
        if edge.get("label"):
            lines.append(f'  {ids[edge["from"]]}-- {_quote(edge["label"])} -->{ids[edge["to"]]}')
        else:
            lines.append(f'  {ids[edge["from"]]}-->{ids[edge["to"]]}')
    return "\n".join(lines) + "\n"


_C4_PERSON_SYSTEM_KINDS = {
    "person": "Person",
    "person_ext": "Person_Ext",
    "system": "System",
    "system_db": "SystemDb",
    "system_queue": "SystemQueue",
    "system_ext": "System_Ext",
    "system_db_ext": "SystemDb_Ext",
    "system_queue_ext": "SystemQueue_Ext",
}
_C4_CONTAINER_KINDS = {
    "container": "Container",
    "container_db": "ContainerDb",
    "container_queue": "ContainerQueue",
    "container_ext": "Container_Ext",
    "container_db_ext": "ContainerDb_Ext",
    "container_queue_ext": "ContainerQueue_Ext",
}
_C4_COMPONENT_KINDS = {
    "component": "Component",
    "component_db": "ComponentDb",
    "component_queue": "ComponentQueue",
    "component_ext": "Component_Ext",
    "component_db_ext": "ComponentDb_Ext",
    "component_queue_ext": "ComponentQueue_Ext",
}
_C4_NODE_KINDS = {**_C4_PERSON_SYSTEM_KINDS, **_C4_CONTAINER_KINDS, **_C4_COMPONENT_KINDS}
_C4_TECH_KINDS = set(_C4_CONTAINER_KINDS) | set(_C4_COMPONENT_KINDS)
_C4_DEPLOYMENT_KINDS = {
    "deployment_node": "Deployment_Node",
    "node": "Node",
    "node_left": "Node_L",
    "node_right": "Node_R",
}
_C4_TARGET_NODE_KINDS = {
    "C4Context": set(_C4_PERSON_SYSTEM_KINDS),
    "C4Container": set(_C4_PERSON_SYSTEM_KINDS) | set(_C4_CONTAINER_KINDS),
    "C4Component": set(_C4_NODE_KINDS),
    "C4Dynamic": set(_C4_NODE_KINDS),
    "C4Deployment": set(_C4_PERSON_SYSTEM_KINDS) | set(_C4_CONTAINER_KINDS),
}
_C4_BOUNDARY_KINDS = {
    "enterprise_boundary": "Enterprise_Boundary",
    "system_boundary": "System_Boundary",
    "container_boundary": "Container_Boundary",
    "boundary": "Boundary",
    **_C4_DEPLOYMENT_KINDS,
}
_C4_TARGET_BOUNDARY_KINDS = {
    "C4Context": {"enterprise_boundary", "system_boundary", "boundary"},
    "C4Container": {"container_boundary", "boundary"},
    "C4Component": {"container_boundary", "boundary"},
    "C4Dynamic": {"container_boundary", "boundary"},
    "C4Deployment": set(_C4_DEPLOYMENT_KINDS),
}
_C4_REL_KINDS = {
    "rel": "Rel",
    "birel": "BiRel",
    "rel_up": "Rel_U",
    "rel_down": "Rel_D",
    "rel_left": "Rel_L",
    "rel_right": "Rel_R",
    "rel_back": "Rel_Back",
    "rel_index": "RelIndex",
}


def render_graph_c4(ir: dict) -> str:
    target = ir.get("target")
    if target not in _C4_TARGET_NODE_KINDS:
        raise IRError(
            f"graph/c4 requires ir['target'] to be one of {sorted(_C4_TARGET_NODE_KINDS)} (got {target!r})"
        )
    _require(ir, ("nodes", "edges"), f"graph/{target}")
    allowed_node_kinds = _C4_TARGET_NODE_KINDS[target]
    allowed_boundary_kinds = _C4_TARGET_BOUNDARY_KINDS[target]
    lines = [target]
    if ir.get("title"):
        lines.append(f"    title {ir['title']}")

    known: set[str] = set()
    by_group: dict[str | None, list[dict]] = {}
    for node in ir["nodes"]:
        _require(node, ("id", "label", "kind"), f"graph/{target} node")
        by_group.setdefault(node.get("group"), []).append(node)

    def emit_node(node: dict, indent: str) -> None:
        kind = node["kind"]
        macro = _C4_NODE_KINDS.get(kind)
        if macro is None or kind not in allowed_node_kinds:
            raise IRError(f"graph/{target} node {node['id']!r} has unknown/unsupported kind {kind!r}")
        node_id = _identifier(node["id"], f"graph/{target} node")
        known.add(node["id"])
        args = [node_id, _quote(node["label"])]
        technology = node.get("technology")
        description = node.get("description")
        if kind in _C4_TECH_KINDS:
            if technology is not None:
                args.append(_quote(technology))
            elif description is not None:
                raise IRError(
                    f"graph/{target} node {node['id']!r} has 'description' but no 'technology'; "
                    "this macro takes technology before description"
                )
        elif technology is not None:
            raise IRError(f"graph/{target} node {node['id']!r} kind {kind!r} does not accept 'technology'")
        if description is not None:
            args.append(_quote(description))
        lines.append(f'{indent}{macro}({", ".join(args)})')

    groups_by_id: dict[str, dict] = {}
    children_by_parent: dict[str | None, list[dict]] = {}
    for group in ir.get("groups", []):
        _require(group, ("id", "label", "kind"), f"graph/{target} group")
        if group["kind"] not in allowed_boundary_kinds:
            raise IRError(f"graph/{target} group {group['id']!r} has unknown/unsupported kind {group['kind']!r}")
        groups_by_id[group["id"]] = group
        children_by_parent.setdefault(group.get("group"), []).append(group)

    def emit_group(group: dict, indent: str) -> None:
        macro = _C4_BOUNDARY_KINDS[group["kind"]]
        group_id = _identifier(group["id"], f"graph/{target} group")
        args = [group_id, _quote(group["label"])]
        if group["kind"] == "boundary":
            if not group.get("boundary_type"):
                raise IRError(f"graph/{target} group {group['id']!r} of kind 'boundary' requires 'boundary_type'")
            args.append(_quote(group["boundary_type"]))
        elif group.get("boundary_type"):
            raise IRError(
                f"graph/{target} group {group['id']!r} kind {group['kind']!r} does not accept 'boundary_type'"
            )
        if group["kind"] in _C4_DEPLOYMENT_KINDS:
            technology = group.get("technology")
            description = group.get("description")
            if technology is not None:
                args.append(_quote(technology))
            elif description is not None:
                raise IRError(
                    f"graph/{target} group {group['id']!r} has 'description' but no 'technology'; "
                    "deployment nodes take type before description"
                )
            if description is not None:
                args.append(_quote(description))
        lines.append(f'{indent}{macro}({", ".join(args)}) {{')
        for node in by_group.pop(group["id"], []):
            emit_node(node, indent + "    ")
        for child in children_by_parent.pop(group["id"], []):
            emit_group(child, indent + "    ")
        lines.append(f"{indent}}}")

    for group in children_by_parent.pop(None, []):
        emit_group(group, "    ")
    leftover_groups = [g for g in children_by_parent if g is not None]
    if leftover_groups:
        raise IRError(f"graph/{target} groups reference undeclared parent group(s): {leftover_groups}")

    for node in by_group.pop(None, []):
        emit_node(node, "    ")
    leftover_nodes = [g for g in by_group if g is not None]
    if leftover_nodes:
        raise IRError(f"graph/{target} nodes reference undeclared group(s): {leftover_nodes}")

    for edge in ir["edges"]:
        _require(edge, ("from", "to", "label"), f"graph/{target} edge")
        kind = edge.get("kind", "rel")
        macro = _C4_REL_KINDS.get(kind)
        if macro is None:
            raise IRError(f"graph/{target} edge has unknown kind {kind!r}")
        if kind == "rel_index" and target != "C4Dynamic":
            raise IRError("graph/c4 edge kind 'rel_index' is only valid on C4Dynamic")
        if edge["from"] not in known or edge["to"] not in known:
            raise IRError(f"graph/{target} edge references undeclared node: {edge}")
        args = [edge["from"], edge["to"], _quote(edge["label"])]
        if kind == "rel_index":
            if edge.get("technology"):
                raise IRError("graph/C4Dynamic rel_index does not take 'technology'")
            index = edge.get("index")
            if isinstance(index, bool) or not isinstance(index, int):
                raise IRError(f"graph/C4Dynamic rel_index edge requires an integer 'index': {edge}")
            args.insert(0, str(index))
        if edge.get("technology"):
            args.append(_quote(edge["technology"]))
        lines.append(f'    {macro}({", ".join(args)})')
    return "\n".join(lines) + "\n"


_ARCH_BASE_ICONS = {"cloud", "database", "disk", "internet", "server"}
_ARCH_SIDES = {"T", "B", "L", "R"}
_ARCH_ARROWS = {
    "none": ("", ""),
    "forward": ("", ">"),
    "backward": ("<", ""),
    "both": ("<", ">"),
}


def _arch_icon(icon: str, where: str) -> str:
    if icon in _ARCH_BASE_ICONS or ":" in icon:
        return icon
    raise IRError(
        f"{where} icon {icon!r} is not a built-in architecture icon {sorted(_ARCH_BASE_ICONS)}; "
        "use 'pack:icon-name' to reference a registered icon pack"
    )


def render_graph_architecture(ir: dict) -> str:
    _require(ir, ("nodes", "edges"), "graph/architecture-beta")
    lines = ["architecture-beta"]
    known: set[str] = set()

    groups_by_id: dict[str, dict] = {}
    children_by_parent: dict[str | None, list[dict]] = {}
    for group in ir.get("groups", []):
        _require(group, ("id", "label"), "graph/architecture-beta group")
        groups_by_id[group["id"]] = group
        children_by_parent.setdefault(group.get("group"), []).append(group)

    def emit_group(group: dict) -> None:
        group_id = _identifier(group["id"], "graph/architecture-beta group")
        parent_id = group.get("group")
        parent = f" in {_identifier(parent_id, 'graph/architecture-beta group')}" if parent_id else ""
        icon = group.get("icon")
        where = f"graph/architecture-beta group {group['id']!r}"
        icon_part = f"({_arch_icon(icon, where)})" if icon else "()"
        lines.append(f'    group {group_id}{icon_part}[{_quote(group["label"])}]{parent}')
        for child in children_by_parent.pop(group["id"], []):
            emit_group(child)

    for group in children_by_parent.pop(None, []):
        emit_group(group)
    leftover_groups = [g for g in children_by_parent if g is not None]
    if leftover_groups:
        raise IRError(f"graph/architecture-beta groups reference undeclared parent group(s): {leftover_groups}")

    for node in ir["nodes"]:
        _require(node, ("id",), "graph/architecture-beta node")
        kind = node.get("kind", "service")
        node_id = _identifier(node["id"], "graph/architecture-beta node")
        group_ref = node.get("group")
        if group_ref is not None and group_ref not in groups_by_id:
            raise IRError(f"graph/architecture-beta node {node['id']!r} references undeclared group {group_ref!r}")
        parent = f" in {_identifier(group_ref, 'graph/architecture-beta node')}" if group_ref else ""
        if kind == "junction":
            if node.get("label") or node.get("icon"):
                raise IRError(f"graph/architecture-beta junction {node['id']!r} cannot have a label or icon")
            lines.append(f"    junction {node_id}{parent}")
        elif kind == "service":
            _require(node, ("label",), "graph/architecture-beta service node")
            icon = node.get("icon")
            where = f"graph/architecture-beta node {node['id']!r}"
            icon_part = f"({_arch_icon(icon, where)})" if icon else "()"
            lines.append(f'    service {node_id}{icon_part}[{_quote(node["label"])}]{parent}')
        else:
            raise IRError(f"graph/architecture-beta node {node['id']!r} has unknown kind {kind!r}")
        known.add(node["id"])

    for edge in ir["edges"]:
        _require(edge, ("from", "to", "from_side", "to_side"), "graph/architecture-beta edge")
        if edge["from"] not in known or edge["to"] not in known:
            raise IRError(f"graph/architecture-beta edge references undeclared node: {edge}")
        if edge["from_side"] not in _ARCH_SIDES or edge["to_side"] not in _ARCH_SIDES:
            raise IRError(f"graph/architecture-beta edge has unknown side(s): {edge}")
        arrow_kind = edge.get("arrow", "none")
        arrow = _ARCH_ARROWS.get(arrow_kind)
        if arrow is None:
            raise IRError(f"graph/architecture-beta edge has unknown arrow kind {arrow_kind!r}")
        left_arrow, right_arrow = arrow
        from_suffix = "{group}" if edge.get("from_group") else ""
        to_suffix = "{group}" if edge.get("to_group") else ""
        lines.append(
            f'    {edge["from"]}{from_suffix}:{edge["from_side"]} {left_arrow}--{right_arrow} '
            f'{edge["to_side"]}:{edge["to"]}{to_suffix}'
        )
    return "\n".join(lines) + "\n"


_ER_LEFT_CARDINALITY = {"zero_or_one": "|o", "exactly_one": "||", "zero_or_more": "}o", "one_or_more": "}|"}
_ER_RIGHT_CARDINALITY = {"zero_or_one": "o|", "exactly_one": "||", "zero_or_more": "o{", "one_or_more": "|{"}


def _er_name(raw: str) -> str:
    if re.fullmatch(r"[A-Za-z0-9_-]+", raw):
        return raw
    return _quote(raw)


def render_graph_er(ir: dict) -> str:
    _require(ir, ("nodes", "edges"), "graph/erDiagram")
    names: dict[str, str] = {}
    lines = ["erDiagram"]
    direction = _direction(ir, "graph/erDiagram", allowed=_CARDINAL_DIRECTIONS)
    if direction:
        lines.append(f"    direction {direction}")
    for node in ir["nodes"]:
        _require(node, ("id",), "graph/erDiagram node")
        bare = _er_name(node["id"])
        names[node["id"]] = bare
        header = f'{bare}[{_er_name(node["label"])}]' if node.get("label") else bare
        lines.append(f"    {header}")
        members = node.get("members", [])
        if members:
            lines.append(f"    {bare} {{")
            for member in members:
                lines.append(f"        {member}")
            lines.append("    }")
    for edge in ir["edges"]:
        _require(edge, ("from", "to", "label", "left", "right"), "graph/erDiagram edge")
        if edge["from"] not in names or edge["to"] not in names:
            raise IRError(f"graph/erDiagram edge references undeclared entity: {edge}")
        left = _ER_LEFT_CARDINALITY.get(edge["left"])
        right = _ER_RIGHT_CARDINALITY.get(edge["right"])
        if left is None:
            raise IRError(f"graph/erDiagram edge has unknown left cardinality {edge['left']!r}")
        if right is None:
            raise IRError(f"graph/erDiagram edge has unknown right cardinality {edge['right']!r}")
        identifying = edge.get("identifying", True)
        if not isinstance(identifying, bool):
            raise IRError(f"graph/erDiagram edge 'identifying' must be a boolean: {edge}")
        dash = "--" if identifying else ".."
        lines.append(
            f'    {names[edge["from"]]} {left}{dash}{right} {names[edge["to"]]} : {edge["label"]}'
        )
    return "\n".join(lines) + "\n"


_CLASS_ANNOTATIONS = {
    "interface": "Interface",
    "abstract": "Abstract",
    "service": "Service",
    "enumeration": "Enumeration",
}
_CLASS_EDGE_KINDS = {
    "inheritance": "<|--",
    "composition": "*--",
    "aggregation": "o--",
    "association": "-->",
    "link": "--",
    "dependency": "..>",
    "realization": "..|>",
    "link_dashed": "..",
}


def render_graph_class(ir: dict) -> str:
    _require(ir, ("nodes", "edges"), "graph/classDiagram")
    names: dict[str, str] = {}
    lines = ["classDiagram"]
    direction = _direction(ir, "graph/classDiagram", allowed=_CARDINAL_DIRECTIONS)
    if direction:
        lines.append(f"    direction {direction}")
    for node in ir["nodes"]:
        _require(node, ("id",), "graph/classDiagram node")
        class_id = _identifier(node["id"], "graph/classDiagram node")
        names[node["id"]] = class_id
        kind = node.get("kind")
        annotation = None
        if kind is not None:
            annotation = _CLASS_ANNOTATIONS.get(kind)
            if annotation is None:
                raise IRError(f"graph/classDiagram node {node['id']!r} has unknown kind {kind!r}")
        header = f'{class_id}[{_quote(node["label"])}]' if node.get("label") else class_id
        members = node.get("members", [])
        if annotation or members:
            lines.append(f"    class {header} {{")
            if annotation:
                lines.append(f"        <<{annotation}>>")
            for member in members:
                lines.append(f"        {member}")
            lines.append("    }")
        else:
            lines.append(f"    class {header}")
    for edge in ir["edges"]:
        _require(edge, ("from", "to", "kind"), "graph/classDiagram edge")
        arrow = _CLASS_EDGE_KINDS.get(edge["kind"])
        if arrow is None:
            raise IRError(f"graph/classDiagram edge has unknown kind {edge['kind']!r}")
        if edge["from"] not in names or edge["to"] not in names:
            raise IRError(f"graph/classDiagram edge references undeclared class: {edge}")
        tokens = [names[edge["from"]]]
        if edge.get("from_card"):
            tokens.append(f'"{edge["from_card"]}"')
        tokens.append(arrow)
        if edge.get("to_card"):
            tokens.append(f'"{edge["to_card"]}"')
        tokens.append(names[edge["to"]])
        label = f' : {edge["label"]}' if edge.get("label") else ""
        lines.append(f"    {' '.join(tokens)}{label}")
    return "\n".join(lines) + "\n"


# --- timeline family ---------------------------------------------------------------------

_GANTT_TAGS = {"active", "done", "crit", "milestone"}


def _gantt_tags(bar: dict) -> list[str]:
    tags = bar.get("tags", [])
    unknown = [tag for tag in tags if tag not in _GANTT_TAGS]
    if unknown:
        raise IRError(f"timeline/gantt bar {bar['id']!r} has unknown tag(s) {unknown!r}")
    if len(set(tags)) != len(tags):
        raise IRError(f"timeline/gantt bar {bar['id']!r} repeats a tag: {tags!r}")
    if {"active", "done"}.issubset(tags):
        raise IRError(f"timeline/gantt bar {bar['id']!r} cannot be both active and done")
    if "milestone" in tags and ({"active", "done"} & set(tags)):
        raise IRError(f"timeline/gantt bar {bar['id']!r} cannot combine milestone with active/done")
    return list(tags)


def render_timeline_gantt(ir: dict) -> str:
    _require(ir, ("dateFormat", "sections"), "timeline/gantt")
    lines = ["gantt"]
    if ir.get("title"):
        lines.append(f"    title {ir['title']}")
    lines.append(f"    dateFormat {ir['dateFormat']}")
    if ir.get("axisFormat"):
        lines.append(f"    axisFormat {ir['axisFormat']}")
    for exclude in ir.get("excludes", []):
        lines.append(f"    excludes {exclude}")
    for section in ir["sections"]:
        _require(section, ("name", "bars"), "timeline/gantt section")
        lines.append(f"    section {section['name']}")
        for bar in section["bars"]:
            _require(bar, ("id", "label", "start", "end"), "timeline/gantt bar")
            label = str(bar["label"])
            if not label.strip() or ":" in label:
                raise IRError(
                    f"timeline/gantt bar {bar['id']!r} label must be non-empty and must not contain ':'; "
                    "Mermaid treats the first colon as the start of task metadata "
                    "(a colon in the title crashes the renderer with TypeError)"
                )
            bar_id = _identifier(bar["id"], "timeline/gantt bar")
            fields = _gantt_tags(bar) + [bar_id, bar["start"], bar["end"]]
            lines.append(f"    {label} :{', '.join(fields)}")
    return "\n".join(lines) + "\n"


# --- state-machine family -----------------------------------------------------------------

_STATE_PSEUDO_KINDS = {"choice", "fork", "join"}


def _emit_state_machine_body(ir: dict, lines: list[str], indent: str) -> None:
    """Emit one (possibly nested/composite) state machine's states/transitions/notes."""
    for state in ir.get("states", []):
        _require(state, ("id",), "state-machine state")
        kind = state.get("kind")
        if kind is not None and kind not in _STATE_PSEUDO_KINDS:
            raise IRError(f"state-machine state {state['id']!r} has unknown kind {kind!r}")
        if kind in _STATE_PSEUDO_KINDS:
            if any(key in state for key in ("states", "transitions", "initial", "final")):
                raise IRError(
                    f"state-machine pseudostate {state['id']!r} cannot contain a nested body"
                )
            lines.append(f'{indent}state {state["id"]} <<{kind}>>')
        elif "states" in state or "transitions" in state:
            lines.append(f'{indent}state {state["id"]} {{')
            _emit_state_machine_body(state, lines, indent + "    ")
            lines.append(f"{indent}}}")
        elif state.get("label"):
            lines.append(f'{indent}{state["id"]} : {state["label"]}')
    if ir.get("initial"):
        lines.append(f"{indent}[*] --> {ir['initial']}")
    for transition in ir.get("transitions", []):
        _require(transition, ("from", "to"), "state-machine transition")
        label = f' : {transition["label"]}' if transition.get("label") else ""
        lines.append(f'{indent}{transition["from"]} --> {transition["to"]}{label}')
    for final in ir.get("final", []):
        lines.append(f"{indent}{final} --> [*]")
    for note in ir.get("notes", []):
        _require(note, ("state", "side", "text"), "state-machine note")
        if note["side"] not in ("right", "left"):
            raise IRError(f"state-machine note on {note['state']!r} has unknown side {note['side']!r}")
        lines.append(f'{indent}note {note["side"]} of {note["state"]} : {note["text"]}')


def render_state_machine(ir: dict) -> str:
    _require(ir, ("transitions",), "state-machine")
    lines = ["stateDiagram-v2"]
    direction = _direction(ir, "state-machine", allowed=_CARDINAL_DIRECTIONS)
    if direction:
        lines.append(f"    direction {direction}")
    _emit_state_machine_body(ir, lines, "    ")
    return "\n".join(lines) + "\n"


# --- sequence family -----------------------------------------------------------------------

_SEQUENCE_ARROWS = {
    "sync": "->>",
    "async": "-)",
    "reply": "-->>",
    "cross": "-x",
}
_SEQUENCE_SINGLE_BODY_BLOCKS = {"loop", "opt", "break"}
_SEQUENCE_BRANCHED_BLOCKS = {
    "alt": ("alt", "else"),
    "par": ("par", "and"),
    "critical": ("critical", "option"),
}
_SEQUENCE_BREAK = "<br/>"


def _sequence_text(value: Any) -> str:
    return str(value).replace("\r\n", _SEQUENCE_BREAK).replace("\n", _SEQUENCE_BREAK)


def _emit_sequence_steps(steps: list[dict], known: set[str], lines: list[str], indent: str) -> None:
    for step in steps:
        step_type = step.get("type", "message")
        if step_type == "message":
            _require(step, ("from", "to", "label"), "sequence message")
            if step["from"] not in known or step["to"] not in known:
                raise IRError(f"sequence message references an undeclared actor: {step}")
            arrow = _SEQUENCE_ARROWS.get(step.get("kind", "sync"))
            if arrow is None:
                raise IRError(f"sequence message has unknown kind {step['kind']!r}")
            lines.append(f'{indent}{step["from"]}{arrow}{step["to"]}: {_sequence_text(step["label"])}')
        elif step_type in ("activate", "deactivate"):
            _require(step, ("actor",), f"sequence {step_type}")
            if step["actor"] not in known:
                raise IRError(f"sequence {step_type} references an undeclared actor: {step}")
            lines.append(f'{indent}{step_type} {step["actor"]}')
        elif step_type == "note":
            _require(step, ("position", "actors", "text"), "sequence note")
            if step["position"] not in ("right_of", "left_of", "over"):
                raise IRError(f"sequence note has unknown position {step['position']!r}")
            unknown_actors = [actor for actor in step["actors"] if actor not in known]
            if unknown_actors:
                raise IRError(f"sequence note references undeclared actor(s): {unknown_actors}")
            side = step["position"].replace("_", " ")
            targets = ",".join(step["actors"])
            lines.append(f'{indent}Note {side} {targets}: {_sequence_text(step["text"])}')
        elif step_type in _SEQUENCE_SINGLE_BODY_BLOCKS:
            _require(step, ("label", "steps"), f"sequence {step_type} block")
            lines.append(f'{indent}{step_type} {step["label"]}')
            _emit_sequence_steps(step["steps"], known, lines, indent + "    ")
            lines.append(f"{indent}end")
        elif step_type == "rect":
            _require(step, ("color", "steps"), "sequence rect block")
            lines.append(f'{indent}rect {step["color"]}')
            _emit_sequence_steps(step["steps"], known, lines, indent + "    ")
            lines.append(f"{indent}end")
        elif step_type in _SEQUENCE_BRANCHED_BLOCKS:
            _require(step, ("branches",), f"sequence {step_type} block")
            if not step["branches"]:
                raise IRError(f"sequence {step_type} block must declare at least one branch")
            first_keyword, rest_keyword = _SEQUENCE_BRANCHED_BLOCKS[step_type]
            for index, branch in enumerate(step["branches"]):
                _require(branch, ("steps",), f"sequence {step_type} branch")
                keyword = first_keyword if index == 0 else rest_keyword
                label = f' {branch["label"]}' if branch.get("label") else ""
                lines.append(f"{indent}{keyword}{label}")
                _emit_sequence_steps(branch["steps"], known, lines, indent + "    ")
            lines.append(f"{indent}end")
        else:
            raise IRError(f"sequence step has unknown type {step_type!r}")


def render_sequence(ir: dict) -> str:
    _require(ir, ("actors", "steps"), "sequence")
    lines = ["sequenceDiagram"]
    known: set[str] = set()
    for actor in ir["actors"]:
        _require(actor, ("id",), "sequence actor")
        keyword = "actor" if actor.get("kind") == "actor" else "participant"
        lines.append(f'    {keyword} {actor["id"]}')
        known.add(actor["id"])
    _emit_sequence_steps(ir["steps"], known, lines, "    ")
    return "\n".join(lines) + "\n"


# --- requirement-links family --------------------------------------------------------------

_REQUIREMENT_TYPES = {
    "requirement", "functionalRequirement", "interfaceRequirement",
    "performanceRequirement", "physicalRequirement", "designConstraint",
}
_REQUIREMENT_RISKS = {"low", "medium", "high"}
_REQUIREMENT_METHODS = {"analysis", "inspection", "test", "demonstration"}
_REQUIREMENT_LINK_KINDS = {"contains", "copies", "derives", "satisfies", "verifies", "refines", "traces"}


def render_requirement_links(ir: dict) -> str:
    _require(ir, ("requirements", "elements", "links"), "requirement-links")
    lines = ["requirementDiagram"]
    direction = _direction(ir, "requirement-links", allowed=_CARDINAL_DIRECTIONS)
    if direction:
        lines.append(f"    direction {direction}")
    known = set()
    for requirement in ir["requirements"]:
        _require(requirement, ("id", "text"), "requirement-links requirement")
        rtype = requirement.get("type", "requirement")
        if rtype not in _REQUIREMENT_TYPES:
            raise IRError(f"requirement {requirement['id']!r} has unknown type {rtype!r}")
        risk = requirement.get("risk")
        if risk is not None and risk not in _REQUIREMENT_RISKS:
            raise IRError(f"requirement {requirement['id']!r} has unknown risk {risk!r}")
        method = requirement.get("verify_method")
        if method is not None and method not in _REQUIREMENT_METHODS:
            raise IRError(f"requirement {requirement['id']!r} has unknown verify_method {method!r}")
        req_id = _identifier(requirement["id"], "requirement-links requirement")
        lines.append(f"    {rtype} {req_id} {{")
        lines.append(f"        id: {requirement['id']}")
        lines.append(f"        text: {_quote(requirement['text'])}")
        if risk:
            lines.append(f"        risk: {risk}")
        if method:
            lines.append(f"        verifymethod: {method}")
        lines.append("    }")
        known.add(requirement["id"])
    for element in ir["elements"]:
        _require(element, ("id", "type"), "requirement-links element")
        elem_id = _identifier(element["id"], "requirement-links element")
        lines.append(f"    element {elem_id} {{")
        lines.append(f"        type: {element['type']}")
        if element.get("docref"):
            lines.append(f"        docref: {element['docref']}")
        lines.append("    }")
        known.add(element["id"])
    for link in ir["links"]:
        _require(link, ("from", "to", "kind"), "requirement-links link")
        if link["kind"] not in _REQUIREMENT_LINK_KINDS:
            raise IRError(f"requirement-links link has unknown kind {link['kind']!r}")
        if link["from"] not in known or link["to"] not in known:
            raise IRError(f"requirement-links link references an undeclared requirement/element: {link}")
        lines.append(f'    {link["from"]} - {link["kind"]} -> {link["to"]}')
    return "\n".join(lines) + "\n"


# --- remaining Mermaid types ---------------------------------------------------------------

def _positive_number(value: Any, where: str) -> str:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise IRError(f"{where} must be a number, got {value!r}")
    return str(value)


def _no_colon(text: str, where: str) -> str:
    if not str(text).strip() or ":" in str(text) or "\n" in str(text):
        raise IRError(f"{where} must be a single line and must not contain ':'")
    return str(text)


def render_graph_swimlane(ir: dict) -> str:
    direction = _direction(ir, "graph/swimlane-beta", allowed=_FLOWCHART_DIRECTIONS, default="TD")
    cloned = dict(ir)
    cloned["direction"] = direction
    body = render_graph_flowchart(cloned)
    return f"swimlane-beta {direction}\n" + body.split("\n", 1)[1]


_AGENT_SHAPES = {"task", "tool", "input", "decision", "refdoc", "action"}
_AGENT_ARROWS = {"sequence": "-->", "reference": "-.-", "failure": "--x"}


def render_graph_agentflow(ir: dict) -> str:
    _require(ir, ("nodes", "edges"), "graph/agentflow-beta")
    direction = _direction(ir, "graph/agentflow-beta", allowed=_FLOWCHART_DIRECTIONS, default="TD")
    lines = [f"agentflow-beta {direction}"]
    known: set[str] = set()
    by_group: dict[str | None, list[dict]] = {}
    for node in ir["nodes"]:
        _require(node, ("id", "label"), "graph/agentflow-beta node")
        by_group.setdefault(node.get("group"), []).append(node)
    children: dict[str | None, list[dict]] = {}
    for group in ir.get("groups", []):
        _require(group, ("id", "label"), "graph/agentflow-beta group")
        children.setdefault(group.get("group"), []).append(group)

    def emit_node(node: dict, indent: str) -> None:
        node_id = _identifier(node["id"], "graph/agentflow-beta node")
        kind = node.get("kind")
        shape = ""
        if kind is not None:
            if kind not in _AGENT_SHAPES:
                raise IRError(f"graph/agentflow-beta node {node['id']!r} has unknown kind {kind!r}")
            shape = f"@{{ shape: {kind} }}"
        lines.append(f'{indent}{node_id}[{_quote(node["label"])}]{shape}')
        known.add(node["id"])

    def emit_flow(group: dict, indent: str) -> None:
        group_id = _identifier(group["id"], "graph/agentflow-beta group")
        lines.append(f'{indent}flow {group_id}[{_quote(group["label"])}]')
        known.add(group["id"])
        for node in by_group.pop(group["id"], []):
            emit_node(node, indent + "    ")
        for child in children.pop(group["id"], []):
            emit_flow(child, indent + "    ")
        lines.append(f"{indent}end")

    for group in children.pop(None, []):
        emit_flow(group, "    ")
    leftover_groups = [group for group in children if group is not None]
    if leftover_groups:
        raise IRError(f"graph/agentflow-beta groups reference undeclared parent group(s): {leftover_groups}")
    for node in by_group.pop(None, []):
        emit_node(node, "    ")
    leftover = [group for group in by_group if group is not None]
    if leftover:
        raise IRError(f"graph/agentflow-beta nodes reference undeclared group(s): {leftover}")
    for edge in ir["edges"]:
        _require(edge, ("from", "to"), "graph/agentflow-beta edge")
        if edge["from"] not in known or edge["to"] not in known:
            raise IRError(f"graph/agentflow-beta edge references undeclared node: {edge}")
        arrow = _AGENT_ARROWS.get(edge.get("kind", "sequence"))
        if arrow is None:
            raise IRError(f"graph/agentflow-beta edge has unknown kind {edge.get('kind')!r}")
        label = f'|{_quote(edge["label"])}|' if edge.get("label") else ""
        lines.append(f'    {edge["from"]} {arrow}{label} {edge["to"]}')
    return "\n".join(lines) + "\n"


def render_graph_wardley(ir: dict) -> str:
    _require(ir, ("nodes", "edges"), "graph/wardley-beta")
    lines = ["wardley-beta"]
    if ir.get("title"):
        lines.append(f"    title {_no_colon(ir['title'], 'graph/wardley-beta title')}")
    names: dict[str, str] = {}
    for node in ir["nodes"]:
        _require(node, ("id", "kind", "visibility", "evolution"), "graph/wardley-beta node")
        if node["kind"] not in ("anchor", "component"):
            raise IRError(f"graph/wardley-beta node {node['id']!r} has unknown kind {node['kind']!r}")
        visibility = float(_positive_number(node["visibility"], "wardley visibility"))
        evolution = float(_positive_number(node["evolution"], "wardley evolution"))
        if not 0 <= visibility <= 1 or not 0 <= evolution <= 1:
            raise IRError(f"graph/wardley-beta node {node['id']!r} coordinates must be between 0 and 1")
        label = _quote(node["id"]) if not re.fullmatch(r"[A-Za-z0-9_]+", str(node["id"])) else str(node["id"])
        names[node["id"]] = label
        lines.append(f"    {node['kind']} {label} [{visibility}, {evolution}]")
    for edge in ir["edges"]:
        _require(edge, ("from", "to"), "graph/wardley-beta edge")
        if edge["from"] not in names or edge["to"] not in names:
            raise IRError(f"graph/wardley-beta edge references undeclared node: {edge}")
        lines.append(f"    {names[edge['from']]} -> {names[edge['to']]}")
    return "\n".join(lines) + "\n"


def render_timeline_events(ir: dict) -> str:
    if not ir.get("sections") and not ir.get("events"):
        raise IRError("timeline/timeline requires 'sections' or 'events'")
    direction = ir.get("direction")
    if direction is not None and direction not in ("LR", "TD"):
        raise IRError(f"timeline/timeline has unknown direction {direction!r}; use LR or TD")
    lines = [f"timeline {direction}" if direction else "timeline"]
    if ir.get("title"):
        lines.append(f"    title {_no_colon(ir['title'], 'timeline/timeline title')}")

    def emit_event(event: dict, indent: str) -> None:
        _require(event, ("period", "text"), "timeline/timeline event")
        period = _no_colon(event["period"], "timeline/timeline period")
        texts = event["text"] if isinstance(event["text"], list) else [event["text"]]
        if not texts:
            raise IRError("timeline/timeline event requires at least one text value")
        rendered = [_no_colon(text, "timeline/timeline event text") for text in texts]
        lines.append(f"{indent}{period} : " + " : ".join(rendered))

    if ir.get("sections"):
        for section in ir["sections"]:
            _require(section, ("name", "events"), "timeline/timeline section")
            lines.append(f"    section {_no_colon(section['name'], 'timeline/timeline section')}")
            for event in section["events"]:
                emit_event(event, "        ")
    else:
        for event in ir["events"]:
            emit_event(event, "    ")
    return "\n".join(lines) + "\n"


def render_chart_pie(ir: dict) -> str:
    _require(ir, ("slices",), "chart/pie")
    if not ir["slices"]:
        raise IRError("chart/pie requires at least one slice")
    header = "pie showData" if ir.get("showData") else "pie"
    lines = [header]
    if ir.get("title"):
        lines.append(f"    title {_no_colon(ir['title'], 'chart/pie title')}")
    for slice_ in ir["slices"]:
        _require(slice_, ("label", "value"), "chart/pie slice")
        label = _no_colon(slice_["label"], "chart/pie label")
        value = float(_positive_number(slice_["value"], "chart/pie value"))
        if value <= 0:
            raise IRError(f"chart/pie slice {label!r} value must be greater than 0")
        lines.append(f"    {_quote(label)} : {value}")
    return "\n".join(lines) + "\n"


def _xy_token(value: Any, where: str) -> str:
    text = str(value)
    if "," in text or "\n" in text:
        raise IRError(f"{where} must not contain commas or newlines")
    if re.fullmatch(r"[A-Za-z0-9_]+", text):
        return text
    return _quote(text)


def render_chart_xy(ir: dict) -> str:
    _require(ir, ("xAxis",), "chart/xychart")
    if not ir.get("bar") and not ir.get("line"):
        raise IRError("chart/xychart requires 'bar' and/or 'line'")
    orientation = ir.get("orientation", "vertical")
    if orientation not in ("vertical", "horizontal"):
        raise IRError(f"chart/xychart has unknown orientation {orientation!r}")
    header = "xychart horizontal" if orientation == "horizontal" else "xychart"
    lines = [header]
    if ir.get("title"):
        lines.append(f"    title {_quote(ir['title'])}")
    categories = [_xy_token(item, "chart/xychart x category") for item in ir["xAxis"]]
    if not categories:
        raise IRError("chart/xychart xAxis must not be empty")
    lines.append(f"    x-axis [{', '.join(categories)}]")
    y_axis = ir.get("yAxis")
    if y_axis is not None:
        _require(y_axis, ("label", "min", "max"), "chart/xychart yAxis")
        low = float(_positive_number(y_axis["min"], "chart/xychart y min"))
        high = float(_positive_number(y_axis["max"], "chart/xychart y max"))
        if low >= high:
            raise IRError("chart/xychart yAxis min must be less than max")
        lines.append(f"    y-axis {_quote(y_axis['label'])} {low} --> {high}")
    for series_name in ("bar", "line"):
        series = ir.get(series_name)
        if series is None:
            continue
        if len(series) != len(categories):
            raise IRError(f"chart/xychart {series_name} length must match xAxis")
        numbers = [_positive_number(item, f"chart/xychart {series_name}") for item in series]
        lines.append(f"    {series_name} [{', '.join(numbers)}]")
    return "\n".join(lines) + "\n"


def _sankey_cell(value: str) -> str:
    if "," in value or "\n" in value:
        return "'" + value.replace("'", "") + "'"
    return value


def render_chart_sankey(ir: dict) -> str:
    _require(ir, ("links",), "chart/sankey")
    if not ir["links"]:
        raise IRError("chart/sankey requires at least one link")
    lines = ["sankey"]
    for link in ir["links"]:
        _require(link, ("from", "to", "value"), "chart/sankey link")
        amount = float(_positive_number(link["value"], "chart/sankey value"))
        if amount < 0:
            raise IRError("chart/sankey value must not be negative")
        lines.append(f"{_sankey_cell(str(link['from']))},{_sankey_cell(str(link['to']))},{amount}")
    return "\n".join(lines) + "\n"


def render_chart_quadrant(ir: dict) -> str:
    _require(ir, ("xAxis", "yAxis", "quadrants", "points"), "chart/quadrantChart")
    _require(ir["xAxis"], ("left", "right"), "chart/quadrantChart xAxis")
    _require(ir["yAxis"], ("bottom", "top"), "chart/quadrantChart yAxis")
    lines = ["quadrantChart"]
    if ir.get("title"):
        lines.append(f"    title {_no_colon(ir['title'], 'chart/quadrantChart title')}")
    lines.append(
        f"    x-axis {_no_colon(ir['xAxis']['left'], 'quadrant x')} --> {_no_colon(ir['xAxis']['right'], 'quadrant x')}"
    )
    lines.append(
        f"    y-axis {_no_colon(ir['yAxis']['bottom'], 'quadrant y')} --> {_no_colon(ir['yAxis']['top'], 'quadrant y')}"
    )
    for index in ("1", "2", "3", "4"):
        if index not in ir["quadrants"] and int(index) not in ir["quadrants"]:
            raise IRError(f"chart/quadrantChart missing quadrant {index}")
        label = ir["quadrants"].get(index, ir["quadrants"].get(int(index)))
        lines.append(f"    quadrant-{index} {_no_colon(label, 'quadrant label')}")
    for point in ir["points"]:
        _require(point, ("label", "x", "y"), "chart/quadrantChart point")
        x_value = float(_positive_number(point["x"], "quadrant x"))
        y_value = float(_positive_number(point["y"], "quadrant y"))
        if not 0 <= x_value <= 1 or not 0 <= y_value <= 1:
            raise IRError(f"chart/quadrantChart point {point['label']!r} must be inside 0..1")
        lines.append(f"    {_quote(_no_colon(point['label'], 'quadrant point'))}: [{x_value}, {y_value}]")
    return "\n".join(lines) + "\n"


def render_chart_radar(ir: dict) -> str:
    _require(ir, ("axes", "curves"), "chart/radar-beta")
    if not ir["axes"] or not ir["curves"]:
        raise IRError("chart/radar-beta requires axes and curves")
    lines = ["radar-beta"]
    if ir.get("title"):
        lines.append(f"    title {_quote(ir['title'])}")
    axis_bits = []
    for axis in ir["axes"]:
        _require(axis, ("id", "label"), "chart/radar-beta axis")
        axis_id = _identifier(axis["id"], "chart/radar-beta axis")
        axis_bits.append(f'{axis_id}[{_quote(axis["label"])}]')
    lines.append("    axis " + ", ".join(axis_bits))
    for curve in ir["curves"]:
        _require(curve, ("id", "label", "values"), "chart/radar-beta curve")
        if len(curve["values"]) != len(ir["axes"]):
            raise IRError(f"chart/radar-beta curve {curve['id']!r} value count must match axes")
        curve_id = _identifier(curve["id"], "chart/radar-beta curve")
        numbers = [_positive_number(item, "chart/radar-beta value") for item in curve["values"]]
        lines.append(f"    curve {curve_id}[{_quote(curve['label'])}]{{{', '.join(numbers)}}}")
    if ir.get("max") is not None:
        lines.append(f"    max {_positive_number(ir['max'], 'chart/radar-beta max')}")
    if ir.get("min") is not None:
        lines.append(f"    min {_positive_number(ir['min'], 'chart/radar-beta min')}")
    return "\n".join(lines) + "\n"


def _emit_treemap(node: dict, lines: list[str], depth: int) -> None:
    _require(node, ("label",), "chart/treemap node")
    indent = "    " * (depth + 1)
    children = node.get("children", [])
    if children:
        lines.append(f"{indent}{_quote(node['label'])}")
        for child in children:
            _emit_treemap(child, lines, depth + 1)
        return
    _require(node, ("value",), "chart/treemap leaf")
    value = float(_positive_number(node["value"], "chart/treemap value"))
    if value < 0:
        raise IRError("chart/treemap leaf value must not be negative")
    lines.append(f"{indent}{_quote(node['label'])}: {value}")


def render_chart_treemap(ir: dict) -> str:
    _require(ir, ("nodes",), "chart/treemap")
    if not ir["nodes"]:
        raise IRError("chart/treemap requires nodes")
    lines = ["treemap-beta"]
    for node in ir["nodes"]:
        _emit_treemap(node, lines, 0)
    return "\n".join(lines) + "\n"


def render_chart_venn(ir: dict) -> str:
    _require(ir, ("sets",), "chart/venn")
    lines = ["venn-beta"]
    if ir.get("title"):
        lines.append(f"    title {_quote(ir['title'])}")
    known: set[str] = set()
    for item in ir["sets"]:
        if isinstance(item, str):
            set_id, label = item, None
        else:
            _require(item, ("id",), "chart/venn set")
            set_id, label = item["id"], item.get("label")
        if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", str(set_id)):
            raise IRError(f"chart/venn set id {set_id!r} must be a bare identifier")
        known.add(str(set_id))
        suffix = f"[{_quote(label)}]" if label else ""
        lines.append(f"    set {set_id}{suffix}")
    for union in ir.get("unions", []):
        _require(union, ("sets", "label"), "chart/venn union")
        if len(union["sets"]) < 2:
            raise IRError("chart/venn union requires at least two sets")
        unknown = [name for name in union["sets"] if name not in known]
        if unknown:
            raise IRError(f"chart/venn union references undeclared set(s): {unknown}")
        lines.append(f"    union {','.join(union['sets'])}[{_quote(union['label'])}]")
    return "\n".join(lines) + "\n"


def render_packet(ir: dict) -> str:
    _require(ir, ("fields",), "packet/packet")
    if not ir["fields"]:
        raise IRError("packet/packet requires fields")
    lines = ["packet"]
    cursor = 0
    for field in ir["fields"]:
        _require(field, ("label",), "packet/packet field")
        label = _quote(field["label"])
        if "bits" in field:
            bits = field["bits"]
            if isinstance(bits, bool) or not isinstance(bits, int) or bits < 1:
                raise IRError(f"packet/packet field {field['label']!r} bits must be a positive integer")
            lines.append(f"    +{bits}: {label}")
            cursor += bits
            continue
        _require(field, ("start", "end"), "packet/packet field")
        start, end = field["start"], field["end"]
        if isinstance(start, bool) or isinstance(end, bool) or not isinstance(start, int) or not isinstance(end, int):
            raise IRError(f"packet/packet field {field['label']!r} start and end must be integers")
        if start < 0 or end < start:
            raise IRError(f"packet/packet field {field['label']!r} has an invalid range")
        if start != cursor:
            raise IRError(
                f"packet/packet field {field['label']!r} starts at {start}, expected {cursor} for a contiguous layout"
            )
        lines.append(f"    {start}: {label}" if start == end else f"    {start}-{end}: {label}")
        cursor = end + 1
    return "\n".join(lines) + "\n"


_KANBAN_PRIORITIES = {"Very High", "High", "Low", "Very Low"}


def render_kanban(ir: dict) -> str:
    _require(ir, ("columns",), "board/kanban")
    if not ir["columns"]:
        raise IRError("board/kanban requires columns")
    lines = ["kanban"]
    seen: set[str] = set()
    for column in ir["columns"]:
        _require(column, ("id", "label", "cards"), "board/kanban column")
        column_id = _identifier(column["id"], "board/kanban column")
        if "]" in str(column["label"]):
            raise IRError(f"board/kanban column {column_id!r} label must not contain ']'")
        lines.append(f"    {column_id}[{column['label']}]")
        seen.add(column_id)
        for card in column["cards"]:
            _require(card, ("id", "label"), "board/kanban card")
            card_id = _identifier(card["id"], "board/kanban card")
            if card_id in seen:
                raise IRError(f"board/kanban duplicate id {card_id!r}")
            seen.add(card_id)
            if "]" in str(card["label"]):
                raise IRError(f"board/kanban card {card_id!r} label must not contain ']'")
            meta = []
            for key in ("assigned", "ticket"):
                if card.get(key):
                    meta.append(f"{key}: {_quote(card[key])}")
            if card.get("priority"):
                if card["priority"] not in _KANBAN_PRIORITIES:
                    raise IRError(f"board/kanban card {card_id!r} has unknown priority {card['priority']!r}")
                meta.append(f"priority: {_quote(card['priority'])}")
            suffix = f"@{{ {', '.join(meta)} }}" if meta else ""
            lines.append(f"        {card_id}[{card['label']}]{suffix}")
    return "\n".join(lines) + "\n"


def render_journey(ir: dict) -> str:
    _require(ir, ("sections",), "journey/journey")
    lines = ["journey"]
    if ir.get("title"):
        lines.append(f"    title {_no_colon(ir['title'], 'journey/journey title')}")
    for section in ir["sections"]:
        _require(section, ("name", "tasks"), "journey/journey section")
        lines.append(f"    section {_no_colon(section['name'], 'journey/journey section')}")
        for task in section["tasks"]:
            _require(task, ("name", "score", "actors"), "journey/journey task")
            score = task["score"]
            if isinstance(score, bool) or not isinstance(score, int) or not 1 <= score <= 5:
                raise IRError(f"journey/journey task {task['name']!r} score must be an integer from 1 to 5")
            if not task["actors"]:
                raise IRError(f"journey/journey task {task['name']!r} requires actors")
            actors = [_no_colon(actor, "journey actor") for actor in task["actors"]]
            lines.append(
                f"        {_no_colon(task['name'], 'journey task')}: {score}: {', '.join(actors)}"
            )
    return "\n".join(lines) + "\n"


def render_git(ir: dict) -> str:
    _require(ir, ("ops",), "git/gitGraph")
    if not ir["ops"]:
        raise IRError("git/gitGraph requires ops")
    lines: list[str] = []
    if ir.get("title"):
        lines.extend(["---", f"title: {_quote(ir['title'])}", "---"])
    lines.append("gitGraph")
    for op in ir["ops"]:
        _require(op, ("type",), "git/gitGraph op")
        kind = op["type"]
        if kind == "commit":
            parts = ["    commit"]
            if op.get("id"):
                parts.append(f"id: {_quote(_identifier(op['id'], 'git/gitGraph commit'))}")
            if op.get("tag"):
                parts.append(f"tag: {_quote(_no_colon(op['tag'], 'git/gitGraph tag'))}")
            lines.append(" ".join(parts))
        elif kind == "branch":
            _require(op, ("name",), "git/gitGraph branch")
            lines.append(f"    branch {_identifier(op['name'], 'git/gitGraph branch')}")
        elif kind in ("checkout", "switch"):
            _require(op, ("name",), "git/gitGraph checkout")
            lines.append(f"    checkout {_identifier(op['name'], 'git/gitGraph checkout')}")
        elif kind == "merge":
            _require(op, ("name",), "git/gitGraph merge")
            lines.append(f"    merge {_identifier(op['name'], 'git/gitGraph merge')}")
        else:
            raise IRError(f"git/gitGraph op has unknown type {kind!r}")
    return "\n".join(lines) + "\n"


def _tree_token(name: str) -> str:
    if re.fullmatch(r"[A-Za-z0-9_./-]+", name):
        return name
    return _quote(name)


def _emit_treeview(node: dict, lines: list[str], depth: int) -> None:
    _require(node, ("name",), "tree/treeView node")
    children = node.get("children", [])
    name = str(node["name"])
    if children and not name.endswith("/"):
        name += "/"
    lines.append(f"{'    ' * (depth + 1)}{_tree_token(name)}")
    for child in children:
        _emit_treeview(child, lines, depth + 1)


def render_treeview(ir: dict) -> str:
    _require(ir, ("nodes",), "tree/treeView-beta")
    if not ir["nodes"]:
        raise IRError("tree/treeView-beta requires nodes")
    lines = ["treeView-beta"]
    for node in ir["nodes"]:
        _emit_treeview(node, lines, 0)
    return "\n".join(lines) + "\n"


def _emit_ishikawa(node: dict, lines: list[str], depth: int) -> None:
    _require(node, ("label",), "tree/ishikawa cause")
    lines.append(f"{'    ' * depth}{_no_colon(node['label'], 'tree/ishikawa label')}")
    for child in node.get("children", []):
        _emit_ishikawa(child, lines, depth + 1)


def render_ishikawa(ir: dict) -> str:
    _require(ir, ("effect", "causes"), "tree/ishikawa-beta")
    lines = ["ishikawa-beta", f"    {_no_colon(ir['effect'], 'tree/ishikawa effect')}"]
    for cause in ir["causes"]:
        _emit_ishikawa(cause, lines, 1)
    return "\n".join(lines) + "\n"


_CYNEFIN_DOMAINS = ("clear", "complicated", "complex", "chaotic", "confusion")


def render_cynefin(ir: dict) -> str:
    _require(ir, ("domains",), "cynefin/cynefin-beta")
    lines = ["cynefin-beta"]
    if ir.get("title"):
        lines.append(f"    title {_no_colon(ir['title'], 'cynefin/cynefin-beta title')}")
    domains = ir["domains"]
    for name in domains:
        if name not in _CYNEFIN_DOMAINS:
            raise IRError(f"cynefin/cynefin-beta has unknown domain {name!r}")
    for name in _CYNEFIN_DOMAINS:
        items = domains.get(name)
        if not items:
            continue
        lines.append(f"    {name}")
        for item in items:
            lines.append(f"        {_quote(_no_colon(item, 'cynefin item'))}")
    for transition in ir.get("transitions", []):
        _require(transition, ("from", "to"), "cynefin/cynefin-beta transition")
        if transition["from"] not in _CYNEFIN_DOMAINS or transition["to"] not in _CYNEFIN_DOMAINS:
            raise IRError(f"cynefin/cynefin-beta transition has an unknown domain: {transition}")
        if transition["from"] == transition["to"]:
            raise IRError("cynefin/cynefin-beta ignores self-loops; from and to must differ")
        label = f' : {_quote(transition["label"])}' if transition.get("label") else ""
        lines.append(f"    {transition['from']} --> {transition['to']}{label}")
    return "\n".join(lines) + "\n"


_EVENT_KINDS = {"ui", "cmd", "evt", "rmo", "pcr"}


def render_eventmodeling(ir: dict) -> str:
    _require(ir, ("frames",), "eventmodeling/eventmodeling")
    if not ir["frames"]:
        raise IRError("eventmodeling/eventmodeling requires frames")
    lines = ["eventmodeling"]
    seen: set[str] = set()
    for frame in ir["frames"]:
        _require(frame, ("n", "kind", "name"), "eventmodeling frame")
        number = frame["n"]
        if isinstance(number, bool) or not isinstance(number, int) or number < 0:
            raise IRError(f"eventmodeling frame number must be a non-negative integer, got {number!r}")
        key = f"{number:02d}"
        if key in seen:
            raise IRError(f"eventmodeling duplicate frame number {key}")
        seen.add(key)
        if frame["kind"] not in _EVENT_KINDS:
            raise IRError(f"eventmodeling frame {key} has unknown kind {frame['kind']!r}")
        if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_.]*", str(frame["name"])):
            raise IRError(f"eventmodeling frame {key} name {frame['name']!r} must be an identifier")
        lines.append(f"    tf {key} {frame['kind']} {frame['name']}")
    return "\n".join(lines) + "\n"


def render_railroad(ir: dict) -> str:
    _require(ir, ("rules",), "grammar/railroad-ebnf-beta")
    if not ir["rules"]:
        raise IRError("grammar/railroad-ebnf-beta requires rules")
    lines = ["railroad-ebnf-beta"]
    if ir.get("title"):
        lines.append(f"    title {_quote(ir['title'])}")
    for rule in ir["rules"]:
        text = str(rule).strip()
        if "\n" in text or "=" not in text or not text.endswith(";"):
            raise IRError(f"grammar/railroad-ebnf-beta rule must be one line containing '=' and ending with ';': {rule!r}")
        lines.append(f"    {text}")
    return "\n".join(lines) + "\n"


def _usecase_id(raw: str, where: str) -> str:
    if not re.fullmatch(r"[A-Za-z0-9_]+", raw):
        raise IRError(f"{where} id {raw!r} must be letters, digits, or underscore")
    return raw


def render_usecase(ir: dict) -> str:
    _require(ir, ("actors", "usecases", "edges"), "usecase/usecase-beta")
    lines = ["usecase-beta"]
    direction = _direction(ir, "usecase/usecase-beta", allowed=_CARDINAL_DIRECTIONS)
    if direction:
        lines.append(f"    direction {direction}")
    actors_by_group: dict[str | None, list[dict]] = {}
    cases_by_group: dict[str | None, list[dict]] = {}
    for actor in ir["actors"]:
        _require(actor, ("id",), "usecase/usecase-beta actor")
        actors_by_group.setdefault(actor.get("group"), []).append(actor)
    for usecase in ir["usecases"]:
        _require(usecase, ("id", "label"), "usecase/usecase-beta usecase")
        cases_by_group.setdefault(usecase.get("group"), []).append(usecase)
    actor_ids: set[str] = set()
    case_ids: set[str] = set()

    def emit_actor(actor: dict, indent: str) -> None:
        actor_id = _usecase_id(actor["id"], "usecase/usecase-beta actor")
        label = f"({_quote(actor['label'])})" if actor.get("label") else ""
        lines.append(f"{indent}actor {actor_id}{label}")
        actor_ids.add(actor_id)

    def emit_case(usecase: dict, indent: str) -> None:
        case_id = _usecase_id(usecase["id"], "usecase/usecase-beta usecase")
        lines.append(f"{indent}{case_id}({_quote(usecase['label'])})")
        case_ids.add(case_id)

    for group in ir.get("groups", []):
        _require(group, ("id", "label"), "usecase/usecase-beta group")
        group_id = _usecase_id(group["id"], "usecase/usecase-beta group")
        lines.append(f"    systemBoundary {group_id}({_quote(group['label'])})")
        for actor in actors_by_group.pop(group["id"], []):
            emit_actor(actor, "        ")
        for usecase in cases_by_group.pop(group["id"], []):
            emit_case(usecase, "        ")
        lines.append("    end")
    for actor in actors_by_group.pop(None, []):
        emit_actor(actor, "    ")
    for usecase in cases_by_group.pop(None, []):
        emit_case(usecase, "    ")
    leftover = [group for group in {**actors_by_group, **cases_by_group} if group is not None]
    if leftover:
        raise IRError(f"usecase/usecase-beta members reference undeclared group(s): {leftover}")
    known = actor_ids | case_ids
    for edge in ir["edges"]:
        _require(edge, ("from", "to"), "usecase/usecase-beta edge")
        if edge["from"] not in known or edge["to"] not in known:
            raise IRError(f"usecase/usecase-beta edge references an undeclared actor or use case: {edge}")
        kind = edge.get("kind", "association")
        if kind == "association":
            if edge.get("label"):
                lines.append(f'    {edge["from"]} -- {_quote(edge["label"])} --> {edge["to"]}')
            else:
                lines.append(f'    {edge["from"]} --> {edge["to"]}')
        elif kind in ("include", "extend"):
            if edge["from"] not in case_ids or edge["to"] not in case_ids:
                raise IRError(f"usecase/usecase-beta {kind} endpoints must both be use cases")
            lines.append(f'    {edge["from"]} ..> : {kind} {edge["to"]}')
        elif kind == "generalization":
            same = (edge["from"] in actor_ids and edge["to"] in actor_ids) or (
                edge["from"] in case_ids and edge["to"] in case_ids
            )
            if not same:
                raise IRError("usecase/usecase-beta generalization must connect two actors or two use cases")
            lines.append(f'    {edge["from"]} --|> {edge["to"]}')
        else:
            raise IRError(f"usecase/usecase-beta edge has unknown kind {kind!r}")
    return "\n".join(lines) + "\n"


def render_sequence_zenuml(ir: dict) -> str:
    _require(ir, ("actors", "steps"), "sequence/zenuml")
    lines = ["zenuml"]
    if ir.get("title"):
        lines.append(f"    title {_no_colon(ir['title'], 'sequence/zenuml title')}")
    known: set[str] = set()
    for actor in ir["actors"]:
        _require(actor, ("id",), "sequence/zenuml actor")
        if actor.get("label") not in (None, actor["id"]):
            raise IRError("sequence/zenuml has no alias; the id is the displayed name")
        if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", actor["id"]):
            raise IRError(f"sequence/zenuml actor id {actor['id']!r} must be a bare identifier")
        prefix = "@Actor " if actor.get("kind") == "actor" else ""
        if actor.get("kind") not in (None, "actor", "participant"):
            raise IRError(f"sequence/zenuml actor {actor['id']!r} has unknown kind {actor['kind']!r}")
        lines.append(f"    {prefix}{actor['id']}")
        known.add(actor["id"])
    for step in ir["steps"]:
        if step.get("type", "message") != "message":
            raise IRError("sequence/zenuml only emits messages; hand-author if/while/try blocks")
        _require(step, ("from", "to", "label"), "sequence/zenuml message")
        if step["from"] not in known or step["to"] not in known:
            raise IRError(f"sequence/zenuml message references an undeclared participant: {step}")
        lines.append(f'    {step["from"]}->{step["to"]}: {_sequence_text(step["label"])}')
    return "\n".join(lines) + "\n"


def render_info(_ir: dict) -> str:
    return "info\n"


# --- dispatch ------------------------------------------------------------------------------

SERIALIZERS: dict[tuple[str, str], Callable[[dict], str]] = {
    ("graph", "flowchart"): render_graph_flowchart,
    ("graph", "mindmap"): render_graph_mindmap,
    ("graph", "block"): render_graph_block,
    ("graph", "C4Context"): render_graph_c4,
    ("graph", "C4Container"): render_graph_c4,
    ("graph", "architecture-beta"): render_graph_architecture,
    ("graph", "erDiagram"): render_graph_er,
    ("graph", "classDiagram"): render_graph_class,
    ("graph", "swimlane-beta"): render_graph_swimlane,
    ("graph", "agentflow-beta"): render_graph_agentflow,
    ("graph", "wardley-beta"): render_graph_wardley,
    ("graph", "C4Component"): render_graph_c4,
    ("graph", "C4Dynamic"): render_graph_c4,
    ("graph", "C4Deployment"): render_graph_c4,
    ("timeline", "gantt"): render_timeline_gantt,
    ("timeline", "timeline"): render_timeline_events,
    ("state-machine", "stateDiagram-v2"): render_state_machine,
    ("sequence", "sequenceDiagram"): render_sequence,
    ("sequence", "zenuml"): render_sequence_zenuml,
    ("requirement-links", "requirementDiagram"): render_requirement_links,
    ("chart", "pie"): render_chart_pie,
    ("chart", "xychart"): render_chart_xy,
    ("chart", "sankey"): render_chart_sankey,
    ("chart", "quadrantChart"): render_chart_quadrant,
    ("chart", "radar-beta"): render_chart_radar,
    ("chart", "treemap-beta"): render_chart_treemap,
    ("chart", "venn-beta"): render_chart_venn,
    ("packet", "packet"): render_packet,
    ("board", "kanban"): render_kanban,
    ("journey", "journey"): render_journey,
    ("git", "gitGraph"): render_git,
    ("tree", "treeView-beta"): render_treeview,
    ("tree", "ishikawa-beta"): render_ishikawa,
    ("cynefin", "cynefin-beta"): render_cynefin,
    ("eventmodeling", "eventmodeling"): render_eventmodeling,
    ("grammar", "railroad-ebnf-beta"): render_railroad,
    ("usecase", "usecase-beta"): render_usecase,
    ("info", "info"): render_info,
}


def render(ir: dict, target: str | None = None, backend: str = "mermaid") -> str:
    if backend != "mermaid":
        raise IRError(f"unsupported backend {backend!r}; only 'mermaid' is implemented today")
    _require(ir, ("diagram",), "IR document")
    family = ir["diagram"]
    resolved_target = target or ir.get("target")
    if not resolved_target:
        raise IRError("IR document has no 'target' and none was passed with --target")
    serializer = SERIALIZERS.get((family, resolved_target))
    if serializer is None:
        known = ", ".join(f"{f}->{t}" for f, t in SERIALIZERS)
        raise IRError(f"no serializer for {family}->{resolved_target}; known: {known}")
    return serializer(ir)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("ir_file", type=Path, help="Path to the diagram IR JSON file.")
    parser.add_argument("--target", default=None, help="Override the IR's 'target' field.")
    parser.add_argument("--backend", default="mermaid", choices=("mermaid",))
    parser.add_argument("-o", "--output", type=Path, default=None, help="Write source here instead of stdout.")
    args = parser.parse_args(argv)

    try:
        ir = json.loads(args.ir_file.read_text(encoding="utf-8"))
        source = render(ir, target=args.target, backend=args.backend)
    except (IRError, json.JSONDecodeError, OSError) as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 1

    if args.output:
        args.output.write_text(source, encoding="utf-8")
    else:
        print(source, end="")
    return 0


if __name__ == "__main__":
    sys.exit(main())
