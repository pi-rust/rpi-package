import { expect, test } from "@playwright/test";
import type { RunTiming, SeqEvent } from "../src/api";
import {
  applyTraceEvent,
  applyTraceSnapshot,
  createExecutionTrace,
  traceView,
  type ExecutionTraceState,
} from "../src/executionTrace";

// Pure node-side projection tests: no page fixture, API implementation or model.
const view = (s: ExecutionTraceState, timings: RunTiming[] = []) =>
  traceView(s, timings);
const nodes = (s: ExecutionTraceState) =>
  view(s).groups.flatMap((g) => g.nodes);
const snapshot = (
  s: ExecutionTraceState,
  messages: unknown[],
  mode: "initial" | "refresh" | "older" = "initial",
) => applyTraceSnapshot(s, { messages }, mode);
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
      state = snapshot(state, messages, mode);
    },
  };
}
const assistant = (
  timestamp?: number,
  content: unknown = [{ type: "text", text: "answer" }],
) => ({
  role: "assistant",
  timestamp,
  content,
  model: "native-model",
  provider: "native-provider",
});
const timing = (overrides: Partial<RunTiming> = {}): RunTiming => ({
  runId: "run",
  lane: "prompt",
  finalEntryId: "final",
  startedAt: 100,
  finishedAt: 150,
  elapsedMs: 50,
  outcome: "completed",
  ...overrides,
});

test("host closure ends running observations as unknown without fabricating settlement or time", () => {
  const h = harness();
  h.send("agent_start", { runId: "closed-run" });
  h.send("message_start", { message: { ...assistant(), entryId: "partial" } });
  h.send("tool_execution_start", { toolCallId: "finished", toolName: "read" });
  h.send("tool_execution_end", { toolCallId: "finished", toolName: "read", result: "done" });
  h.send("tool_execution_start", { toolCallId: "unfinished", toolName: "bash" });
  h.send("host_closed");
  const result = view(h.state);
  expect(result.groups[0]).toMatchObject({ status: "unknown", partial: true });
  expect(nodes(h.state).some(n => n.status === "running")).toBe(false);
  expect(nodes(h.state).find(n => n.kind === "llm")?.status).toBe("unknown");
  expect(nodes(h.state).find(n => n.name === "bash")?.status).toBe("unknown");
  expect(nodes(h.state).find(n => n.name === "read")?.status).toBe("completed");
  expect(nodes(h.state).some(n => n.kind === "settlement")).toBe(false);
  expect(result.groups[0].elapsedMs).toBeUndefined();
  expect(h.state.active).toBeUndefined();
  expect(h.state.assistant).toBeUndefined();
  expect(h.state.user).toBeUndefined();
});

test("history is one explicitly partial unknown boundary, with explicit user input and message metadata", () => {
  const s = applyTraceSnapshot(
    createExecutionTrace(),
    {
      messages: [
        { role: "user", entryId: "u", content: "my question" },
        {
          ...assistant(20),
          entryId: "a",
          usage: {
            input: 5,
            output: 2,
            cacheRead: 3,
            cacheWrite: 1,
            totalTokens: 11,
            cost: { total: 0.12 },
          },
        },
        { role: "user", entryId: "u2", content: "another question" },
      ],
      status: "ready",
      latestSettlement: { runId: "latest", outcome: "completed" },
    },
    "initial",
  );
  expect(view(s).groups).toHaveLength(2);
  expect(view(s).groups[1]).toMatchObject({
    source: "history",
    partial: true,
    runId: "latest",
    status: "completed",
  });
  expect(view(s).groups[0]).toMatchObject({
    source: "history",
    partial: true,
    status: "unknown",
  });
  expect(view(s).groups[0].runId).toBeUndefined();
  expect(nodes(s)[0]).toMatchObject({
    kind: "input",
    input: "my question",
    entryId: "u",
  });
  expect(nodes(s)[1]).toMatchObject({
    kind: "llm",
    output: "answer",
    model: "native-model",
    provider: "native-provider",
    usage: {
      input: 5,
      output: 2,
      cached: 3,
      cacheWrite: 1,
      total: 11,
      cost: 0.12,
    },
  });
});

