import { expect, test } from "@playwright/test";
import type { RunTiming } from "../src/api";
import {
  applyTraceEvent,
  applyTraceSnapshot,
  createExecutionTrace,
  traceView,
  type ExecutionTraceState,
  type TraceGroup,
  type TraceNode,
  type TraceView,
} from "../src/executionTrace";
import { traceBar, traceLayout, type TraceTreeNode } from "../src/traceLayout";

// Pure node-side tests: API types only, no browser fixture or native host.
const call = (id: string) => ({
  type: "toolCall",
  id,
  name: "read",
  arguments: {},
});
const assistant = (entryId: string, content: unknown[] = []) => ({
  role: "assistant",
  entryId,
  content,
  stopReason: "stop",
});
const result = (toolCallId: string) => ({
  role: "toolResult",
  toolCallId,
  content: "result",
  isError: false,
});
const seed = (
  state: ExecutionTraceState,
  messages: unknown[],
  mode: "initial" | "refresh" | "older" = "initial",
) => applyTraceSnapshot(state, { messages }, mode);
const all = (view: TraceView) => view.groups.flatMap((g) => g.nodes);
const flatten = (roots: TraceTreeNode[]): TraceNode[] =>
  roots.flatMap((tree) => [tree.node, ...flatten(tree.children)]);
const node = (
  id: string,
  kind: TraceNode["kind"],
  extra: Partial<TraceNode> = {},
): TraceNode => ({ id, kind, status: "unknown", ...extra });
const group = (
  id: string,
  nodes: TraceNode[],
  extra: Partial<TraceGroup> = {},
): TraceGroup => ({
  id,
  nodes,
  source: "history",
  partial: true,
  status: "unknown",
  ...extra,
});
const timing = (extra: Partial<RunTiming> = {}): RunTiming => ({
  runId: "run",
  lane: "prompt",
  finalEntryId: "final",
  outcome: "completed",
  startedAt: 100,
  finishedAt: 200,
  elapsedMs: 100,
  ...extra,
});

function harness() {
  let state = createExecutionTrace(),
    seq = 0;
  return {
    get state() {
      return state;
    },
    send(type: string, fields: Record<string, unknown> = {}) {
      state = applyTraceEvent(state, { type, seq: ++seq, ...fields }, seq);
    },
    seed(
      messages: unknown[],
      mode: "initial" | "refresh" | "older" = "initial",
    ) {
      state = seed(state, messages, mode);
    },
  };
}

test("only an exact assistant toolCall ID supplies a parent, not the nearest LLM", () => {
  const h = harness();
  h.seed([
    assistant("first", [
      { ...call("exact"), arguments: { path: "exact.txt" } },
    ]),
    assistant("nearest"),
    result("other"),
  ]);
  h.send("tool_execution_start", {
    toolCallId: "event-only",
    toolName: "read",
  });
  const nodes = all(traceView(h.state, []));
  const parent = nodes.find((n) => n.entryId === "first")!;
  const correlated = nodes.find(
    (n) => n.kind === "tool" && n.input?.includes("exact.txt"),
  )!;
  expect(correlated.parentId).toBe(parent.id);
  expect(
    nodes
      .filter((n) => n.kind === "tool" && n !== correlated)
      .every((n) => n.parentId === undefined),
  ).toBe(true);
  expect(nodes.filter((n) => n.parentId)).toHaveLength(1);
});

test("result-first tools join an older-page call without changing identity or duplicating roots", () => {
  const h = harness();
  h.seed([result("join")]);
  const before = all(traceView(h.state, []))[0];
  h.seed([assistant("older-parent", [call("join")])], "older");
  const view = traceView(h.state, []);
  const tool = all(view).find((n) => n.kind === "tool")!;
  const parent = all(view).find((n) => n.kind === "llm")!;
  expect(tool).toMatchObject({
    id: before.id,
    parentId: parent.id,
    output: "result",
    status: "completed",
  });
  expect(view.groups).toHaveLength(2);
  const layout = traceLayout(view);
  expect(layout.groups).toHaveLength(1);
  expect(layout.groups[0].roots[0].node.id).toBe(parent.id);
  expect(layout.groups[0].roots[0].children.map((n) => n.node.id)).toEqual([
    tool.id,
  ]);
});

