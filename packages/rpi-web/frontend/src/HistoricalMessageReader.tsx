import { createPortal } from "react-dom";
import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { api, type MessageWindowPage } from "./api";
import { rowsFromMessages, type Row } from "./transcript";
import { translate, type Language } from "./i18n";

/** Replace one bounded window at a time; never prepend history into the live chat. */
export function HistoricalMessageReader({
  sessionId,
  anchorId,
  leafId,
  language,
  renderRow,
  onClose,
}: {
  sessionId: string;
  anchorId: string;
  leafId: string;
  language: Language;
  renderRow: (row: Row) => ReactNode;
  onClose: () => void;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  const body = useRef<HTMLDivElement>(null);
  const request = useRef<AbortController | null>(null);
  const [page, setPage] = useState<MessageWindowPage | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const [retry, setRetry] = useState(0);
  const t = (key: Parameters<typeof translate>[1]) => translate(language, key);
  const load = useCallback(
    async (cursor: {
      anchorId?: string;
      beforeId?: string;
      afterId?: string;
    }) => {
      if (request.current) return;
      const controller = new AbortController();
      request.current = controller;
      setLoading(true);
      setError("");
      try {
        // A closed/provisional mount must not consume a native window request.
        await Promise.resolve();
        if (controller.signal.aborted) return;
        const result = await api.messageWindow(
          sessionId,
          { ...cursor, leafId, limit: 48 },
          controller.signal,
        );
        if (!controller.signal.aborted) setPage(result);
      } catch (cause) {
        if (!controller.signal.aborted)
          setError(cause instanceof Error ? cause.message : String(cause));
      } finally {
        if (!controller.signal.aborted) {
          request.current = null;
          setLoading(false);
        }
      }
    },
    [sessionId, leafId],
  );
  useEffect(() => {
    setPage(null);
    request.current = null;
    void load({ anchorId });
    return () => {
      request.current?.abort();
      request.current = null;
    };
  }, [load, anchorId, retry]);
  useEffect(() => {
    const previous = document.activeElement;
    const node = dialog.current!;
    node.showModal();
    return () => {
      node.close();
      if (previous instanceof HTMLElement && previous.isConnected)
        previous.focus({ preventScroll: true });
    };
  }, []);
  const rows = useMemo(() => rowsFromMessages(page?.messages ?? []), [page]);
  useEffect(() => {
    const node = body.current;
    if (!node || !page) return;
    const target = Array.from(
      node.querySelectorAll<HTMLElement>("[data-history-entry]"),
    ).find((element) => element.dataset.historyEntry === anchorId);
    node.scrollTop = target ? target.offsetTop - node.offsetTop : 0;
  }, [page, anchorId]);
  return createPortal(
    <dialog
      className="historical-reader"
      ref={dialog}
      aria-labelledby="historical-reader-title"
      onCancel={(event) => {
        event.preventDefault();
        onClose();
      }}
    >
      <header>
        <div>
          <h2 id="historical-reader-title">{t("historyContextTitle")}</h2>
          <p>{t("historyContextScope")}</p>
        </div>
        <button type="button" onClick={onClose} aria-label={t("closeDialog")}>
          ×
        </button>
      </header>
      <nav aria-label={t("historyContextPagination")}>
        <button
          type="button"
          disabled={loading || !page?.hasMoreBefore || !page.nextBeforeId}
          onClick={() => void load({ beforeId: page!.nextBeforeId! })}
        >
          {t("historyContextEarlier")}
        </button>
        <button
          type="button"
          disabled={loading || !page?.hasMoreAfter || !page.nextAfterId}
          onClick={() => void load({ afterId: page!.nextAfterId! })}
        >
          {t("historyContextLater")}
        </button>
      </nav>
      {loading && <p role="status">{t("loadingEarlierMessages")}</p>}
      {error && (
        <div role="alert">
          {error}
          <button type="button" onClick={() => setRetry((value) => value + 1)}>
            {t("historyContextRetry")}
          </button>
        </div>
      )}
      <div className="historical-reader-body" ref={body} aria-busy={loading}>
        {rows.map((row) => (
          <div key={row.id} data-history-entry={row.entryId}>
            {renderRow(row)}
          </div>
        ))}
        {page && !rows.length && <p>{t("historyContextEmpty")}</p>}
      </div>
    </dialog>,
    document.body,
  );
}
