import {
  useEffect,
  useId,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type RefObject,
} from "react";
import { ListTree, X } from "lucide-react";
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
  onNavigate,
}: {
  rows: Row[];
  transcript: RefObject<HTMLDivElement | null>;
  language: Language;
  hasMore: boolean;
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
  const toggle = useRef<HTMLButtonElement>(null);
  const [active, setActive] = useState("");
  const [open, setOpen] = useState(false);
  const panelId = useId();
  const t = (
    key: Parameters<typeof translate>[1],
    values?: Record<string, string | number>,
  ) => translate(language, key, values);

  useLayoutEffect(() => {
    const node = transcript.current;
    const rowIds = JSON.parse(targetKey) as string[];
    if (!node || rowIds.length < 2) return;
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
      targets.current.clear();
    };
    // Previews may change while typing; only new/replaced targets require rebinding.
  }, [transcript, targetKey]);

  useEffect(() => {
    const container = list.current;
    const selected = container?.querySelector<HTMLElement>(
      '[aria-current="location"]',
    );
    if (!container || !selected) return;
    const offset = selected.offsetTop;
    if (offset < container.scrollTop) container.scrollTop = offset;
    else if (
      offset + selected.offsetHeight >
      container.scrollTop + container.clientHeight
    )
      container.scrollTop =
        offset + selected.offsetHeight - container.clientHeight;
  }, [active, open, targetKey]);

  useEffect(() => {
    if (!open) return;
    const dismiss = (event: PointerEvent) => {
      if (event.target instanceof Node && !root.current?.contains(event.target))
        setOpen(false);
    };
    const escape = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.stopPropagation();
      setOpen(false);
      toggle.current?.focus();
    };
    document.addEventListener("pointerdown", dismiss);
    document.addEventListener("keydown", escape);
    return () => {
      document.removeEventListener("pointerdown", dismiss);
      document.removeEventListener("keydown", escape);
    };
  }, [open]);

  if (entries.length < 2) return null;
  return (
    <aside
      className={`conversation-timeline${open ? " expanded" : ""}`}
      ref={root}
    >
      <button
        type="button"
        className="timeline-toggle"
        ref={toggle}
        aria-label={t("conversationTimeline")}
        title={t("conversationTimeline")}
        aria-expanded={open}
        aria-controls={panelId}
        onClick={() => setOpen((value) => !value)}
      >
        {open ? (
          <X size={15} aria-hidden="true" />
        ) : (
          <ListTree size={15} aria-hidden="true" />
        )}
      </button>
      <nav
        id={panelId}
        className="timeline-panel"
        aria-label={t("conversationTimeline")}
      >
        <div className="timeline-heading">
          {t("conversationTimeline")} <span>{entries.length}</span>
        </div>
        <ol ref={list}>
          {entries.map((entry, index) => (
            <li key={entry.id}>
              <button
                type="button"
                className="timeline-node"
                title={entry.preview}
                data-timeline-target={entry.rowId}
                aria-label={t("timelineGoToMessage", {
                  index: index + 1,
                  preview: entry.preview,
                })}
                aria-current={active === entry.rowId ? "location" : undefined}
                onClick={() => {
                  const target = targets.current.get(entry.rowId);
                  if (!target || !transcript.current?.contains(target)) return;
                  onNavigate(target);
                  setActive(entry.rowId);
                  if (open) {
                    setOpen(false);
                    toggle.current?.focus();
                  }
                }}
              >
                <span className="timeline-mark" aria-hidden="true" />
                <span className="timeline-label">
                  <small>{index + 1}</small>
                  {entry.preview}
                </span>
              </button>
            </li>
          ))}
        </ol>
        {hasMore && (
          <p className="timeline-earlier">{t("timelineLoadedOnly")}</p>
        )}
      </nav>
    </aside>
  );
}
