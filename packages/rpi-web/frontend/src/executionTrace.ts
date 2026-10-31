import type { RunTiming, SeqEvent } from "./api";

export type TraceStatus =
  "running" | "completed" | "failed" | "stopped" | "declined" | "unknown";
export type TraceUsage = {
  input?: number;
  output?: number;
  cached?: number;
  cacheWrite?: number;
  total?: number;
  cost?: number;
};
export interface TraceNode {
  id: string;
  kind: "input" | "llm" | "tool" | "retry" | "settlement" | "notice";
  status: TraceStatus;
  entryId?: string;
  runId?: string;
  name?: string;
  model?: string;
  provider?: string;
  input?: string;
  output?: string;
  error?: string;
  elapsedMs?: number;
  startedAt?: number;
  finishedAt?: number;
  parentId?: string;
  usage?: TraceUsage;
  attempt?: number;
  maxRetries?: number;
  delayMs?: number;
}
export interface TraceGroup {
  id: string;
  source: "live" | "history";
  partial: boolean;
  runId?: string;
  status: TraceStatus;
  elapsedMs?: number;
  startedAt?: number;
  finishedAt?: number;
  nodes: TraceNode[];
}
export interface TraceView {
  groups: TraceGroup[];
  clipped: boolean;
}

type RecordValue = Record<string, unknown>;
type Node = TraceNode & {
  role?: string;
  stamp?: number;
  call?: string;
  calls?: string[];
  slot?: string;
  ended?: boolean;
};
type Group = Omit<TraceGroup, "nodes"> & { nodes: Node[] };
/** Sanitized projection only. No native messages, deltas, clocks or unbounded aliases. */
export type ExecutionTraceState = {
  groups: Group[];
  clipped: boolean;
  nextId: number;
  olderPage: number;
  lastSeq: number;
  active?: string;
  user?: string;
  assistant?: string;
};
type Frame = {
  messages: unknown[];
  latestSettlement?: RecordValue | null;
  status?: string;
  lastSeq?: number;
};
type Mode = "initial" | "refresh" | "older";
const MAX_NODES = 300,
  MAX_GROUPS = 50,
  MAX_TEXT = 2000;
const record = (v: unknown): RecordValue =>
  v && typeof v === "object" && !Array.isArray(v) ? (v as RecordValue) : {};
const number = (v: unknown): number | undefined =>
  typeof v === "number" && Number.isFinite(v) && v >= 0 ? v : undefined;
const hidden =
  /thinking|reasoning|signature|image|base64|^data$|context|^messages$|^prompt$/i;
const secret = /key|token|password|authorization|cookie|secret/i;

