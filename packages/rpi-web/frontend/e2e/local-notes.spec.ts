import { test, expect } from "@playwright/test";
import type { Row } from "../src/transcript";
import { restoreRowsWithNotes } from "../src/localNotes";

const native = (id: string, text = id, entryId = id): Row => ({
  kind: "text",
  role: "assistant",
  id,
  entryId,
  text,
});
const note = (id: string, text = id): Row => ({ kind: "note", id, text });
const ids = (rows: readonly Row[]) => rows.map((row) => row.id);

// Pure projection tests: deliberately no browser fixture or native host.
test("an initial model notice stays before new messages through repeated refreshes", () => {
  const notice = note("model", "Model → DeepSeek Flash (deepseek)");
  let live: Row[] = [notice];
  for (const restored of [
    [native("a")],
    [native("a"), native("b")],
    [native("a"), native("b"), native("c")],
  ]) {
    live = restoreRowsWithNotes(restored, live);
    expect(ids(live)).toEqual(["model", ...ids(restored)]);
    expect(live[0]).toEqual(notice);
  }
});

test("a trailing notice remains after its old message but before future messages", () => {
  expect(
    ids(
      restoreRowsWithNotes(
        [native("a"), native("b")],
        [native("a"), note("n")],
      ),
    ),
  ).toEqual(["a", "n", "b"]);
});

test("a notice between two stable native anchors keeps that gap", () => {
  expect(
    ids(
      restoreRowsWithNotes(
        [native("a"), native("b"), native("c")],
        [native("a"), note("n"), native("b")],
      ),
    ),
  ).toEqual(["a", "n", "b", "c"]);
});

test("command, thinking and run-status notices retain relative order across durable gaps", () => {
  const previous = [
    note("command"),
    native("a"),
    note("thinking"),
    note("status"),
    native("b"),
    note("done"),
  ];
  expect(
    ids(
      restoreRowsWithNotes([native("a"), native("b"), native("c")], previous),
    ),
  ).toEqual(["command", "a", "thinking", "status", "b", "done", "c"]);
});

test("an evicted left anchor still places a notice before its surviving right anchor", () => {
  expect(
    ids(
      restoreRowsWithNotes(
        [native("b"), native("c")],
        [native("evicted"), note("n"), native("b")],
      ),
    ),
  ).toEqual(["n", "b", "c"]);
});

test("when both anchors are clipped, notices conservatively prefix the latest page in order", () => {
  const previous = [
    native("old-a"),
    note("first"),
    native("old-b"),
    note("second"),
    note("third"),
  ];
  expect(
    ids(
      restoreRowsWithNotes([native("latest-a"), native("latest-b")], previous),
    ),
  ).toEqual(["first", "second", "third", "latest-a", "latest-b"]);
});

test("exact native entry identity anchors a notice when block IDs change", () => {
  const previous = [
    native("old-block", "same entry", "entry-a"),
    note("n"),
    native("old-right", "right", "entry-b"),
  ];
  const restored = [
    native("new-block", "revised", "entry-a"),
    native("new-right", "revised right", "entry-b"),
  ];
  expect(ids(restoreRowsWithNotes(restored, previous))).toEqual([
    "new-block",
    "n",
    "new-right",
  ]);
});

test("a notice after an entry remains after its entire restored multi-block span", () => {
  const previous = [native("a:b0", "answer", "entry-a"), note("n")];
  const restored: Row[] = [
    native("a:b0", "answer revised", "entry-a"),
    {
      kind: "thinking",
      id: "a:b1",
      entryId: "entry-a",
      text: "native reasoning",
    },
    {
      kind: "tool",
      id: "call-read",
      entryId: "entry-a",
      name: "read",
      status: "done",
      result: "file",
    },
    native("b"),
  ];
  expect(ids(restoreRowsWithNotes(restored, previous))).toEqual([
    "a:b0",
    "a:b1",
    "call-read",
    "n",
    "b",
  ]);
});

test("an authoritative restored notice wins by ID and duplicate local notices appear once", () => {
  const restored = [
    native("a"),
    note("shared", "authoritative notice"),
    native("b"),
  ];
  const previous = [
    native("a"),
    note("shared", "stale notice"),
    note("local"),
    note("local"),
    native("b"),
  ];
  const result = restoreRowsWithNotes(restored, previous);
  expect(ids(result)).toEqual(["a", "shared", "local", "b"]);
  expect(result.find((row) => row.id === "shared")).toEqual(restored[1]);
});

test("readonly inputs stay unchanged and restored native order/content is authoritative", () => {
  const restored: readonly Row[] = Object.freeze([
    Object.freeze(native("b", "new B")),
    Object.freeze(native("a", "new A")),
  ]);
  const previous: readonly Row[] = Object.freeze([
    Object.freeze(native("a", "stale A")),
    Object.freeze(note("n")),
    Object.freeze(native("removed")),
    Object.freeze(native("b", "stale B")),
  ]);
  const before = JSON.stringify({ restored, previous });
  const result = restoreRowsWithNotes(restored, previous);
  expect(result.filter((row) => row.kind !== "note")).toEqual(restored);
  expect(result.filter((row) => row.kind === "note")).toEqual([note("n")]);
  expect(JSON.stringify({ restored, previous })).toBe(before);
});

test("earlier pages precede an old leading notice rather than making it lead all history", () => {
  expect(
    ids(
      restoreRowsWithNotes(
        [native("older"), native("a"), native("b")],
        [note("n"), native("a")],
      ),
    ),
  ).toEqual(["older", "n", "a", "b"]);
});

test("legacy offset IDs cannot anchor a notice to unrelated messages in a sliding window", () => {
  const old: Row = {
    kind: "text",
    role: "assistant",
    id: "s0:b0",
    text: "Old response",
  };
  const latest: Row = { ...old, text: "Unrelated later response" };
  expect(ids(restoreRowsWithNotes([latest], [old, note("n")]))).toEqual([
    "n",
    "s0:b0",
  ]);
  // Actual native IDs remain valid even when their spelling resembles a fallback.
  expect(
    ids(restoreRowsWithNotes([native("s0:b0")], [native("s0:b0"), note("n")])),
  ).toEqual(["s0:b0", "n"]);
  // The restored snapshot still wins deduplication for legacy custom notices.
  expect(
    restoreRowsWithNotes(
      [note("s0", "Current native notice")],
      [note("s0", "Old native notice")],
    ),
  ).toEqual([note("s0", "Current native notice")]);
});

test("an empty restored window retains only local notices in their previous order", () => {
  expect(restoreRowsWithNotes([], [])).toEqual([]);
  expect(
    restoreRowsWithNotes(
      [],
      [native("evicted"), note("first"), note("second")],
    ),
  ).toEqual([note("first"), note("second")]);
});
