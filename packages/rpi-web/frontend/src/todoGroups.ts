import { todoInfo, type Row } from "./transcript";

type ToolRow = Extract<Row, { kind: "tool" }>;

export const isTodoTool = (name: string) =>
  name
    .trim()
    .toLowerCase()
    .replace(/^functions\./, "") === "todo";

export interface TodoGroup {
  latest: ToolRow;
  previous: ToolRow[];
}

/** Display projection only. Keep native rows, IDs, replay and footer positions intact. */
export function groupTodoUpdates(
  rows: Row[],
  breakAfter: ReadonlyMap<string, unknown>,
  preservedOwners: ReadonlySet<string> = new Set(),
): Map<string, TodoGroup | null> {
  const groups = new Map<string, TodoGroup | null>();
  let group: TodoGroup | undefined;
  let owner = "";
  for (const row of rows) {
    if (row.kind !== "boundary") {
      const eligible =
        row.kind === "tool" &&
        isTodoTool(row.name) &&
        !row.images?.length &&
        (row.status === "pending" ||
          row.status === "running" ||
          (row.status === "done" && todoInfo(row.result) !== null));
      if (!eligible) group = undefined;
      else {
        // Never hide an earlier active call behind a later/parallel one.
        if (group?.latest.status === "done") {
          group.previous.push(group.latest);
          group.latest = row;
          // Extending a loaded group with an older page must not unmount its
          // existing card or leave the viewport anchored to an empty wrapper.
          if (!preservedOwners.has(owner) && preservedOwners.has(row.id)) {
            groups.set(owner, null);
            owner = row.id;
            groups.set(owner, group);
          } else groups.set(row.id, null);
        } else {
          group = { latest: row, previous: [] };
          owner = row.id;
          groups.set(owner, group);
        }
      }
    }
    // A native run footer is a visible chronological boundary, even on a
    // zero-content tool-result row. Do not move a later run in front of it.
    if (breakAfter.has(row.id)) group = undefined;
  }
  return groups;
}
