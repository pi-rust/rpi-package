import { useEffect, useRef, useState, type PointerEvent } from "react";
import {
  ChevronRight,
  ChevronUp,
  FileCode2,
  Files,
  Folder,
  GitBranch,
  GitCompareArrows,
  Loader2,
  RefreshCw,
  X,
} from "lucide-react";
import {
  api,
  type ChangedFile,
  type FileEntry,
  type GitStatus,
  type Project,
} from "./api";
import type { Language } from "./i18n";

export function useStoredNumber(
  key: string,
  initial: number,
  min: number,
  max: number,
) {
  const [value, setValue] = useState(() => {
    try {
      const saved = localStorage.getItem(key);
      const parsed = saved === null ? initial : Number(saved);
      return Number.isFinite(parsed)
        ? Math.max(min, Math.min(max, parsed))
        : initial;
    } catch {
      return initial;
    }
  });
  useEffect(() => {
    try {
      localStorage.setItem(key, String(value));
    } catch {
      /* optional preference */
    }
  }, [key, value]);
  return [value, setValue] as const;
}

export function Splitter({
  label,
  value,
  min,
  max,
  reverse = false,
  onChange,
}: {
  label: string;
  value: number;
  min: number;
  max: number;
  reverse?: boolean;
  onChange: (value: number) => void;
}) {
  const drag = useRef<{ x: number; value: number } | null>(null);
  function start(event: PointerEvent<HTMLDivElement>) {
    if (event.button !== 0) return;
    drag.current = { x: event.clientX, value };
    event.currentTarget.setPointerCapture(event.pointerId);
  }
  return (
    <div
      className="workspace-splitter"
      role="separator"
      aria-label={label}
      aria-orientation="vertical"
      aria-valuenow={Math.round(value)}
      aria-valuemin={min}
      aria-valuemax={max}
      tabIndex={0}
      onPointerDown={start}
      onPointerMove={(event) => {
        if (drag.current)
          onChange(
            Math.max(
              min,
              Math.min(
                max,
                drag.current.value +
                  (event.clientX - drag.current.x) * (reverse ? -1 : 1),
              ),
            ),
          );
      }}
      onPointerUp={() => {
        drag.current = null;
      }}
      onPointerCancel={() => {
        drag.current = null;
      }}
      onLostPointerCapture={() => {
        drag.current = null;
      }}
      onKeyDown={(event) => {
        if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
        event.preventDefault();
        onChange(
          Math.max(
            min,
            Math.min(
              max,
              value +
                (event.key === "ArrowRight" ? 20 : -20) * (reverse ? -1 : 1),
            ),
          ),
        );
      }}
    />
  );
}

type View = { path: string; mode: "file" | "diff"; staged: boolean };
function viewKey(view: View) {
  return `${view.mode}:${view.staged}:${view.path}`;
}