function text(value: unknown): string | undefined {
  if (typeof value !== "string") return;
  // Inspect only a bounded prefix; never retain an image, credential or reasoning block.
  const clean = value
    .slice(0, 8192)
    .replace(
      /<(think|thinking|reasoning)\b[^>]*>[\s\S]*?(?:<\/\1>|$)/gi,
      "[omitted]",
    )
    .replace(/data:[^\s,]*;base64,[a-z\d+/=]*/gi, "[image omitted]")
    // Header values can contain spaces, commas and multiple semicolon-separated
    // cookies. Redacting just their first token would retain real credentials.
    .replace(
      /\b((?:proxy-)?authorization|(?:set-)?cookie)(["']?\s*[:=]\s*)[^\r\n]*/gi,
      "$1$2[redacted]",
    )
    .replace(/\bBearer\s+[^\s"'<>]+/gi, "Bearer [redacted]")
    .replace(/\bsk-[a-z\d_-]+/gi, "[redacted]")
    .replace(
      /((?:api[_-]?)?key|token|password|authorization|cookie|secret)(["']?\s*[:=]\s*["']?)[^\s,;"'<>}]+/gi,
      "$1$2[redacted]",
    )
    .replace(/[a-z\d+/=]{160,}/gi, "[encoded data omitted]")
    .replace(/[\u0000-\u0008\u000b\u000c\u000e-\u001f]/g, "");
  return clean.length > MAX_TEXT || value.length > 8192
    ? clean.slice(0, MAX_TEXT - 1) + "…"
    : clean;
}
function identity(value: unknown): string | undefined {
  return typeof value === "string" &&
    value.length > 0 &&
    value.length <= 256 &&
    text(value) === value
    ? value
    : undefined;
}
/** Bounded traversal, including cycles. Text blocks only; never serialize raw objects. */
function summary(value: unknown): string | undefined {
  const seen = new WeakSet<object>();
  let visits = 0;
  function walk(v: unknown, depth: number): string {
    if (++visits > 64 || depth > 3) return "…";
    if (typeof v === "string") {
      if (v.length <= 8192 && /^\s*[\[{]/.test(v)) {
        try {
          const parsed: unknown = JSON.parse(v);
          if (parsed && typeof parsed === "object")
            return walk(parsed, depth + 1);
        } catch {
          /* Ordinary tool text, not a JSON transport envelope. */
        }
      }
      return text(v) ?? "";
    }
    if (typeof v === "number") return Number.isFinite(v) ? String(v) : "";
    if (typeof v === "boolean") return String(v);
    if (!v || typeof v !== "object") return "";
    if (seen.has(v)) return "[circular]";
    seen.add(v);
    if (Array.isArray(v)) {
      const parts: string[] = [];
      for (let i = 0; i < Math.min(v.length, 16); i++) {
        const part = walk(v[i], depth + 1);
        if (part) parts.push(part);
      }
      return parts.join("\n").slice(0, MAX_TEXT);
    }
    const r = record(v);
    if (typeof r.type === "string" && hidden.test(r.type)) return "";
    if (r.type === "text") return text(r.text) ?? "";
    const parts: string[] = [];
    let keys = 0;
    for (const key in r) {
      if (!Object.prototype.hasOwnProperty.call(r, key)) continue;
      if (++keys > 12) break;
      if (hidden.test(key)) continue;
      const part = secret.test(key) ? "[redacted]" : walk(r[key], depth + 1);
      if (part) parts.push(`${text(key.slice(0, 80))}: ${part}`);
    }
    return parts.join("\n").slice(0, MAX_TEXT);
  }
  return text(walk(value, 0)) || undefined;
}
function messageText(content: unknown): string | undefined {
  if (typeof content === "string") return text(content);
  if (!Array.isArray(content)) return;
  let result = "";
  for (
    let i = 0;
    i < Math.min(content.length, 64) && result.length < MAX_TEXT;
    i++
  ) {
    const block = record(content[i]);
    if (block.type === "text" && typeof block.text === "string")
      result += (result ? "\n" : "") + (text(block.text) ?? "");
  }
  return text(result) || undefined;
}
function usage(value: unknown): TraceUsage | undefined {
  const r = record(value),
    cost = record(r.cost);
  const u: TraceUsage = {
    input: number(r.input),
    output: number(r.output),
    cached: number(r.cacheRead),
    cacheWrite: number(r.cacheWrite),
    total: number(r.totalTokens),
    cost: number(cost.total) ?? number(r.cost),
  };
  return Object.values(u).some((v) => v !== undefined) ? u : undefined;
}
function status(value: unknown, aborted?: unknown): TraceStatus {
  if (value === "failed" || value === "error") return "failed";
  if (value === "declined") return "declined";
  if (
    aborted === true ||
    (typeof value === "string" &&
      ["aborted", "stopped", "cancelled", "interrupted"].includes(value))
  )
    return "stopped";
  if (
    value === "completed" ||
    value === "success" ||
    value === "stop" ||
    value === "toolUse"
  )
    return "completed";
  return value === "running" ? "running" : "unknown";
}
export function createExecutionTrace(): ExecutionTraceState {
  return { groups: [], clipped: false, nextId: 0, olderPage: 0, lastSeq: -1 };
}
function copy(s: ExecutionTraceState): ExecutionTraceState {
  return {
    ...s,
    groups: s.groups.map((g) => ({
      ...g,
      nodes: g.nodes.map((n) => ({ ...n })),
    })),
  };
}
function find(s: ExecutionTraceState, id?: string): Node | undefined {
  return id
    ? s.groups.flatMap((g) => g.nodes).find((n) => n.id === id)
    : undefined;
}
function active(s: ExecutionTraceState): Group | undefined {
  return s.groups.find((g) => g.id === s.active && g.status === "running");
}
function group(
  s: ExecutionTraceState,
  source: "live" | "history",
  prepend = false,
): Group {
  const g: Group = {
    id: `g${++s.nextId}`,
    source,
    partial: source === "history",
    status: source === "live" ? "running" : "unknown",
    nodes: [],
  };
  if (prepend) s.groups.unshift(g);
  else s.groups.push(g);
  return g;
}
function liveGroup(s: ExecutionTraceState): Group {
  // A completed group may be the context of snapshot replay; observing its old
  // steps must not manufacture a second, currently-running execution.
  const existing = s.groups.find((g) => g.id === s.active);
  if (existing) return existing;
  const g = group(s, "live");
  g.partial = true; // Message/tool observed without a native start boundary.
  s.active = g.id;
  return g;
}
function add(
  s: ExecutionTraceState,
  g: Group,
  kind: TraceNode["kind"],
  initial: TraceStatus,
): Node {
  const n: Node = { id: `n${++s.nextId}`, kind, status: initial };
  g.nodes.push(n);
  return n;
}
function bound(s: ExecutionTraceState): ExecutionTraceState {
  s.groups = s.groups.filter((g) => g.nodes.length || g.id === s.active);
  while (s.groups.length > MAX_GROUPS) {
    s.groups.shift();
    s.clipped = true;
  }
  let excess = s.groups.reduce((sum, g) => sum + g.nodes.length, 0) - MAX_NODES;
  for (const g of s.groups) {
    if (excess <= 0) break;
    const count = Math.min(excess, g.nodes.length);
    g.nodes.splice(0, count);
    g.partial = true;
    s.clipped = true;
    excess -= count;
  }
  s.groups = s.groups.filter((g) => g.nodes.length || g.id === s.active);
  if (!s.groups.some((g) => g.id === s.active)) s.active = undefined;
  if (!find(s, s.user)) s.user = undefined;
  if (!find(s, s.assistant)) s.assistant = undefined;
  // All identities live on the retained nodes: eviction removes them too.
  return s;
}
function tool(
  s: ExecutionTraceState,
  g: Group,
  call: string | undefined,
  initial: TraceStatus,
): Node {
  const existing =
    call &&
    s.groups
      .flatMap((item) => item.nodes)
      .find((n) => n.kind === "tool" && n.call === call);
  if (existing) return existing;
  const n = add(s, g, "tool", initial);
  n.call = call;
  return n;
}
function toolTiming(n: Node, value: unknown) {
  const r = record(value),
    elapsed = number(r.elapsedMs);
  if (
    elapsed !== undefined &&
    typeof r.running === "boolean" &&
    !(n.ended && r.running)
  ) {
    n.elapsedMs = elapsed;
    if (!n.ended && r.running) n.status = "running";
  }
}
function callsOf(r: RecordValue): RecordValue[] {
  if (!Array.isArray(r.content)) return [];
  const calls: RecordValue[] = [];
  for (let i = 0; i < Math.min(r.content.length, 64); i++) {
    const b = record(r.content[i]);
    if (b.type === "toolCall") calls.push(b);
    if (calls.length === 32) break;
  }
  return calls;
}
function match(
  s: ExecutionTraceState,
  r: RecordValue,
  slot?: string,
): Node | undefined {
  const role = r.role,
    entry = identity(r.entryId),
    stamp = number(r.timestamp);
  if (role === "toolResult" && identity(r.toolCallId)) {
    const result = s.groups
      .flatMap((g) => g.nodes)
      .find((n) => n.kind === "tool" && n.call === r.toolCallId);
    if (result) return result;
  }
  const nodes = s.groups.flatMap((g) => g.nodes).filter((n) => n.role === role);
  if (entry) {
    const exact = nodes.find((n) => n.entryId === entry);
    if (exact) return exact;
  }
  const calls = callsOf(r)
    .map((c) => identity(c.id))
    .filter((id): id is string => !!id);
  const candidates = nodes.filter((n) => {
    if (entry && n.entryId && n.entryId !== entry) return false;
    if (n.model && identity(r.model) && n.model !== r.model) return false;
    if (n.provider && identity(r.provider) && n.provider !== r.provider)
      return false;
    if (stamp !== undefined && n.stamp !== undefined)
      return (
        n.stamp === stamp &&
        (!calls.length ||
          !n.calls?.length ||
          calls.some((id) => n.calls?.includes(id)))
      );
    return calls.length > 0 && calls.some((id) => n.calls?.includes(id));
  });
  if (candidates.length === 1) return candidates[0];
  // Page-local slots are only for metadata-free history, never content/live matching.
  if (!entry && stamp === undefined && slot)
    return nodes.find(
      (n) => !n.entryId && n.stamp === undefined && n.slot === slot,
    );
}
function message(
  s: ExecutionTraceState,
  r: RecordValue,
  g: Group,
  end: boolean,
  slot?: string,
  current?: string,
): Node | undefined {
  if (r.role === "toolResult") {
    const call = identity(r.toolCallId);
    const n = call
      ? tool(s, g, call, "unknown")
      : (match(s, r, slot) ?? add(s, g, "tool", "unknown"));
    n.role = "toolResult";
    n.slot ??= slot;
    n.entryId = identity(r.entryId) ?? n.entryId;
    n.name = text(r.toolName) ?? n.name;
    n.output = summary(r.content) ?? n.output;
    n.status = r.isError === true ? "failed" : "completed";
    n.error = r.isError === true ? (text(r.errorMessage) ?? n.output) : n.error;
    n.ended = true;
    toolTiming(n, r.webTiming);
    return n;
  }
  if (r.role !== "assistant" && r.role !== "user") return;
  const n =
    find(s, current) ??
    match(s, r, slot) ??
    add(s, g, r.role === "user" ? "input" : "llm", end ? "unknown" : "running");
  n.role = r.role;
  n.slot ??= slot;
  n.stamp = number(r.timestamp) ?? n.stamp;
  n.entryId = identity(r.entryId) ?? n.entryId;
  const owner = s.groups.find((item) => item.nodes.includes(n)) ?? g;
  if (!slot && owner !== g) {
    owner.partial = true;
    g.partial = true;
  }
  if (r.role === "user") {
    n.input = messageText(r.content) ?? n.input;
    n.status = "completed";
    return n;
  }
  n.model = text(r.model) ?? n.model;
  n.provider = text(r.provider) ?? n.provider;
  // A stale snapshot/replayed partial cannot overwrite a completed live response.
  const preserve = n.ended && (slot !== undefined || !end);
  const output = messageText(r.content),
    u = usage(r.usage),
    error = text(r.errorMessage);
  n.output = preserve ? (n.output ?? output) : (output ?? n.output);
  n.usage = preserve ? (n.usage ?? u) : (u ?? n.usage);
  n.error = preserve ? (n.error ?? error) : (error ?? n.error);
  const finalStatus = status(r.stopReason);
  if (end && !preserve) {
    n.ended = true;
    n.status =
      finalStatus === "unknown"
        ? n.error
          ? "failed"
          : slot === undefined
            ? "completed"
            : "unknown"
        : finalStatus;
  } else if (!n.ended) n.status = "running";
  const calls = callsOf(r);
  if (
    (Array.isArray(r.content) && r.content.length > 64) ||
    calls.length === 32
  )
    s.clipped = true;
  const ids = calls
    .map((c) => identity(c.id))
    .filter((id): id is string => !!id);
  if (ids.length) n.calls = ids;
  for (const c of calls) {
    const id = identity(c.id);
    if (!id) continue;
    const t = tool(s, owner, id, "unknown");
    t.parentId = n.id;
    t.name = text(c.name) ?? t.name;
    t.input = summary(c.arguments) ?? t.input;
    toolTiming(t, c.webTiming);
  }
  return n;
}

function pageScope(messages: unknown[], start: number): string {
  // Missing native identity must not let a sliding snapshot overwrite another
  // message at the same array offset. Slots dedupe only the same loaded window.
  let hash = 2166136261;
  for (let i = start; i < messages.length; i++) {
    const r = record(messages[i]);
    const signature = [
      r.role,
      identity(r.entryId),
      number(r.timestamp),
      text(r.model),
      text(r.provider),
      identity(r.toolCallId),
      messageText(r.content),
      ...callsOf(r).map((c) => identity(c.id)),
    ].join("\n");
    for (let j = 0; j < signature.length; j++)
      hash = Math.imul(hash ^ signature.charCodeAt(j), 16777619);
  }
  return (hash >>> 0).toString(36);
}
function settle(s: ExecutionTraceState, g: Group, event: RecordValue) {
  g.status = status(event.outcome, event.aborted);
  let n = g.nodes.find((item) => item.kind === "settlement");
  if (!n) n = add(s, g, "settlement", g.status);
  n.status = g.status;
  n.runId = g.runId;
  n.output = text(event.summary) ?? text(event.finalText) ?? n.output;
  n.error = summary(event.error) ?? n.error;
  for (const item of g.nodes)
    if (item.status === "running")
      item.status = g.status === "completed" ? "unknown" : g.status;
  if (s.active === g.id) {
    s.active = undefined;
    s.user = undefined;
    s.assistant = undefined;
  }
}
export function applyTraceSnapshot(
  state: ExecutionTraceState,
  frame: Frame,
  mode: Mode,
): ExecutionTraceState {
  const s = copy(state);
  let history: Group | undefined;
  const messages = Array.isArray(frame.messages) ? frame.messages : [];
  const start = Math.max(0, messages.length - MAX_NODES);
  // A page is not a run. Unidentified older pages get their own local scope.
  const prefix =
    mode === "older"
      ? `older${++s.olderPage}`
      : `page:${pageScope(messages, start)}`;
  if (start) s.clipped = true;
  for (let i = start; i < messages.length; i++) {
    const r = record(messages[i]);
    if (r.role !== "assistant" && r.role !== "user" && r.role !== "toolResult")
      continue;
    const slot = `${prefix}:${i}`;
    const known = match(s, r, slot);
    const owner = known && s.groups.find((g) => g.nodes.includes(known));
    if (!owner && (!history || !s.groups.includes(history))) {
      history = group(s, "history", mode === "older");
      if (active(s)) active(s)!.partial = true;
      // Unassociated durable history precedes observed live activity.
      if (mode !== "older") {
        s.groups.pop();
        const live = s.groups.findIndex((g) => g.source === "live");
        s.groups.splice(live < 0 ? s.groups.length : live, 0, history);
      }
    }
    message(s, r, owner || history!, true, slot);
    bound(s);
  }
  // Snapshot settlement is authoritative too, but only for its exact run ID.
  // Without a matching observed run, keep it in a separate partial fragment;
  // never declare the earlier loaded messages to belong to that execution.
  const latest = record(frame.latestSettlement);
  if (mode !== "older" && Object.keys(latest).length) {
    const runId = identity(latest.runId);
    let g = runId
      ? s.groups.find((item) => item.runId === runId)
      : s.groups.find((item) =>
          item.nodes.some((n) => n.slot === "latest-settlement"),
        );
    if (!g) {
      g = group(s, "history");
      g.runId = runId;
    }
    settle(s, g, latest);
    if (!runId)
      g.nodes.find((n) => n.kind === "settlement")!.slot = "latest-settlement";
  }
  // Snapshot lastSeq is NOT a replay cursor: parent forwards settlement/turn events.
  return bound(s);
}
export function applyTraceEvent(
  state: ExecutionTraceState,
  event: SeqEvent,
  seq: number,
): ExecutionTraceState {
  if (!Number.isSafeInteger(seq) || seq < 0 || seq <= state.lastSeq)
    return state;
  const s = copy(state);
  s.lastSeq = seq;
  const runId = identity(event.runId);
  if (event.type === "agent_start" || event.type === "operation_accepted") {
    let g = active(s);
    if (g && runId && g.runId && runId !== g.runId) {
      g.partial = true;
      g.status = "unknown";
      g = undefined;
    }
    const known = runId
      ? s.groups.find((item) => item.runId === runId)
      : undefined;
    if (known && g && known !== g && !g.runId) {
      // Native agent_start can precede operation_accepted and has no run ID.
      // Join that anonymous replay context to the snapshot's exact settlement,
      // rather than leaving a phantom running group with the same run ID.
      known.nodes = [...g.nodes, ...known.nodes];
      known.partial ||= g.partial;
      s.groups = s.groups.filter((item) => item !== g);
      g = known;
    }
    if (!g && known) g = known;
    if (!g) {
      g = group(s, "live");
      s.user = undefined;
      s.assistant = undefined;
    }
    g.runId = runId ?? g.runId;
    s.active = g.id;
  } else if (event.type === "agent_settled") {
    let g = runId
      ? s.groups.find((item) => item.runId === runId)
      : s.groups.find((item) => item.id === s.active);
    if (!g && active(s) && (!runId || !active(s)!.runId)) g = active(s);
    if (!g) {
      g = group(s, "live");
      g.partial = true;
    }
    g.runId = runId ?? g.runId;
    settle(s, g, event);
  } else if (event.type === "retry_scheduled") {
    const n = add(s, liveGroup(s), "retry", "completed");
    n.error = summary(event.error);
    n.attempt = number(event.attempt);
    n.maxRetries = number(event.maxRetries);
    n.delayMs = number(event.delayMs);
  } else if (
    ["message_start", "message_update", "message_end"].includes(event.type)
  ) {
    const r = record(event.message),
      role = r.role;
    if (role !== "assistant" && role !== "user" && role !== "toolResult")
      return bound(s);
    const current =
      event.type === "message_start"
        ? undefined
        : role === "assistant"
          ? s.assistant
          : role === "user"
            ? s.user
            : undefined;
    const known = find(s, current) ?? match(s, r);
    const owner = known && s.groups.find((g) => g.nodes.includes(known));
    const n = message(
      s,
      r,
      active(s) ?? owner ?? liveGroup(s),
      event.type === "message_end",
      undefined,
      current,
    );
    if (role === "assistant")
      s.assistant = event.type === "message_end" ? undefined : n?.id;
    if (role === "user")
      s.user = event.type === "message_end" ? undefined : n?.id;
  } else if (event.type.startsWith("tool_execution_")) {
    const call = identity(event.toolCallId);
    const owner = call
      ? s.groups.find((g) =>
          g.nodes.some((n) => n.kind === "tool" && n.call === call),
        )
      : undefined;
    const n = tool(s, active(s) ?? owner ?? liveGroup(s), call, "unknown");
    n.name = text(event.toolName) ?? n.name;
    n.input = summary(event.args) ?? n.input;
    const end = event.type === "tool_execution_end";
    // A late start/update must not overwrite an already observed result.
    if (!n.ended || end) {
      n.output = summary(end ? event.result : event.partialResult) ?? n.output;
      n.status = end
        ? event.isError === true
          ? "failed"
          : "completed"
        : "running";
      if (end) {
        n.ended = true;
        n.error = event.isError === true ? n.output : undefined;
      }
      toolTiming(n, event.webTiming);
    }
  }
  // A closed host cannot keep unfinished observations running, but transport
  // closure is not an authoritative run settlement or a duration measurement.
  if (event.type === "host_closed") {
    for (const g of s.groups) {
      if (g.status === "running") {
        g.status = "unknown";
        g.partial = true;
      }
      for (const n of g.nodes)
        if (n.status === "running") n.status = "unknown";
    }
    s.active = undefined;
    s.user = undefined;
    s.assistant = undefined;
  }
  // agent_end and turn_end are not settlement, nor LLM duration measurements.
  return bound(s);
}
function validTiming(t: RunTiming): boolean {
  return (
    !!identity(t?.runId) &&
    !!identity(t?.finalEntryId) &&
    Number.isSafeInteger(t.elapsedMs) &&
    t.elapsedMs >= 0 &&
    Number.isSafeInteger(t.startedAt) &&
    t.startedAt >= 0 &&
    Number.isSafeInteger(t.finishedAt) &&
    t.finishedAt >= t.startedAt &&
    t.finishedAt - t.startedAt === t.elapsedMs &&
    ["completed", "aborted", "failed", "declined"].includes(t.outcome)
  );
}
export function traceView(
  state: ExecutionTraceState,
  timings: RunTiming[],
): TraceView {
  const groups: TraceGroup[] = state.groups.map((g) => ({
    ...g,
    nodes: g.nodes.map((n) => {
      const {
        role: _role,
        stamp: _stamp,
        call: _call,
        calls: _calls,
        slot: _slot,
        ended: _ended,
        ...node
      } = n;
      return { ...node, usage: node.usage && { ...node.usage } };
    }),
  }));
  const seen = new Set<string>();
  let count = groups.reduce((sum, g) => sum + g.nodes.length, 0),
    clipped = state.clipped;
  for (const t of timings.slice(-MAX_NODES)) {
    if (!validTiming(t) || seen.has(t.runId)) continue;
    const matches = groups.flatMap((g) =>
      g.nodes
        .filter((n) => n.entryId === t.finalEntryId)
        .map((n) => ({ g, n })),
    );
    if (matches.length !== 1) continue;
    const { g, n } = matches[0];
    if (g.runId && g.runId !== t.runId) continue;
    seen.add(t.runId);
    const run = groups.find((item) => item.runId === t.runId);
    const settlement = run?.nodes.find((item) => item.kind === "settlement");
    if (run) {
      run.elapsedMs = t.elapsedMs;
      run.startedAt = t.startedAt;
      run.finishedAt = t.finishedAt;
    }
    if (settlement) {
      settlement.elapsedMs = t.elapsedMs;
      settlement.startedAt = t.startedAt;
      settlement.finishedAt = t.finishedAt;
    } else if (count < MAX_NODES) {
      g.nodes.splice(g.nodes.indexOf(n) + 1, 0, {
        id: `timing-${t.runId}`,
        kind: "settlement",
        runId: t.runId,
        entryId: t.finalEntryId,
        status: status(t.outcome),
        elapsedMs: t.elapsedMs,
        startedAt: t.startedAt,
        finishedAt: t.finishedAt,
      });
      count++;
    } else clipped = true;
    // Unknown history remains partial/unknown; never label its earlier messages as this run.
  }
  return { groups, clipped };
}