test("stream updates and refresh preserve the correlated parent and node IDs", () => {
  const h = harness();
  h.send("message_start", { message: assistant("stream", [call("stable")]) });
  const initial = all(traceView(h.state, []));
  const parent = initial.find((n) => n.kind === "llm")!;
  const tool = initial.find((n) => n.kind === "tool")!;
  h.send("message_update", {
    message: assistant("stream", [{ type: "text", text: "more" }]),
  });
  h.send("tool_execution_update", {
    toolCallId: "stable",
    partialResult: "partial",
  });
  h.send("message_end", { message: assistant("stream", [call("stable")]) });
  h.seed([assistant("stream"), result("stable")], "refresh");
  const nodes = all(traceView(h.state, []));
  expect(nodes.filter((n) => n.kind === "llm").map((n) => n.id)).toEqual([
    parent.id,
  ]);
  expect(nodes.filter((n) => n.kind === "tool")).toHaveLength(1);
  expect(nodes.find((n) => n.kind === "tool")).toMatchObject({
    id: tool.id,
    parentId: parent.id,
  });
});

test("unknown or evicted parents remain roots rather than guessed history hierarchy", () => {
  let state = seed(createExecutionTrace(), [
    assistant("evicted", [call("retained-tool")]),
  ]);
  const originalParent = all(traceView(state, [])).find(
    (n) => n.kind === "llm",
  )!.id;
  state = seed(
    state,
    Array.from({ length: 299 }, (_, i) => assistant(`filler-${i}`)),
    "refresh",
  );
  const view = traceView(state, []);
  expect(view.clipped).toBe(true);
  expect(all(view).some((n) => n.id === originalParent)).toBe(false);
  const tool = all(view).find((n) => n.kind === "tool")!;
  expect(tool.parentId).toBe(originalParent);
  expect(
    traceLayout(view)
      .groups.flatMap((g) => g.roots)
      .find((r) => r.node.id === tool.id)?.children,
  ).toEqual([]);
  const unknown = node("orphan", "tool", { parentId: "not-retained" });
  expect(
    traceLayout({ groups: [group("g", [unknown])], clipped: false }).groups[0]
      .roots[0].node,
  ).toBe(unknown);
});

test("cross-group trees retain every unique node once and do not mutate their view", () => {
  const parent = node("llm", "llm"),
    tool = node("tool", "tool", { parentId: "llm" });
  const view: TraceView = {
    groups: [
      group("old", [tool]),
      group("new", [parent, node("input", "input")]),
    ],
    clipped: true,
  };
  const before = JSON.stringify(view);
  for (const g of view.groups) {
    g.nodes.forEach(Object.freeze);
    Object.freeze(g.nodes);
    Object.freeze(g);
  }
  Object.freeze(view.groups);
  Object.freeze(view);
  const layout = traceLayout(view);
  const projected = layout.groups.flatMap((g) => flatten(g.roots));
  expect(projected.map((n) => n.id).sort()).toEqual(["input", "llm", "tool"]);
  expect(new Set(projected.map((n) => n.id)).size).toBe(projected.length);
  expect(layout.groups[0].roots[0].children[0].node).toBe(tool);
  expect(JSON.stringify(view)).toBe(before);
});

test("cycles, self references, invalid parent kinds and ambiguous IDs cannot lose nodes", () => {
  const nodes = [
    node("llm", "llm", { parentId: "tool" }),
    node("tool", "tool", { parentId: "llm" }),
    node("self", "tool", { parentId: "self" }),
    node("chain", "tool", { parentId: "tool" }),
    node("duplicate", "llm"),
    node("duplicate", "llm"),
    node("ambiguous", "tool", { parentId: "duplicate" }),
    node("same-tool", "tool", { parentId: "llm" }),
    node("same-tool", "tool", { parentId: "llm" }),
  ];
  const layout = traceLayout({ groups: [group("g", nodes)], clipped: false });
  const projected = flatten(layout.groups[0].roots);
  expect(projected).toHaveLength(nodes.length);
  for (const n of nodes)
    expect(projected.filter((p) => p === n)).toHaveLength(1);
  expect(
    layout.groups[0].roots
      .find((r) => r.node.id === "llm")
      ?.children.map((r) => r.node.id),
  ).toEqual(["tool"]);
  expect(layout.groups[0].roots.map((r) => r.node.id)).toContain("ambiguous");
});

test("validated timings annotate only the exact run root and its settlement", () => {
  const h = harness();
  h.send("operation_accepted", { runId: "run" });
  h.send("message_end", { message: assistant("final") });
  h.send("agent_settled", { runId: "run", outcome: "completed" });
  const view = traceView(h.state, [timing()]);
  const g = view.groups.find((g) => g.runId === "run")!;
  expect(g).toMatchObject({ elapsedMs: 100, startedAt: 100, finishedAt: 200 });
  expect(g.nodes.find((n) => n.kind === "settlement")).toMatchObject({
    elapsedMs: 100,
    startedAt: 100,
    finishedAt: 200,
  });
  expect(g.nodes.find((n) => n.kind === "llm")?.startedAt).toBeUndefined();
  expect(traceView(h.state, []).groups[0].startedAt).toBeUndefined();
  const history = traceView(
    seed(createExecutionTrace(), [assistant("final")]),
    [timing()],
  );
  expect(history.groups[0]).toMatchObject({ partial: true, status: "unknown" });
  expect(history.groups[0].startedAt).toBeUndefined();
  expect(
    history.groups[0].nodes.find((n) => n.kind === "settlement"),
  ).toMatchObject({ startedAt: 100, finishedAt: 200, elapsedMs: 100 });
});

