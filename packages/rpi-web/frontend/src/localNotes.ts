import type { Row } from "./transcript";

type Span = { first: number; last: number };

/**
 * Restore authoritative message rows without moving browser-only notices to
 * the tail on every snapshot. Notices stay in their previous message gap;
 * messages themselves always retain the snapshot's order and contents.
 *
 * Use row/native-entry identities, never text equality or array offsets. When
 * a bounded snapshot has evicted both neighbours, put the notice before that
 * newer window rather than repeatedly making it look like a new message.
 */
export function restoreRowsWithNotes(
  restored: readonly Row[],
  previous: readonly Row[],
): Row[] {
  const rows = new Map<string, Span>();
  const entries = new Map<string, Span>();
  restored.forEach((row, index) => {
    rows.set(row.id, { first: index, last: index });
    if (row.entryId !== undefined) {
      const span = entries.get(row.entryId);
      entries.set(row.entryId, { first: span?.first ?? index, last: index });
    }
  });
  const spanOf = (row: Row) => {
    const entry =
      row.entryId === undefined ? undefined : entries.get(row.entryId);
    if (entry) return entry;
    // Older hosts synthesize s<offset> IDs per snapshot. An equal offset in
    // a sliding window is not proof that the native message survived.
    if (row.entryId === undefined && /^s\d+(?::|$)/.test(row.id))
      return undefined;
    return rows.get(row.id);
  };
  const next: (number | undefined)[] = new Array(previous.length);
  let right: number | undefined;
  for (let index = previous.length - 1; index >= 0; index--) {
    next[index] = right;
    const span = spanOf(previous[index]);
    if (span) right = span.first;
  }
  const gaps = new Map<number, Row[]>();
  const retained = new Set<string>();
  let left: number | undefined;
  let lastGap = 0;
  previous.forEach((row, index) => {
    const span = spanOf(row);
    if (span) {
      left = span.last + 1;
      return;
    }
    if (row.kind !== "note" || rows.has(row.id) || retained.has(row.id)) return;
    retained.add(row.id);
    // Clamp to a monotone gap so local notices cannot reverse each other if
    // compaction or a replacement page removed their original neighbours.
    const gap = Math.max(lastGap, left ?? next[index] ?? 0);
    lastGap = gap;
    const notes = gaps.get(gap) ?? [];
    notes.push(row);
    gaps.set(gap, notes);
  });
  const merged: Row[] = [];
  for (let index = 0; index <= restored.length; index++) {
    const notes = gaps.get(index);
    if (notes) merged.push(...notes);
    if (index < restored.length) merged.push(restored[index]);
  }
  return merged;
}
