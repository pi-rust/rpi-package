import { expect, test } from "@playwright/test";
import { compareTraceDurations } from "../src/traceComparison";
import type { TraceNode, TraceView } from "../src/executionTrace";

const step = (
  id: string,
  elapsedMs?: number,
  kind: TraceNode["kind"] = "tool",
): TraceNode => ({ id, kind, elapsedMs, status: "completed" });
const view = (nodes: TraceNode[]): TraceView => ({
  clipped: false,
  groups: [
    {
      id: "history",
      source: "history",
      partial: true,
      status: "unknown",
      nodes,
    },
  ],
});

test("duration comparison sorts recorded steps without summing or mixing inclusive totals/retry delays", () => {
  const source = view([
    step("tool-fast", 100),
    step("run-total", 90000, "settlement"),
    { ...step("retry", 80000, "retry"), delayMs: 80000 },
    step("llm", 400, "llm"),
    step("tool-slow", 2000),
    step("input", 70000, "input"),
  ]);
  const before = structuredClone(source);
  const result = compareTraceDurations(source);
  expect(result.steps.map((s) => s.node.id)).toEqual([
    "tool-slow",
    "llm",
    "tool-fast",
  ]);
  expect(result.steps.map((s) => s.ratio)).toEqual([1, 0.2, 0.05]);
  expect(result).toMatchObject({
    knownCount: 3,
    totalCount: 3,
    unknownCount: 0,
    maxMs: 2000,
    slowestCount: 1,
  });
  expect(source).toEqual(before);
});

test("unknown and invalid timings are not zero or eligible for a bottleneck badge", () => {
  const result = compareTraceDurations(
    view([
      step("missing"),
      step("negative", -1),
      step("infinite", Infinity),
      step("nan", NaN),
      step("zero", 0),
    ]),
  );
  expect(result).toMatchObject({
    knownCount: 1,
    totalCount: 5,
    unknownCount: 4,
    maxMs: 0,
    slowestCount: 0,
  });
  expect(result.steps.map((s) => [s.node.id, s.ratio])).toEqual([["zero", 0]]);
  expect(compareTraceDurations(view([step("missing")])).steps).toEqual([]);
});

test("ranking is bounded to five with honest full coverage and stable ties", () => {
  const result = compareTraceDurations(
    view([
      ...Array.from({ length: 8 }, (_, i) => step(`tool-${i}`, 200)),
      step("unknown"),
    ]),
  );
  expect(result.steps.map((s) => s.node.id)).toEqual([
    "tool-0",
    "tool-1",
    "tool-2",
    "tool-3",
    "tool-4",
  ]);
  expect(result).toMatchObject({
    knownCount: 8,
    totalCount: 9,
    unknownCount: 1,
    maxMs: 200,
    slowestCount: 8,
  });
});

test("partial history and separate observed runs retain their display context", () => {
  const source = view([step("history-tool", 200)]);
  source.groups.push({
    id: "live",
    source: "live",
    partial: false,
    status: "running",
    elapsedMs: 90000,
    nodes: [
      { ...step("live-tool", 500), status: "running" },
      step("unknown-llm", undefined, "llm"),
    ],
  });
  const result = compareTraceDurations(source);
  expect(
    result.steps.map((s) => [s.node.id, s.groupSource, s.groupIndex]),
  ).toEqual([
    ["live-tool", "live", 1],
    ["history-tool", "history", 0],
  ]);
  expect(result.maxMs).toBe(500);
  expect(result.unknownCount).toBe(1);
  expect(result.unknownLlmCount).toBe(1);
});

test("live updates rerank from recorded native values without caching or mutating earlier views", () => {
  const initial = view([
    step("a", 50),
    { ...step("b", 100), status: "running" },
  ]);
  const first = compareTraceDurations(initial);
  const updated = view([step("a", 250), step("b", 100)]);
  expect(compareTraceDurations(updated).steps.map((s) => s.node.id)).toEqual([
    "a",
    "b",
  ]);
  expect(first.steps.map((s) => s.node.id)).toEqual(["b", "a"]);
  expect(first.steps[0].node.status).toBe("running");
});
