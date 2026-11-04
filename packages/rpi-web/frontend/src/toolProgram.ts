import {
  ApiError,
  api,
  type ToolProgramSnapshot,
  type ToolProgramPhase,
  type ToolProgramStreamItem,
  type ToolProgramChildEvent,
  type ToolProgramBodyResponse,
  type ToolProgramCursor,
} from "./api";

const record = (v: unknown): Record<string, unknown> =>
  v && typeof v === "object" && !Array.isArray(v)
    ? (v as Record<string, unknown>)
    : {};
const integer = (v: unknown) =>
  typeof v === "number" && Number.isSafeInteger(v) && v >= 0;
export class ChildResync extends Error {}
/** Frontend-only provenance for rendered overlays, never a native wire field. */
export type ToolProgramView = ToolProgramSnapshot & {
  overlayPhaseIds: string[];
};
/** Preserve the core cursor verbatim; never rebase a leaf/session/child binding. */
export function validateChildCursor(
  cursor: ToolProgramCursor | null | undefined,
  sessionId: string,
  leafId: string | null | undefined,
  childId: string | null = null,
) {
  if (
    cursor &&
    (cursor.sessionId !== sessionId ||
      cursor.lane !== "main" ||
      cursor.leafId !== leafId ||
      cursor.childId !== childId ||
      cursor.direction !== "newestFirst" ||
      !integer(cursor.afterSeq) ||
      typeof cursor.afterEntryId !== "string" ||
      !cursor.afterEntryId)
  )
    throw new Error("Invalid child cursor binding");
}
export const childUnsupported = (error: unknown) =>
  error instanceof ApiError && [404, 405, 501].includes(error.status);

/** Exact native call ID from the existing frontend row contract, not names/ordinals. */
export function parentToolCallId(row: {
  kind: string;
  id: string;
  name?: string;
  nativeToolCallId?: string;
}): string | null {
  // ToolProgram records are rooted at Code Mode's `program` tool. A native
  // call ID exists on every ordinary tool too, so it is not an eligibility
  // signal by itself.
  return row.kind === "tool" && row.name === "program" && row.nativeToolCallId
    ? row.nativeToolCallId
    : null;
}

export function validateChildSnapshot(
  value: unknown,
  sessionId: string,
): ToolProgramSnapshot {
  const v = record(value),
    p = record(v.page);
  if (
    v.sessionId !== sessionId ||
    v.lane !== "main" ||
    !integer(v.lastSeq) ||
    !Array.isArray(p.children) ||
    p.children.length > 8 ||
    !Array.isArray(v.phases) ||
    v.phases.length > 8 ||
    !Array.isArray(v.observations) ||
    !Array.isArray(v.events) ||
    typeof p.scanExhausted !== "boolean"
  )
    throw new Error("Invalid child snapshot");
  validateChildCursor(
    p.nextCursor as ToolProgramCursor | null,
    sessionId,
    p.leafId as string | null,
  );
  for (const child of p.children) {
    const c = record(child);
    if (
      typeof c.childId !== "string" ||
      typeof c.rootCallId !== "string" ||
      typeof c.rootToolName !== "string" ||
      typeof c.toolName !== "string" ||
      !integer(c.seq)
    )
      throw new Error("Invalid child identity");
  }
  for (const page of v.phases as ToolProgramSnapshot["phases"]) {
    if (
      !p.children.some((c) => record(c).childId === page.childId) ||
      page.leafId !== p.leafId ||
      !Array.isArray(page.phases) ||
      page.phases.length > 8
    )
      throw new Error("Invalid child phase page");
    validateChildCursor(page.nextCursor, sessionId, page.leafId, page.childId);
  }
  if (v.resyncRequired === true)
    throw new ChildResync("Child snapshot requires refresh");
  return value as ToolProgramSnapshot;
}