test("metadata-free page slots keep identical messages distinct and refresh idempotent", () => {
  const messages = [
    { role: "user", content: "same" },
    { role: "user", content: "same" },
    { role: "assistant", content: "same" },
    { role: "assistant", content: "same" },
  ];
  let s = snapshot(createExecutionTrace(), messages);
  const ids = nodes(s).map((n) => n.id);
  s = snapshot(s, messages, "refresh");
  s = snapshot(s, messages, "refresh");
  expect(nodes(s).map((n) => n.id)).toEqual(ids);
  expect(new Set(ids).size).toBe(4);
  expect(view(s).groups[0].partial).toBe(true);
});

test("live current assistant updates merge durable entry IDs by native identity without losing retries", () => {
  const h = harness();
  h.send("agent_start");
  h.send("operation_accepted", { runId: "run" });
  h.send("retry_scheduled", {
    error: "busy",
    attempt: 1,
    maxRetries: 2,
    delayMs: 10,
  });
  h.send("message_start", { message: assistant(30, []) });
  h.send("message_update", {
    message: assistant(30, [{ type: "text", text: "partial" }]),
  });
  h.send("message_end", { message: assistant(30) });
  h.send("agent_settled", { runId: "run", outcome: "completed" });
  const oldId = nodes(h.state).find((n) => n.kind === "llm")!.id;
  h.seed([{ ...assistant(30), entryId: "durable" }], "refresh");
  h.seed([{ ...assistant(30), entryId: "durable" }], "refresh");
  expect(view(h.state).groups).toHaveLength(1);
  expect(nodes(h.state).filter((n) => n.kind === "llm")).toEqual([
    expect.objectContaining({
      id: oldId,
      entryId: "durable",
      output: "answer",
    }),
  ]);
  expect(nodes(h.state).filter((n) => n.kind === "retry")).toHaveLength(1);
  expect(view(h.state).groups[0].status).toBe("completed");
});

test("two live requests with identical timestamp-free responses are never merged by text", () => {
  const h = harness();
  for (const runId of ["first", "second"]) {
    h.send("agent_start", { runId });
    h.send("message_start", {
      message: { role: "user", content: "same question" },
    });
    h.send("message_end", {
      message: { role: "user", content: "same question" },
    });
    h.send("message_start", {
      message: { role: "assistant", content: "same answer" },
    });
    h.send("message_update", {
      message: { role: "assistant", content: "same answer" },
    });
    h.send("message_end", {
      message: { role: "assistant", content: "same answer" },
    });
    h.send("agent_settled", { runId, outcome: "completed" });
  }
  expect(nodes(h.state).filter((n) => n.kind === "llm")).toHaveLength(2);
  expect(nodes(h.state).filter((n) => n.kind === "input")).toHaveLength(2);
  expect(view(h.state).groups.map((g) => g.runId)).toEqual(["first", "second"]);
});

test("older pages prepend chronologically and merge tool calls with newer result entry links", () => {
  const h = harness();
  h.seed([
    {
      role: "toolResult",
      entryId: "result",
      toolCallId: "call",
      toolName: "read",
      content: [{ type: "text", text: "file" }],
      webTiming: { elapsedMs: 4, running: false },
    },
  ]);
  h.seed(
    [
      { role: "user", entryId: "question", content: "read file" },
      {
        ...assistant(1, [
          {
            type: "toolCall",
            id: "call",
            name: "read",
            arguments: { path: "src/a.ts" },
          },
        ]),
        entryId: "call-entry",
      },
    ],
    "older",
  );
  expect(view(h.state).groups[0].nodes[0].entryId).toBe("question");
  expect(nodes(h.state).filter((n) => n.kind === "tool")).toEqual([
    expect.objectContaining({
      entryId: "result",
      input: "path: src/a.ts",
      output: "file",
      status: "completed",
      elapsedMs: 4,
    }),
  ]);
  const before = nodes(h.state).map((n) => n.id);
  h.seed(
    [
      {
        role: "toolResult",
        entryId: "result",
        toolCallId: "call",
        content: [{ type: "text", text: "file" }],
      },
    ],
    "refresh",
  );
  expect(nodes(h.state).map((n) => n.id)).toEqual(before);
});

