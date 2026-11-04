import {
  createContext,
  useContext,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { api, type ToolProgramCursor } from "./api";
import type { Language } from "./i18n";
import { useToolProgram } from "./useToolProgram";
import { ToolProgramChildCard } from "./ToolProgramChildCard";
import "./tool-program.css";
import { validateChildCursor, type ToolProgramView } from "./toolProgram";

const text = {
  en: {
    inspect: "Inspect child calls",
    hide: "Hide child calls",
    title: "Code Mode child calls",
    scope:
      "Read-only bounded window. No tools are run or replayed. Unobserved lifecycle remains unknown.",
    loading: "Loading child metadata…",
    reconnecting: "Child feed stopped; refreshing a bounded snapshot…",
    unsupported:
      "Child inspection unavailable on this backend or native host. Ordinary tool output is unchanged.",
    error: "Child inspection failed",
    retry: "Refresh newest children",
    empty:
      "No children for this exact parent call in this bounded window (not a full-history claim).",
    older: "Earlier children page",
    historical: "Historical child page — no live updates on this page.",
    newest: "Back to newest children",
    more: "More children exist outside this window.",
    exhausted:
      "This scan is exhausted; this does not establish child completion.",
  },
  zh: {
    inspect: "查看子调用",
    hide: "收起子调用",
    title: "Code Mode 子调用",
    scope: "只读、有界窗口；不执行或重放工具。未观测到的生命周期保持未知。",
    loading: "正在读取子调用元数据…",
    reconnecting: "子调用订阅已停止；正在刷新有界快照…",
    unsupported: "当前后端或原生宿主不支持子调用查询。普通工具输出保持不变。",
    error: "子调用查询失败",
    retry: "刷新最新子调用",
    empty: "此有界窗口中没有与该父调用精确匹配的子调用（不代表全部历史）。",
    older: "更早子调用页",
    historical: "历史子调用页——此页不合并实时更新。",
    newest: "返回最新子调用",
    more: "窗口外仍有更多子调用。",
    exhausted: "当前扫描已结束；不代表子调用已经完成。",
  },
};
type ContextValue = {
  activate: () => () => void;
  live: ReturnType<typeof useToolProgram>;
  refresh: () => void;
  sessionId: string;
};
const Context = createContext<ContextValue | null>(null);

/** One feed per visible session scope; resource owner cancels reads on navigation. */
export function ToolProgramProvider({
  sessionId,
  enabled,
  children,
}: {
  sessionId: string;
  enabled: boolean;
  children: ReactNode;
}) {
  const [readers, setReaders] = useState(0);
  const [revision, setRevision] = useState(0);
  const live = useToolProgram(sessionId, enabled && readers > 0, revision);
  const activate = useRef(() => {
    setReaders((n) => n + 1);
    return () => setReaders((n) => Math.max(0, n - 1));
  }).current;
  return (
    <Context.Provider
      value={{
        sessionId,
        live,
        activate,
        refresh: () => setRevision((n) => n + 1),
      }}
    >
      {children}
    </Context.Provider>
  );
}

export function ToolProgramChildren({
  rootCallId,
  rootToolName,
  language,
}: {
  rootCallId: string;
  rootToolName: string;
  language: Language;
}) {
  const ctx = useContext(Context);
  const [open, setOpen] = useState(false);
  useEffect(() => (open ? ctx?.activate() : undefined), [open, ctx?.activate]);
  if (!ctx) return null;
  const t = text[language];
  return (
    <section
      className="tool-program"
      data-parent-tool-call-id={rootCallId}
      aria-label={t.title}
    >
      <button
        type="button"
        className="tool-program-toggle"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
      >
        {open ? t.hide : t.inspect}
      </button>
      {open && (
        <ChildWindow
          key={ctx.live.generation}
          ctx={ctx}
          rootCallId={rootCallId}
          rootToolName={rootToolName}
          language={language}
        />
      )}
    </section>
  );
}

function ChildWindow({
  ctx,
  rootCallId,
  rootToolName,
  language,
}: {
  ctx: ContextValue;
  rootCallId: string;
  rootToolName: string;
  language: Language;
}) {
  const t = text[language];
  const { live, sessionId } = ctx;
  const [history, setHistory] = useState<ToolProgramView | null>(null);
  const [busy, setBusy] = useState(false),
    [error, setError] = useState("");
  const request = useRef<AbortController | null>(null);
  useEffect(
    () => () => {
      request.current?.abort();
    },
    [],
  );
  async function older(cursor: ToolProgramCursor) {
    if (request.current) return;
    const controller = new AbortController();
    request.current = controller;
    setBusy(true);
    setError("");
    try {
      const page = await api.toolProgramChildren(
        encodeURIComponent(sessionId),
        { cursor, leafId: cursor.leafId, limit: 8, maxBytes: 32768 },
        controller.signal,
      );
      if (controller.signal.aborted) return;
      if (
        !Array.isArray(page.children) ||
        page.children.length > 8 ||
        page.leafId !== cursor.leafId ||
        (page.nextCursor && page.nextCursor.afterSeq >= cursor.afterSeq)
      )
        throw new Error("Invalid child page/cursor");
      validateChildCursor(page.nextCursor, sessionId, page.leafId);
      setHistory({
        ...live.snapshot!,
        page,
        phases: [],
        observations: [],
        events: [],
        overlayPhaseIds: [],
      });
    } catch (cause) {
      if (!controller.signal.aborted)
        setError(cause instanceof Error ? cause.message : t.error);
    } finally {
      if (!controller.signal.aborted) {
        request.current = null;
        setBusy(false);
      }
    }
  }
  const snapshot = history ?? live.snapshot;
  const children =
    snapshot?.page.children.filter(
      (c) => c.rootCallId === rootCallId && c.rootToolName === rootToolName,
    ) ?? [];
  return (
    <div className="tool-program-window">
      <p className="muted">{t.scope}</p>
      {live.status !== "live" && (
        <p role={live.status === "error" ? "alert" : "status"}>
          {t[live.status]}
          {live.error ? ` ${live.error}` : ""}
        </p>
      )}
      {live.status === "error" || live.status === "unsupported" ? (
        <button type="button" onClick={ctx.refresh}>
          {t.retry}
        </button>
      ) : null}
      {history && (
        <p>
          {t.historical}{" "}
          <button
            type="button"
            onClick={() => {
              setHistory(null);
              ctx.refresh();
            }}
          >
            {t.newest}
          </button>
        </p>
      )}
      {snapshot && (
        <>
          {!children.length && <p className="muted">{t.empty}</p>}
          <ol className="tool-program-children">
            {children.map((child) => (
              <li key={child.childId}>
                <ToolProgramChildCard
                  sessionId={sessionId}
                  child={child}
                  leafId={snapshot.page.leafId ?? undefined}
                  page={snapshot.phases.find(
                    (p) => p.childId === child.childId,
                  )}
                  observation={snapshot.observations.find(
                    (o) => o.childId === child.childId,
                  )}
                  overlayPhaseIds={snapshot.overlayPhaseIds}
                  language={language}
                />
              </li>
            ))}
          </ol>
          <p className="muted">
            {snapshot.page.scanExhausted ? t.exhausted : t.more}
          </p>
          {snapshot.page.nextCursor && (
            <button
              type="button"
              disabled={busy}
              onClick={() => void older(snapshot.page.nextCursor!)}
            >
              {t.older}
            </button>
          )}
        </>
      )}
      {error && (
        <p role="alert">
          {t.error}: {error}
        </p>
      )}
    </div>
  );
}
