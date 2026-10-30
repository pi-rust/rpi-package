import {
  useEffect,
  useId,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
  type PointerEvent,
  type RefObject,
} from "react";
import type { Row } from "./transcript";
import { userMessageParts } from "./attachments";
import { translate, type Language } from "./i18n";

const previews = new WeakMap<Row, string>();

/** A view of loaded messages only, never a second history or transcript index. */
function timelineEntries(rows: Row[], language: Language) {
  const seen = new Set<string>();
  return rows.flatMap((row) => {
    if ((row.kind !== "text" && row.kind !== "image") || row.role !== "user")
      return [];
    const id = row.entryId ?? row.id.replace(/:image\d+$/, "");
    if (seen.has(id)) return [];
    seen.add(id);
    let preview = previews.get(row);
    if (preview === undefined) {
      preview =
        row.kind === "text"
          ? userMessageParts(row.text)
              .map((part) =>
                part.kind === "attachment" ? part.name : part.text,
              )
              .join(" · ")
              .replace(/\s+/g, " ")
              .trim()
              .slice(0, 140)
          : "";
      previews.set(row, preview);
    }
    return [
      {
        id,
        rowId: row.id,
        preview: preview || translate(language, "timelineImageMessage"),
      },
    ];
  });
}

export function ConversationTimeline({
  rows,
  transcript,
  language,
  hasMore,
  loadingEarlier,
  onLoadEarlier,
  onNavigate,
}: {
  rows: Row[];
  transcript: RefObject<HTMLDivElement | null>;
  language: Language;
  hasMore: boolean;
  loadingEarlier: boolean;
  onLoadEarlier: () => void;
  onNavigate: (target: HTMLElement) => void;
}) {
  const entries = useMemo(
    () => timelineEntries(rows, language),
    [rows, language],
  );
  const targetKey = JSON.stringify(entries.map((entry) => entry.rowId));
  const targets = useRef(new Map<string, HTMLElement>());
  const root = useRef<HTMLElement>(null);
  const list = useRef<HTMLOListElement>(null);
  const tooltip = useRef<HTMLDivElement>(null);
  const drag = useRef<{
    pointerId: number;
    node: HTMLOListElement;
    buttons: HTMLButtonElement[];
    startY: number;
    x: number;
    y: number;
    moved: boolean;
    startId: string;
    lastId: string;
    frame: number;
    time: number;
  } | null>(null);
  const ignorePointerClick = useRef(false);
  const releasedAt = useRef<{ x: number; y: number } | null>(null);
  const browsing = useRef({
    pointer: false,
    focus: false,
    until: 0,
  });
  const [active, setActive] = useState("");
  const [focused, setFocused] = useState("");
  const [preview, setPreview] = useState<{ id: string; top: number } | null>(
    null,
  );
  const descriptionId = useId();
  const previewId = useId();
  const t = (
    key: Parameters<typeof translate>[1],
    values?: Record<string, string | number>,
  ) => translate(language, key, values);

  const stopDrag = () => {
    const gesture = drag.current;
    if (!gesture) return;
    drag.current = null;
    releasedAt.current = { x: gesture.x, y: gesture.y };
    cancelAnimationFrame(gesture.frame);
    delete gesture.node.dataset.scrubbing;
    if (gesture.node.hasPointerCapture(gesture.pointerId))
      gesture.node.releasePointerCapture(gesture.pointerId);
    browsing.current.until = performance.now() + 1000;
  };

  useLayoutEffect(() => {
    const node = transcript.current;
    const rowIds = JSON.parse(targetKey) as string[];
    if (!node || !rowIds.length) return;
    const wanted = new Set(rowIds);
    const ordered: { id: string; element: HTMLElement }[] = [];
    targets.current.clear();
    node
      .querySelectorAll<HTMLElement>("[data-transcript-row]")
      .forEach((element) => {
        const id = element.dataset.transcriptRow!;
        if (!wanted.has(id)) return;
        targets.current.set(id, element);
        ordered.push({ id, element });
      });
    let frame = 0;
    const update = () => {
      frame = 0;
      const top =
        node.getBoundingClientRect().top +
        Math.min(96, node.clientHeight * 0.15);
      let left = 0;
      let right = ordered.length;
      while (left < right) {
        const middle = (left + right) >>> 1;
        if (ordered[middle].element.getBoundingClientRect().top <= top)
          left = middle + 1;
        else right = middle;
      }
      setActive(ordered[Math.max(0, left - 1)]?.id ?? "");
    };
    const schedule = () => {
      if (!frame) frame = requestAnimationFrame(update);
    };
    node.addEventListener("scroll", schedule, { passive: true });
    const observer = new ResizeObserver(schedule);
    observer.observe(node);
    const messages = node.querySelector(".message-list");
    if (messages) observer.observe(messages);
    schedule();
    return () => {
      node.removeEventListener("scroll", schedule);
      observer.disconnect();
      cancelAnimationFrame(frame);
      stopDrag();
      targets.current.clear();
    };
    // Previews may change while typing; only new/replaced targets require rebinding.
  }, [transcript, targetKey]);

  const reveal = (button: HTMLElement) => {
    const container = list.current;
    if (!container) return;
    const offset = button.offsetTop;
    if (offset < container.scrollTop) container.scrollTop = offset;
    else if (
      offset + button.offsetHeight >
      container.scrollTop + container.clientHeight
    )
      container.scrollTop =
        offset + button.offsetHeight - container.clientHeight;
  };

  useEffect(() => {
    if (
      browsing.current.pointer ||
      browsing.current.focus ||
      drag.current ||
      performance.now() < browsing.current.until
    )
      return;
    const selected = list.current?.querySelector<HTMLElement>(
      '[aria-current="location"]',
    );
    if (selected) reveal(selected);
  }, [active, targetKey]);

  const previewEntry = entries.find((entry) => entry.rowId === preview?.id);
  const hasPreview = Boolean(previewEntry);
  useEffect(() => {
    if (!hasPreview) return;
    const dismiss = (event: globalThis.KeyboardEvent) => {
      if (event.key !== "Escape" || event.isComposing) return;
      event.stopPropagation();
      stopDrag();
      setPreview(null);
    };
    document.addEventListener("keydown", dismiss, true);
    return () => document.removeEventListener("keydown", dismiss, true);
  }, [hasPreview]);
  useLayoutEffect(() => {
    const rail = root.current;
    const node = tooltip.current;
    if (!preview || !rail || !node) return;
    const half = node.offsetHeight / 2;
    const top = Math.max(half, Math.min(preview.top, rail.clientHeight - half));
    if (top !== preview.top) setPreview({ ...preview, top });
  }, [preview, previewEntry?.preview]);

  const showPreview = (button: HTMLElement, id: string) => {
    const rail = root.current;
    if (!rail) return;
    const box = button.getBoundingClientRect();
    const half = (tooltip.current?.offsetHeight ?? 0) / 2;
    const center = box.top + box.height / 2 - rail.getBoundingClientRect().top;
    const top = Math.max(half, Math.min(center, rail.clientHeight - half));
    setPreview((previous) =>
      previous?.id === id && previous.top === top ? previous : { id, top },
    );
  };
  const navigate = (rowId: string) => {
    const target = targets.current.get(rowId);
    if (!target || !transcript.current?.contains(target)) return;
    onNavigate(target);
    setActive(rowId);
    setPreview(null);
  };
  // The rail scrolls independently, but a held pointer scrubs the loaded turns.
  const scrub = (gesture: NonNullable<typeof drag.current>, elapsed = 0) => {
    const { node, buttons, y } = gesture;
    const box = node.getBoundingClientRect();
    if (!box.height || !buttons.length) return;
    const edge = Math.min(24, box.height / 4);
    const direction = y < box.top + edge ? -1 : y > box.bottom - edge ? 1 : 0;
    node.scrollTop += (direction * 480 * elapsed) / 1000;
    const offset =
      node.scrollTop + Math.max(0, Math.min(y - box.top, box.height - 1));
    const index = Math.max(
      0,
      Math.min(
        buttons.length - 1,
        Math.floor((offset - buttons[0].offsetTop) / buttons[0].offsetHeight),
      ),
    );
    const button = buttons[index];
    const id = button.dataset.timelineTarget!;
    if (gesture.lastId !== id) {
      gesture.lastId = id;
      navigate(id);
    }
    showPreview(button, id);
  };
  const startDrag = (event: PointerEvent<HTMLOListElement>) => {
    if (!event.isPrimary || event.button !== 0 || drag.current) return;
    const button = (event.target as HTMLElement).closest<HTMLButtonElement>(
      ".timeline-node",
    );
    if (!button || !event.currentTarget.contains(button)) return;
    const node = event.currentTarget;
    drag.current = {
      pointerId: event.pointerId,
      node,
      buttons: Array.from(
        node.querySelectorAll<HTMLButtonElement>(".timeline-node"),
      ),
      startY: event.clientY,
      x: event.clientX,
      y: event.clientY,
      moved: false,
      startId: button.dataset.timelineTarget!,
      lastId: "",
      frame: 0,
      time: 0,
    };
    ignorePointerClick.current = true;
    node.setPointerCapture(event.pointerId);
    // Keep the composer/window in place; taps are handled on pointerup.
    event.preventDefault();
  };
  const moveDrag = (event: PointerEvent<HTMLOListElement>) => {
    const gesture = drag.current;
    if (!gesture || gesture.pointerId !== event.pointerId) return;
    gesture.x = event.clientX;
    gesture.y = event.clientY;
    // A chorded mouse release emits pointermove until its last button is up.
    if (!(event.buttons & 1)) {
      stopDrag();
      setPreview(null);
      return;
    }
    if (!gesture.moved && Math.abs(gesture.y - gesture.startY) < 4) return;
    gesture.moved = true;
    gesture.node.dataset.scrubbing = "true";
    if (gesture.frame) return;
    const step = (time: number) => {
      if (drag.current !== gesture) return;
      const elapsed = gesture.time ? Math.min(32, time - gesture.time) : 0;
      gesture.time = time;
      scrub(gesture, elapsed);
      gesture.frame = requestAnimationFrame(step);
    };
    gesture.frame = requestAnimationFrame(step);
  };
  const finishDrag = (event: PointerEvent<HTMLOListElement>) => {
    const gesture = drag.current;
    if (!gesture || gesture.pointerId !== event.pointerId) return;
    gesture.x = event.clientX;
    gesture.y = event.clientY;
    if (gesture.moved) scrub(gesture);
    else navigate(gesture.startId);
    stopDrag();
    setPreview(null);
  };
  const cancelDrag = (event: PointerEvent<HTMLOListElement>) => {
    if (event.target !== event.currentTarget) return;
    if (drag.current?.pointerId !== event.pointerId) return;
    stopDrag();
    setPreview(null);
  };

  const moveFocus = (
    event: KeyboardEvent<HTMLButtonElement>,
    index: number,
  ) => {
    if (event.key === "Escape") {
      event.stopPropagation();
      setPreview(null);
      return;
    }
    const next =
      event.key === "ArrowUp"
        ? Math.max(0, index - 1)
        : event.key === "ArrowDown"
          ? Math.min(entries.length - 1, index + 1)
          : event.key === "Home"
            ? 0
            : event.key === "End"
              ? entries.length - 1
              : -1;
    if (next < 0) return;
    event.preventDefault();
    const button =
      list.current?.querySelectorAll<HTMLButtonElement>(".timeline-node")[next];
    if (!button) return;
    reveal(button);
    button.focus({ preventScroll: true });
    showPreview(button, entries[next].rowId);
  };
  const tabTarget = entries.some((entry) => entry.rowId === focused)
    ? focused
    : active || entries[0]?.rowId;

  if (!entries.length && !hasMore) return null;
  return (
    <aside
      className="conversation-timeline"
      ref={root}
      onPointerEnter={(event) => {
        if (event.pointerType !== "touch") browsing.current.pointer = true;
      }}
      onPointerLeave={() => {
        browsing.current.pointer = false;
        if (!drag.current) {
          releasedAt.current = null;
          setPreview(null);
        }
      }}
    >
      <nav
        className="timeline-panel"
        aria-label={t("conversationTimeline")}
        aria-describedby={descriptionId}
        onFocusCapture={() => {
          browsing.current.focus = true;
        }}
        onBlurCapture={(event) => {
          if (
            event.relatedTarget instanceof Node &&
            event.currentTarget.contains(event.relatedTarget)
          )
            return;
          browsing.current.focus = false;
          setFocused("");
          setPreview(null);
        }}
      >
        <p id={descriptionId} className="timeline-description">
          {t("timelineScrollHint")}
          {hasMore ? ` ${t("timelineLoadedOnly")}` : ""}
        </p>
        {hasMore && (
          <button
            type="button"
            className="timeline-load-earlier"
            aria-label={t("timelineLoadEarlier")}
            title={t("timelineLoadEarlier")}
            disabled={loadingEarlier}
            aria-busy={loadingEarlier}
            onClick={onLoadEarlier}
          >
            <span aria-hidden="true">{loadingEarlier ? "…" : "↑"}</span>
          </button>
        )}
        <ol
          ref={list}
          onPointerDown={startDrag}
          onPointerMove={moveDrag}
          onPointerUp={finishDrag}
          onPointerCancel={cancelDrag}
          onLostPointerCapture={cancelDrag}
          onClickCapture={(event) => {
            if (!ignorePointerClick.current || event.detail === 0) return;
            ignorePointerClick.current = false;
            event.preventDefault();
            event.stopPropagation();
          }}
          onScroll={() => {
            if (drag.current) return;
            const button = document.activeElement;
            if (
              preview &&
              button instanceof HTMLElement &&
              button.dataset.timelineTarget === preview.id &&
              list.current?.contains(button)
            )
              showPreview(button, preview.id);
            else setPreview(null);
          }}
        >
          {entries.map((entry, index) => (
            <li key={entry.id}>
              <button
                type="button"
                className="timeline-node"
                data-timeline-target={entry.rowId}
                tabIndex={entry.rowId === tabTarget ? 0 : -1}
                aria-label={t("timelineGoToMessage", {
                  index: index + 1,
                  preview: entry.preview,
                })}
                aria-current={active === entry.rowId ? "location" : undefined}
                aria-describedby={
                  previewEntry?.id === entry.id ? previewId : undefined
                }
                onPointerEnter={(event) => {
                  // Releasing capture can re-enter the tick under a stationary pointer.
                  const released = releasedAt.current;
                  if (
                    released?.x === event.clientX &&
                    released.y === event.clientY
                  )
                    return;
                  releasedAt.current = null;
                  if (event.pointerType !== "touch" && !drag.current)
                    showPreview(event.currentTarget, entry.rowId);
                }}
                onFocus={(event) => {
                  setFocused(entry.rowId);
                  showPreview(event.currentTarget, entry.rowId);
                }}
                onKeyDown={(event) => moveFocus(event, index)}
                onClick={() => navigate(entry.rowId)}
              >
                <span className="timeline-mark" aria-hidden="true" />
              </button>
            </li>
          ))}
        </ol>
      </nav>
      {previewEntry && preview && (
        <div
          id={previewId}
          role="tooltip"
          className="timeline-preview"
          ref={tooltip}
          style={{
            top: preview.top,
            maxWidth: Math.min(
              240,
              Math.max(0, (transcript.current?.clientWidth ?? 304) - 64),
            ),
          }}
        >
          <span className="timeline-preview-index">
            {entries.indexOf(previewEntry) + 1} / {entries.length}
          </span>
          <span className="timeline-label">{previewEntry.preview}</span>
          {hasMore && (
            <small className="timeline-earlier">
              {t("timelineLoadedOnly")}
            </small>
          )}
        </div>
      )}
    </aside>
  );
}
