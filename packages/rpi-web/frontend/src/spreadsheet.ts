/**
 * Workbook preview backed by SheetJS.
 *
 * Parsing happens in the page: the host only has to hand over the bytes. The
 * library is loaded on demand so opening a chat never pays for it.
 */

/** A preview is a glance at a workbook, not the whole file. */
const MAX_ROWS = 200;
const MAX_COLUMNS = 40;
const MAX_CELL = 200;
const MAX_SHEETS = 8;

export interface SheetTable {
  name: string;
  rows: string[][];
  columns: number;
  truncated: boolean;
}

export interface WorkbookPreview {
  sheets: SheetTable[];
  truncated: boolean;
}

type SheetJs = typeof import("xlsx");
let library: Promise<SheetJs> | null = null;

function loadSheetJs(): Promise<SheetJs> {
  library ??= import("xlsx");
  return library;
}

function cellText(value: unknown): string {
  if (value === null || value === undefined) return "";
  const text = typeof value === "string" ? value : String(value);
  return text.length > MAX_CELL ? text.slice(0, MAX_CELL) : text;
}

/**
 * Parse a workbook into at most {@link MAX_SHEETS} sheets of formatted text.
 *
 * `sheetRows` bounds what the reader materializes, `raw: false` keeps number
 * formats (including dates) instead of serial numbers, and every cell becomes
 * plain text so no file content is ever rendered as HTML.
 */
export async function readWorkbook(
  bytes: Uint8Array,
): Promise<WorkbookPreview> {
  const XLSX = await loadSheetJs();
  const workbook = XLSX.read(bytes, {
    type: "array",
    sheetRows: MAX_ROWS + 1,
    cellDates: true,
    cellFormula: false,
    cellHTML: false,
  });
  const names = workbook.SheetNames.slice(0, MAX_SHEETS);
  if (names.length === 0) throw new Error("workbook has no readable worksheet");
  const sheets: SheetTable[] = [];
  let truncated = workbook.SheetNames.length > names.length;
  for (const name of names) {
    const worksheet = workbook.Sheets[name];
    if (!worksheet) continue;
    const raw = XLSX.utils.sheet_to_json<unknown[]>(worksheet, {
      header: 1,
      raw: false,
      defval: "",
      blankrows: false,
    });
    let sheetTruncated = raw.length > MAX_ROWS;
    const rows = raw.slice(0, MAX_ROWS).map((row) => {
      const cells = Array.isArray(row) ? row.map(cellText) : [];
      if (cells.length > MAX_COLUMNS) sheetTruncated = true;
      return cells.slice(0, MAX_COLUMNS);
    });
    const columns = Math.min(
      MAX_COLUMNS,
      Math.max(1, ...rows.map((row) => row.length)),
    );
    if (rows.some((row) => row.some((cell) => cell.length >= MAX_CELL)))
      sheetTruncated = true;
    truncated ||= sheetTruncated;
    sheets.push({
      name,
      columns,
      truncated: sheetTruncated,
      rows: rows.map((row) =>
        Array.from({ length: columns }, (_, index) => row[index] ?? ""),
      ),
    });
  }
  if (sheets.length === 0)
    throw new Error("workbook has no readable worksheet");
  return { sheets, truncated };
}