function eventPhase(
  event: ToolProgramChildEvent,
  sessionId: string,
): ToolProgramPhase | null {
  if (event.type === "tool_program_child_resync_required")
    throw new ChildResync("Native child resync");
  if (event.sourceSessionId !== sessionId || event.lane !== "main")
    throw new ChildResync("Child binding changed");
  if (event.type !== "tool_program_child_phase") return null;
  const d = record(event.data);
  if (
    typeof d.childId !== "string" ||
    typeof event.entryId !== "string" ||
    !integer(event.seq) ||
    !integer(d.index)
  )
    throw new ChildResync("Invalid child phase");
  return {
    ...d,
    entryId: event.entryId,
    seq: event.seq,
  } as unknown as ToolProgramPhase;
}
function mergePhase(
  snapshot: ToolProgramView,
  phase: ToolProgramPhase,
): ToolProgramView {
  if (!snapshot.page.children.some((c) => c.childId === phase.childId))
    throw new ChildResync("Child outside bounded window");
  const overlays = new Set(snapshot.overlayPhaseIds);
  const phases = snapshot.phases.map((page) => {
    if (page.childId !== phase.childId) return page;
    const old = page.phases.find((p) => p.entryId === phase.entryId);
    if (old) {
      if (JSON.stringify(old) !== JSON.stringify(phase)) {
        // Property order is not identity. Core owns canonical conflict checks.
        if (
          old.seq !== phase.seq ||
          old.index !== phase.index ||
          old.phase !== phase.phase ||
          old.descriptor?.descriptorId !== phase.descriptor?.descriptorId
        )
          throw new ChildResync("Child phase identity conflict");
      }
      return page;
    }
    overlays.add(phase.entryId);
    return {
      ...page,
      phases: [phase, ...page.phases].sort((a, b) => b.seq - a.seq).slice(0, 8),
    };
  });
  // Keep only the bounded rendered window; pinned historical cursors/leaves do
  // not become valid for an overlay merely because its phase was observed.
  const retained = new Set(
    phases.flatMap((page) => page.phases.map((p) => p.entryId)),
  );
  return {
    ...snapshot,
    phases,
    overlayPhaseIds: [...overlays].filter((id) => retained.has(id)),
  };
}
/** Merge only dedicated metadata. Never manufacture transcript messages or outcomes. */
export function seedChildSnapshot(
  value: unknown,
  sessionId: string,
): ToolProgramView {
  let snapshot: ToolProgramView = {
    ...validateChildSnapshot(value, sessionId),
    overlayPhaseIds: [],
  };
  for (const raw of snapshot.events) {
    const replay = record(raw);
    if (
      replay.disposition === "resyncRequired" ||
      replay.disposition === "outsideWindow"
    )
      throw new ChildResync("Child replay requires refresh");
    const phase = eventPhase(replay.event as ToolProgramChildEvent, sessionId);
    if (phase) snapshot = mergePhase(snapshot, phase);
  }
  return snapshot;
}
export function applyChildUpdate(
  snapshot: ToolProgramView,
  item: ToolProgramStreamItem,
): ToolProgramView {
  if (
    item.type !== "update" ||
    item.disposition === "resyncRequired" ||
    item.disposition === "outsideWindow" ||
    !item.event ||
    !integer(item.seq)
  )
    throw new ChildResync("Child feed requires refresh");
  if (item.seq <= snapshot.lastSeq) return snapshot;
  const phase = eventPhase(item.event, snapshot.sessionId);
  let next = phase ? mergePhase(snapshot, phase) : snapshot;
  if (item.observation) {
    if (
      !snapshot.page.children.some(
        (c) => c.childId === item.observation!.childId,
      )
    )
      throw new ChildResync("Unknown child observation");
    next = {
      ...next,
      observations: next.observations.map((o) =>
        o.childId === item.observation!.childId ? item.observation! : o,
      ),
    };
  }
  return { ...next, lastSeq: item.seq };
}

/** One caller-owned GET stream. No EventSource auto-reconnect, after or Last-Event-ID. */
export async function readChildFeed(
  sessionId: string,
  signal: AbortSignal,
  onFrame: (name: string, value: unknown) => void,
) {
  const response = await fetch(
    api.toolProgramEventsUrl(encodeURIComponent(sessionId)),
    {
      method: "GET",
      headers: { "X-Rpi-Web": "1", Accept: "text/event-stream" },
      credentials: "same-origin",
      cache: "no-store",
      redirect: "error",
      signal,
    },
  );
  if (!response.ok)
    throw new ApiError(`Child feed HTTP ${response.status}`, response.status);
  if (
    !response.headers.get("content-type")?.includes("text/event-stream") ||
    !response.body
  )
    throw new Error("Invalid child event stream");
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let buffer = "",
    name = "",
    data: string[] = [],
    frameSize = 0;
  try {
    while (!signal.aborted) {
      const chunk = await reader.read();
      if (chunk.done) throw new ChildResync("Child feed disconnected");
      buffer += decoder.decode(chunk.value, { stream: true });
      let end: number;
      while ((end = buffer.indexOf("\n")) >= 0) {
        const line = buffer.slice(0, end).replace(/\r$/, "");
        buffer = buffer.slice(end + 1);
        frameSize += line.length;
        if (frameSize > 150000)
          throw new ChildResync("Child event exceeded bound");
        if (!line) {
          if (data.length) {
            if (name === "auth_required")
              throw new ApiError("Child feed authentication required", 401);
            if (name === "resync_required") throw new ChildResync(name);
            onFrame(name, JSON.parse(data.join("\n")));
          }
          name = "";
          data = [];
          frameSize = 0;
        } else if (line.startsWith("event:")) name = line.slice(6).trim();
        else if (line.startsWith("data:"))
          data.push(line.slice(5).replace(/^ /, ""));
        // SSE IDs intentionally ignored: dedicated snapshots establish every handoff.
      }
      if (buffer.length > 150000)
        throw new ChildResync("Child event exceeded bound");
    }
  } finally {
    await reader.cancel().catch(() => {});
    reader.releaseLock();
  }
}

/** A bounded byte fragment, never parsed as a full body or rendered as HTML. */
export function childBodyText(page: ToolProgramBodyResponse): string {
  if (!page.body || page.body.byteLength > 65536 || page.body.chunks.length > 2)
    throw new Error("Invalid child body page");
  const bytes: number[] = [];
  for (const chunk of page.body.chunks) {
    if (chunk.data.length > 43692) throw new Error("Invalid child body chunk");
    const decoded = atob(chunk.data);
    if (
      decoded.length !== chunk.byteLength ||
      bytes.length + decoded.length > 65536
    )
      throw new Error("Invalid child body size");
    for (let i = 0; i < decoded.length; i++) bytes.push(decoded.charCodeAt(i));
  }
  if (bytes.length !== page.body.byteLength)
    throw new Error("Invalid child body length");
  return new TextDecoder().decode(new Uint8Array(bytes));
}
