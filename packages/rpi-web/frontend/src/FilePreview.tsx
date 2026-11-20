import { useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import hljs from "highlight.js/lib/common";
import {
  Code2,
  Copy,
  Eye,
  FileCode2,
  Link2,
  Maximize2,
  Minimize2,
  X,
} from "lucide-react";
import { api, ApiError, type Project } from "./api";
import { resolveProjectPath } from "./fileLookup";
import { translate, type Language } from "./i18n";
import type { ProjectFileLink } from "./messageLinks";
import { MarkdownBody } from "./MarkdownBody";
import { isProjectImagePath, ProjectImageContext } from "./projectImages";
import { absoluteProjectPath } from "./projectPaths";
import { ZoomableImage } from "./ZoomableImage";
import "./file-preview.css";

/** Rendered Markdown is the default reading view; other files stay source-only. */
function isMarkdownPath(path: string) {
  return /\.(?:md|markdown|mdx)$/i.test(path);
}

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
  const t = (key: Parameters<typeof translate>[1]) => translate(language, key);
  // The server root determines the platform-specific separator, not the
  // browser. Show this exact value so "Copy path" is what the user can see.
  // A linked-worktree link is joined onto that worktree, not the project root.
  const [resolved, setResolved] = useState<string | null>(null);
  const shownPath = resolved ?? file.path;
  const absolutePath = absoluteProjectPath(
    file.root ?? project.path,
    shownPath,
  );
  const dialogRef = useRef<HTMLDialogElement>(null);
  const targetRef = useRef<HTMLSpanElement>(null);
  const [attempt, setAttempt] = useState(0);
  const [content, setContent] = useState<string | null>(null);
  const [imageUrl, setImageUrl] = useState<string | null>(null);
  const image = isProjectImagePath(file.path);
  const markdown = isMarkdownPath(file.path);
  const [error, setError] = useState("");
  const [copied, setCopied] = useState("");
  // A line deep link is only visible in the numbered source, so it wins over
  // the rendered reading view the first time the preview opens.
  const [source, setSource] = useState(!markdown || !!file.line);
  const [fullscreen, setFullscreen] = useState(false);
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
    let objectUrl: string | undefined;
    setContent(null);
    setImageUrl(null);
    setError("");
    setCopied("");
    setResolved(null);
    setSource(!markdown || !!file.line);
    const timer = window.setTimeout(() => {
      void (async () => {
        try {
          if (image) {
            const blob = await api.image(project.id, file.path, {
              root: file.root,
              signal: controller.signal,
            });
            if (controller.signal.aborted) return;
            objectUrl = URL.createObjectURL(blob);
            setImageUrl(objectUrl);
          } else {
            const result = await api.file(project.id, file.path, {
              root: file.root,
              signal: controller.signal,
            });
            if (!controller.signal.aborted) setContent(result.content);
          }
        } catch (cause: unknown) {
          if (controller.signal.aborted) return;
          // A relative path may have been written from a nested package, so the
          // literal join does not exist. Resolve one unique suffix and retry.
          if (
            !image &&
            !file.root &&
            cause instanceof ApiError &&
            cause.status === 404
          ) {
            const found = await resolveProjectPath(project.id, file.path, {
              signal: controller.signal,
            });
            if (controller.signal.aborted) return;
            if (found) {
              const retried = await api.file(project.id, found, {
                signal: controller.signal,
              });
              if (controller.signal.aborted) return;
              setResolved(found);
              setContent(retried.content);
              return;
            }
          }
          setError(cause instanceof Error ? cause.message : String(cause));
        }
      })();
    }, 0);
    return () => {
      window.clearTimeout(timer);
      controller.abort();
      if (objectUrl) URL.revokeObjectURL(objectUrl);
    };
  }, [
    project.id,
    project.path,
    file.path,
    file.root,
    image,
    markdown,
    file.line,
    attempt,
  ]);
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
      data-fullscreen={fullscreen || undefined}
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
        <div className="file-preview-tools">
          <span className="file-preview-note" role="status">
            {copied && <>{copied}</>}
          </span>
          <button
            type="button"
            className="file-preview-icon"
            disabled={!absolutePath}
            aria-label={t("copyAbsolutePath")}
            title={t("copyAbsolutePath")}
            onClick={() => void copy(absolutePath, t("absolutePathCopied"))}
          >
            <Copy size={15} aria-hidden="true" />
          </button>
          <button
            type="button"
            className="file-preview-icon"
            aria-label={t("copyRelativePath")}
            title={t("copyRelativePath")}
            onClick={() => void copy(shownPath, t("relativePathCopied"))}
          >
            <Link2 size={15} aria-hidden="true" />
          </button>
          {!image && (
            <button
              type="button"
              className="file-preview-icon"
              disabled={content === null}
              aria-label={zh ? "复制代码" : "Copy code"}
              title={zh ? "复制代码" : "Copy code"}
              onClick={() =>
                void copy(content!, zh ? "已复制代码" : "Code copied")
              }
            >
              <FileCode2 size={15} aria-hidden="true" />
            </button>
          )}
          {markdown && (
            <button
              type="button"
              className="file-preview-icon"
              aria-pressed={!source}
              aria-label={
                source
                  ? zh
                    ? "预览排版"
                    : "Rendered view"
                  : zh
                    ? "查看源码"
                    : "View source"
              }
              title={
                source
                  ? zh
                    ? "预览排版"
                    : "Rendered view"
                  : zh
                    ? "查看源码"
                    : "View source"
              }
              onClick={() => setSource((value) => !value)}
            >
              {source ? (
                <Eye size={15} aria-hidden="true" />
              ) : (
                <Code2 size={15} aria-hidden="true" />
              )}
            </button>
          )}
          <button
            type="button"
            className="file-preview-icon"
            aria-pressed={fullscreen}
            aria-label={
              fullscreen
                ? zh
                  ? "退出全屏"
                  : "Exit fullscreen"
                : zh
                  ? "全屏查看"
                  : "Fullscreen"
            }
            title={
              fullscreen
                ? zh
                  ? "退出全屏"
                  : "Exit fullscreen"
                : zh
                  ? "全屏查看"
                  : "Fullscreen"
            }
            onClick={() => setFullscreen((value) => !value)}
          >
            {fullscreen ? (
              <Minimize2 size={15} aria-hidden="true" />
            ) : (
              <Maximize2 size={15} aria-hidden="true" />
            )}
          </button>
          <button
            type="button"
            className="file-preview-icon file-preview-close"
            aria-label={zh ? "关闭" : "Close"}
            title={zh ? "关闭" : "Close"}
            onClick={onClose}
          >
            <X size={16} aria-hidden="true" />
          </button>
        </div>
      </header>
      <div className="file-preview-path">
        {absolutePath || `${project.name}: ${file.path}`}
        {file.line
          ? `:${file.line}${file.endLine !== file.line ? `-${file.endLine}` : ""}`
          : ""}
        {resolved && (
          <span className="file-preview-resolved">
            {zh ? "已定位到项目内路径" : "resolved inside the project"}
          </span>
        )}
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
      ) : image && imageUrl ? (
        <div className="file-preview-image">
          <ZoomableImage
            key={imageUrl}
            src={imageUrl}
            alt={file.path}
            language={language}
            onError={() =>
              setError(
                zh
                  ? "无法解码图片，文件可能已损坏"
                  : "Could not decode image; the file may be corrupt",
              )
            }
          />
        </div>
      ) : content === null ? (
        <p className="file-preview-status" role="status">
          {zh ? "正在加载…" : "Loading…"}
        </p>
      ) : markdown && !source ? (
        <div className="file-preview-markdown" tabIndex={0}>
          <ProjectImageContext.Provider value={project}>
            <MarkdownBody text={content} language={language} />
          </ProjectImageContext.Provider>
        </div>
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
