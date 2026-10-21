/**
 * Transcript rows.
 *
 * Native messages are not flat text: an assistant turn can carry thinking blocks
 * and tool calls, and tool results arrive as their own messages. Rendering them
 * as two parallel arrays (messages + tools) loses both the order and the empty
 * assistant turns, so everything is projected into one ordered row list instead.
 */

import type { MessageKey } from "./i18n";
import type { SeqEvent } from "./api";

function snapshotMessageKey(message: unknown): string | null {
  if (!message || typeof message !== "object") return null;
  const record = message as Record<string, unknown>;
  if (
    typeof record.timestamp !== "number" ||
    !Number.isFinite(record.timestamp)
  )
    return null;
  const stable = (value: unknown): unknown => {
    if (Array.isArray(value)) return value.map(stable);
    if (value && typeof value === "object")
      return Object.fromEntries(
        Object.entries(value)
          .filter(([key]) => key !== "webTiming")
          .sort(([a], [b]) => a.localeCompare(b))
          .map(([key, item]) => [key, stable(item)]),
      );
    return value;
  };
  const { entryId: _entryId, ...native } = record;
  return JSON.stringify(stable(native));
}

/** Native history can already contain messages completed during snapshot I/O. */
export function eventsAfterSnapshot(
  messages: unknown[],
  events: { seq: number; event: SeqEvent }[],
) {
  const committed = new Set(
    messages.map(snapshotMessageKey).filter((key) => key !== null),
  );
  const results = new Set(
    messages
      .filter(
        (value) =>
          !!value &&
          typeof value === "object" &&
          (value as Record<string, unknown>).role === "toolResult",
      )
      .map((value) =>
        String((value as Record<string, unknown>).toolCallId ?? ""),
      ),
  );
  const segments = new Map<string, number[]>();
  const redundant = new Set<number>();
  events.forEach(({ event }, index) => {
    if (
      !["message_start", "message_update", "message_end"].includes(event.type)
    )
      return;
    const message = event.message as Record<string, unknown> | undefined;
    const role = String(message?.role ?? "");
    if (role !== "assistant" && role !== "user") return;
    const segment =
      event.type === "message_start" ? [] : (segments.get(role) ?? []);
    segment.push(index);
    segments.set(role, segment);
    const key = snapshotMessageKey(message);
    if (event.type === "message_end") {
      if (key && committed.has(key))
        segment.forEach((item) => redundant.add(item));
      segments.delete(role);
    }
  });
  return events.filter(
    ({ event }, index) =>
      !redundant.has(index) &&
      !(
        event.type.startsWith("tool_execution_") &&
        results.has(String(event.toolCallId ?? ""))
      ),
  );
}

export interface ToolTiming {
  elapsedMs: number;
  running: boolean;
  receivedAt: number;
}

export function toolTiming(value: unknown): ToolTiming | undefined {
  if (!value || typeof value !== "object") return;
  const timing = value as Record<string, unknown>;
  if (
    typeof timing.elapsedMs !== "number" ||
    !Number.isFinite(timing.elapsedMs) ||
    timing.elapsedMs < 0
  )
    return;
  return {
    elapsedMs: timing.elapsedMs,
    running: timing.running === true,
    receivedAt: performance.now(),
  };
}

export function elapsedMs(timing: ToolTiming): number {
  return (
    timing.elapsedMs +
    (timing.running ? Math.max(0, performance.now() - timing.receivedAt) : 0)
  );
}

export function stopToolTimers(rows: Row[]): Row[] {
  return rows.map((row) =>
    row.kind === "tool" && row.timing?.running
      ? {
          ...row,
          status: "error",
          timing: {
            ...row.timing,
            elapsedMs: elapsedMs(row.timing),
            running: false,
          },
        }
      : row,
  );
}

export type ToolStatus = "pending" | "running" | "done" | "error";

export type Row =
  | { kind: "text"; id: string; role: "user" | "assistant"; text: string }
  | { kind: "thinking"; id: string; text: string }
  | {
      kind: "tool";
      id: string;
      name: string;
      args?: unknown;
      result?: unknown;
      status: ToolStatus;
      timing?: ToolTiming;
    }
  | {
      kind: "note";
      id: string;
      text: string;
      translation?: {
        key: MessageKey;
        values?: Record<string, string | number>;
      };
    };

type Block = Record<string, unknown>;

function blocksOf(message: Record<string, unknown>): Block[] {
  const content = message.content;
  if (!Array.isArray(content)) return [];
  return content.filter(
    (block): block is Block => !!block && typeof block === "object",
  );
}

function textBlocks(message: Record<string, unknown>): string[] {
  const content = message.content;
  if (typeof content === "string") return content.trim() ? [content] : [];
  return blocksOf(message)
    .filter((block) => block.type === "text" && typeof block.text === "string")
    .map((block) => String(block.text))
    .filter((text) => text.trim().length > 0);
}

/** Visible text of a message; thinking and tool calls are projected separately. */
export function textOf(value: unknown): string {
  if (!value || typeof value !== "object") return "";
  return textBlocks(value as Record<string, unknown>).join("\n\n");
}

