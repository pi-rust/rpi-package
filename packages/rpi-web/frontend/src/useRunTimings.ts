import { useEffect, useRef, useState } from "react";
import { api, ApiError, type RunTiming, type RunTimingsPage } from "./api";
import type { Row } from "./transcript";

/** Supplemental native timing lookups never change transcript/SSE cursors. */
export function useRunTimings(
  sessionId: string,
  rows: Row[],
  revision: string,
  ready: boolean,
  settled: number,
) {
  const entryKey = JSON.stringify([
    ...new Set(rows.flatMap((row) => (row.entryId ? [row.entryId] : []))),
  ]);
  const binding = `${sessionId}:${revision}`;
  const cache = useRef<{
    binding: string;
    cursor?: string;
    values: Map<string, RunTiming>;
    unsupported: boolean;
  }>({ binding, values: new Map(), unsupported: false });
  const [result, setResult] = useState<{
    binding: string;
    values: RunTiming[];
  }>({ binding, values: [] });
  useEffect(() => {
    if (cache.current.binding !== binding)
      cache.current = { binding, values: new Map(), unsupported: false };
    const state = cache.current;
    if (!sessionId || !ready || state.unsupported) return;
    const ids: string[] = JSON.parse(entryKey);
    if (!ids.length) return;
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout> | undefined;
    const delay = (ms: number) =>
      new Promise<void>((resolve) => {
        const done = () => {
          clearTimeout(timer);
          controller.signal.removeEventListener("abort", done);
          resolve();
        };
        timer = setTimeout(done, ms);
        controller.signal.addEventListener("abort", done, { once: true });
      });
    const publish = () => {
      if (!controller.signal.aborted && cache.current === state)
        setResult({ binding, values: [...state.values.values()] });
    };
    void (async () => {
      let resets = 0;
      for (let start = 0; start < ids.length && !controller.signal.aborted;) {
        const batch = ids.slice(start, start + 400);
        let previousBytes = -1;
        let slices = 0;
        let failures = 0;
        let restart = false;
        while (!controller.signal.aborted) {
          try {
            const page: RunTimingsPage = await api.runTimings(
              sessionId,
              batch,
              state.cursor,
              controller.signal,
            );
            if (controller.signal.aborted || cache.current !== state) return;
            if (
              !page ||
              !Array.isArray(page.timings) ||
              typeof page.complete !== "boolean"
            )
              return;
            state.cursor = page.cursor ?? undefined;
            for (const timing of page.timings) {
              if (
                timing &&
                batch.includes(timing.finalEntryId) &&
                validTiming(timing)
              )
                state.values.set(timing.runId, timing);
            }
            publish();
            if (page.complete) break;
            // At an unfinished native trailing line, wait rather than busy-loop.
            const stalled = page.scannedBytes === previousBytes;
            previousBytes = page.scannedBytes;
            if (stalled && ++failures > 3) return;
            await delay(stalled ? 1000 : ++slices % 8 === 0 ? 150 : 20);
          } catch (error) {
            if (controller.signal.aborted || cache.current !== state) return;
            if (error instanceof ApiError && error.status === 409) {
              state.values.clear();
              state.cursor = undefined;
              publish();
              // Every conflict invalidates displayed data, even once the
              // bounded restart budget is exhausted.
              if (resets++ >= 2) return;
              start = 0;
              restart = true;
              break;
            }
            if (
              error instanceof ApiError &&
              [404, 405, 501].includes(error.status)
            )
              state.unsupported = true;
            // No invented local timer fallback. Transient unavailability may be
            // retried at the next settlement/navigation/older-page boundary.
            return;
          }
        }
        // A conflict reset needs every earlier batch queried again too.
        if (restart) continue;
        start += batch.length;
      }
    })();
    return () => {
      controller.abort();
      clearTimeout(timer);
    };
  }, [binding, sessionId, entryKey, ready, settled]);
  return result.binding === binding ? result.values : [];
}

function validTiming(t: RunTiming) {
  return (
    typeof t.runId === "string" &&
    !!t.runId &&
    typeof t.finalEntryId === "string" &&
    Number.isSafeInteger(t.elapsedMs) &&
    t.elapsedMs >= 0 &&
    Number.isSafeInteger(t.startedAt) &&
    Number.isSafeInteger(t.finishedAt) &&
    t.finishedAt - t.startedAt === t.elapsedMs &&
    ["completed", "aborted", "failed", "declined"].includes(t.outcome)
  );
}

/** One footer on the last projected row for the native final message entry. */
export function timingByRow(rows: Row[], timings: RunTiming[]) {
  const lastRow = new Map<string, string>();
  for (const row of rows) if (row.entryId) lastRow.set(row.entryId, row.id);
  const result = new Map<string, RunTiming[]>();
  for (const timing of timings) {
    const id = lastRow.get(timing.finalEntryId);
    if (id) result.set(id, [...(result.get(id) ?? []), timing]);
  }
  return result;
}
