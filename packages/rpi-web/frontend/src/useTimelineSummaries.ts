import { useCallback, useEffect, useRef, useState } from "react";
import { api, ApiError, type MessageSummaryPage } from "./api";

/** Ephemeral paginated native metadata, not another transcript/index. */
export function useTimelineSummaries(
  sessionId: string,
  supported: boolean,
  snapshotLeafId: string,
) {
  const sourceKey = JSON.stringify([sessionId, supported, snapshotLeafId]);
  const [loadedFor, setLoadedFor] = useState<string | null>(null);
  const [page, setPage] = useState<MessageSummaryPage | null>(null);
  const [loading, setLoading] = useState(supported);
  const [error, setError] = useState("");
  const [legacy, setLegacy] = useState(false);
  const [revision, setRevision] = useState(0);
  const pending = useRef<AbortController | null>(null);
  const epoch = useRef(0);
  useEffect(() => {
    const owner = ++epoch.current;
    pending.current?.abort();
    setPage(null);
    setLoadedFor(sourceKey);
    setLegacy(false);
    setError("");
    if (!supported || !sessionId) {
      setLoading(false);
      return;
    }
    const controller = new AbortController();
    pending.current = controller;
    setLoading(true);
    // Let cleanup cancel a provisional StrictMode mount before issuing I/O.
    void Promise.resolve()
      .then(() =>
        controller.signal.aborted
          ? null
          : api.messageSummaries(
              sessionId,
              {
                limit: 10,
                ...(snapshotLeafId ? { leafId: snapshotLeafId } : {}),
              },
              controller.signal,
            ),
      )
      .then((result) => {
        if (result && owner === epoch.current && !controller.signal.aborted)
          setPage(result);
      })
      .catch((cause: unknown) => {
        if (owner !== epoch.current || controller.signal.aborted) return;
        if (cause instanceof ApiError && [404, 501].includes(cause.status))
          setLegacy(true);
        else setError(cause instanceof Error ? cause.message : String(cause));
      })
      .finally(() => {
        if (owner === epoch.current && !controller.signal.aborted) {
          pending.current = null;
          setLoading(false);
        }
      });
    return () => {
      controller.abort();
      pending.current?.abort();
      ++epoch.current;
    };
  }, [sessionId, supported, revision, snapshotLeafId, sourceKey]);
  const loadEarlier = useCallback(async () => {
    if (!page?.hasMore || !page.nextBeforeId || !page.leafId || pending.current)
      return;
    const controller = new AbortController();
    pending.current = controller;
    const owner = epoch.current;
    setLoading(true);
    setError("");
    try {
      const result = await api.messageSummaries(
        sessionId,
        { limit: 10, beforeId: page.nextBeforeId, leafId: page.leafId },
        controller.signal,
      );
      if (owner !== epoch.current || controller.signal.aborted) return;
      const seen = new Set(result.messages.map((message) => message.entryId));
      if (
        result.hasMore &&
        (!result.nextBeforeId ||
          result.nextBeforeId === page.nextBeforeId ||
          !result.messages.length)
      )
        throw new Error("Native summary cursor made no progress");
      setPage({
        ...result,
        firstId: result.firstId ?? page.firstId,
        lastId: page.lastId,
        messages: [
          ...result.messages,
          ...page.messages.filter((message) => !seen.has(message.entryId)),
        ],
      });
    } catch (cause) {
      if (owner === epoch.current && !controller.signal.aborted)
        setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      if (owner === epoch.current && !controller.signal.aborted) {
        pending.current = null;
        setLoading(false);
      }
    }
  }, [page, sessionId]);
  return {
    enabled: supported && !(loadedFor === sourceKey && legacy),
    page: loadedFor === sourceKey ? page : null,
    loading: loading || (supported && loadedFor !== sourceKey),
    error: loadedFor === sourceKey ? error : "",
    loadEarlier,
    retry: () => setRevision((value) => value + 1),
  };
}