export function Workbench({
  project,
  language,
  tick,
  onClose,
}: {
  project: Project;
  language: Language;
  tick: number;
  onClose: () => void;
}) {
  const zh = language === "zh";
  const [tab, setTab] = useState<"files" | "changes">("files");
  const [directory, setDirectory] = useState("");
  const [entries, setEntries] = useState<FileEntry[]>([]);
  const [git, setGit] = useState<GitStatus | null>(null);
  const [gitError, setGitError] = useState("");
  const [listError, setListError] = useState("");
  const [contentError, setContentError] = useState("");
  const [loading, setLoading] = useState(false);
  const [listLoading, setListLoading] = useState(false);
  const [content, setContent] = useState("");
  const [views, setViews] = useState<View[]>([]);
  const [active, setActive] = useState<View | null>(null);
  const [refresh, setRefresh] = useState(0);

  useEffect(() => {
    let cancelled = false;
    setListLoading(true);
    setListError("");
    void api
      .files(project.id, directory)
      .then((data) => {
        if (!cancelled) setEntries(data.entries);
      })
      .catch((error: Error) => {
        if (!cancelled) {
          setEntries([]);
          setListError(error.message);
        }
      })
      .finally(() => {
        if (!cancelled) setListLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [project.id, directory, refresh, tick]);
  useEffect(() => {
    let cancelled = false;
    setGitError("");
    void api
      .gitStatus(project.id)
      .then((data) => {
        if (!cancelled) setGit(data);
      })
      .catch((error: Error) => {
        if (!cancelled) {
          setGit(null);
          setGitError(error.message);
        }
      });
    return () => {
      cancelled = true;
    };
  }, [project.id, refresh, tick]);
  useEffect(() => {
    if (!active) return;
    let cancelled = false;
    setLoading(true);
    setContent("");
    setContentError("");
    const job =
      active.mode === "file"
        ? api.file(project.id, active.path).then((data) => data.content)
        : api
            .diff(project.id, active.path, active.staged)
            .then((data) => data.patch);
    void job
      .then((text) => {
        if (!cancelled) setContent(text);
      })
      .catch((error: Error) => {
        if (!cancelled) setContentError(error.message);
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [project.id, active, refresh, tick]);

  function open(view: View) {
    setViews((current) =>
      current.some((item) => viewKey(item) === viewKey(view))
        ? current
        : [...current.slice(-7), view],
    );
    setActive(view);
  }
  function close(view: View) {
    const remaining = views.filter((item) => viewKey(item) !== viewKey(view));
    setViews(remaining);
    if (active && viewKey(active) === viewKey(view))
      setActive(remaining.at(-1) ?? null);
  }
  function changeGroup(files: ChangedFile[], staged: boolean) {
    return (
      files.length > 0 && (
        <section className="change-group">
          <h3>
            {staged
              ? zh
                ? "已暂存"
                : "Staged"
              : zh
                ? "工作区变更"
                : "Working changes"}{" "}
            <span>{files.length}</span>
          </h3>
          {files.map((file) => (
            <button
              key={file.path}
              className={`workspace-file-row ${active?.mode === "diff" && active.path === file.path && active.staged === staged ? "selected" : ""}`}
              onClick={() => open({ path: file.path, mode: "diff", staged })}
              title={
                file.originalPath
                  ? `${file.originalPath} → ${file.path}`
                  : file.path
              }
            >
              <FileCode2 size={14} />
              <span>{file.path}</span>
              <code className="git-status-label">{file.status.trim()}</code>
            </button>
          ))}
        </section>
      )
    );
  }
  const lines = content.split("\n");
  if (lines.at(-1) === "") lines.pop();

  return (
    <aside
      className="workbench"
      aria-label={zh ? "项目文件与变更" : "Project files and changes"}
    >
      <div className="workbench-toolbar">
        <div
          className="workbench-tabs"
          role="tablist"
          aria-label={zh ? "项目视图" : "Project views"}
        >
          <button
            role="tab"
            aria-selected={tab === "files"}
            onClick={() => setTab("files")}
          >
            <Files size={14} />
            {zh ? "文件" : "Files"}
          </button>
          <button
            role="tab"
            aria-selected={tab === "changes"}
            onClick={() => setTab("changes")}
          >
            <GitCompareArrows size={14} />
            {zh ? "变更" : "Changes"}
            {git && git.files.length > 0 && (
              <span className="count">{git.files.length}</span>
            )}
          </button>
        </div>
        <button
          className="icon-button"
          aria-label={zh ? "刷新项目" : "Refresh project"}
          onClick={() => setRefresh((value) => value + 1)}
        >
          <RefreshCw size={14} />
        </button>
        <button
          className="icon-button"
          aria-label={zh ? "关闭项目面板" : "Close project panel"}
          onClick={onClose}
        >
          <X size={16} />
        </button>
      </div>
      <div className="workbench-project">
        <Folder size={14} />
        <strong title={project.path}>{project.name}</strong>
        {git?.branch && (
          <span>
            <GitBranch size={12} />
            {git.branch}
          </span>
        )}
      </div>
      <div
        className="workbench-browser"
        role="tabpanel"
        aria-label={
          tab === "files"
            ? zh
              ? "文件列表"
              : "File list"
            : zh
              ? "变更列表"
              : "Change list"
        }
      >
        {tab === "files" ? (
          <>
            <div className="directory-breadcrumb">
              <button onClick={() => setDirectory("")} title={project.path}>
                {project.name}
              </button>
              {directory
                .split("/")
                .filter(Boolean)
                .map((name, index, parts) => (
                  <span key={index}>
                    <ChevronRight size={11} />
                    <button
                      onClick={() =>
                        setDirectory(parts.slice(0, index + 1).join("/"))
                      }
                    >
                      {name}
                    </button>
                  </span>
                ))}
            </div>
            {directory && (
              <button
                className="workspace-file-row parent-directory"
                onClick={() =>
                  setDirectory(directory.split("/").slice(0, -1).join("/"))
                }
              >
                <ChevronUp size={14} />
                <span>{zh ? "上一级" : "Parent directory"}</span>
              </button>
            )}
            {listLoading ? (
              <p className="workbench-empty">
                <Loader2 className="spin" size={15} />
                {zh ? "加载中…" : "Loading…"}
              </p>
            ) : listError ? (
              <p className="workbench-error" role="alert">
                {listError}
              </p>
            ) : entries.length === 0 ? (
              <p className="workbench-empty">
                {zh ? "此目录为空" : "This directory is empty"}
              </p>
            ) : (
              entries.map((entry) => (
                <button
                  key={entry.path}
                  className={`workspace-file-row ${active?.mode === "file" && active.path === entry.path ? "selected" : ""}`}
                  onClick={() =>
                    entry.kind === "directory"
                      ? setDirectory(entry.path)
                      : open({ path: entry.path, mode: "file", staged: false })
                  }
                  title={entry.path}
                >
                  {entry.kind === "directory" ? (
                    <Folder size={14} />
                  ) : (
                    <FileCode2 size={14} />
                  )}
                  <span>{entry.name}</span>
                  {entry.kind === "directory" && <ChevronRight size={12} />}
                </button>
              ))
            )}
          </>
        ) : gitError ? (
          <p className="workbench-error" role="alert">
            {gitError}
          </p>
        ) : !git ? (
          <p className="workbench-empty">
            {zh ? "读取 Git 状态…" : "Reading Git status…"}
          </p>
        ) : !git.isRepository ? (
          <p className="workbench-empty">
            {zh
              ? "此项目不是 Git 仓库"
              : "This project is not a Git repository"}
          </p>
        ) : git.files.length === 0 ? (
          <p className="workbench-empty">
            {zh ? "工作区没有未提交的变更" : "No uncommitted changes"}
          </p>
        ) : (
          <>
            {changeGroup(
              git.files.filter((file) => file.staged),
              true,
            )}
            {changeGroup(
              git.files.filter((file) => file.unstaged),
              false,
            )}
          </>
        )}
      </div>
      <div
        className="source-tabs"
        aria-label={zh ? "已打开的文件" : "Open files"}
      >
        {views.map((view) => (
          <div
            key={viewKey(view)}
            className={`source-tab ${active && viewKey(active) === viewKey(view) ? "selected" : ""}`}
          >
            <button onClick={() => setActive(view)} title={view.path}>
              {view.mode === "diff" ? (
                <GitCompareArrows size={12} />
              ) : (
                <FileCode2 size={12} />
              )}
              <span>{view.path.split("/").at(-1)}</span>
              {view.mode === "diff" && (
                <small>
                  {view.staged ? (zh ? "已暂存" : "staged") : "diff"}
                </small>
              )}
            </button>
            <button
              aria-label={`${zh ? "关闭" : "Close"} ${view.path}`}
              onClick={() => close(view)}
            >
              <X size={11} />
            </button>
          </div>
        ))}
      </div>
      {active ? (
        <>
          <div className="source-heading">
            <span title={active.path}>{active.path}</span>
            <span>
              {active.mode === "diff"
                ? zh
                  ? "差异 · 只读"
                  : "Diff · Read only"
                : zh
                  ? "只读"
                  : "Read only"}
            </span>
          </div>
          <div
            className="source-preview"
            tabIndex={0}
            aria-label={zh ? "文件内容" : "File contents"}
          >
            {loading ? (
              <p className="workbench-empty">
                <Loader2 className="spin" size={16} />
                {zh ? "加载中…" : "Loading…"}
              </p>
            ) : contentError ? (
              <p className="workbench-error" role="alert">
                {contentError}
              </p>
            ) : !content ? (
              <p className="workbench-empty">
                {active.mode === "diff"
                  ? zh
                    ? "当前没有差异，文件可能已被更新"
                    : "No diff remains; the file may have changed"
                  : zh
                    ? "空文件"
                    : "Empty file"}
              </p>
            ) : (
              <pre
                className={
                  active.mode === "diff"
                    ? "source-code diff-code"
                    : "source-code"
                }
              >
                <code>
                  {lines.map((line, index) => (
                    <span
                      key={index}
                      className={`source-line ${active.mode === "diff" ? (line.startsWith("@@") ? "hunk" : line.startsWith("+") ? "added" : line.startsWith("-") ? "removed" : "") : ""}`}
                    >
                      <span className="line-number" aria-hidden="true">
                        {index + 1}
                      </span>
                      <span>{line || " "}</span>
                    </span>
                  ))}
                </code>
              </pre>
            )}
          </div>
        </>
      ) : (
        <div className="source-welcome">
          <FileCode2 size={32} strokeWidth={1.2} />
          <strong>{zh ? "在这里查看项目" : "Explore your project"}</strong>
          <p>
            {zh
              ? "选择文件预览内容，或查看 Agent 修改的代码差异。"
              : "Select a file to preview it, or review the agent’s code changes."}
          </p>
        </div>
      )}
    </aside>
  );
}