test("invalid, mismatched and ambiguous run timings supply no native boundaries", () => {
  const state = seed(createExecutionTrace(), [assistant("final")]);
  const bad: Partial<RunTiming>[] = [
    { elapsedMs: -1 },
    { elapsedMs: NaN },
    { elapsedMs: 99 },
    { startedAt: 100.5 },
    { startedAt: -1 },
    { finishedAt: Infinity },
    { finishedAt: Number.MAX_SAFE_INTEGER + 1 },
    { finishedAt: 90 },
    { runId: "" },
    { finalEntryId: "missing" },
    { outcome: "invalid" as RunTiming["outcome"] },
  ];
  for (const extra of bad) {
    const view = traceView(state, [timing(extra)]);
    expect(
      all(view).some(
        (n) => n.kind === "settlement" || n.startedAt !== undefined,
      ),
    ).toBe(false);
    expect(view.groups[0].startedAt).toBeUndefined();
  }
  const conflicting = harness();
  conflicting.send("operation_accepted", { runId: "different-run" });
  conflicting.send("message_end", { message: assistant("final") });
  expect(
    all(traceView(conflicting.state, [timing()])).some(
      (n) => n.startedAt !== undefined,
    ),
  ).toBe(false);
  const ambiguous: ExecutionTraceState = {
    ...state,
    groups: state.groups.map((g) => ({
      ...g,
      nodes: [...g.nodes, { ...g.nodes[0], id: "duplicate-final" }],
    })),
  };
  expect(
    all(traceView(ambiguous, [timing()])).some((n) => n.kind === "settlement"),
  ).toBe(false);
});

test("the common scale is a maximum, never a sum, retry delay or user duration", () => {
  const view: TraceView = {
    clipped: false,
    groups: [
      group(
        "a",
        [
          node("a", "llm", { elapsedMs: 40 }),
          node("b", "tool", { elapsedMs: 70 }),
          node("c", "settlement", { elapsedMs: 80 }),
          node("retry", "retry", { elapsedMs: 9999, delayMs: 50000 }),
          node("user", "input", { elapsedMs: 10000 }),
          node("bad", "llm", { elapsedMs: Infinity }),
          node("negative", "tool", { elapsedMs: -2 }),
        ],
        { elapsedMs: 100 },
      ),
      group("b", [node("d", "llm", { elapsedMs: 90 })]),
    ],
  };
  expect(traceLayout(view).maxMs).toBe(100);
  expect(
    traceLayout({
      groups: [group("g", [node("zero", "tool", { elapsedMs: 0 })])],
      clipped: false,
    }).maxMs,
  ).toBe(0);
  expect(
    traceLayout({
      groups: [group("g", [node("bad", "llm", { elapsedMs: NaN })])],
      clipped: false,
    }).maxMs,
  ).toBe(0);
  expect(traceLayout({ groups: [], clipped: false }).maxMs).toBe(0);
});

test("compatible native boundaries position bars using each run's origin and common scale", () => {
  const first = group("one", [], {
    startedAt: 100,
    finishedAt: 200,
    elapsedMs: 100,
  });
  const second = group("two", [], {
    startedAt: 10000,
    finishedAt: 10200,
    elapsedMs: 200,
  });
  expect(
    traceBar({ startedAt: 125, finishedAt: 175, elapsedMs: 50 }, first, 200),
  ).toEqual({ mode: "positioned", left: 0.125, width: 0.25, elapsedMs: 50 });
  expect(traceBar(first, first, 200)).toEqual({
    mode: "positioned",
    left: 0,
    width: 0.5,
    elapsedMs: 100,
  });
  expect(traceBar(second, second, 200)).toEqual({
    mode: "positioned",
    left: 0,
    width: 1,
    elapsedMs: 200,
  });
  expect(
    traceBar(
      { startedAt: 10025, finishedAt: 10075, elapsedMs: 50 },
      second,
      200,
    ).left,
  ).toBe(0.125);
});

