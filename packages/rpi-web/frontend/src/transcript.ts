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

export interface ImageBlock {
  data?: string;
  mimeType?: string;
  url?: string;
  alt?: string;
}

export function imageBlocksOf(value: unknown): ImageBlock[] {
  if (!value || typeof value !== "object") return [];
  const content = (value as Record<string, unknown>).content;
  if (!Array.isArray(content)) return [];
  const images: ImageBlock[] = [];
  for (const block of content) {
    if (!block || typeof block !== "object") continue;
    const item = block as Record<string, unknown>;
    if (item.type === "image_url" && item.image_url && typeof item.image_url === "object") {
      const url = (item.image_url as Record<string, unknown>).url;
      if (typeof url === "string") images.push({ url });
      continue;
    }
    if (item.type !== "image") continue;
    const data = typeof item.data === "string" ? item.data : undefined;
    if (data) images.push({
      data,
      mimeType: typeof item.mimeType === "string" ? item.mimeType : undefined,
      alt: typeof item.alt === "string" ? item.alt : undefined,
    });
  }
  return images;
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

const SHELL_TOOLS = new Set([
  "bash",
  "shell",
  "sh",
  "zsh",
  "run_command",
  "exec",
  "powershell",
  "pwsh",
]);

/** Shell tools render as a command card instead of a generic JSON call. */
export function isShellTool(name: string): boolean {
  return SHELL_TOOLS.has(name.trim().toLowerCase());
}

const COMMAND_KEYS = ["command", "cmd", "script"] as const;
const COMMAND_KEY_SET = new Set<string>(COMMAND_KEYS);

/** Split shell arguments into the command line and its scalar extras. */
export function shellCommand(args: unknown): {
  command: string;
  meta: string;
} {
  if (typeof args === "string") return { command: args.trim(), meta: "" };
  if (!args || typeof args !== "object" || Array.isArray(args))
    return { command: "", meta: "" };
  const record = args as Record<string, unknown>;
  let command = "";
  for (const key of COMMAND_KEYS) {
    const value = record[key];
    if (typeof value === "string" && value.trim()) {
      command = value;
      break;
    }
  }
  const meta = Object.entries(record)
    .filter(
      ([key, value]) =>
        !COMMAND_KEY_SET.has(key) &&
        (typeof value === "string" ||
          typeof value === "number" ||
          typeof value === "boolean") &&
        String(value).trim().length > 0,
    )
    .map(([key, value]) => `${key}=${String(value)}`)
    .join(" · ");
  return { command, meta };
}

const ANSI_ESCAPE = /\u001b\[[0-?]*[ -/]*[@-~]/g;

/** Terminal output is plain text: drop colour codes and normalise newlines. */
export function stripAnsi(text: string): string {
  return text.replace(ANSI_ESCAPE, "").replace(/\r\n/g, "\n");
}

function blocksToText(value: unknown): string {
  if (!Array.isArray(value)) return "";
  return value
    .map((block) =>
      block && typeof block === "object"
        ? String((block as Record<string, unknown>).text ?? "")
        : typeof block === "string"
          ? block
          : "",
    )
    .filter((text) => text.length > 0)
    .join("\n");
}

/** Extract text blocks returned by the read tool without rendering its transport envelope. */
export function readOutput(result: unknown): string | null {
  if (typeof result === "string") {
    try {
      const parsed: unknown = JSON.parse(result);
      if (
        parsed &&
        typeof parsed === "object" &&
        Array.isArray((parsed as Record<string, unknown>).content) &&
        ("details" in parsed || "usage" in parsed || "terminate" in parsed)
      ) {
        return readOutput(parsed);
      }
    } catch {
      // A normal source file is not necessarily JSON.
    }
    return result;
  }
  if (!result || typeof result !== "object") return null;
  const record = result as Record<string, unknown>;
  if (Array.isArray(record.content)) {
    const text = blocksToText(record.content);
    return text || null;
  }
  for (const key of ["text", "output", "result"]) {
    if (typeof record[key] === "string") return record[key] as string;
  }
  return null;
}
/** Shell results are text or `{stdout, stderr}` objects, not JSON payloads. */
export function shellOutput(result: unknown): string {
  if (result === undefined || result === null) return "";
  let text = "";
  if (typeof result === "string") {
    text = result;
  } else if (typeof result === "object") {
    const record = result as Record<string, unknown>;
    const streams = [record.stdout, record.stderr].filter(
      (value): value is string => typeof value === "string" && !!value.trim(),
    );
    if (streams.length > 0) {
      text = streams.join("\n");
    } else if (Array.isArray(record.content)) {
      text = blocksToText(record.content);
    } else {
      for (const key of ["output", "text", "result"]) {
        const value = record[key];
        if (typeof value === "string" && value.trim()) {
          text = value;
          break;
        }
      }
      if (!text.trim()) {
        try {
          text = JSON.stringify(result, null, 2);
        } catch {
          text = String(result);
        }
      }
    }
  } else {
    text = String(result);
  }
  const cleaned = stripAnsi(text).replace(/\n+$/, "");
  return cleaned.trim().length > 0 ? cleaned : "";
}

export interface PlanInfo {
  plan: string;
  outline: string[];
  active: boolean;
}

function outlineFromMarkdown(plan: string): string[] {
  const lines = plan.split("\n");
  const list: string[] = [];
  const headings: string[] = [];
  for (const line of lines) {
    const item = line.match(/^\s*(?:[-*+]\s+|\d+\.\s+)(.+)$/);
    if (item) list.push(item[1].trim());
    const heading = line.match(/^\s*#{1,3}\s+(.+)$/);
    if (heading) headings.push(heading[1].trim());
    if (list.length >= 10) break;
  }
  return (list.length > 0 ? list : headings).slice(0, 10);
}

/** Extract a structured plan from a `plan` tool call or its result. */
export function planInfo(args: unknown, result: unknown): PlanInfo | null {
  let plan = "";
  let outline: string[] = [];
  let active = false;
  const read = (value: unknown) =>
    value && typeof value === "object"
      ? (value as Record<string, unknown>)
      : null;
  const res = read(result);
  const details = res?.details;
  if (details && typeof details === "object") {
    const record = details as Record<string, unknown>;
    if (record.kind === "plan") {
      if (typeof record.plan === "string") plan = record.plan;
      if (Array.isArray(record.outline))
        outline = record.outline.filter(
          (item): item is string => typeof item === "string",
        );
      active = record.active === true;
    }
  }
  if (!plan && res && Array.isArray(res.content)) {
    plan = blocksToText(res.content);
  }
  if (!plan && typeof result === "string") plan = result;
  if (!plan) {
    const argObj = read(args);
    if (argObj && typeof argObj.plan === "string") plan = argObj.plan;
  }
  if (!plan.trim()) return null;
  if (outline.length === 0) outline = outlineFromMarkdown(plan);
  return { plan, outline, active };
}

export interface TodoItem {
  text: string;
  done: boolean;
}

/** Parse the `todo` tool's `[x] #id: text` list into a checklist. */
export function todoItems(result: unknown): TodoItem[] | null {
  let text = "";
  if (typeof result === "string") text = result;
  else if (result && typeof result === "object") {
    const record = result as Record<string, unknown>;
    if (Array.isArray(record.content)) text = blocksToText(record.content);
    else if (typeof record.text === "string") text = record.text;
  }
  if (!text.trim()) return null;
  const items: TodoItem[] = [];
  let matched = 0;
  for (const line of text.split("\n")) {
    const match = line.match(
      /^\s*\[([\u2713\u2714xX\u2022 ])\]\s*(?:#\d+:\s*)?(.+)$/,
    );
    if (match) {
      const mark = match[1];
      items.push({
        done: /[\u2713\u2714xX]/.test(mark),
        text: match[2].trim(),
      });
      matched++;
    }
  }
  return matched > 0 ? items : null;
}

export interface EditInfo {
  path: string;
  edits: { oldText: string; newText: string }[];
}

function stringField(value: unknown): string {
  return typeof value === "string" ? value : "";
}

/** Extract `edit`/`write` tool arguments into a diff-shaped view. */
export function editInfo(name: string, args: unknown): EditInfo | null {
  if (!args || typeof args !== "object" || Array.isArray(args)) return null;
  const record = args as Record<string, unknown>;
  const path = stringField(record.path ?? record.filePath);

  if (name === "write") {
    const content = stringField(record.content);
    if (!content) return null;
    return { path, edits: [{ oldText: "", newText: content }] };
  }

  if (name !== "edit") return null;
  const edits: { oldText: string; newText: string }[] = [];
  if (Array.isArray(record.edits)) {
    for (const edit of record.edits) {
      if (!edit || typeof edit !== "object") continue;
      const item = edit as Record<string, unknown>;
      edits.push({
        oldText: stringField(item.oldText),
        newText: stringField(item.newText),
      });
    }
  } else if (
    typeof record.oldText === "string" ||
    typeof record.newText === "string"
  ) {
    edits.push({
      oldText: stringField(record.oldText),
      newText: stringField(record.newText),
    });
  }
  if (!path && edits.length === 0) return null;
  return { path, edits };
}

/** Heuristic for tool results that are prose worth rendering as Markdown. */
export function isMarkdownText(text: string): boolean {
  if (/\*\*[^*]+\*\*|__[^_]+__|\[[^\]]+\]\([^)]+\)/.test(text)) return true;
  return /(^|\n)\s*(#{1,6}\s|[-*+]\s|\d+\.\s|```|>\s|\|.*\|)/.test(text);
}

export type ToolStatus = "pending" | "running" | "done" | "error";

export type Row = RowContent & { entryId?: string };

type RowContent =
  | { kind: "boundary"; id: string }
  | { kind: "text"; id: string; role: "user" | "assistant"; text: string }
  | { kind: "image"; id: string; role: "user" | "assistant"; image: ImageBlock }
  | { kind: "thinking"; id: string; text: string }
  | {
      kind: "tool";
      id: string;
      name: string;
      args?: unknown;
      result?: unknown;
      images?: ImageBlock[];
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
      const previous = rows[rows.length - 1];
      if (previous?.kind === "text" && previous.role === "assistant") {
        previous.text = `${previous.text}\n\n${block.text}`;
      } else {
        rows.push({
          kind: "text",
          id: `${prefix}:b${index}`,
          role: "assistant",
          text: block.text,
        });
      }
    } else if (block.type === "image" || block.type === "image_url") {
      const image = imageBlocksOf({ content: [block] })[0];
      if (image) rows.push({ kind: "image", id: `${prefix}:b${index}`, role: "assistant", image });
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

/** Keep user text IDs compatible while projecting native image blocks too. */
export function rowsFromUser(message: unknown, prefix: string): Row[] {
  const rows: Row[] = [];
  const text = textOf(message);
  if (text) rows.push({ kind: "text", id: prefix, role: "user", text });
  imageBlocksOf(message).forEach((image, index) => {
    rows.push({ kind: "image", id: `${prefix}:image${index}`, role: "user", image });
  });
  return rows;
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
    const before = rows.length;
    try {
    if (role === "user") {
      rows.push(...rowsFromUser(message, key));
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
            images: imageBlocksOf(message),
            status,
            timing: toolTiming(message.webTiming) ?? row.timing,
          };
        }
        // Tool results fold into an earlier card, but their chronological
        // boundary must stay here: a stopped run can end on this result.
        rows.push({ kind: "boundary", id: `${key}:end` });
        return;
      }
      rows.push({
        kind: "tool",
        id,
        name: String(message.toolName ?? "tool"),
        result: resultOf(message),
        images: imageBlocksOf(message),
        status,
        timing: toolTiming(message.webTiming),
      });
      rows.push({ kind: "boundary", id: `${key}:end` });
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
    } finally {
      // Multiple projected blocks share one native entry. The footer goes
      // after its last row, never after every text/tool/image block.
      if (typeof message.entryId === "string") {
        for (let n = before; n < rows.length; n++) rows[n].entryId = message.entryId;
      }
    }
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
      images: row.images ?? candidate.images,
      timing: row.timing ?? candidate.timing,
      entryId: row.result !== undefined ? row.entryId ?? candidate.entryId
        : candidate.result !== undefined ? candidate.entryId ?? row.entryId : row.entryId ?? candidate.entryId,
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
    images?: ImageBlock[];
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
        images: patch.images,
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
      images: patch.images ?? row.images,
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
