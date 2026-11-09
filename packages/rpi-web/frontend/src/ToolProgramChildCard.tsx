import { useEffect, useRef, useState } from "react";
import {
  api,
  type ToolProgramChildSummary,
  type ToolProgramPhasePage,
  type ToolProgramObservation,
  type ToolProgramPhase,
  type ToolProgramBodyResponse,
} from "./api";
import type { Language } from "./i18n";
import { childBodyText, validateChildCursor } from "./toolProgram";
const labels = {
  en: {
    unknown: "Unknown",
    finalObserved: "Final observed (closure not established)",
    succeeded: "Succeeded",
    failed: "Failed",
    cancelled: "Cancelled",
    interrupted: "Interrupted",
    requested: "Requested",
    prepared: "Prepared",
    update: "Update",
    final: "Final",
    closed: "Closed",
    copied: "Historical fork copy — not execution in this session",
    parent: "Parent child",
    provenance: "Native origin",
    phases: "Load phases",
    older: "Earlier phases page",
    noPhases: "No phases in this bounded scan; lifecycle unknown.",
    exhausted: "Phase scan exhausted — not proof of completion.",
    body: "Read body page",
    nextBody: "Read next body page",
    fragment:
      "Body byte fragment (up to 64 KiB); unread chunks / whole-body integrity not verified here. UTF-8 edge characters may be partial.",
    closeBody: "Hide body",
    error: "Read failed",
    bodyBytes: "Bytes",
    status: "Observed outcome",
  },
  zh: {
    unknown: "未知",
    finalObserved: "已观测 final（尚未确认关闭）",
    succeeded: "成功",
    failed: "失败",
    cancelled: "已取消",
    interrupted: "已中断",
    requested: "请求",
    prepared: "已准备",
    update: "更新",
    final: "最终结果",
    closed: "已关闭",
    copied: "历史 fork 副本——不代表在本会话中执行",
    parent: "父子调用",
    provenance: "原生来源",
    phases: "读取阶段",
    older: "更早阶段页",
    noPhases: "此有界扫描中没有阶段；生命周期未知。",
    exhausted: "阶段扫描已结束——不代表已完成。",
    body: "读取正文页",
    nextBody: "读取下一正文页",
    fragment:
      "正文为字节片段（最多 64 KiB）；未读取片段及全文完整性未在此验证。UTF-8 边缘字符可能不完整。",
    closeBody: "隐藏正文",
    error: "读取失败",
    bodyBytes: "字节",
    status: "观测结果",
  },
};