test("elapsed-only bars distinguish zero from unknown and handle unusable scales safely", () => {
  const g = group("g", []);
  expect(traceBar({ elapsedMs: 25 }, g, 100)).toEqual({
    mode: "duration",
    left: 0,
    width: 0.25,
    elapsedMs: 25,
  });
  expect(traceBar({ elapsedMs: 0 }, g, 0)).toEqual({
    mode: "duration",
    left: 0,
    width: 0,
    elapsedMs: 0,
  });
  for (const value of [undefined, -1, NaN, Infinity])
    expect(traceBar({ elapsedMs: value }, g, 100)).toEqual({
      mode: "unknown",
      left: 0,
      width: 0,
    });
  for (const scale of [0, -1, NaN, Infinity]) {
    const bar = traceBar({ elapsedMs: 20 }, g, scale);
    expect(bar).toMatchObject({ mode: "duration", left: 0, width: 0 });
  }
  expect(traceBar({ elapsedMs: 0.5 }, g, 1).width).toBe(0.5);
});

test("unchecked, incompatible or out-of-group bounds fall back to duration and clamp ratios", () => {
  const g = group("g", [], { startedAt: 100, finishedAt: 200, elapsedMs: 100 });
  for (const item of [
    { startedAt: 90, finishedAt: 140, elapsedMs: 50 },
    { startedAt: 180, finishedAt: 230, elapsedMs: 50 },
    { startedAt: 120, finishedAt: 170, elapsedMs: 49 },
    { startedAt: 120.5, finishedAt: 170.5, elapsedMs: 50 },
    { startedAt: NaN, finishedAt: 170, elapsedMs: 50 },
    { startedAt: 120, elapsedMs: 50 },
  ])
    expect(traceBar(item, g, 100)).toMatchObject({ mode: "duration", left: 0 });
  expect(traceBar({ startedAt: 120, finishedAt: 170 }, g, 100)).toEqual({
    mode: "unknown",
    left: 0,
    width: 0,
  });
  expect(
    traceBar(
      { startedAt: 120, finishedAt: 170, elapsedMs: 50 },
      { ...g, elapsedMs: 99 },
      100,
    ).mode,
  ).toBe("duration");
  const clipped = traceBar(
    { startedAt: 125, finishedAt: 175, elapsedMs: 50 },
    g,
    30,
  );
  expect(clipped.left).toBeGreaterThanOrEqual(0);
  expect(clipped.left + clipped.width).toBeLessThanOrEqual(1);
  expect(traceBar({ elapsedMs: 100 }, g, Number.MIN_VALUE).width).toBe(1);
});

test("timestamps, observedAt and sequence never manufacture LLM latency or request bounds", () => {
  const h = harness();
  h.send("operation_accepted", { runId: "run", observedAt: 1000 });
  h.send("message_start", {
    message: { ...assistant("final"), timestamp: 2000 },
    observedAt: 3000,
  });
  h.send("message_end", {
    message: { ...assistant("final"), timestamp: 8000 },
    observedAt: 9000,
  });
  h.send("agent_settled", {
    runId: "run",
    outcome: "completed",
    observedAt: 10000,
    elapsedMs: 9000,
  });
  const view = traceView(h.state, []);
  for (const item of [...view.groups, ...all(view)]) {
    expect(item.elapsedMs).toBeUndefined();
    expect(item.startedAt).toBeUndefined();
    expect(item.finishedAt).toBeUndefined();
  }
  expect(traceLayout(view).maxMs).toBe(0);
  expect(
    traceBar(
      all(view).find((n) => n.kind === "llm")!,
      view.groups[0],
      100,
    ).mode,
  ).toBe("unknown");
});

test("empty real metadata groups survive relocation while native partial/status stay unchanged", () => {
  const parent = node("parent", "llm");
  const child = (id: string) => node(id, "tool", { parentId: "parent" });
  const view: TraceView = {
    clipped: false,
    groups: [
      group("anonymous", [child("a")]),
      group("native", [child("b")], { runId: "run" }),
      group("timed", [child("c")], { elapsedMs: 0 }),
      group("settled", [child("d"), node("settlement", "settlement")], {
        status: "completed",
      }),
      group("live", [child("e")], {
        source: "live",
        status: "running",
        partial: false,
      }),
      group("parent-group", [parent]),
    ],
  };
  const layout = traceLayout(view);
  expect(layout.groups.map((g) => g.group.id)).toEqual([
    "native",
    "timed",
    "settled",
    "live",
    "parent-group",
  ]);
  expect(layout.groups.find((g) => g.group.id === "native")?.roots).toEqual([]);
  expect(
    layout.groups.find((g) => g.group.id === "settled")?.roots[0].node.kind,
  ).toBe("settlement");
  expect(layout.groups.find((g) => g.group.id === "live")?.group).toMatchObject(
    { status: "running", partial: false },
  );
  expect(view.groups[0]).toMatchObject({ partial: true, status: "unknown" });
  expect(view.groups[0].nodes).toHaveLength(1);
});