/** Rows for one assistant message, in block order. */
export function rowsFromAssistant(
  message: Record<string, unknown>,
  prefix: string,
): Row[] {
  const rows: Row[] = [];
  for (const [index, block] of blocksOf(message).entries()) {
    if (block.type === "text" && typeof block.text === "string") {
      if (!block.text.trim()) continue;
      rows.push({
        kind: "text",
        id: `${prefix}:b${index}`,
        role: "assistant",
        text: block.text,
      });
    } else if (block.type === "thinking") {
      const text =
        typeof block.thinking === "string"
          ? block.thinking
          : typeof block.text === "string"
            ? block.text
            : "";
      if (!text.trim()) continue;
      rows.push({ kind: "thinking", id: `${prefix}:b${index}`, text });
    } else if (block.type === "toolCall") {
      // A streamed tool call only carries its id once it is complete; before
      // that there is nothing to key the row on, and `tool_execution_start`
      // opens the row anyway.
      const callId = typeof block.id === "string" ? block.id : "";
      if (!callId) continue;
      rows.push({
        kind: "tool",
        id: `call-${callId}`,
        name: String(block.name ?? "tool"),
        args: block.arguments,
        status: (block.webTiming as { running?: boolean } | undefined)?.running
          ? "running"
          : "pending",
        timing: toolTiming(block.webTiming),
      });
    }
  }
  return rows;
}

/** Result payload of a `toolResult` message, reduced to the useful text. */
export function resultOf(message: Record<string, unknown>): unknown {
  const texts = textBlocks(message);
  if (texts.length > 0) return texts.join("\n");
  return (message.content as unknown) ?? "";
}

/**
 * Project a native `get_messages` snapshot into rows, keeping message order and
 * folding each tool result into the row of the call it answers.
 */
export function rowsFromMessages(raw: unknown[]): Row[] {
  const rows: Row[] = [];
  raw.forEach((value, index) => {
    if (!value || typeof value !== "object") return;
    const message = value as Record<string, unknown>;
    const role = String(message.role ?? "assistant");
    // The session entry id makes a row stable across pages and reloads; the
    // index only backs it when a source lacks one (older hosts).
    const key =
      typeof message.entryId === "string" ? message.entryId : `s${index}`;

    if (role === "user") {
      const text = textOf(value);
      if (text) rows.push({ kind: "text", id: key, role: "user", text });
      return;
    }

    if (role === "assistant") {
      rows.push(...rowsFromAssistant(message, key));
      return;
    }

    if (role === "toolResult") {
      const callId = String(message.toolCallId ?? "");
      const id = callId ? `call-${callId}` : `call-${key}`;
      const status: ToolStatus = message.isError ? "error" : "done";
      const existing = rows.findIndex(
        (row) => row.kind === "tool" && row.id === id,
      );
      if (existing >= 0) {
        const row = rows[existing];
        if (row.kind === "tool") {
          rows[existing] = {
            ...row,
            result: resultOf(message),
            status,
            timing: toolTiming(message.webTiming) ?? row.timing,
          };
        }
        return;
      }
      rows.push({
        kind: "tool",
        id,
        name: String(message.toolName ?? "tool"),
        result: resultOf(message),
        status,
        timing: toolTiming(message.webTiming),
      });
      return;
    }

    if (role === "compaction") {
      const before = Number(message.tokensBefore ?? 0);
      const kept = Number(message.retainedMessages ?? 0);
      rows.push({
        kind: "note",
        id: `compaction-${key}`,
        text: `Conversation compacted · ${before.toLocaleString()} tokens before, ${kept} messages kept.`,
        translation: {
          key: "compactedMessage",
          values: { before: before.toLocaleString(), kept },
        },
      });
      return;
    }

    // `custom` covers injected notices such as an automatic continue.
    const text = textOf(value);
    if (text) rows.push({ kind: "note", id: key, text });
  });
  return rows;
}

export function prependRows(older: Row[], current: Row[]): Row[] {
  const merged = [...older];
  for (const row of current) {
    const existing = merged.findIndex(
      (candidate) =>
        candidate.kind === "tool" &&
        row.kind === "tool" &&
        candidate.id === row.id,
    );
    if (existing < 0) {
      if (!merged.some((candidate) => candidate.id === row.id))
        merged.push(row);
      continue;
    }
    const candidate = merged[existing];
    if (candidate.kind !== "tool" || row.kind !== "tool") continue;
    merged[existing] = {
      ...candidate,
      name: row.name !== "tool" ? row.name : candidate.name,
      args: row.args ?? candidate.args,
      result: row.result ?? candidate.result,
      timing: row.timing ?? candidate.timing,
      status:
        row.status === "error" || row.status === "done"
          ? row.status
          : candidate.status,
    };
  }
  return merged;
}

/** Replace the rows a live tool row would be rendered from. */
export function upsertTool(
  rows: Row[],
  id: string,
  patch: {
    name?: string;
    args?: unknown;
    result?: unknown;
    status?: ToolStatus;
    timing?: ToolTiming;
  },
): Row[] {
  const rowId = `call-${id}`;
  const index = rows.findIndex(
    (row) => row.kind === "tool" && row.id === rowId,
  );
  if (index < 0) {
    return [
      ...rows,
      {
        kind: "tool",
        id: rowId,
        name: patch.name ?? "tool",
        args: patch.args,
        result: patch.result,
        status: patch.status ?? "running",
        timing: patch.timing,
      },
    ];
  }
  const next = [...rows];
  const row = next[index];
  if (row.kind === "tool") {
    next[index] = {
      ...row,
      name: patch.name ?? row.name,
      args: patch.args ?? row.args,
      result: patch.result ?? row.result,
      status: patch.status ?? row.status,
      timing: patch.timing ?? row.timing,
    };
  }
  return next;
}

/** Drop every row produced by one streaming assistant message. */
export function withoutPrefix(rows: Row[], prefix: string): Row[] {
  return rows.filter((row) => !row.id.startsWith(`${prefix}:`));
}