test("agent_start and operation_accepted are duplicate boundaries in either order", () => {
  for (const order of [
    ["agent_start", "operation_accepted"],
    ["operation_accepted", "agent_start"],
  ]) {
    const h = harness();
    h.send(order[0], order[0] === "operation_accepted" ? { runId: "run" } : {});
    h.send(order[1], order[1] === "operation_accepted" ? { runId: "run" } : {});
    expect(view(h.state).groups).toHaveLength(1);
    expect(view(h.state).groups[0]).toMatchObject({
      source: "live",
      status: "running",
      runId: "run",
    });
  }
});

test("agent_end and turn_end never settle a run or manufacture LLM latency", () => {
  const h = harness();
  h.send("agent_start");
  h.send("message_start", { message: assistant(10) });
  h.send("message_end", { message: assistant(10) });
  h.send("turn_end", { durationMs: 9999, elapsedMs: 9999 });
  h.send("agent_end", { durationMs: 9999 });
  expect(view(h.state).groups[0].status).toBe("running");
  expect(
    nodes(h.state).find((n) => n.kind === "llm")!.elapsedMs,
  ).toBeUndefined();
  expect(view(h.state).groups[0].elapsedMs).toBeUndefined();
  h.send("agent_settled", { outcome: "completed" });
  expect(view(h.state).groups[0].status).toBe("completed");
});

test("retries retain independent errors, valid metadata, and at most one node per seq", () => {
  let s = createExecutionTrace();
  const event: SeqEvent = {
    type: "retry_scheduled",
    seq: 4,
    error: "Bearer privatecredential",
    attempt: 0,
    maxRetries: 3,
    delayMs: 0,
  };
  s = applyTraceEvent(s, event, 4);
  const prior = s;
  s = applyTraceEvent(s, event, 4);
  expect(s).toBe(prior);
  s = applyTraceEvent(
    s,
    { ...event, seq: 5, attempt: -1, delayMs: Infinity },
    5,
  );
  const retries = nodes(s).filter((n) => n.kind === "retry");
  expect(retries).toHaveLength(2);
  expect(retries[0]).toMatchObject({
    attempt: 0,
    maxRetries: 3,
    delayMs: 0,
    error: "Bearer [redacted]",
  });
  expect(retries[1].attempt).toBeUndefined();
  expect(retries[1].delayMs).toBeUndefined();
});

test("parallel and out-of-order tools correlate by call ID, including tool-only assistant messages", () => {
  const h = harness();
  h.send("agent_start", { runId: "run" });
  h.send("tool_execution_end", {
    toolCallId: "b",
    toolName: "read",
    result: "B",
    webTiming: { elapsedMs: 9, running: false },
  });
  h.send("message_start", {
    message: assistant(10, [
      {
        type: "toolCall",
        id: "a",
        name: "bash",
        arguments: { command: "pwd" },
      },
      {
        type: "toolCall",
        id: "b",
        name: "read",
        arguments: { path: "b.ts" },
        webTiming: { elapsedMs: 1, running: true },
      },
    ]),
  });
  h.send("tool_execution_start", { toolCallId: "a", toolName: "bash" });
  h.send("tool_execution_update", {
    toolCallId: "a",
    partialResult: { stdout: "A partial" },
  });
  h.send("tool_execution_end", { toolCallId: "a", result: "A", isError: true });
  h.send("tool_execution_start", { toolCallId: "b", toolName: "read" });
  h.send("message_end", { message: assistant(10, []) });
  h.send("message_end", {
    message: {
      role: "toolResult",
      entryId: "b-result",
      toolCallId: "b",
      content: [{ type: "text", text: "B" }],
    },
  });
  expect(nodes(h.state).filter((n) => n.kind === "llm")).toHaveLength(1);
  const tools = nodes(h.state).filter((n) => n.kind === "tool");
  expect(tools).toHaveLength(2);
  expect(tools.find((n) => n.name === "read")).toMatchObject({
    status: "completed",
    output: "B",
    elapsedMs: 9,
    entryId: "b-result",
  });
  expect(tools.find((n) => n.name === "bash")).toMatchObject({
    status: "failed",
    output: "A",
    error: "A",
  });
});

