import type { Row } from "./transcript";

export interface MessageSearchResult {
  entryId: string;
  rowId?: string;
  preview: string;
  source: "loaded" | "history";
  leafId: string;
}

function toolText(row: Extract<Row, { kind: "tool" }>): string {
  const values = [row.name];
  for (const value of [row.args, row.result]) {
    if (typeof value === "string") values.push(value);
    else if (value != null) {
      try {
        values.push(JSON.stringify(value));
      } catch {
        // Ignore unserializable tool payloads.
      }
    }
  }
  return values.join("\n");
}

export function searchableText(row: Row): string {
  switch (row.kind) {
    case "text":
    case "thinking":
    case "note":
      return row.text;
    case "tool":
      return toolText(row);
    default:
      return "";
  }
}

export function previewOf(text: string, query: string, limit = 180): string {
  const value = text.replace(/\s+/g, " ").trim();
  if (value.length <= limit) return value;
  const start = value.toLocaleLowerCase().indexOf(query.toLocaleLowerCase());
  if (start < 0) return `${value.slice(0, limit - 1)}…`;
  const begin = Math.max(0, Math.min(start - 48, value.length - limit));
  return `${begin > 0 ? "…" : ""}${value.slice(begin, begin + limit - 1)}…`;
}

export function loadedSearchResults(
  rows: Row[],
  query: string,
  leafId: string,
): MessageSearchResult[] {
  const needle = query.trim().toLocaleLowerCase();
  if (!needle) return [];
  const seen = new Set<string>();
  return rows.flatMap((row) => {
    const text = searchableText(row);
    const entryId = row.entryId ?? row.id;
    if (!text.toLocaleLowerCase().includes(needle) || seen.has(entryId)) return [];
    seen.add(entryId);
    return [{
      entryId,
      rowId: row.id,
      preview: previewOf(text, query),
      source: "loaded" as const,
      leafId,
    }];
  });
}

export function summaryMatches(
  messages: Array<{ entryId: string; preview: string }>,
  query: string,
  leafId: string,
): MessageSearchResult[] {
  const needle = query.trim().toLocaleLowerCase();
  if (!needle) return [];
  return messages.flatMap((message) =>
    message.preview.toLocaleLowerCase().includes(needle)
      ? [{
          entryId: message.entryId,
          preview: previewOf(message.preview, query),
          source: "history" as const,
          leafId,
        }]
      : [],
  );
}

