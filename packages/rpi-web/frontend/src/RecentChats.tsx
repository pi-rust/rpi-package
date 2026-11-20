import { useEffect, useMemo, useRef } from "react";
import { Check, Clock3, X } from "lucide-react";
import type { Project, SessionSummary } from "./api";
import { translate, type Language } from "./i18n";

/**
 * Recent chats as a small floating window.
 *
 * The permanent in-sidebar list duplicated the project tree row for row (the
 * tree already contains every session). The content is still worth keeping —
 * cross-project recency is the one thing the tree cannot express — so it is
 * opt-in and costs no vertical space while closed.
 */
export function RecentChats({
  language,
  sessions,
  projects,
  activeSessionId,
  onSelect,
  onClose,
}: {
  language: Language;
  sessions: SessionSummary[];
  projects: Project[];
  activeSessionId: string;
  onSelect: (session: SessionSummary) => void;
  onClose: () => void;
}) {
  const box = useRef<HTMLDivElement>(null);
  const recent = useMemo(
    () =>
      [...sessions]
        .filter((session) => !session.archived && !session.deleting)
        .sort((a, b) => {
          const endedDelta =
            (b.lastTurnEndedAt ?? 0) - (a.lastTurnEndedAt ?? 0);
          return endedDelta || b.updatedAt - a.updatedAt;
        }),
    [sessions],
  );

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    const onPointerDown = (event: PointerEvent) => {
      const target = event.target as Element;
      if (box.current?.contains(target)) return;
      if (target.closest(".slim-recent-trigger")) return;
      onClose();
    };
    document.addEventListener("keydown", onKeyDown);
    document.addEventListener("pointerdown", onPointerDown);
    return () => {
      document.removeEventListener("keydown", onKeyDown);
      document.removeEventListener("pointerdown", onPointerDown);
    };
  }, [onClose]);

  return (
    <div
      ref={box}
      id="sidebar-recent-chats"
      className="slim-recent"
      role="dialog"
      aria-label={translate(language, "recentChats")}
    >
      <div className="slim-recent-heading">
        <span>
          <Clock3 size={13} aria-hidden="true" />
          {translate(language, "recentChats")}
        </span>
        <button
          type="button"
          className="icon-button"
          aria-label={translate(language, "closePanel")}
          onClick={onClose}
        >
          <X size={14} />
        </button>
      </div>
      <div className="slim-recent-list">
        {recent.map((session) => {
          const project = projects.find(
            (entry) => entry.id === session.projectId,
          );
          const title = session.title || translate(language, "newSession");
          const selected = session.sessionId === activeSessionId;
          const awaiting = session.status === "awaiting_input";
          const running = ["running", "starting"].includes(session.status);
          return (
            <button
              key={session.sessionId}
              type="button"
              className={`slim-recent-option${selected ? " selected" : ""}`}
              aria-current={selected ? "true" : undefined}
              onClick={() => {
                onSelect(session);
                onClose();
              }}
            >
              {awaiting ? (
                <span className="session-answer-dot" aria-hidden="true" />
              ) : running ? (
                <span className="session-running-dot" aria-hidden="true" />
              ) : (
                <span className="session-indent-dot" aria-hidden="true" />
              )}
              <span className="slim-recent-title">{title}</span>
              {project && (
                <span className="slim-recent-project">{project.name}</span>
              )}
              {selected && <Check size={13} aria-hidden="true" />}
            </button>
          );
        })}
      </div>
    </div>
  );
}