test("native settlement outcomes distinguish failures, stops and unknown without overriding older history", () => {
  for (const [outcome, expected] of [
    ["completed", "completed"],
    ["failed", "failed"],
    ["aborted", "stopped"],
    ["declined", "declined"],
    ["mystery", "unknown"],
  ]) {
    const h = harness();
    h.seed([{ role: "user", entryId: "old", content: "old" }]);
    h.send("agent_start", { runId: "run" });
    h.send("agent_settled", {
      runId: "run",
      outcome,
      error: outcome === "failed" ? "bad" : undefined,
    });
    expect(view(h.state).groups[0].status).toBe("unknown");
    expect(view(h.state).groups[1].status).toBe(expected);
  }
  const h = harness();
  h.send("agent_start");
  h.send("agent_settled", { outcome: "failed", aborted: true });
  expect(view(h.state).groups[0].status).toBe("failed");
});

test("input, tool summaries and errors redact credentials while HTML remains an inert plain string", () => {
  const h = harness();
  h.send("message_start", {
    message: {
      role: "user",
      content:
        'password=hello api_key="secretvalue" Bearer abcdef sk-abcdef1234 <img onerror=alert(1)>',
    },
  });
  h.send("tool_execution_start", {
    toolCallId: "x",
    args: {
      apiKey: "KEYVALUE",
      token: "TOKENVALUE",
      password: "PASSVALUE",
      authorization: "AUTHVALUE",
      cookie: "COOKIEVALUE",
      nested: { useful: "hello" },
    },
  });
  h.send("agent_settled", {
    outcome: "failed",
    error: { token: "ERRORVALUE", message: "sk-othersecret" },
  });
  const serialized = JSON.stringify(h.state);
  for (const value of [
    "hello api",
    "secretvalue",
    "abcdef",
    "KEYVALUE",
    "TOKENVALUE",
    "PASSVALUE",
    "AUTHVALUE",
    "COOKIEVALUE",
    "ERRORVALUE",
    "othersecret",
  ])
    expect(serialized).not.toContain(value);
  expect(nodes(h.state)[0].input).toContain("<img onerror=alert(1)>");
  expect(serialized).toContain("[redacted]");
  expect(serialized).toContain("useful: hello");
});

test("header text redacts complete authorization and multi-cookie values before retention", () => {
  const h = harness();
  const headers = [
    "Authorization: Basic dXNlcjpwYXNz",
    'Proxy-Authorization: Digest username="PRIVATE_USER", response="PRIVATE_RESPONSE"',
    "Cookie: session=PRIVATE_SESSION; csrf=PRIVATE_CSRF",
    "Set-Cookie: auth=PRIVATE_AUTH; Path=/; HttpOnly",
    "Useful following line",
  ].join("\r\n");
  h.send("message_start", { message: { role: "user", content: headers } });
  h.send("tool_execution_end", {
    toolCallId: "headers",
    toolName: "read",
    result: headers,
  });
  h.send("agent_settled", { outcome: "failed", error: headers });
  const serialized = JSON.stringify(h.state);
  expect(serialized).not.toContain("dXNlcjpwYXNz");
  expect(serialized).not.toContain("PRIVATE_");
  expect(serialized).toContain("Authorization: [redacted]");
  expect(serialized).toContain("Cookie: [redacted]");
  expect(serialized).toContain("Useful following line");
});