export function ToolProgramChildCard({
  sessionId,
  child,
  leafId,
  page,
  observation,
  overlayPhaseIds,
  language,
}: {
  sessionId: string;
  child: ToolProgramChildSummary;
  leafId?: string;
  page?: ToolProgramPhasePage;
  observation?: ToolProgramObservation;
  overlayPhaseIds: readonly string[];
  language: Language;
}) {
  const t = labels[language];
  const [history, setHistory] = useState<ToolProgramPhasePage | null>(null);
  const [busy, setBusy] = useState(false),
    [error, setError] = useState("");
  const request = useRef<AbortController | null>(null);
  useEffect(() => () => request.current?.abort(), []);
  const shown = history ?? page;
  async function load(earlier: boolean) {
    if (request.current) return;
    const controller = new AbortController();
    request.current = controller;
    setBusy(true);
    setError("");
    const cursor = earlier ? (shown?.nextCursor ?? undefined) : undefined;
    try {
      const result = await api.toolProgramPhases(
        encodeURIComponent(sessionId),
        { childId: child.childId, leafId, cursor, limit: 8, maxBytes: 32768 },
        controller.signal,
      );
      if (controller.signal.aborted) return;
      if (
        result.childId !== child.childId ||
        result.leafId !== leafId ||
        !Array.isArray(result.phases) ||
        result.phases.length > 8 ||
        (cursor &&
          result.nextCursor &&
          result.nextCursor.afterSeq >= cursor.afterSeq)
      )
        throw new Error("Invalid child phase page/cursor");
      validateChildCursor(
        result.nextCursor,
        sessionId,
        result.leafId,
        child.childId,
      );
      setHistory(result);
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
  return (
    <details className="tool-program-child" data-child-id={child.childId}>
      <summary>
        <strong>{child.toolName}</strong>
        <span>#{child.ordinal}</span>
        <span>{t[observation?.outcome ?? "unknown"]}</span>
      </summary>
      <div className="tool-program-child-detail">
        <p>
          {t.status}: {t[observation?.outcome ?? "unknown"]}
        </p>
        {child.originVerification === "forkCopy" && (
          <p className="tool-program-warning">{t.copied}</p>
        )}
        <dl>
          <dt>ID</dt>
          <dd>{child.childId}</dd>
          <dt>{t.provenance}</dt>
          <dd>
            {child.origin?.runId} · {child.origin?.assistantEntryId} ·{" "}
            {child.origin?.toolIndex}
          </dd>
          {child.parentChildId && (
            <>
              <dt>{t.parent}</dt>
              <dd>{child.parentChildId}</dd>
            </>
          )}
        </dl>
        {!shown && (
          <button
            type="button"
            disabled={busy}
            onClick={() => void load(false)}
          >
            {t.phases}
          </button>
        )}
        {shown && (
          <>
            {!shown.phases.length && <p>{t.noPhases}</p>}
            <ol className="tool-program-phases">
              {shown.phases.map((phase) => (
                <li key={phase.entryId}>
                  <span>
                    {t[phase.phase]} #{phase.index}
                  </span>
                  {phase.descriptor && (
                    <BodyPage
                      key={`${phase.entryId}:${shown.leafId}`}
                      sessionId={sessionId}
                      phase={phase}
                      // A replay/live phase may be newer than the immutable
                      // page leaf. Its explicit first body read pins the current
                      // native branch independently; continuations retain that
                      // returned leaf instead of rebasing historical cursors.
                      leafId={
                        history || !overlayPhaseIds.includes(phase.entryId)
                          ? shown.leafId
                          : undefined
                      }
                      language={language}
                    />
                  )}
                </li>
              ))}
            </ol>
            {shown.scanExhausted && <p className="muted">{t.exhausted}</p>}
            {shown.nextCursor && (
              <button
                type="button"
                disabled={busy}
                onClick={() => void load(true)}
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
    </details>
  );
}

function BodyPage({
  sessionId,
  phase,
  leafId,
  language,
}: {
  sessionId: string;
  phase: ToolProgramPhase;
  leafId?: string;
  language: Language;
}) {
  const t = labels[language];
  const [body, setBody] = useState<{
    page: ToolProgramBodyResponse;
    text: string;
  } | null>(null);
  const [busy, setBusy] = useState(false),
    [error, setError] = useState("");
  const request = useRef<AbortController | null>(null);
  useEffect(() => () => request.current?.abort(), []);
  async function load(next: boolean) {
    if (request.current || !phase.descriptor) return;
    const controller = new AbortController();
    request.current = controller;
    setBusy(true);
    setError("");
    try {
      const pinnedLeaf = next ? body?.page.body.leafId : leafId;
      const page = await api.toolProgramBody(
        encodeURIComponent(sessionId),
        {
          childId: phase.childId,
          phaseEntryId: phase.entryId,
          descriptorId: phase.descriptor.descriptorId,
          leafId: pinnedLeaf,
          cursor: next ? (body?.page.nextCursor ?? undefined) : undefined,
          limit: 2,
          maxBytes: 65536,
        },
        controller.signal,
      );
      if (controller.signal.aborted) return;
      if (
        page.childId !== phase.childId ||
        page.phaseEntryId !== phase.entryId ||
        typeof page.body.leafId !== "string" ||
        !page.body.leafId ||
        (pinnedLeaf !== undefined && page.body.leafId !== pinnedLeaf) ||
        page.body.descriptor.descriptorId !== phase.descriptor.descriptorId
      )
        throw new Error("Invalid child body binding");
      const cursor = page.nextCursor;
      const previousIndex = next
        ? (body?.page.nextCursor?.body.nextIndex ?? 0)
        : 0;
      if (
        page.body.byteOffset !== previousIndex * phase.descriptor.chunkBytes ||
        (cursor &&
          (cursor.direction !== "oldestFirst" ||
            cursor.childId !== phase.childId ||
            cursor.phaseEntryId !== phase.entryId ||
            cursor.body.sessionId !== sessionId ||
            cursor.body.lane !== "main" ||
            cursor.body.leafId !== page.body.leafId ||
            cursor.body.descriptorId !== phase.descriptor.descriptorId ||
            !Number.isSafeInteger(cursor.body.nextIndex) ||
            cursor.body.nextIndex <= previousIndex))
      )
        throw new Error("Invalid child body cursor");
      setBody({ page, text: childBodyText(page) });
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
  return (
    <div className="tool-program-body">
      {!body && (
        <button type="button" disabled={busy} onClick={() => void load(false)}>
          {t.body}
        </button>
      )}
      {body && (
        <>
          <p className="muted">{t.fragment}</p>
          <p>
            {t.bodyBytes}: {body.page.body.byteOffset}–
            {body.page.body.byteOffset + body.page.body.byteLength} /{" "}
            {body.page.body.totalBytes}
          </p>
          <pre>{body.text}</pre>
          {body.page.nextCursor && (
            <button
              type="button"
              disabled={busy}
              onClick={() => void load(true)}
            >
              {t.nextBody}
            </button>
          )}
          <button
            type="button"
            onClick={() => {
              request.current?.abort();
              request.current = null;
              setBusy(false);
              setBody(null);
              setError("");
            }}
          >
            {t.closeBody}
          </button>
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
