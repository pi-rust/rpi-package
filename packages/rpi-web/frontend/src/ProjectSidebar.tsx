import {
  useEffect,
  useRef,
  useState,
  type FormEvent,
  type ReactNode,
} from "react";
import {
  Archive,
  ArchiveRestore,
  Check,
  ChevronDown,
  ChevronRight,
  Folder,
  FolderOpen,
  FolderPlus,
  Loader2,
  MoreHorizontal,
  Pin,
  PinOff,
  Plus,
  SquarePen,
  Tag,
  X,
} from "lucide-react";
import { api, type Project, type SessionSummary } from "./api";
import { translate, type Language } from "./i18n";
import { SessionIssueBadge } from "./SessionIssueBadge";
import { createPortal } from "react-dom";
type FolderNode = {
  name: string;
  path: string;
  children?: FolderNode[];
};

function useMountedFlag() {
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);
  return mounted;
}

export function Modal({
  title,
  onClose,
  children,
  language = "en",
}: {
  title: string;
  onClose: () => void;
  children: ReactNode;
  language?: Language;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;
  useEffect(() => {
    const input = ref.current?.querySelector<HTMLInputElement>(
      'input:not([type="checkbox"])',
    );
    input?.focus();
    input?.select();
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") onCloseRef.current();
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, []);
  return createPortal(
    <div
      ref={ref}
      className="workspace-dialog"
      role="dialog"
      aria-modal="true"
      aria-label={title}
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div className="workspace-dialog-surface">
        <div className="dialog-heading">
          <h2>{title}</h2>
          <button
            className="icon-button"
            type="button"
            aria-label={translate(language, "closeDialog")}
            title={translate(language, "closeDialog")}
            onClick={onClose}
          >
            <X size={17} />
          </button>
        </div>
        {children}
      </div>
    </div>,
    document.body,
  );
}

export function PromptDialog({
  language,
  title,
  message,
  defaultValue = "",
  onClose,
  onSubmit,
}: {
  language: Language;
  title: string;
  message?: string;
  defaultValue?: string;
  onClose: () => void;
  onSubmit: (value: string) => void;
}) {
  const [value, setValue] = useState(defaultValue);
  const t = (key: "cancel" | "submit") => translate(language, key);
  return (
    <Modal language={language} title={title} onClose={onClose}>
      <form
        className="project-form"
        onSubmit={(event) => {
          event.preventDefault();
          onSubmit(value);
        }}
      >
        {message && <p className="dialog-description">{message}</p>}
        <input
          autoFocus
          value={value}
          onChange={(event) => setValue(event.target.value)}
        />
        <div className="dialog-actions">
          <button type="button" onClick={onClose}>{t("cancel")}</button>
          <button type="submit" className="primary">{t("submit")}</button>
        </div>
      </form>
    </Modal>
  );
}

export function ConfirmDialog({
  language,
  title,
  message,
  onClose,
  onConfirm,
  confirmLabel,
}: {
  language: Language;
  title: string;
  message: string;
  onClose: () => void;
  onConfirm: () => void;
  confirmLabel?: string;
}) {
  return (
    <Modal language={language} title={title} onClose={onClose}>
      <div className="project-form">
        <p className="dialog-description">{message}</p>
        <div className="dialog-actions">
          <button type="button" onClick={onClose}>
            {translate(language, "cancel")}
          </button>
          <button type="button" className="danger" onClick={onConfirm}>
            {confirmLabel ?? translate(language, "submit")}
          </button>
        </div>
      </div>
    </Modal>
  );
}
export function ProjectDialog({
  language,
  root,
  onClose,
  onCreated,
}: {
  language: Language;
  root: string;
  onClose: () => void;
  onCreated: (project: Project) => Promise<void>;
}) {
  const zh = language === "zh";
  const [mode, setMode] = useState<"create" | "open">("create");
  const [name, setName] = useState("");
  const [nameEdited, setNameEdited] = useState(false);
  const [path, setPath] = useState("");
  const [folderQuery, setFolderQuery] = useState("");
  const [git, setGit] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [created, setCreated] = useState<Project | null>(null);
  const [folderNodes, setFolderNodes] = useState<FolderNode[]>([]);
  const [expandedFolders, setExpandedFolders] = useState<Set<string>>(
    () => new Set(),
  );
  const [loadingFolders, setLoadingFolders] = useState<Set<string>>(
    () => new Set(),
  );
  const mounted = useMountedFlag();
  function updateFolderChildren(
    nodes: FolderNode[],
    parentPath: string,
    children: FolderNode[],
  ): FolderNode[] {
    return nodes.map((node) =>
      node.path === parentPath
        ? { ...node, children }
        : node.children
          ? { ...node, children: updateFolderChildren(node.children, parentPath, children) }
          : node,
    );
  }

  async function loadFolders(relative = "") {
    setLoadingFolders((previous) => new Set(previous).add(relative));
    try {
      const result = await api.projectFolders(relative);
      const children = result.folders.map((folder) => ({
        name: folder.name,
        path: folder.path,
      }));
      setFolderNodes((previous) =>
        relative ? updateFolderChildren(previous, relative, children) : children,
      );
      setExpandedFolders((previous) => new Set(previous).add(relative));
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setLoadingFolders((previous) => {
        const next = new Set(previous);
        next.delete(relative);
        return next;
      });
    }
  }

  useEffect(() => {
    if (mode !== "open") return;
    setFolderNodes([]);
    setExpandedFolders(new Set());
    void loadFolders();
  }, [mode]);
  async function submit(event: FormEvent) {
    event.preventDefault();
    if (busy || !name.trim() || (mode === "open" && !path.trim())) return;
    setBusy(true);
    setError("");
    try {
      const selected =
        created ??
        (
          await api.createProject(
            name.trim(),
            git,
            mode === "open" ? path.trim() : undefined,
          )
        ).project;
      if (!mounted.current) return;
      setCreated(selected);
      await onCreated(selected);
      if (mounted.current) onClose();
    } catch (cause) {
      if (mounted.current)
        setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      if (mounted.current) setBusy(false);
    }
  }
  const allFolders = (nodes: FolderNode[]): FolderNode[] =>
    nodes.flatMap((node) => [node, ...(node.children ? allFolders(node.children) : [])]);
  const folderMatches = folderQuery.trim()
    ? allFolders(folderNodes).filter((folder) =>
        `${folder.name} ${folder.path}`.toLowerCase().includes(folderQuery.trim().toLowerCase()),
      )
    : [];
  function chooseFolder(folder: FolderNode) {
    setPath(folder.path);
    setFolderQuery(folder.path);
    if (!nameEdited) setName(folder.name);
  }
  function updateFolderQuery(value: string) {
    setFolderQuery(value);
    setPath(value);
    if (!nameEdited) {
      const candidate = value.replace(/[\/]+$/, "").split(/[\/]/).pop() ?? "";
      if (candidate) setName(candidate);
    }
  }
  function toggleFolder(path: string) {
    setExpandedFolders((previous) => {
      const next = new Set(previous);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  }
  function renderFolder(folder: FolderNode, depth = 0): ReactNode {
    const expanded = expandedFolders.has(folder.path);
    const loading = loadingFolders.has(folder.path);
    const hasChildren = folder.children === undefined || folder.children.length > 0;
    return (
      <div className="folder-tree-node" role="treeitem" aria-expanded={expanded} key={folder.path}>
        <div className="folder-option-row" style={{ paddingLeft: 4 + depth * 16 }}>
          <button
            type="button"
            className="folder-toggle"
            aria-label={expanded ? (zh ? "折叠目录" : "Collapse folder") : (zh ? "展开目录" : "Expand folder")}
            disabled={!hasChildren || loading || !!created}
            onClick={() => {
              if (!expanded && folder.children === undefined) void loadFolders(folder.path);
              toggleFolder(folder.path);
            }}
          >
            {loading ? <Loader2 size={13} className="spin" /> : hasChildren ? (expanded ? <ChevronDown size={13} /> : <ChevronRight size={13} />) : <span className="folder-toggle-placeholder" />}
          </button>
          <button
            type="button"
            role="option"
            aria-selected={path === folder.path}
            className="folder-option"
            onClick={() => chooseFolder(folder)}
            disabled={busy || !!created}
          >
            <Folder size={14} />
            <span><strong>{folder.name}</strong><small>{folder.path}</small></span>
            {path === folder.path && <Check size={14} />}
          </button>
        </div>
        {expanded && folder.children?.map((child) => renderFolder(child, depth + 1))}
      </div>
    );
  }
  return (
    <Modal
      language={language}
      title={zh ? "添加项目" : "Add project"}
      onClose={() => {
        if (!busy) onClose();
      }}
    >
      <form onSubmit={submit} className="project-form">
        <p className="dialog-description">
          {zh
            ? "每个项目都有自己的文件、代码变更和会话。"
            : "Keep files, changes, and conversations together in a project."}
        </p>
        <div
          className="project-mode"
          role="group"
          aria-label={zh ? "项目来源" : "Project source"}
        >
          <button
            type="button"
            aria-pressed={mode === "create"}
            disabled={busy || !!created}
            onClick={() => setMode("create")}
          >
            <FolderPlus size={16} />
            {zh ? "新建项目" : "Create project"}
          </button>
          <button
            type="button"
            aria-pressed={mode === "open"}
            disabled={busy || !!created}
            onClick={() => setMode("open")}
          >
            <FolderOpen size={16} />
            {zh ? "打开已有目录" : "Open folder"}
          </button>
        </div>
        <label htmlFor="project-name">{zh ? "项目名称" : "Project name"}</label>
        <input
          id="project-name"
          value={name}
          onChange={(event) => {
            setName(event.target.value);
            setNameEdited(true);
          }}
          placeholder={zh ? "例如：我的网站" : "e.g. my-website"}
          maxLength={80}
          autoFocus
          required
          disabled={busy || !!created}
        />
        {mode === "open" ? (
          <>
            <label htmlFor="project-path">
              {zh ? "选择文件夹" : "Choose a folder"}
            </label>
            <input
              id="project-path"
              value={folderQuery}
              onChange={(event) => updateFolderQuery(event.target.value)}
              placeholder={
                zh ? "搜索文件夹，或粘贴路径…" : "Search folders or paste a path…"
              }
              autoComplete="off"
              spellCheck={false}
              required
              disabled={busy || !!created}
            />
            <div
              className="folder-picker"
              role="tree"
              aria-label={zh ? "可用文件夹" : "Available folders"}
            >
              {loadingFolders.has("") ? (
                <p className="folder-picker-empty muted">
                  {zh ? "正在加载文件夹…" : "Loading folders…"}
                </p>
              ) : folderQuery.trim() ? (
                folderMatches.length > 0 ? (
                  folderMatches.map((folder) => (
                    <button
                      key={folder.path}
                      type="button"
                      role="option"
                      aria-selected={path === folder.path}
                      className="folder-option"
                      onClick={() => chooseFolder(folder)}
                      disabled={busy || !!created}
                    >
                      <Folder size={14} />
                      <span><strong>{folder.name}</strong><small>{folder.path}</small></span>
                      {path === folder.path && <Check size={14} />}
                    </button>
                  ))
                ) : (
                  <p className="folder-picker-empty muted">
                    {zh ? "没有匹配的文件夹；也可以直接粘贴路径。" : "No matching folder; you can also paste a path directly."}
                  </p>
                )
              ) : folderNodes.length > 0 ? (
                folderNodes.map((folder) => renderFolder(folder))
              ) : (
                <p className="folder-picker-empty muted">
                  {zh ? "没有可用的文件夹" : "No folders available"}
                </p>
              )}
            </div>
            <p className="form-hint">
              {zh
                ? "可搜索下方目录，也可以直接输入或粘贴 root 内的相对路径："
                : "Search the folders below, or type/paste a path inside:"}{" "}
              <code>{root}</code>
            </p>
          </>
        ) : (
          <>
            <div className="project-location">
              <Folder size={14} />
              <span>
                {root}
                {root.endsWith("/") || root.endsWith("\\") ? "" : "/"}
                {name.trim() || "…"}
              </span>
            </div>
            <label className="checkbox-field">
              <input
                type="checkbox"
                checked={git}
                onChange={(event) => setGit(event.target.checked)}
                disabled={busy || !!created}
              />
              <span>
                {zh ? "初始化 Git 仓库" : "Initialize a Git repository"}
              </span>
            </label>
          </>
        )}
        {error && (
          <p className="form-error" role="alert">
            {error}
          </p>
        )}
        <div className="dialog-actions">
          <button type="button" onClick={onClose} disabled={busy}>
            {zh ? "取消" : "Cancel"}
          </button>
          <button
            className="primary"
            type="submit"
            disabled={busy || !name.trim() || (mode === "open" && !path.trim())}
          >
            {busy ? <Loader2 size={14} className="spin" /> : <Plus size={14} />}
            {created && !busy
              ? zh
                ? "继续打开项目"
                : "Continue opening project"
              : busy
                ? zh
                  ? "正在创建…"
                  : "Creating…"
                : zh
                  ? "添加项目并新建会话"
                  : "Add project and start chat"}
          </button>
        </div>
      </form>
    </Modal>
  );
}

export function RenameDialog({
  language,
  session,
  onClose,
  onSave,
}: {
  language: Language;
  session: SessionSummary;
  onClose: () => void;
  onSave: (name: string) => Promise<void>;
}) {
  const zh = language === "zh";
  const [name, setName] = useState(session.title ?? "");
  const mounted = useMountedFlag();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  return (
    <Modal
      language={language}
      title={zh ? "重命名会话" : "Rename session"}
      onClose={() => {
        if (!busy) onClose();
      }}
    >
      <form
        className="project-form"
        onSubmit={(event) => {
          event.preventDefault();
          if (!name.trim() || busy) return;
          setBusy(true);
          void onSave(name.trim())
            .then(() => {
              if (mounted.current) onClose();
            })
            .catch((cause: Error) => {
              if (mounted.current) setError(cause.message);
            })
            .finally(() => {
              if (mounted.current) setBusy(false);
            });
        }}
      >
        <label htmlFor="session-title">
          {zh ? "会话名称" : "Session name"}
        </label>
        <input
          id="session-title"
          autoFocus
          maxLength={120}
          value={name}
          onChange={(event) => setName(event.target.value)}
          disabled={busy}
        />
        {error && (
          <p role="alert" className="form-error">
            {error}
          </p>
        )}
        <div className="dialog-actions">
          <button type="button" disabled={busy} onClick={onClose}>
            {zh ? "取消" : "Cancel"}
          </button>
          <button className="primary" disabled={busy || !name.trim()}>
            {busy ? (
              <Loader2 className="spin" size={14} />
            ) : (
              <Check size={14} />
            )}
            {zh ? "保存" : "Save"}
          </button>
        </div>
      </form>
    </Modal>
  );
}

export function ProjectRenameDialog({
  language,
  project,
  onClose,
  onSave,
}: {
  language: Language;
  project: Project;
  onClose: () => void;
  onSave: (name: string) => Promise<void>;
}) {
  const zh = language === "zh";
  const [name, setName] = useState(project.name);
  const mounted = useMountedFlag();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  return (
    <Modal
      language={language}
      title={zh ? "重命名项目" : "Rename project"}
      onClose={() => {
        if (!busy) onClose();
      }}
    >
      <form
        className="project-form"
        onSubmit={(event) => {
          event.preventDefault();
          if (!name.trim() || busy) return;
          setBusy(true);
          void onSave(name.trim())
            .then(() => {
              if (mounted.current) onClose();
            })
            .catch((cause: Error) => {
              if (mounted.current) setError(cause.message);
            })
            .finally(() => {
              if (mounted.current) setBusy(false);
            });
        }}
      >
        <label htmlFor="project-rename">{zh ? "项目名称" : "Project name"}</label>
        <input
          id="project-rename"
          autoFocus
          maxLength={80}
          value={name}
          onChange={(event) => setName(event.target.value)}
          disabled={busy}
        />
        {error && <p role="alert" className="form-error">{error}</p>}
        <div className="dialog-actions">
          <button type="button" disabled={busy} onClick={onClose}>
            {zh ? "取消" : "Cancel"}
          </button>
          <button className="primary" disabled={busy || !name.trim()}>
            {busy ? <Loader2 className="spin" size={14} /> : <Check size={14} />}
            {zh ? "保存" : "Save"}
          </button>
        </div>
      </form>
    </Modal>
  );
}

export function TagsDialog({
  language,
  session,
  onClose,
  onSave,
}: {
  language: Language;
  session: SessionSummary;
  onClose: () => void;
  onSave: (tags: string[]) => Promise<void>;
}) {
  const zh = language === "zh";
  const mounted = useMountedFlag();
  const [value, setValue] = useState((session.tags ?? []).join(", "));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  return (
    <Modal
      language={language}
      title={translate(language, "editTags")}
      onClose={() => {
        if (!busy) onClose();
      }}
    >
      <form
        className="project-form"
        onSubmit={(event) => {
          event.preventDefault();
          if (busy) return;
          const tags = [...new Set(value.split(",").map((tag) => tag.trim()).filter(Boolean))];
          setBusy(true);
          setError("");
          void onSave(tags)
            .then(() => {
              if (mounted.current) onClose();
            })
            .catch((cause: Error) => {
              if (mounted.current) setError(cause.message);
            })
            .finally(() => {
              if (mounted.current) setBusy(false);
            });
        }}
      >
        <label htmlFor="session-tags">{translate(language, "tags")}</label>
        <input
          id="session-tags"
          autoFocus
          value={value}
          onChange={(event) => setValue(event.target.value)}
          placeholder={translate(language, "tagsPlaceholder")}
          disabled={busy}
        />
        <p className="form-hint">
          {zh ? "使用逗号分隔多个标签。" : "Separate multiple tags with commas."}
        </p>
        {error && <p className="form-error" role="alert">{error}</p>}
        <div className="dialog-actions">
          <button type="button" disabled={busy} onClick={onClose}>
            {zh ? "取消" : "Cancel"}
          </button>
          <button className="primary" disabled={busy}>
            {busy ? <Loader2 className="spin" size={14} /> : <Check size={14} />}
            {translate(language, "saveTags")}
          </button>
        </div>
      </form>
    </Modal>
  );
}

function shortTime(time: number, language: Language) {
  const minutes = Math.max(0, Math.floor((Date.now() - time) / 60_000));
  const zh = language === "zh";
  if (minutes < 1) return zh ? "刚刚" : "now";
  if (minutes < 60) return zh ? `${minutes} 分钟` : `${minutes}m`;
  if (minutes < 1440)
    return zh
      ? `${Math.floor(minutes / 60)} 小时`
      : `${Math.floor(minutes / 60)}h`;
  return new Intl.DateTimeFormat(zh ? "zh-CN" : "en", {
    month: "short",
    day: "numeric",
  }).format(time);
}

interface SidebarProps {
  projects: Project[];
  sessions: SessionSummary[];
  draftSessionIds: ReadonlySet<string>;
  activeProjectId: string;
  sessionId: string;
  language: Language;
  creating: boolean;
  canManage: boolean;
  canCreate: boolean;
  onSelectProject: (project: Project) => void;
  onSelectSession: (session: SessionSummary) => void;
  onNewSession: (projectId: string) => void;
  onNewProject: () => void;
  onRename: (session: SessionSummary) => void;
  onRenameProject: (project: Project) => void;
  onProjectMetadata: (project: Project, archived: boolean) => Promise<void>;
  onEditTags: (session: SessionSummary) => void;
  onDelete: (session: SessionSummary) => void;
  onMetadata: (
    session: SessionSummary,
    patch: { pinned?: boolean; archived?: boolean; tags?: string[] },
  ) => Promise<void>;
}

export function ProjectSidebar({
  projects,
  sessions,
  draftSessionIds,
  activeProjectId,
  sessionId,
  language,
  creating,
  canManage,
  canCreate,
  onSelectProject,
  onSelectSession,
  onNewSession,
  onNewProject,
  onRename,
  onRenameProject,
  onProjectMetadata,
  onEditTags,
  onDelete,
  onMetadata,
}: SidebarProps) {
  const zh = language === "zh";
  const [archived, setArchived] = useState(false);
  const [collapsed, setCollapsed] = useState<Set<string>>(() => {
    try {
      return new Set(
        JSON.parse(
          localStorage.getItem("rpi-collapsed-projects") ?? "[]",
        ) as string[],
      );
    } catch {
      return new Set();
    }
  });
  const [menu, setMenu] = useState("");
  const [pending, setPending] = useState("");
  const [menuPosition, setMenuPosition] = useState({ top: 0, left: 0 });
  useEffect(() => {
    try {
      localStorage.setItem(
        "rpi-collapsed-projects",
        JSON.stringify([...collapsed]),
      );
    } catch {
      /* optional preference */
    }
  }, [collapsed]);
  useEffect(() => {
    if (!menu) return;
    const close = (event: PointerEvent) => {
      if (
        !(event.target as Element).closest(
          ".session-menu,.session-context-menu",
        )
      )
        setMenu("");
    };
    const escape = (event: KeyboardEvent) => {
      if (event.key === "Escape") setMenu("");
    };
    const scroll = () => setMenu("");
    document.addEventListener("pointerdown", close);
    document.addEventListener("keydown", escape);
    document.querySelector(".project-tree")?.addEventListener("scroll", scroll);
    return () => {
      document.removeEventListener("pointerdown", close);
      document.removeEventListener("keydown", escape);
      document
        .querySelector(".project-tree")
        ?.removeEventListener("scroll", scroll);
    };
  }, [menu]);
  function toggle(id: string) {
    setCollapsed((current) => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }
  async function metadata(
    session: SessionSummary,
    patch: { pinned?: boolean; archived?: boolean; tags?: string[] },
  ) {
    if (pending) return;
    setPending(session.sessionId);
    setMenu("");
    try {
      await onMetadata(session, patch);
    } finally {
      setPending("");
    }
  }
  async function projectMetadata(project: Project, archived: boolean) {
    if (pending) return;
    setPending(`project:${project.id}`);
    setMenu("");
    try {
      await onProjectMetadata(project, archived);
    } finally {
      setPending("");
    }
  }
  function sessionRow(session: SessionSummary) {
    const title = session.title || (zh ? "新会话" : "New session");
    const running = ["running", "starting", "awaiting_input"].includes(
      session.status,
    );
    return (
      <div
        className={`project-session ${session.sessionId === sessionId ? "active" : ""}`}
        key={session.sessionId}
        onContextMenu={(event) => {
          if (!canManage) return;
          event.preventDefault();
          setMenuPosition({
            top: Math.min(event.clientY, window.innerHeight - 150),
            left: Math.max(8, Math.min(event.clientX, window.innerWidth - 180)),
          });
          setMenu(session.sessionId);
        }}
      >
        <button
          className="project-session-link"
          onClick={() => onSelectSession(session)}
          title={title}
          aria-current={session.sessionId === sessionId ? "page" : undefined}
        >
          {running ? (
            <span
              className="session-running-dot"
              title={zh ? "运行中" : "Running"}
            />
          ) : session.pinned ? (
            <Pin size={12} />
          ) : (
            <span className="session-indent-dot" />
          )}
          <span className="session-title">
            {session.relation && <span className="fork-label">↳ </span>}
            {title}
          </span>
          <SessionIssueBadge session={session} language={language} />
          {draftSessionIds.has(session.sessionId) && (
            <span
              className="session-draft"
              title={translate(language, "draft")}
            >
              {translate(language, "draft")}
            </span>
          )}
          {(session.tags ?? []).length > 0 && (
            <span className="session-tags" aria-label={translate(language, "tags")}>
              {(session.tags ?? []).slice(0, 2).map((tag) => (
                <span className="session-tag" key={tag}>{tag}</span>
              ))}
              {(session.tags ?? []).length > 2 && (
                <span className="session-tag-more">+{(session.tags ?? []).length - 2}</span>
              )}
            </span>
          )}
          <time dateTime={new Date(session.updatedAt).toISOString()}>
            {shortTime(session.updatedAt, language)}
          </time>
        </button>
        {canManage && (
          <div className="session-menu">
            <button
              className="icon-button session-menu-trigger"
              aria-label={`${zh ? "会话操作" : "Session actions"}: ${title}`}
              aria-expanded={menu === session.sessionId}
              aria-haspopup="menu"
              onClick={(event) => {
                const box = event.currentTarget.getBoundingClientRect();
                setMenuPosition({
                  top: Math.min(box.bottom + 4, window.innerHeight - 150),
                  left: Math.max(
                    8,
                    Math.min(box.right - 165, window.innerWidth - 180),
                  ),
                });
                setMenu(menu === session.sessionId ? "" : session.sessionId);
              }}
              disabled={pending === session.sessionId}
            >
              <MoreHorizontal size={15} />
            </button>
            {menu === session.sessionId &&
              createPortal(
                <div
                  className="session-context-menu"
                  role="menu"
                  aria-label={title}
                  style={menuPosition}
                >
                  <button
                    role="menuitem"
                    onClick={() => {
                      setMenu("");
                      onRename(session);
                    }}
                  >
                    <SquarePen size={14} />
                    {zh ? "重命名" : "Rename"}
                  </button>
                  <button
                    role="menuitem"
                    onClick={() => {
                      setMenu("");
                      onEditTags(session);
                    }}
                  >
                    <Tag size={14} />
                    {translate(language, "editTags")}
                  </button>
                  <button
                    role="menuitem"
                    onClick={() =>
                      void metadata(session, { pinned: !session.pinned })
                    }
                  >
                    {session.pinned ? <PinOff size={14} /> : <Pin size={14} />}
                    {session.pinned
                      ? zh
                        ? "取消置顶"
                        : "Unpin"
                      : zh
                        ? "置顶"
                        : "Pin"}
                  </button>
                  <button
                    role="menuitem"
                    onClick={() =>
                      void metadata(session, { archived: !session.archived })
                    }
                  >
                    {session.archived ? (
                      <ArchiveRestore size={14} />
                    ) : (
                      <Archive size={14} />
                    )}
                    {session.archived
                      ? zh
                        ? "恢复会话"
                        : "Restore chat"
                      : zh
                        ? "归档"
                        : "Archive"}
                  </button>
                  <button
                    role="menuitem"
                    onClick={() => {
                      setMenu("");
                      onDelete(session);
                    }}
                  >
                    <X size={14} />
                    {zh ? "永久删除" : "Delete permanently"}
                  </button>

                </div>,
                document.body,
              )}
          </div>
        )}
      </div>
    );
  }
  const fallback: Project = { id: "", name: zh ? "会话" : "Chats", path: "" };
  const catalog = projects.length ? projects : [fallback];
  const belongsToProject = (session: SessionSummary, project: Project) =>
    !projects.length ||
    session.projectId === project.id ||
    (!session.projectId && project.id === catalog[0].id);
  const groups = catalog.filter((project) =>
    archived
      ? Boolean(project.archived) ||
        sessions.some(
          (session) => session.archived && belongsToProject(session, project),
        )
      : !project.archived,
  );
  return (
    <div className="project-navigation">
      <div className="sidebar-primary-actions">
        <button
          className="sidebar-new-chat"
          onClick={() => onNewSession(activeProjectId)}
          disabled={creating}
        >
          <SquarePen size={16} />
          <span>{zh ? "新建会话" : "New session"}</span>
          <kbd>Ctrl N</kbd>
        </button>
      </div>
      <div className="project-section-heading">
        <span>
          {archived ? (zh ? "已归档" : "Archived") : zh ? "项目" : "Projects"}
        </span>
        <div>
          <button
            className={`icon-button ${archived ? "selected" : ""}`}
            aria-label={
              archived
                ? zh
                  ? "返回项目"
                  : "Show projects"
                : zh
                  ? "查看已归档"
                  : "Show archived chats"
            }
            title={
              zh
                ? archived
                  ? "已归档项目和会话"
                  : "查看已归档项目和会话"
                : archived
                  ? "Archived projects and chats"
                  : "Show archived projects and chats"
            }
            onClick={() => setArchived(!archived)}
          >
            <Archive size={14} />
          </button>
          {canCreate && (
            <button
              className="icon-button"
              aria-label={zh ? "新建项目" : "New project"}
              title={zh ? "新建项目" : "New project"}
              onClick={onNewProject}
            >
              <Plus size={16} />
            </button>
          )}
        </div>
      </div>
      <nav
        className="project-tree"
        aria-label={zh ? "项目与会话" : "Projects and chats"}
      >
        {groups.map((project) => {
          const chats = sessions
            .filter(
              (session) =>
                belongsToProject(session, project) &&
                (Boolean(project.archived) ||
                  Boolean(session.archived) === archived),
            )
            .sort(
              (a, b) =>
                Number(Boolean(b.pinned)) - Number(Boolean(a.pinned)) ||
                b.updatedAt - a.updatedAt,
            );
          const expanded = !collapsed.has(project.id);
          return (
            <section
              key={project.id}
              className={`project-group ${project.id === activeProjectId ? "selected-project" : ""}`}
              aria-label={project.name}
            >
              <div className="project-group-header">
                <button
                  className="icon-button project-collapse"
                  aria-label={`${expanded ? (zh ? "折叠项目" : "Collapse project") : zh ? "展开项目" : "Expand project"}: ${project.name}`}
                  aria-expanded={expanded}
                  onClick={() => toggle(project.id)}
                >
                  {expanded ? (
                    <ChevronDown size={12} />
                  ) : (
                    <ChevronRight size={12} />
                  )}
                </button>
                <button
                  className="project-name"
                  aria-label={`${zh ? "展开项目" : "Expand project"}: ${project.name}`}
                  title={project.path}
                  aria-expanded={expanded}
                  onClick={() => {
                    toggle(project.id);
                    onSelectProject(project);
                  }}
                >
                  {project.id === activeProjectId ? (
                    <FolderOpen size={15} />
                  ) : (
                    <Folder size={15} />
                  )}
                  <span>{project.name}</span>
                </button>
                {canManage && (
                  <>
                    <button
                      className="icon-button project-menu-trigger"
                      aria-label={`${zh ? "项目操作" : "Project actions"}: ${project.name}`}
                      aria-expanded={menu === `project:${project.id}`}
                      aria-haspopup="menu"
                      onClick={(event) => {
                        const box = event.currentTarget.getBoundingClientRect();
                        setMenuPosition({
                          top: Math.min(box.bottom + 4, window.innerHeight - 100),
                          left: Math.max(8, Math.min(box.right - 165, window.innerWidth - 180)),
                        });
                        setMenu(menu === `project:${project.id}` ? "" : `project:${project.id}`);
                      }}
                    >
                      <MoreHorizontal size={15} />
                    </button>
                    {menu === `project:${project.id}` &&
                      createPortal(
                        <div
                          className="session-context-menu"
                          role="menu"
                          aria-label={project.name}
                          style={menuPosition}
                        >
                          <button
                            role="menuitem"
                            onClick={() => {
                              setMenu("");
                              onRenameProject(project);
                            }}
                          >
                            <SquarePen size={14} />
                            {zh ? "重命名" : "Rename"}
                          </button>
                          <button
                            role="menuitem"
                            onClick={() =>
                              void projectMetadata(project, !project.archived)
                            }
                          >
                            {project.archived ? (
                              <ArchiveRestore size={14} />
                            ) : (
                              <Archive size={14} />
                            )}
                            {project.archived
                              ? zh
                                ? "恢复项目"
                                : "Restore project"
                              : zh
                                ? "归档项目"
                                : "Archive project"}
                          </button>
                        </div>,
                        document.body,
                      )}
                  </>
                )}
                <button
                  className="icon-button project-new-session"
                  aria-label={`${zh ? "项目内新建会话" : "New session in"}: ${project.name}`}
                  onClick={() => {
                    setArchived(false);
                    setCollapsed((current) => {
                      const next = new Set(current);
                      next.delete(project.id);
                      return next;
                    });
                    onNewSession(project.id);
                  }}
                  disabled={archived || Boolean(project.archived) || creating}
                >
                  <Plus size={14} />
                </button>
              </div>
              {expanded && (
                <div className="project-chats">
                  {chats.map(sessionRow)}
                  {!chats.length && (
                    <button
                      className="project-empty"
                      disabled={archived || creating}
                      onClick={() => onNewSession(project.id)}
                    >
                      {archived
                        ? project.archived
                          ? zh
                            ? "没有会话"
                            : "No chats"
                          : zh
                            ? "没有已归档会话"
                            : "No archived chats"
                        : zh
                          ? "开始第一个会话"
                          : "Start your first chat"}
                      {!archived && <Plus size={12} />}
                    </button>
                  )}
                </div>
              )}
            </section>
          );
        })}
      </nav>
      {canCreate && (
        <button className="sidebar-add-project" onClick={onNewProject}>
          <FolderPlus size={15} />
          {zh ? "添加项目" : "Add project"}
        </button>
      )}
    </div>
  );
}