test("thinking, image/base64 and hidden context never enter state; recursive summaries handle cycles", () => {
  const h = harness();
  const args: Record<string, unknown> = {
    reasoning: "HIDDEN_ARGS",
    fullContext: "HIDDEN_CONTEXT",
    normal: "visible",
  };
  args.self = args;
  h.send("message_start", {
    message: assistant(1, [
      { type: "thinking", thinking: "HIDDEN_THINKING", text: "HIDDEN_TEXT" },
      { type: "image", data: "HIDDEN_IMAGE", mimeType: "image/png" },
      { type: "image_url", image_url: { url: "HIDDEN_URL" } },
      { type: "text", text: "visible <think>HIDDEN_TAG</think> output" },
      { type: "toolCall", id: "safe-call", name: "tool", arguments: args },
    ]),
  });
  h.send("tool_execution_end", {
    toolCallId: "safe-call",
    result: {
      content: [
        { type: "thinking", text: "HIDDEN_RESULT" },
        { type: "image", data: "HIDDEN_RESULT_IMAGE" },
        { type: "text", text: "visible result" },
      ],
      base64: "HIDDEN_BASE64",
    },
  });
  const serialized = JSON.stringify(h.state);
  expect(serialized).not.toContain("HIDDEN_");
  expect(serialized).toContain("[circular]");
  expect(serialized).toContain("visible result");
  expect(nodes(h.state).find((n) => n.kind === "llm")!.output).toBe(
    "visible [omitted] output",
  );
});

test("usage rejects negative/nonfinite/coerced values but preserves explicit zeros and no runtime fallback", () => {
  const s = snapshot(createExecutionTrace(), [
    {
      role: "assistant",
      entryId: "a",
      usage: {
        input: -1,
        output: NaN,
        cacheRead: Infinity,
        cacheWrite: "5",
        totalTokens: -2,
        cost: { total: -1 },
      },
    },
    {
      role: "assistant",
      entryId: "b",
      usage: {
        input: 0,
        output: 0,
        cacheRead: 0,
        cacheWrite: 0,
        totalTokens: 0,
        cost: { total: 0 },
      },
    },
    { role: "assistant", entryId: "c" },
  ]);
  expect(nodes(s)[0].usage).toBeUndefined();
  expect(nodes(s)[1].usage).toEqual({
    input: 0,
    output: 0,
    cached: 0,
    cacheWrite: 0,
    total: 0,
    cost: 0,
  });
  expect(nodes(s)[2].usage).toBeUndefined();
  expect(nodes(s)[2].model).toBeUndefined();
  expect(nodes(s)[2].provider).toBeUndefined();
});

test("tool durations use only validated native webTiming and missing snapshots invent no duration", () => {
  const h = harness();
  h.send("tool_execution_start", {
    toolCallId: "zero",
    webTiming: { elapsedMs: 0, running: true },
  });
  h.send("tool_execution_end", {
    toolCallId: "missing",
    elapsedMs: 500,
    durationMs: 500,
    result: "done",
  });
  h.send("tool_execution_end", {
    toolCallId: "invalid",
    webTiming: { elapsedMs: -5, running: false },
  });
  h.send("tool_execution_end", {
    toolCallId: "bad-running",
    webTiming: { elapsedMs: 20, running: "yes" },
  });
  expect(nodes(h.state).map((n) => n.elapsedMs)).toEqual([
    0,
    undefined,
    undefined,
    undefined,
  ]);
  const before = view(h.state);
  expect(view(h.state)).toEqual(before); // No wall-clock advancement.
  h.seed(
    [
      {
        role: "toolResult",
        entryId: "zero-result",
        toolCallId: "zero",
        content: "result",
      },
    ],
    "refresh",
  );
  expect(
    nodes(h.state).find((n) => n.entryId === "zero-result")!.elapsedMs,
  ).toBe(0);
});

test("native timings attach a final-entry settlement without claiming all unknown history as a run", () => {
  const s = snapshot(createExecutionTrace(), [
    { role: "user", entryId: "old", content: "old question" },
    { ...assistant(1), entryId: "old-answer" },
    { ...assistant(2), entryId: "final" },
  ]);
  const projected = view(s, [timing()]);
  expect(projected.groups[0]).toMatchObject({
    partial: true,
    status: "unknown",
    source: "history",
  });
  expect(projected.groups[0].runId).toBeUndefined();
  expect(projected.groups[0].elapsedMs).toBeUndefined();
  expect(projected.groups[0].nodes.at(-1)).toMatchObject({
    kind: "settlement",
    entryId: "final",
    elapsedMs: 50,
    status: "completed",
  });
  expect(
    projected.groups[0].nodes
      .filter((n) => n.kind === "llm")
      .every((n) => n.elapsedMs === undefined),
  ).toBe(true);
  expect(nodes(s)).toHaveLength(3); // traceView is pure.
});

