import { useEffect, useMemo, useRef, useState } from "react";
import { Check, Search, X } from "lucide-react";
import type { Project, SessionSummary } from "./api";
import { translate, type Language } from "./i18n";

/**
 * Chat search: finds *sessions* across projects, not messages inside the open
 * conversation.
 *
 * Settings has always advertised "Search chats — Ctrl / ⌘ K", but the only
 * search shipped was message search scoped to one conversation and the shortcut
 * was never bound. This is the missing cross-session entry point: one query
 * over session titles, project names and tags. Message search remains a
 * separate feature (Ctrl / ⌘ F).
 */
export function SessionSearch({
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
  const [query, setQuery] = useState("");
  const [index, setIndex] = useState(0);
  const input = useRef<HTMLInputElement>(null);
  const list = useRef<HTMLDivElement>(null);
  const box = useRef<HTMLDivElement>(null);

  const results = useMemo(() => {
    const q = query.trim().toLowerCase();
    const rows = sessions
      .filter((session) => !session.deleting)
      .map((session) => ({
        session,
        project:
          projects.find((project) => project.id === session.projectId)?.name ??
          "",
      }));
    if (!q) {
      return [...rows]
        .sort((a, b) => b.session.updatedAt - a.session.updatedAt)
        .slice(0, 8);
    }
    return rows
      .filter(({ session, project }) =>
        [session.title ?? "", project, ...(session.tags ?? [])]
          .join(" ")
          .toLowerCase()
          .includes(q),
      )
      .sort((a, b) => b.session.updatedAt - a.session.updatedAt);
  }, [projects, query, sessions]);

  useEffect(() => {
    input.current?.focus();
    input.current?.select();
  }, []);

  useEffect(() => {
    setIndex(0);
  }, [query]);

  useEffect(() => {
    list.current
      ?.querySelector<HTMLElement>(`[data-index="${index}"]`)
      ?.scrollIntoView({ block: "nearest" });
  }, [index]);

  // Escape / outside click close the window; the trigger button stops its own
  // pointer event so clicking it again is a normal toggle rather than
  // close-then-reopen.
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    const onPointerDown = (event: PointerEvent) => {
      const target = event.target as Element;
      if (box.current?.contains(target)) return;
      if (target.closest(".slim-chat-search-trigger")) return;
      onClose();
    };
    document.addEventListener("keydown", onKeyDown);
    document.addEventListener("pointerdown", onPointerDown);
    return () => {
      document.removeEventListener("keydown", onKeyDown);
      document.removeEventListener("pointerdown", onPointerDown);
    };
  }, [onClose]);

  function commit(hit: SessionSummary | undefined) {
    if (!hit) return;
    onSelect(hit);
    onClose();
  }

  return (
    <div
      ref={box}
      id="sidebar-chat-search"
      className="slim-chat-search"
      role="dialog"
      aria-modal="false"
      aria-label={translate(language, "searchChats")}
    >
      <div className="slim-chat-search-field">
        <Search size={14} aria-hidden="true" />
        <input
          ref={input}
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          placeholder={translate(language, "chatSearchPlaceholder")}
          aria-label={translate(language, "searchChats")}
          onKeyDown={(event) => {
            if (event.key === "Escape") {
              event.preventDefault();
              onClose();
            }
            if (event.key === "ArrowDown") {
              event.preventDefault();
              setIndex((value) => Math.min(results.length - 1, value + 1));
            }
            if (event.key === "ArrowUp") {
              event.preventDefault();
              setIndex((value) => Math.max(0, value - 1));
            }
            if (event.key === "Enter") {
              event.preventDefault();
              commit(results[index]?.session);
            }
          }}
        />
        <button
          type="button"
          className="icon-button"
          aria-label={translate(language, "closePanel")}
          onClick={onClose}
        >
          <X size={14} />
        </button>
      </div>

      <div className="slim-chat-search-list" ref={list} role="listbox">
        {results.length === 0 && (
          <p className="slim-chat-search-empty">
            {translate(language, "chatSearchEmpty")}
          </p>
        )}
        {results.map(({ session, project }, position) => {
          const title = session.title || translate(language, "newSession");
          const running = ["running", "starting"].includes(session.status);
          return (
            <button
              key={session.sessionId}
              type="button"
              role="option"
              data-index={position}
              aria-selected={position === index}
              className={`slim-chat-search-option${position === index ? " selected" : ""}`}
              onMouseMove={() => setIndex(position)}
              onClick={() => commit(session)}
            >
              <span className="slim-chat-search-copy">
                <strong>{title}</strong>
                <small>
                  {project}
                  {session.archived && ` · ${translate(language, "archived")}`}
                  {running && ` · ${translate(language, "running")}`}
                </small>
              </span>
              {(session.tags ?? []).slice(0, 2).map((tag) => (
                <span className="slim-chat-search-tag" key={tag}>
                  {tag}
                </span>
              ))}
              {session.sessionId === activeSessionId && (
                <Check size={13} aria-hidden="true" />
              )}
            </button>
          );
        })}
      </div>

      <p className="slim-chat-search-foot">
        {query.trim()
          ? translate(language, "chatSearchMatches", {
              count: results.length,
            })
          : translate(language, "chatSearchRecent")}
        <span aria-hidden="true"> · </span>
        <kbd>↑</kbd>
        <kbd>↓</kbd>
        <span aria-hidden="true"> </span>
        <kbd>Enter</kbd>
      </p>
    </div>
  );
}
