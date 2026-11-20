import { strFromU8, unzipSync } from "fflate";

/** Bounded preview: a spreadsheet cell is a glance, not the whole workbook. */
const MAX_ROWS = 200;
const MAX_COLUMNS = 40;
const MAX_CELL = 200;

export interface SheetPreview {
  name: string;
  /** Truncated header row, already padded to `columns`. */
  rows: string[][];
  columns: number;
  truncated: boolean;
}

function decodeXmlText(value: string): string {
  return value
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&quot;/g, '"')
    .replace(/&apos;/g, "'")
    .replace(/&#(\d+);/g, (_, code: string) =>
      String.fromCodePoint(Number(code)),
    )
    .replace(/&#x([\da-f]+);/gi, (_, code: string) =>
      String.fromCodePoint(parseInt(code, 16)),
    )
    .replace(/&amp;/g, "&");
}

function textOf(fragment: string): string {
  // Concatenate every run inside a rich-text cell.
  const parts = [...fragment.matchAll(/<t(?:\s[^>]*)?>([\s\S]*?)<\/t>/g)].map(
    (match) => decodeXmlText(match[1]),
  );
  return parts.length > 0
    ? parts.join("")
    : decodeXmlText(fragment.replace(/<[^>]*>/g, ""));
}

function sharedStrings(xml: string | undefined): string[] {
  if (!xml) return [];
  return [...xml.matchAll(/<si>([\s\S]*?)<\/si>/g)].map((match) =>
    textOf(match[1]),
  );
}

/** Column letters from a cell reference (`BC12` → 54), one-based. */
function columnIndex(reference: string): number {
  const letters = /^([A-Z]+)/.exec(reference)?.[1] ?? "";
  let index = 0;
  for (const letter of letters)
    index = index * 26 + (letter.charCodeAt(0) - 64);
  return index;
}

function firstSheetPath(files: Record<string, Uint8Array>): string | null {
  const workbook = files["xl/workbook.xml"]
    ? strFromU8(files["xl/workbook.xml"])
    : "";
  const rels = files["xl/_rels/workbook.xml.rels"]
    ? strFromU8(files["xl/_rels/workbook.xml.rels"])
    : "";
  const firstSheet = /<sheet\b[^>]*r:id="([^"]+)"/.exec(workbook)?.[1];
  if (firstSheet) {
    const target = new RegExp(
      `<Relationship\\b[^>]*Id="${firstSheet}"[^>]*Target="([^"]+)"`,
    ).exec(rels)?.[1];
    if (target) {
      const path = target.startsWith("/")
        ? target.slice(1)
        : `xl/${target.replace(/^\.\//, "")}`;
      if (files[path]) return path;
    }
  }
  return (
    Object.keys(files).find((path) =>
      /^xl\/worksheets\/sheet\d+\.xml$/.test(path),
    ) ?? null
  );
}

function sheetName(workbook: string | undefined): string {
  return /<sheet\b[^>]*name="([^"]+)"/.exec(workbook ?? "")?.[1] ?? "";
}

/**
 * Parse the first worksheet of an `.xlsx` workbook into plain rows.
 *
 * Only what a preview needs is decoded: shared/inline strings, numbers,
 * booleans and cached formula results. Dates keep their serial number because
 * rendering them as dates would require the styles table. Nothing renders HTML
 * from the file, so a hostile sheet cannot inject markup.
 */
export function readSheet(bytes: Uint8Array): SheetPreview {
  const files = unzipSync(bytes);
  const sheetPath = firstSheetPath(files);
  if (!sheetPath) throw new Error("workbook has no readable worksheet");
  const shared = sharedStrings(
    files["xl/sharedStrings.xml"]
      ? strFromU8(files["xl/sharedStrings.xml"])
      : undefined,
  );
  const sheet = strFromU8(files[sheetPath]);
  const rows: string[][] = [];
  let columns = 1;
  let truncated = false;
  for (const rowMatch of sheet.matchAll(/<row\b[^>]*>([\s\S]*?)<\/row>/g)) {
    if (rows.length >= MAX_ROWS) {
      truncated = true;
      break;
    }
    const cells: string[] = [];
    for (const cell of rowMatch[1].matchAll(/<c\b([^>]*)>([\s\S]*?)<\/c>/g)) {
      const attributes = cell[1];
      const reference = /\br="([A-Z]+)\d+"/.exec(attributes)?.[1] ?? "";
      const index = Math.max(1, columnIndex(reference)) - 1;
      if (index >= MAX_COLUMNS) {
        truncated = true;
        continue;
      }
      const type = /\bt="([^"]+)"/.exec(attributes)?.[1] ?? "n";
      const raw =
        /<v>([\s\S]*?)<\/v>/.exec(cell[2])?.[1] ??
        (/<is>/.test(cell[2]) ? textOf(cell[2]) : "");
      let value: string;
      if (type === "s") value = shared[Number(raw)] ?? "";
      else if (type === "b") value = raw === "1" ? "TRUE" : "FALSE";
      else if (type === "inlineStr") value = textOf(cell[2]);
      else value = decodeXmlText(raw);
      if (value.length > MAX_CELL) {
        value = value.slice(0, MAX_CELL);
        truncated = true;
      }
      cells[index] = value;
    }
    columns = Math.max(columns, Math.min(cells.length, MAX_COLUMNS));
    rows.push(cells);
  }
  const padded = rows.map((row) =>
    Array.from({ length: columns }, (_, index) => row[index] ?? ""),
  );
  return {
    name: sheetName(
      files["xl/workbook.xml"]
        ? strFromU8(files["xl/workbook.xml"])
        : undefined,
    ),
    rows: padded,
    columns,
    truncated,
  };
}
