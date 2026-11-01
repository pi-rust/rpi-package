import { useCallback, useLayoutEffect, useMemo, useRef, useState } from "react";
import { api, type MessageWindowPage } from "./api";
import { rowsFromMessages } from "./transcript";

export type HistoryTarget = { entryId: string; leafId: string };
type Cursor = { anchorId: string } | { beforeId: string } | { afterId: string };
type WindowState = {
  scope: string;
  target: HistoryTarget;
  cursor: Cursor;
  page: MessageWindowPage | null;
  loading: boolean;
  error: string;
};

/** One replaceable, pinned reading window. Live rows and their SSE cursor stay independent. */
export function useHistoricalMessages(sessionId: string, scope: string) {
  const [state, setState] = useState<WindowState | null>(null);
  const binding = useRef(scope);
  binding.current = scope;
  const request = useRef<AbortController | null>(null);
  useLayoutEffect(() => () => {
    request.current?.abort();
    request.current = null;
  }, [scope]);

  const load = useCallback(async (target: HistoryTarget, cursor: Cursor) => {
    request.current?.abort();
    const controller = new AbortController();
    request.current = controller;
    const current = () => !controller.signal.aborted && request.current === controller && binding.current === scope;
    setState(previous => ({
      scope, target, cursor,
      page: previous?.scope === scope && previous.target === target ? previous.page : null,
      loading: true, error: "",
    }));
    try {
      const page = await api.messageWindow(sessionId, { ...cursor, leafId: target.leafId, limit: 48 }, controller.signal);
      if (current()) setState({ scope, target, cursor, page, loading: false, error: "" });
    } catch (cause) {
      if (current()) setState(previous => previous?.scope === scope ? {
        ...previous, loading: false, error: cause instanceof Error ? cause.message : String(cause),
      } : previous);
    } finally {
      if (request.current === controller) request.current = null;
    }
  }, [sessionId, scope]);
  const close = useCallback(() => {
    request.current?.abort();
    request.current = null;
    setState(null);
  }, []);
  const selected = state?.scope === scope ? state : null;
  const rows = useMemo(() => rowsFromMessages(selected?.page?.messages ?? []), [selected?.page]);
  return {
    active: Boolean(selected), rows,
    page: selected?.page ?? null,
    cursor: selected?.cursor,
    target: selected?.target,
    loading: selected?.loading ?? false,
    error: selected?.error ?? "",
    open: (target: HistoryTarget) => void load(target, { anchorId: target.entryId }),
    retry: () => { if (selected && !request.current) void load(selected.target, selected.cursor); },
    earlier: () => { if (selected && !request.current && !selected.loading && selected.page?.hasMoreBefore && selected.page.nextBeforeId) void load(selected.target, { beforeId: selected.page.nextBeforeId }); },
    later: () => { if (selected && !request.current && !selected.loading && selected.page?.hasMoreAfter && selected.page.nextAfterId) void load(selected.target, { afterId: selected.page.nextAfterId }); },
    cancelPending: () => {
      request.current?.abort();
      request.current = null;
      setState(previous => previous?.scope === scope ? { ...previous, loading: false, error: "" } : previous);
    },
    close,
  };
}
