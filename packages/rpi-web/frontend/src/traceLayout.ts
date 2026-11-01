import type { TraceGroup, TraceNode, TraceView } from "./executionTrace";

export interface TraceTreeNode {
  node: TraceNode;
  children: TraceTreeNode[];
}
export interface TraceTreeGroup {
  group: TraceGroup;
  roots: TraceTreeNode[];
}

type Timed = { elapsedMs?: number; startedAt?: number; finishedAt?: number };
const elapsed = (value: number | undefined): value is number =>
  typeof value === "number" && Number.isFinite(value) && value >= 0;
const boundary = (value: number | undefined): value is number =>
  typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
function bounded(item: Timed): item is Required<Timed> {
  return (
    boundary(item.elapsedMs) &&
    boundary(item.startedAt) &&
    boundary(item.finishedAt) &&
    item.finishedAt >= item.startedAt &&
    item.finishedAt - item.startedAt === item.elapsedMs
  );
}

/** Linear, presentation-only projection of the retained (at most 300) nodes. */
export function traceLayout(view: TraceView): {
  groups: TraceTreeGroup[];
  maxMs: number;
} {
  const groups = view.groups.map((group) => ({
    group,
    roots: [] as TraceTreeNode[],
  }));
  const entries: { tree: TraceTreeNode; group: TraceTreeGroup }[] = [];
  // Ambiguous IDs are not evidence of a parent. Keep every occurrence, even
  // malformed duplicates, rather than silently dropping or duplicating nodes.
  const byId = new Map<string, TraceTreeNode | undefined>();
  let maxMs = 0;
  for (const group of groups) {
    if (elapsed(group.group.elapsedMs))
      maxMs = Math.max(maxMs, group.group.elapsedMs);
    for (const node of group.group.nodes) {
      const tree: TraceTreeNode = { node, children: [] };
      entries.push({ tree, group });
      byId.set(node.id, byId.has(node.id) ? undefined : tree);
      if (
        ["llm", "tool", "settlement"].includes(node.kind) &&
        elapsed(node.elapsedMs)
      )
        maxMs = Math.max(maxMs, node.elapsedMs);
    }
  }
  for (const { tree, group } of entries) {
    const { node } = tree;
    const parent = node.parentId ? byId.get(node.parentId) : undefined;
    // Only tools can be children and only LLMs can be parents: no chains or
    // cycles, including malformed metadata on LLMs and self references.
    if (
      node.kind === "tool" &&
      byId.get(node.id) === tree &&
      parent?.node.kind === "llm"
    )
      parent.children.push(tree);
    else group.roots.push(tree);
  }
  return {
    groups: groups.filter(
      ({ group, roots }) =>
        // Suppress only an emptied anonymous history fragment; real run,
        // settlement, status or timing metadata must remain visible.
        roots.length ||
        !group.nodes.length ||
        group.source !== "history" ||
        group.runId !== undefined ||
        group.status !== "unknown" ||
        group.elapsedMs !== undefined ||
        group.startedAt !== undefined ||
        group.finishedAt !== undefined,
    ),
    maxMs,
  };
}

/** Native offsets are local to each run, with a common duration scale. */
export function traceBar(
  item: { elapsedMs?: number; startedAt?: number; finishedAt?: number },
  group: TraceGroup,
  maxMs: number,
): {
  mode: "positioned" | "duration" | "unknown";
  left: number;
  width: number;
  elapsedMs?: number;
} {
  if (!elapsed(item.elapsedMs)) return { mode: "unknown", left: 0, width: 0 };
  const positioned =
    bounded(item) &&
    bounded(group) &&
    item.startedAt >= group.startedAt &&
    item.finishedAt <= group.finishedAt;
  const scale = Number.isFinite(maxMs) && maxMs > 0;
  const clamp = (ratio: number) => Math.max(0, Math.min(1, ratio));
  const left =
    scale && positioned
      ? clamp((item.startedAt! - group.startedAt!) / maxMs)
      : 0;
  return {
    mode: positioned ? "positioned" : "duration",
    left,
    width: scale ? Math.min(1 - left, clamp(item.elapsedMs / maxMs)) : 0,
    elapsedMs: item.elapsedMs,
  };
}
