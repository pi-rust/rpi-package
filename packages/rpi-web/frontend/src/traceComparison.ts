import type { TraceGroup, TraceNode, TraceView } from "./executionTrace";

export interface TimedTraceStep {
  node: TraceNode & { elapsedMs: number };
  groupSource: TraceGroup["source"];
  groupIndex: number;
  /** Relative to the longest recorded step, never a share of run elapsed. */
  ratio: number;
}

/** Presentation only: compare recorded LLM/tool steps, not inclusive run totals
 * or scheduled retry delays. Parallel steps overlap and must never be summed. */
export function compareTraceDurations(view: TraceView) {
  const known: TimedTraceStep[] = [];
  let totalCount = 0;
  let unknownLlmCount = 0;
  view.groups.forEach((group, groupIndex) => {
    for (const node of group.nodes) {
      if (node.kind !== "tool" && node.kind !== "llm") continue;
      totalCount++;
      const ms = node.elapsedMs;
      if (typeof ms !== "number" || !Number.isFinite(ms) || ms < 0) {
        if (node.kind === "llm") unknownLlmCount++;
        continue;
      }
      known.push({
        node: { ...node, elapsedMs: ms },
        groupSource: group.source,
        groupIndex,
        ratio: 0,
      });
    }
  });
  // Stable ties retain transcript order; do not reorder the underlying trace.
  known.sort((a, b) => b.node.elapsedMs - a.node.elapsedMs);
  const maxMs = known[0]?.node.elapsedMs ?? 0;
  return {
    steps: known.slice(0, 5).map((step) => ({
      ...step,
      ratio: maxMs > 0 ? step.node.elapsedMs / maxMs : 0,
    })),
    knownCount: known.length,
    unknownCount: totalCount - known.length,
    unknownLlmCount,
    totalCount,
    maxMs,
    slowestCount:
      maxMs > 0
        ? known.filter((step) => step.node.elapsedMs === maxMs).length
        : 0,
  };
}
