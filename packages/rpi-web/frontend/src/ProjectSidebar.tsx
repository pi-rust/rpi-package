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
  Search,
  SquarePen,
  X,
} from "lucide-react";
import { api, type Project, type SessionSummary } from "./api";
import { translate, type Language } from "./i18n";
import { createPortal } from "react-dom";

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
  const ref = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const dialog = ref.current;
    dialog?.showModal();
    const input = dialog?.querySelector<HTMLInputElement>(
      'input:not([type="checkbox"])',
    );
    input?.focus();
    input?.select();
    return () => dialog?.close();
  }, []);
  return (
    <dialog
      ref={ref}
      className="workspace-dialog"
      onCancel={(event) => {
        event.preventDefault();
        onClose();
      }}
      aria-label={title}
      onClick={(event) => {
        if (event.target === event.currentTarget) {
          const box = event.currentTarget.getBoundingClientRect();
          if (
            event.clientX < box.left ||
            event.clientX > box.right ||
            event.clientY < box.top ||
            event.clientY > box.bottom
          )
            onClose();
        }
      }}
    >
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
    </dialog>
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
  const [path, setPath] = useState("");
  const [git, setGit] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [created, setCreated] = useState<Project | null>(null);
  const mounted = useMountedFlag();
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
          onChange={(event) => setName(event.target.value)}
          placeholder={zh ? "例如：我的网站" : "e.g. my-website"}
          maxLength={80}
          autoFocus
          required
          disabled={busy || !!created}
        />
        {mode === "open" ? (
          <>
            <label htmlFor="project-path">
              {zh ? "目录路径" : "Folder path"}
            </label>
            <input
              id="project-path"
              value={path}
              onChange={(event) => setPath(event.target.value)}
              placeholder={root}
              required
              disabled={busy || !!created}
            />
            <p className="form-hint">
              {zh
                ? "选择项目目录中的已有文件夹："
                : "Choose an existing folder inside:"}{" "}
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
  onMetadata: (
    session: SessionSummary,
    patch: { pinned?: boolean; archived?: boolean },
  ) => Promise<void>;
}

export function ProjectSidebar({
  projects,
  sessions,
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
  onMetadata,
}: SidebarProps) {
  const zh = language === "zh";
  const [query, setQuery] = useState("");
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
  const normalized = query.trim().toLowerCase();
  async function metadata(
    session: SessionSummary,
    patch: { pinned?: boolean; archived?: boolean },
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
                </div>,
                document.body,
              )}
          </div>
        )}
      </div>
    );
  }
  const fallback: Project = { id: "", name: zh ? "会话" : "Chats", path: "" };
  const groups = projects.length ? projects : [fallback];
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
        <label className="sidebar-search">
          <Search size={15} />
          <input
            aria-label={zh ? "搜索项目或会话" : "Search projects or chats"}
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder={zh ? "搜索" : "Search"}
          />
          <kbd>Ctrl K</kbd>
        </label>
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
            title={zh ? "已归档会话" : "Archived chats"}
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
          const projectMatches = project.name
            .toLowerCase()
            .includes(normalized);
          const chats = sessions
            .filter(
              (session) =>
                (!projects.length ||
                  session.projectId === project.id ||
                  (!session.projectId && project.id === groups[0].id)) &&
                Boolean(session.archived) === archived &&
                (projectMatches ||
                  `${session.title ?? ""} ${session.sessionId}`
                    .toLowerCase()
                    .includes(normalized)),
            )
            .sort(
              (a, b) =>
                Number(Boolean(b.pinned)) - Number(Boolean(a.pinned)) ||
                b.updatedAt - a.updatedAt,
            );
          if (normalized && !projectMatches && !chats.length) return null;
          const expanded = !collapsed.has(project.id) || Boolean(normalized);
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
                  aria-label={`${zh ? "打开项目" : "Open project"}: ${project.name}`}
                  title={project.path}
                  onClick={() => {
                    onSelectProject(project);
                    setCollapsed((current) => {
                      const next = new Set(current);
                      next.delete(project.id);
                      return next;
                    });
                    setArchived(false);
                  }}
                >
                  {project.id === activeProjectId ? (
                    <FolderOpen size={15} />
                  ) : (
                    <Folder size={15} />
                  )}
                  <span>{project.name}</span>
                </button>
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
                  disabled={creating}
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
                        ? zh
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