test("known run timings require exact run/final-entry validation; invalid or unrelated timings stay unknown", () => {
  const h = harness();
  h.send("agent_start", { runId: "run" });
  h.send("message_start", { message: assistant(10) });
  h.send("message_end", { message: { ...assistant(10), entryId: "final" } });
  h.send("agent_settled", { runId: "run", outcome: "completed" });
  for (const bad of [
    timing({ runId: "different" }),
    timing({ finalEntryId: "missing" }),
    timing({ elapsedMs: -1 }),
    timing({ finishedAt: 151 }),
    timing({ elapsedMs: Infinity }),
    timing({ startedAt: -1, finishedAt: 49 }),
    timing({ startedAt: 0.5, finishedAt: 50.5 }),
  ])
    expect(view(h.state, [bad]).groups[0].elapsedMs).toBeUndefined();
  const projected = view(h.state, [timing(), timing()]);
  expect(projected.groups[0].elapsedMs).toBe(50);
  expect(
    projected.groups[0].nodes.filter((n) => n.kind === "settlement"),
  ).toHaveLength(1);
  expect(
    projected.groups[0].nodes.find((n) => n.kind === "llm")!.elapsedMs,
  ).toBeUndefined();
});

test("snapshot then filtered replay preserves order, original state, durable identities and settlement events", () => {
  const initial = createExecutionTrace();
  const seeded = applyTraceSnapshot(
    initial,
    { messages: [{ ...assistant(10), entryId: "final" }], lastSeq: 100 },
    "initial",
  );
  let s = applyTraceEvent(seeded, { type: "agent_start", seq: 1 }, 1);
  s = applyTraceEvent(
    s,
    { type: "message_start", message: assistant(10), seq: 2 },
    2,
  );
  s = applyTraceEvent(
    s,
    { type: "message_end", message: assistant(10), seq: 3 },
    3,
  );
  s = applyTraceEvent(s, { type: "turn_end", seq: 4 }, 4);
  s = applyTraceEvent(
    s,
    { type: "agent_settled", runId: "run", outcome: "completed", seq: 5 },
    5,
  );
  expect(initial.groups).toEqual([]);
  expect(nodes(seeded)).toHaveLength(1);
  expect(nodes(s).filter((n) => n.kind === "llm")).toHaveLength(1);
  expect(nodes(s).find((n) => n.kind === "llm")!.entryId).toBe("final");
  expect(nodes(s).filter((n) => n.kind === "settlement")).toHaveLength(1);
});

test("node/group/text limits clip visibly and identities are evicted with nodes rather than retained in alias maps", () => {
  let s = snapshot(
    createExecutionTrace(),
    Array.from({ length: 400 }, (_, i) => ({
      role: "user",
      entryId: `u-${i}`,
      content: "bounded words ".repeat(1000),
    })),
  );
  expect(nodes(s)).toHaveLength(300);
  expect(view(s).clipped).toBe(true);
  expect(nodes(s).every((n) => (n.input?.length ?? 0) <= 2000)).toBe(true);
  expect(JSON.stringify(s)).not.toContain('"u-0"');
  const h = harness();
  for (let i = 0; i < 75; i++) {
    h.send("agent_start", { runId: `run-${i}` });
    h.send("tool_execution_end", {
      toolCallId: `call-${i}`,
      result: `result-${i}`,
    });
    h.send("agent_settled", { runId: `run-${i}`, outcome: "completed" });
  }
  s = h.state;
  expect(view(s).groups.length).toBeLessThanOrEqual(50);
  expect(nodes(s).length).toBeLessThanOrEqual(300);
  expect(view(s).clipped).toBe(true);
  expect(JSON.stringify(s)).not.toContain('"call-0"');
  expect(JSON.stringify(s)).not.toContain('"run-0"');
  expect(
    Object.values(s).some((v) => v instanceof Map || v instanceof Set),
  ).toBe(false);
});

