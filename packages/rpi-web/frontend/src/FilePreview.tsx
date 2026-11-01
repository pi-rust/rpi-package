import { useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import hljs from "highlight.js/lib/common";
import { api, type Project } from "./api";
import type { Language } from "./i18n";
import type { ProjectFileLink } from "./messageLinks";
import "./file-preview.css";

/** Mounted only by an explicit click. The deferred effect avoids StrictMode's probe I/O. */
export function FilePreview({
  project,
  file,
  language,
  onClose,
}: {
  project: Project;
  file: ProjectFileLink;
  language: Language;
  onClose: () => void;
}) {
  const zh = language === "zh";
  const dialogRef = useRef<HTMLDialogElement>(null);
  const targetRef = useRef<HTMLSpanElement>(null);
  const [attempt, setAttempt] = useState(0);
  const [content, setContent] = useState<string | null>(null);
  const [error, setError] = useState("");
  const [copied, setCopied] = useState("");
  useEffect(() => {
    const previousFocus = document.activeElement;
    const dialog = dialogRef.current!;
    dialog.showModal();
    return () => {
      dialog.close();
      if (previousFocus instanceof HTMLElement && previousFocus.isConnected)
        previousFocus.focus({ preventScroll: true });
    };
  }, []);
  useEffect(() => {
    const controller = new AbortController();
    setContent(null);
    setError("");
    const timer = window.setTimeout(() => {
      void api
        .file(project.id, file.path, controller.signal)
        .then((result) => {
          if (!controller.signal.aborted) setContent(result.content);
        })
        .catch((cause: unknown) => {
          if (!controller.signal.aborted)
            setError(cause instanceof Error ? cause.message : String(cause));
        });
    }, 0);
    return () => {
      window.clearTimeout(timer);
      controller.abort();
    };
  }, [project.id, project.path, file.path, attempt]);
  const lines = useMemo(() => {
    if (content === null) return [];
    const extension = file.path.split(".").pop()?.toLowerCase() ?? "";
    const aliases: Record<string, string> = {
      tsx: "typescript",
      jsx: "javascript",
      rs: "rust",
      sh: "bash",
      md: "markdown",
      h: "cpp",
    };
    const syntax = aliases[extension] ?? extension;
    // Highlight each line separately so token spans cannot leak across numbered rows.
    return content
      .split("\n")
      .map((line) =>
        hljs.getLanguage(syntax)
          ? hljs.highlight(line, { language: syntax, ignoreIllegals: true })
              .value
          : line
              .replace(/&/g, "&amp;")
              .replace(/</g, "&lt;")
              .replace(/>/g, "&gt;"),
      );
  }, [content, file.path]);
  useEffect(() => {
    if (content !== null)
      targetRef.current?.scrollIntoView({ block: "center" });
  }, [content]);
  async function copy(value: string, label: string) {
    try {
      await navigator.clipboard.writeText(value);
      setCopied(label);
    } catch {
      setCopied(
        zh ? "复制失败，请手动选择" : "Copy unavailable; select text manually",
      );
    }
  }
  return createPortal(
    <dialog
      ref={dialogRef}
      className="file-preview"
      aria-label={zh ? "文件预览" : "File preview"}
      onCancel={(event) => {
        event.preventDefault();
        onClose();
      }}
      onClick={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
      onKeyDown={(event) => {
        if (event.key === "Escape") event.stopPropagation();
      }}
    >
      <header className="file-preview-header">
        <h2>{zh ? "文件预览（只读）" : "File preview (read-only)"}</h2>
        <button type="button" onClick={onClose}>
          {zh ? "关闭" : "Close"}
        </button>
      </header>
      <div className="file-preview-path">
        {project.name}: {file.path}
        {file.line
          ? `:${file.line}${file.endLine !== file.line ? `-${file.endLine}` : ""}`
          : ""}
      </div>
      <div className="file-preview-actions">
        <button
          type="button"
          onClick={() =>
            void copy(file.path, zh ? "已复制路径" : "Path copied")
          }
        >
          {zh ? "复制路径" : "Copy path"}
        </button>
        <button
          type="button"
          disabled={content === null}
          onClick={() => void copy(content!, zh ? "已复制代码" : "Code copied")}
        >
          {zh ? "复制代码" : "Copy code"}
        </button>
        <span role="status">{copied}</span>
      </div>
      {error ? (
        <div className="file-preview-status" role="alert">
          <p>
            {zh ? "文件读取失败" : "Could not load file"}: {error}
          </p>
          <button
            type="button"
            onClick={() => setAttempt((value) => value + 1)}
          >
            {zh ? "重试" : "Retry"}
          </button>
        </div>
      ) : content === null ? (
        <p className="file-preview-status" role="status">
          {zh ? "正在加载…" : "Loading…"}
        </p>
      ) : (
        <pre
          className="file-preview-code"
          tabIndex={0}
          aria-label={zh ? "文件代码" : "File code"}
        >
          <code>
            {lines.map((html, index) => {
              const number = index + 1;
              const selected =
                !!file.line &&
                number >= file.line &&
                number <= (file.endLine ?? file.line);
              return (
                <span
                  key={number}
                  ref={number === file.line ? targetRef : undefined}
                  className="file-preview-line"
                  data-line={number}
                  data-highlighted={selected || undefined}
                >
                  <span className="file-preview-number" aria-hidden="true">
                    {number}
                  </span>
                  <span dangerouslySetInnerHTML={{ __html: html || " " }} />
                  {"\n"}
                </span>
              );
            })}
          </code>
        </pre>
      )}
    </dialog>,
    document.body,
  );
}