test("a changed metadata-free snapshot window cannot overwrite earlier messages by array offset", () => {
  const old = snapshot(createExecutionTrace(), [
    { role: "user", content: "Earlier instruction" },
    assistant(),
  ]);
  const next = snapshot(
    old,
    [
      { role: "user", content: "Different instruction" },
      assistant(undefined, [{ type: "text", text: "Different answer" }]),
    ],
    "refresh",
  );
  expect(
    nodes(next)
      .filter((n) => n.kind === "input")
      .map((n) => n.input),
  ).toEqual(["Earlier instruction", "Different instruction"]);
  expect(nodes(old).find((n) => n.kind === "input")?.input).toBe(
    "Earlier instruction",
  );
  const repeated = snapshot(
    next,
    [
      { role: "user", content: "Different instruction" },
      assistant(undefined, [{ type: "text", text: "Different answer" }]),
    ],
    "refresh",
  );
  expect(nodes(repeated)).toHaveLength(4);
});

test("snapshot settlement stays separate from unknown history and deduplicates replay by exact run identity", () => {
  const frame = {
    messages: [{ ...assistant(10), entryId: "final" }],
    latestSettlement: {
      runId: "run",
      outcome: "failed",
      error: "Native failure",
      finalText: "Final summary",
    },
  };
  let s = applyTraceSnapshot(createExecutionTrace(), frame, "initial");
  expect(view(s).groups).toHaveLength(2);
  expect(view(s).groups[0].status).toBe("unknown");
  expect(view(s).groups[1]).toMatchObject({
    runId: "run",
    status: "failed",
    partial: true,
  });
  expect(nodes(s).find((n) => n.kind === "settlement")).toMatchObject({
    error: "Native failure",
    output: "Final summary",
  });
  s = applyTraceEvent(s, { type: "agent_start", runId: "run", seq: 1 }, 1);
  s = applyTraceEvent(
    s,
    { type: "message_start", message: assistant(10), seq: 2 },
    2,
  );
  s = applyTraceEvent(
    s,
    { type: "agent_settled", runId: "run", outcome: "failed", seq: 3 },
    3,
  );
  s = applyTraceSnapshot(s, frame, "refresh");
  expect(nodes(s).filter((n) => n.kind === "llm")).toHaveLength(1);
  expect(nodes(s).filter((n) => n.kind === "settlement")).toHaveLength(1);
  expect(view(s).groups.some((g) => g.status === "running")).toBe(false);
  expect(view(s, [timing()]).groups[1].elapsedMs).toBe(50);
  expect(
    view(s, [timing()])
      .groups.flatMap((g) => g.nodes)
      .filter((n) => n.kind === "settlement"),
  ).toHaveLength(1);
});

test("snapshot root from another run never settles the currently observed execution", () => {
  const h = harness();
  h.send("agent_start", { runId: "new-run" });
  const s = applyTraceSnapshot(
    h.state,
    {
      messages: [],
      latestSettlement: { runId: "old-run", outcome: "completed" },
    },
    "refresh",
  );
  expect(view(s).groups.find((g) => g.runId === "new-run")?.status).toBe(
    "running",
  );
  expect(view(s).groups.find((g) => g.runId === "old-run")?.status).toBe(
    "completed",
  );
});

test("anonymous native start then accepted identity joins a snapshot settlement without phantom running state", () => {
  let s = applyTraceSnapshot(
    createExecutionTrace(),
    { messages: [], latestSettlement: { runId: "run", outcome: "failed" } },
    "initial",
  );
  s = applyTraceEvent(s, { type: "agent_start", seq: 1 }, 1);
  s = applyTraceEvent(
    s,
    { type: "retry_scheduled", error: "native retry", seq: 2 },
    2,
  );
  s = applyTraceEvent(
    s,
    { type: "operation_accepted", runId: "run", seq: 3 },
    3,
  );
  s = applyTraceEvent(
    s,
    { type: "agent_settled", runId: "run", outcome: "failed", seq: 4 },
    4,
  );
  expect(view(s).groups).toHaveLength(1);
  expect(view(s).groups[0]).toMatchObject({
    runId: "run",
    status: "failed",
    partial: true,
  });
  expect(nodes(s).map((n) => n.kind)).toEqual(["retry", "settlement"]);
});
