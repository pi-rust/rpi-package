import { createPortal } from "react-dom";
import {
  memo,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type FormEvent,
  type CSSProperties,
  type ReactNode,
} from "react";
import {
  Activity,
  AlertTriangle,
  ArrowDown,
  ArrowUp,
  Bot,
  Check,
  ClipboardList,
  CircleDollarSign,
  ChevronDown,
  Code2,
  Copy,
  Database,
  Files,
  FileText,
  Folder,
  Gauge,
  GitBranch,
  Globe,
  History,
  ListTodo,
  Loader2,
  LogOut,
  Moon,
  Paperclip,
  PanelLeft,
  Play,
  RefreshCw,
  RotateCcw,
  Settings2,
  ShieldCheck,
  Sparkles,
  SquarePen,
  Sigma,
  Sun,
  Search,
  Terminal,
  Wrench,
  X,
} from "lucide-react";
import { MarkdownBody } from "./MarkdownBody";
import {
  api,
  ApiError,
  commandData,
  roleOf,
  type AuthProviderStatus,
  type Capabilities,
  type HistoryResponse,
  type ModelDescriptor,
  type Project,
  type QueueItem,
  type RuntimeState,
  type SeqEvent,
  type SessionSummary,
  type ToolCatalog,
} from "./api";
import {
  detectLanguage,
  saveLanguage,
  translate,
  valueLabel,
  type Language,
  type MessageKey,
} from "./i18n";
import { Splitter, useStoredNumber, Workbench } from "./Workbench";
import { MenuSelect } from "./MenuSelect";
import { LoginScreen } from "./LoginScreen";
import { ProjectContext } from "./ProjectContext";
import {
  ConfirmDialog,
  Modal,
  PromptDialog,
  ProjectDialog,
  ProjectRenameDialog,
  ProjectSidebar,
  RenameDialog,
  TagsDialog,
} from "./ProjectSidebar";
import {
  rowsFromAssistant,
  rowsFromMessages,
  prependRows,
  upsertTool,
  withoutPrefix,
  eventsAfterSnapshot,
  toolTiming,
  isShellTool,
  readOutput,
  shellCommand,
  shellOutput,
  planInfo,
  todoItems,
  editInfo,
  isMarkdownText,
  imageBlocksOf,
  type ImageBlock,
  type PlanInfo,
  type TodoItem,
  type EditInfo,
  elapsedMs,
  stopToolTimers,
  type ToolTiming,
  textOf,
  type Row,
} from "./transcript";

type Attachment = {
  name: string;
  path: string;
  size: number;
  previewUrl?: string;
};

const DRAFT_STORAGE_KEY = "rpi-session-drafts";

function loadDrafts(): Map<string, string> {
  try {
    const raw = localStorage.getItem(DRAFT_STORAGE_KEY);
    if (!raw) return new Map();
    const parsed = JSON.parse(raw) as unknown;
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed))
      return new Map();
    return new Map(
      Object.entries(parsed as Record<string, unknown>).filter(
        (entry): entry is [string, string] =>
          typeof entry[0] === "string" &&
          typeof entry[1] === "string" &&
          entry[1].length > 0,
      ),
    );
  } catch {
    return new Map();
  }
}

function saveDrafts(drafts: Map<string, string>) {
  try {
    if (drafts.size === 0) {
      localStorage.removeItem(DRAFT_STORAGE_KEY);
      return;
    }
    localStorage.setItem(
      DRAFT_STORAGE_KEY,
      JSON.stringify(Object.fromEntries(drafts)),
    );
  } catch {
    /* optional browser persistence */
  }
}

const EVENT_TYPES = [
  "ready",
  "agent_start",
  "operation_accepted",
  "agent_end",
  "agent_settled",
  "turn_start",
  "turn_end",
  "message_start",
  "message_update",
  "message_end",
  "tool_execution_start",
  "tool_execution_update",
  "tool_execution_end",
  "ui_dialog_request",
  "ui_dialog_closed",
  "retry_scheduled",
  "host_error",
  "host_closed",
] as const;

type ToolRow = Extract<Row, { kind: "tool" }>;

type UiDialogOption = {
  title?: string;
  description?: string;
  value?: unknown;
  permission?: string;
};

type UiDialog = {
  requestId: string;
  toolCallId?: string;
  ui: {
    purpose?: "question" | "permission" | string;
    question?: string;
    message?: string;
    context?: string;
    kind?: string;
    options?: UiDialogOption[];
    allowFreeform?: boolean;
    allowMultiple?: boolean;
    allowComment?: boolean;
    suggest?: string;
    freeform?: boolean;
  };
};

type TextRequest =
  | {
      kind: "prompt";
      title: string;
      message?: string;
      defaultValue?: string;
    }
  | { kind: "confirm"; title: string; message: string };
type SlashCommand = {
  name: string;
  usage: string;
  hint: string;
  enabled: boolean;
};

/** Resolve a `/model` argument against the advertised catalog. */
function modelKey(model: { provider?: string | null; id: string }) {
  return JSON.stringify([model.provider ?? "", model.id]);
}

function matchModel(models: ModelDescriptor[], query: string) {
  const needle = query.toLowerCase();
  if (!needle) return undefined;
  for (const predicate of [
    (entry: ModelDescriptor) =>
      `${entry.provider}/${entry.id}`.toLowerCase() === needle,
    (entry: ModelDescriptor) => entry.id.toLowerCase() === needle,
    (entry: ModelDescriptor) => entry.name.toLowerCase() === needle,
    (entry: ModelDescriptor) => entry.provider.toLowerCase() === needle,
    (entry: ModelDescriptor) => entry.id.toLowerCase().includes(needle),
    (entry: ModelDescriptor) => entry.name.toLowerCase().includes(needle),
    (entry: ModelDescriptor) => entry.provider.toLowerCase().includes(needle),
  ]) {
    const matches = models.filter(predicate);
    if (matches.length) return matches.length === 1 ? matches[0] : undefined;
  }
  return undefined;
}

/**
 * Read the native snapshot, reopening the session first when its host is not
 * running (a bridge restart leaves only the registry entry behind).
 */
async function snapshotOf(sessionId: string) {
  try {
    return await api.snapshot(sessionId);
  } catch {
    await api.openSession(sessionId);
    return api.snapshot(sessionId);
  }
}

function Mark({ large = false }: { large?: boolean }) {
  return (
    <div className={`brand-mark ${large ? "large" : ""}`} aria-hidden="true">
      <Code2 size={large ? 30 : 19} />
    </div>
  );
}

function safeJson(value: unknown) {
  if (typeof value === "string") return value;
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return String(value);
  }
}

function pathOfArgs(args: unknown) {
  if (!args || typeof args !== "object" || Array.isArray(args)) return "";
  const record = args as Record<string, unknown>;
  return typeof record.path === "string"
    ? record.path
    : typeof record.filePath === "string"
      ? record.filePath
      : "";
}

function syntaxForPath(path: string) {
  const file = path.split(/[\\/]/).pop()?.toLowerCase() ?? "";
  if (file === "dockerfile") return "dockerfile";
  if (file === "makefile") return "makefile";
  const extension = file.includes(".") ? file.split(".").pop() : "";
  const syntax: Record<string, string> = {
    c: "c",
    cc: "cpp",
    cpp: "cpp",
    cs: "csharp",
    css: "css",
    gd: "gdscript",
    go: "go",
    h: "c",
    hpp: "cpp",
    html: "xml",
    java: "java",
    js: "javascript",
    json: "json",
    jsx: "jsx",
    md: "markdown",
    mjs: "javascript",
    py: "python",
    rs: "rust",
    sh: "bash",
    sql: "sql",
    svelte: "xml",
    toml: "ini",
    ts: "typescript",
    tsx: "tsx",
    vue: "xml",
    xml: "xml",
    yaml: "yaml",
    yml: "yaml",
  };
  return syntax[extension ?? ""] ?? "text";
}

function fencedCode(text: string, syntax: string) {
  const longestFence = Math.max(
    2,
    ...Array.from(text.matchAll(/`+/g), (match) => match[0].length),
  );
  const fence = "`".repeat(longestFence + 1);
  return `${fence}${syntax}\n${text.replace(/\r\n/g, "\n")}\n${fence}`;
}

/** Render read results as source code instead of wrapping them as prose. */
function ReadCode({
  path,
  text,
  language,
}: {
  path: string;
  text: string;
  language: Language;
}) {
  return (
    <div className="read-code">
      {path && <div className="read-code-path">{path}</div>}
      <MarkdownBody
        text={fencedCode(text, syntaxForPath(path))}
        language={language}
      />
    </div>
  );
}

/** 1234 -> 1.2k, 1234567 -> 1.2M. */
function formatTokens(value: number) {
  if (!Number.isFinite(value) || value <= 0) return "0";
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(1)}M`;
  if (value >= 1_000)
    return `${(value / 1_000).toFixed(value >= 10_000 ? 0 : 1)}k`;
  return String(Math.round(value));
}

function statNumber(
  stats: RuntimeState["stats"],
  keys: string[],
): number | null {
  if (!stats) return null;
  const values = stats as unknown as Record<string, unknown>;
  for (const key of keys) {
    const value = values[key];
    if (typeof value === "number" && Number.isFinite(value)) return value;
  }
  return null;
}

function normalizeRuntimeStats(value: unknown): RuntimeState["stats"] {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const record = value as Record<string, unknown>;
  const looksLikeStats = (candidate: Record<string, unknown>) =>
    [
      "input",
      "output",
      "inputTokens",
      "outputTokens",
      "cacheRead",
      "cachedTokens",
      "totalTokens",
      "operationCount",
    ].some((key) => key in candidate);
  if (looksLikeStats(record)) return record as unknown as RuntimeState["stats"];
  for (const key of ["stats", "usage", "data"]) {
    const nested = record[key];
    if (nested && typeof nested === "object" && !Array.isArray(nested)) {
      const nestedRecord = nested as Record<string, unknown>;
      if (looksLikeStats(nestedRecord)) {
        return nestedRecord as unknown as RuntimeState["stats"];
      }
    }
  }
  for (const nested of Object.values(record)) {
    if (nested && typeof nested === "object" && !Array.isArray(nested)) {
      const nestedRecord = nested as Record<string, unknown>;
      if (looksLikeStats(nestedRecord)) {
        return nestedRecord as unknown as RuntimeState["stats"];
      }
    }
  }
  return record as unknown as RuntimeState["stats"];
}

function usageNumber(value: unknown, keys: string[]): number {
  if (!value || typeof value !== "object") return 0;
  const record = value as Record<string, unknown>;
  for (const key of keys) {
    const candidate = record[key];
    if (typeof candidate === "number" && Number.isFinite(candidate)) {
      return candidate;
    }
  }
  return 0;
}

function inputTokenCount(stats: RuntimeState["stats"]) {
  const input = statNumber(stats, ["inputTokens", "input", "promptTokens"]);
  if (input != null) return input;
  const cached = cachedTokenCount(stats);
  const uncached = statNumber(stats, ["uncachedTokens"]);
  return cached != null && uncached != null ? cached + uncached : null;
}

function outputTokenCount(stats: RuntimeState["stats"]) {
  const output = statNumber(stats, [
    "outputTokens",
    "output",
    "completionTokens",
  ]);
  if (output != null) return output;
  const total = statNumber(stats, ["totalTokens"]);
  const input = inputTokenCount(stats);
  return total != null && input != null ? Math.max(0, total - input) : null;
}

function cachedTokenCount(stats: RuntimeState["stats"]) {
  return statNumber(stats, ["cachedTokens", "cacheRead"]);
}

function totalTokenCount(stats: RuntimeState["stats"]) {
  const total = statNumber(stats, ["totalTokens"]);
  if (total != null) return total;
  const input = inputTokenCount(stats) ?? 0;
  const output = outputTokenCount(stats) ?? 0;
  const cached = cachedTokenCount(stats) ?? 0;
  const written = statNumber(stats, ["cacheWrite"]) ?? 0;
  return input + output + cached + written > 0
    ? input + output + cached + written
    : null;
}

function costTotal(stats: RuntimeState["stats"]) {
  return statNumber(stats, ["costTotal", "cost"]);
}

function cacheHitRatio(stats: RuntimeState["stats"]): number | null {
  if (!stats) return null;
  if (
    typeof stats.cacheHitRatio === "number" &&
    Number.isFinite(stats.cacheHitRatio)
  ) {
    return Math.max(0, Math.min(1, stats.cacheHitRatio));
  }
  const cached = cachedTokenCount(stats) ?? 0;
  const uncached = statNumber(stats, ["uncachedTokens"]);
  const input = inputTokenCount(stats);
  const total = uncached != null ? cached + uncached : (input ?? 0);
  return total > 0 ? cached / total : null;
}

/** Native message usage keeps cache reads/writes separate from uncached input. */
function latestMessageStats(messages: unknown[]): RuntimeState["stats"] {
  for (const message of [...messages].reverse()) {
    if (!message || typeof message !== "object") continue;
    if (roleOf(message) !== "assistant") continue;
    const usage = (message as Record<string, unknown>).usage;
    const input = usageNumber(usage, ["input", "inputTokens", "promptTokens"]);
    const output = usageNumber(usage, [
      "output",
      "outputTokens",
      "completionTokens",
    ]);
    const cached = usageNumber(usage, ["cacheRead", "cachedTokens"]);
    const written = usageNumber(usage, ["cacheWrite"]);
    const total =
      usageNumber(usage, ["totalTokens"]) || input + output + cached + written;
    if (total <= 0) continue;
    const cost = (usage as Record<string, unknown>).cost;
    return {
      messageCount: 1,
      inputTokens: input + cached + written,
      outputTokens: output,
      cachedTokens: cached,
      uncachedTokens: input + written,
      totalTokens: total,
      cacheHitRatio: null,
      costTotal: typeof cost === "number" ? cost : usageNumber(cost, ["total"]),
    };
  }
  return null;
}

/** One line of a provider error, for a transcript row. */
function shortReason(value: unknown) {
  const text = typeof value === "string" ? value : errorText(value);
  const line = text.split("\n")[0] ?? "";
  return line.length > 160 ? `${line.slice(0, 157)}…` : line;
}

function errorText(cause: unknown) {
  return cause instanceof Error ? cause.message : String(cause);
}

function normalizeLevel(value: unknown) {
  return typeof value === "string" ? value : "";
}

export default function App() {
  const [capabilities, setCapabilities] = useState<Capabilities | null>(null);
  const [authRequired, setAuthRequired] = useState(false);
  const [authenticated, setAuthenticated] = useState(false);
  const [authChecked, setAuthChecked] = useState(false);
  const [accessCheckFailed, setAccessCheckFailed] = useState(false);
  const [authRevision, setAuthRevision] = useState(0);
  const [passwordDraft, setPasswordDraft] = useState("");
  const [authBusy, setAuthBusy] = useState(false);
  const [authError, setAuthError] = useState("");
  const [sessions, setSessions] = useState<SessionSummary[]>([]);
  const [sessionId, setSessionId] = useState("");
  const [projects, setProjects] = useState<Project[]>([]);
  const [projectsRoot, setProjectsRoot] = useState("");
  const [projectDialog, setProjectDialog] = useState(false);
  const [projectRenameTarget, setProjectRenameTarget] =
    useState<Project | null>(null);
  const [renameTarget, setRenameTarget] = useState<SessionSummary | null>(null);
  const [tagsTarget, setTagsTarget] = useState<SessionSummary | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [mobileSessionPicker, setMobileSessionPicker] = useState(false);
  const [mobileSessionQuery, setMobileSessionQuery] = useState("");
  const [creatingSession, setCreatingSession] = useState(false);
  const creatingSessionRef = useRef(false);
  const [theme, setTheme] = useState<"light" | "dark">(() => {
    try {
      return localStorage.getItem("rpi-theme") === "dark" ? "dark" : "light";
    } catch {
      return "light";
    }
  });
  const [projectId, setProjectId] = useState("");
  const projectRef = useRef("");
  const [workbenchOpen, setWorkbenchOpen] = useState(
    () => window.innerWidth >= 1100,
  );
  const [sidebarWidth, setSidebarWidth] = useStoredNumber(
    "rpi-sidebar-width",
    248,
    200,
    360,
  );
  const [workbenchWidth, setWorkbenchWidth] = useStoredNumber(
    "rpi-workbench-width",
    440,
    300,
    900,
  );
  const [sending, setSending] = useState(false);
  const sendingRef = useRef(false);
  const sendingRevision = useRef(0);
  const localMessageSeq = useRef(0);
  const activeSessionRef = useRef(sessionId);
  activeSessionRef.current = sessionId;
  const diagnosticsRevision = useRef(0);
  const navigationRevision = useRef(0);
  const sessionListRevision = useRef(0);
  const sessionListRequest = useRef<Promise<SessionSummary[] | null> | null>(
    null,
  );
  const sessionListValue = useRef<SessionSummary[] | null>(null);
  const draftBySession = useRef(loadDrafts());
  const transcriptRef = useRef<HTMLDivElement>(null);
  const followingRef = useRef(true);
  const [following, setFollowing] = useState(true);
  const [rows, setRows] = useState<Row[]>([]);
  const rowsBySession = useRef(new Map<string, Row[]>());
  const rowsSessionRef = useRef(sessionId);
  const earlierPage = useRef<{
    rows: Row[];
    firstId: string;
    hasMore: boolean;
  } | null>(null);
  const [runtime, setRuntime] = useState<RuntimeState | null>(null);
  const [messageStats, setMessageStats] = useState<{
    sessionId: string;
    stats: RuntimeState["stats"];
  } | null>(null);
  const usingMessageStats =
    (totalTokenCount(runtime?.stats) ?? 0) <= 0 &&
    (inputTokenCount(runtime?.stats) ?? 0) <= 0 &&
    (outputTokenCount(runtime?.stats) ?? 0) <= 0 &&
    messageStats?.sessionId === sessionId &&
    !!messageStats.stats;
  const displayStats = usingMessageStats
    ? { ...runtime?.stats, ...messageStats!.stats! }
    : runtime?.stats;
  const [authProviders, setAuthProviders] = useState<AuthProviderStatus[]>([]);
  const [authProvider, setAuthProvider] = useState("");
  const [apiKeyDraft, setApiKeyDraft] = useState("");
  const [authMessage, setAuthMessage] = useState<{
    key: MessageKey;
    values: Record<string, string>;
  } | null>(null);
  const [historyData, setHistory] = useState<HistoryResponse | null>(null);
  const [queueItems, setQueueItems] = useState<QueueItem[]>([]);
  const [queueOpen, setQueueOpen] = useState(true);
  const [queueRunning, setQueueRunning] = useState(false);
  const pendingQueueItems = queueItems.filter((item) =>
    isEditableQueueItem(item),
  );
  const [showQueueHistory, setShowQueueHistory] = useState(false);
  const [queueDraft, setQueueDraft] = useState("");
  const queueInputRef = useRef<HTMLInputElement>(null);
  const composerInputRef = useRef<HTMLTextAreaElement>(null);
  const [queueKind, setQueueKind] = useState("followUp");
  const [queueEditId, setQueueEditId] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  function updateDraft(value: string, targetSession = activeSessionRef.current) {
    setDraft(value);
    if (!targetSession) return;
    if (value.length > 0) draftBySession.current.set(targetSession, value);
    else draftBySession.current.delete(targetSession);
    saveDrafts(draftBySession.current);
  }

  const clearDrafts = useCallback(() => {
    draftBySession.current.clear();
    saveDrafts(draftBySession.current);
  }, []);

  const draftSessionIds = useMemo(
    () => new Set(draftBySession.current.keys()),
    [draft],
  );
  const [attachments, setAttachments] = useState<Attachment[]>([]);
  const [uploading, setUploading] = useState(false);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const [status, setStatus] = useState("connecting");
  const [loadingSession, setLoadingSession] = useState(false);
  const [runActive, setRunActive] = useState(false);
  const [error, setError] = useState("");
  const [connectionFailed, setConnectionFailed] = useState(false);
  const [sidebar, setSidebar] = useState(() => window.innerWidth >= 768);
  const [panel, setPanel] = useState(false);
  const [inspector, setInspector] = useState(false);
  const [seen, setSeen] = useState(0);
  const [settleTick, setSettleTick] = useState(0);
  const [connectionRevision, setConnectionRevision] = useState(0);
  const [leafId, setLeafId] = useState("");
  const [interrupted, setInterrupted] = useState(false);
  const [firstId, setFirstId] = useState("");
  const [hasMore, setHasMore] = useState(false);
  const [loadingEarlier, setLoadingEarlier] = useState(false);
  const [historyLoading, setHistoryLoading] = useState(false);
  const [historyHasMore, setHistoryHasMore] = useState(false);
  const [mutationPending, setMutationPending] = useState(false);
  const [models, setModels] = useState<ModelDescriptor[]>([]);
  const [levels, setLevels] = useState<string[]>([]);
  const [modelPending, setModelPending] = useState(false);
  const [thinkingPending, setThinkingPending] = useState(false);
  const [language, setLanguage] = useState<Language>(() => detectLanguage());
  const [dialog, setDialog] = useState<UiDialog | null>(null);
  const dialogRequests = useRef(new Map<string, UiDialog>());
  const activeDialogId = useRef("");
  const [dialogAnswer, setDialogAnswer] = useState("");
  const [dialogBusy, setDialogBusy] = useState(false);
  const [textRequest, setTextRequest] = useState<TextRequest | null>(null);
  const textRequestResolver = useRef<((value: string | null) => void) | null>(
    null,
  );

  useEffect(() => {
    setAttachments((current) => {
      current.forEach((file) => {
        if (file.previewUrl) URL.revokeObjectURL(file.previewUrl);
      });
      return [];
    });
  }, [sessionId]);
  function requestText(request: TextRequest) {
    return new Promise<string | null>((resolve) => {
      textRequestResolver.current = resolve;
      setTextRequest(request);
    });
  }

  function resolveTextRequest(value: string | null) {
    const resolve = textRequestResolver.current;
    textRequestResolver.current = null;
    setTextRequest(null);
    resolve?.(value);
  }

  const lastSeq = useRef(0);
  const noteSeq = useRef(0);
  const currentAssistant = useRef<string | null>(null);
  const currentUser = useRef<string | null>(null);
  const credentialRevision = useRef(0);
  const dialogRevision = useRef(0);
  const queueSubmission = useRef(new Set<string>());
  const queueRunSubmission = useRef(new Set<string>());
  const queueDraftRevision = useRef(0);
  const metadataQueues = useRef(new Map<string, Promise<void>>());
  const metadataRevisions = useRef(new Map<string, number>());
  const source = useRef<EventSource | null>(null);

  const supported = useMemo(
    () => new Set(capabilities?.supportedCommands ?? []),
    [capabilities],
  );
  const actions = useMemo(
    () => new Set(capabilities?.supportedActions ?? []),
    [capabilities],
  );
  const busy =
    runActive ||
    status === "running" ||
    status === "starting" ||
    status === "awaiting_input";
  const t = useCallback(
    (key: MessageKey, values?: Record<string, string | number>) =>
      translate(language, key, values),
    [language],
  );
  const connectionLabel = t(
    (status === "awaiting_input" ? "waitingForAnswer" : status) as MessageKey,
  );
  const current = sessions.find((session) => session.sessionId === sessionId);
  const sidebarSessions = useMemo(
    () =>
      sessions.map((entry) =>
        entry.sessionId === sessionId ? { ...entry, status } : entry,
      ),
    [sessions, sessionId, status],
  );
  const mobileSessionResults = useMemo(() => {
    const query = mobileSessionQuery.trim().toLowerCase();
    return [...sidebarSessions]
      .filter((entry) => !entry.archived)
      .filter((entry) => {
        if (!query) return true;
        const projectName =
          projects.find((item) => item.id === entry.projectId)?.name ?? "";
        return `${entry.title} ${projectName}`.toLowerCase().includes(query);
      })
      .sort((a, b) => b.updatedAt - a.updatedAt);
  }, [mobileSessionQuery, projects, sidebarSessions]);
  const project = projects.find((entry) => entry.id === projectId);
  const activeSession = sidebarSessions.find(
    (entry) => entry.sessionId === sessionId,
  );
  const activeSessionRunning = [
    "running",
    "starting",
    "awaiting_input",
  ].includes(activeSession?.status ?? "");
  const slashCommands = useMemo<SlashCommand[]>(() => {
    const model = models.find(
      (entry) =>
        entry.id === runtime?.modelId && entry.provider === runtime?.provider,
    );
    return [
      {
        name: "/model",
        usage: "/model <provider/model or name>",
        hint: t("slashModel", {
          value: model?.name ?? runtime?.modelId ?? t("unknownModel"),
        }),
        enabled: supported.has("set_model"),
      },
      {
        name: "/thinking",
        usage: "/thinking <level>",
        hint: t("slashThinking", {
          value: valueLabel(language, runtime?.thinkingLevel ?? "—"),
        }),
        enabled: supported.has("set_thinking_level"),
      },
      {
        name: "/tools",
        usage: "/tools",
        hint: t("slashTools"),
        enabled: supported.has("get_available_tools"),
      },
      {
        name: "/compact",
        usage: "/compact [instructions]",
        hint: t("slashCompact"),
        enabled: supported.has("compact"),
      },
      {
        name: "/continue",
        usage: "/continue",
        hint: t("slashContinue"),
        enabled: supported.has("resume_pending"),
      },
      {
        name: "/abort",
        usage: "/abort",
        hint: t("slashAbort"),
        enabled: supported.has("abort"),
      },
      {
        name: "/new",
        usage: "/new",
        hint: t("slashNew"),
        enabled: true,
      },
      {
        name: "/help",
        usage: "/help",
        hint: t("slashHelp"),
        enabled: true,
      },
    ];
  }, [models, runtime, supported, language, t]);

  const slashMatches = useMemo(() => {
    const query = draft.trim().toLowerCase();
    if (!query.startsWith("/")) return [];
    if (/\s/.test(query)) return [];
    return slashCommands.filter((command) => command.name.startsWith(query));
  }, [draft, slashCommands]);

  const showNextDialog = useCallback(() => {
    const next = dialogRequests.current.values().next().value ?? null;
    const id = next?.requestId ?? "";
    if (id !== activeDialogId.current) {
      activeDialogId.current = id;
      dialogRevision.current++;
      setDialogBusy(false);
      setDialogAnswer("");
    }
    setDialog(next);
  }, []);

  const applyEvent = useCallback(
    (event: SeqEvent, seq: number) => {
      switch (event.type) {
        case "agent_start":
        case "operation_accepted":
          setRunActive(true);
          setStatus("running");
          break;
        case "agent_settled": {
          setRows(stopToolTimers);
          setRunActive(false);
          dialogRequests.current.clear();
          activeDialogId.current = "";
          setDialog(null);
          dialogRevision.current++;
          setDialogBusy(false);
          setStatus("ready");
          setInterrupted(Boolean(event.pendingResume));
          const outcome = String(event.outcome ?? "settled");
          const aborted =
            event.aborted && outcome !== "aborted" ? " · aborted" : "";
          const reason =
            typeof event.error === "string" && event.error.trim()
              ? ` · ${shortReason(event.error)}`
              : "";
          const summary =
            typeof event.summary === "string" && event.summary.trim()
              ? event.summary
              : `Run ${outcome}${aborted}${reason}`;
          if (reason) setError(String(event.error));
          setRows((previous) => [
            ...previous.filter(
              (row) =>
                row.id !== "retry-note" &&
                row.id !== `settled-${String(event.runId ?? seq)}` &&
                (row.kind !== "note" ||
                  row.translation?.key !== "restoredRun" ||
                  String(row.translation.values?.outcome ?? "") !== outcome),
            ),
            {
              kind: "note",
              id: `settled-${String(event.runId ?? seq)}`,
              text: summary,
              translation:
                typeof event.summary === "string" && event.summary.trim()
                  ? undefined
                  : {
                      key: "liveRun",
                      values: {
                        outcome,
                        reason,
                        aborted: aborted ? "aborted" : "",
                      },
                    },
            },
          ]);
          setSettleTick((tick) => tick + 1);
          break;
        }
        case "ui_dialog_request": {
          const request = event as unknown as UiDialog;
          if (request.requestId && request.ui) {
            dialogRequests.current.set(request.requestId, request);
            showNextDialog();
            setStatus("awaiting_input");
          }
          break;
        }
        case "ui_dialog_closed":
          if (!dialogRequests.current.delete(String(event.requestId ?? "")))
            break;
          showNextDialog();
          setStatus((status) =>
            dialogRequests.current.size
              ? "awaiting_input"
              : status === "awaiting_input"
                ? "running"
                : status,
          );
          break;
        case "host_error":
          setStatus("error");
          setError(String(event.error ?? "Native host error"));
          break;
        case "host_closed":
          setRows(stopToolTimers);
          setRunActive(false);
          dialogRequests.current.clear();
          activeDialogId.current = "";
          setStatus("closed");
          setDialog(null);
          break;
        case "retry_scheduled": {
          const attempt = Number(event.attempt ?? 1);
          const max = Number(event.maxRetries ?? 0);
          setRows((previous) => [
            ...previous.filter((row) => row.id !== "retry-note"),
            {
              kind: "note",
              id: "retry-note",
              text: `Retrying ${attempt}/${max} · ${shortReason(event.error)}`,
              translation: {
                key: "retryMessage",
                values: { attempt, max, reason: shortReason(event.error) },
              },
            },
          ]);
          break;
        }
        case "message_start": {
          if (roleOf(event.message) === "user") {
            const text = textOf(event.message);
            const prefix = `u${seq}`;
            currentUser.current = prefix;
            if (text)
              setRows((previous) => {
                const optimistic = previous.find(
                  (row) =>
                    row.kind === "text" &&
                    row.role === "user" &&
                    row.id.startsWith("local-") &&
                    row.text === text,
                );
                return [
                  ...previous.filter((row) => row.id !== optimistic?.id),
                  { kind: "text", id: prefix, role: "user", text },
                ];
              });
            break;
          }
          if (roleOf(event.message) !== "assistant") break;
          const prefix = `m${seq}`;
          currentAssistant.current = prefix;
          const initial = rowsFromAssistant(
            (event.message ?? {}) as Record<string, unknown>,
            prefix,
          );
          if (initial.length > 0)
            setRows((previous) => [...previous, ...initial]);
          break;
        }
        case "message_update":
        case "message_end": {
          if (roleOf(event.message) === "user") {
            if (event.type === "message_end") {
              const text = textOf(event.message);
              const prefix = currentUser.current ?? `u${seq}`;
              currentUser.current = null;
              if (text)
                setRows((previous) => {
                  const optimistic = previous.find(
                    (row) =>
                      row.kind === "text" &&
                      row.role === "user" &&
                      row.id.startsWith("local-") &&
                      row.text === text,
                  );
                  return [
                    ...previous.filter(
                      (row) => row.id !== prefix && row.id !== optimistic?.id,
                    ),
                    { kind: "text", id: prefix, role: "user", text },
                  ];
                });
            }
            break;
          }
          if (roleOf(event.message) !== "assistant") break;
          if (event.type === "message_end") {
            const stats = latestMessageStats([event.message]);
            if (stats)
              setMessageStats({ sessionId: activeSessionRef.current, stats });
          }
          const prefix = currentAssistant.current ?? `restored-${seq}`;
          currentAssistant.current = prefix;
          const rebuilt = rowsFromAssistant(
            (event.message ?? {}) as Record<string, unknown>,
            prefix,
          );
          const rebuiltIds = new Set(rebuilt.map((row) => row.id));
          setRows((previous) => {
            // A tool row may already hold the result from `tool_execution_end`;
            // keep that while refreshing the name and arguments it was called with.
            const priorTools = new Map<string, ToolRow>(
              previous
                .filter((row): row is ToolRow => row.kind === "tool")
                .map((row) => [row.id, row]),
            );
            const merged = rebuilt.map((row) => {
              if (row.kind !== "tool") return row;
              const prior = priorTools.get(row.id);
              return prior
                ? { ...prior, name: row.name, args: row.args ?? prior.args }
                : row;
            });
            return [
              ...withoutPrefix(previous, prefix).filter(
                (row) => !(row.kind === "tool" && rebuiltIds.has(row.id)),
              ),
              ...merged,
            ];
          });
          break;
        }
        case "tool_execution_start":
          setRows((previous) =>
            upsertTool(previous, String(event.toolCallId ?? seq), {
              name: String(event.toolName ?? "tool"),
              args: event.args,
              timing: toolTiming(event.webTiming),
              status: "running",
            }),
          );
          break;
        case "tool_execution_update":
          setRows((previous) =>
            upsertTool(previous, String(event.toolCallId ?? seq), {
              name: String(event.toolName ?? "tool"),
              args: event.args,
              result: event.partialResult,
              images: imageBlocksOf(event.partialResult),
              timing: toolTiming(event.webTiming),
              status: "running",
            }),
          );
          break;
        case "tool_execution_end":
          setRows((previous) =>
            upsertTool(previous, String(event.toolCallId ?? seq), {
              name: String(event.toolName ?? "tool"),
              result: event.result,
              images: imageBlocksOf(event.result),
              timing: toolTiming(event.webTiming),
              status: event.isError ? "error" : "done",
            }),
          );
          break;
        default:
          break;
      }
    },
    [showNextDialog],
  );

  const handleEvent = useCallback(
    (raw: MessageEvent) => {
      let event: SeqEvent;
      try {
        event = JSON.parse(raw.data) as SeqEvent;
      } catch {
        return;
      }
      const seq = Number(raw.lastEventId ?? 0);
      // Replay and live fan-out overlap on purpose; sequence numbers dedupe them.
      if (seq && seq <= lastSeq.current) return;
      if (seq) lastSeq.current = seq;
      setSeen((count) => count + 1);
      applyEvent(event, seq);
    },
    [applyEvent],
  );

  const clearAccess = useCallback(() => {
    diagnosticsRevision.current++;
    navigationRevision.current++;
    sessionListRevision.current++;
    sessionListRequest.current = null;
    sessionListValue.current = null;
    activeSessionRef.current = "";
    rowsBySession.current.clear();
    source.current?.close();
    setAuthenticated(false);
    setAuthRequired(true);
    setCapabilities(null);
    setSessionId("");
    setSessions([]);
    setRows([]);
    earlierPage.current = null;
    dialogRequests.current.clear();
    activeDialogId.current = "";
    setDialog(null);
    resolveTextRequest(null);
    setRuntime(null);
    setModels([]);
    setAuthProviders([]);
    setAuthProvider("");
    setApiKeyDraft("");
    credentialRevision.current++;
    setAuthMessage(null);
    setPasswordDraft("");
    setProjectDialog(false);
    setRenameTarget(null);
    setSettingsOpen(false);
    setDraft("");
    clearDrafts();
  }, [clearDrafts]);

  const refreshSessions = useCallback(
    (force = true) => {
      if (!force && sessionListRequest.current)
        return sessionListRequest.current;
      const revision = ++sessionListRevision.current;
      const latest = () =>
        sessionListRequest.current ?? sessionListValue.current;
      const pending = api
        .listSessions()
        .then(
          ({ sessions: list }) => {
            if (revision !== sessionListRevision.current) return latest();
            sessionListValue.current = list;
            setSessions(list);
            return list;
          },
          (cause) => {
            if (revision !== sessionListRevision.current) return latest();
            sessionListValue.current = null;
            if (cause instanceof ApiError && cause.status === 401)
              clearAccess();
            setError(errorText(cause));
            return null;
          },
        )
        .finally(() => {
          if (sessionListRequest.current === pending)
            sessionListRequest.current = null;
        });
      sessionListRequest.current = pending;
      return pending;
    },
    [clearAccess],
  );

  const createSession = useCallback(
    async (selectedProject?: string) => {
      if (creatingSessionRef.current) return;
      creatingSessionRef.current = true;
      const navigation = navigationRevision.current;
      const targetProject = selectedProject || projectRef.current;
      setCreatingSession(true);
      try {
        setError("");
        const created = await api.createSession(targetProject || undefined);
        if (navigation !== navigationRevision.current) {
          void refreshSessions();
          return;
        }
        navigationRevision.current++;
        if (targetProject) {
          projectRef.current = targetProject;
          setProjectId(targetProject);
        }
        activeSessionRef.current = created.sessionId;
        setSessionId(created.sessionId);
        if (window.innerWidth < 768) setSidebar(false);
        const url = new URL(location.href);
        url.searchParams.set("session", created.sessionId);
        history.replaceState(null, "", url);
        void refreshSessions();
      } catch (cause) {
        if (navigation !== navigationRevision.current) return;
        if (!activeSessionRef.current) setStatus("error");
        setError(errorText(cause));
      } finally {
        creatingSessionRef.current = false;
        setCreatingSession(false);
      }
    },
    [refreshSessions],
  );

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    try {
      localStorage.setItem("rpi-theme", theme);
    } catch {
      /* optional preference */
    }
  }, [theme]);
  useEffect(() => {
    const keydown = (event: KeyboardEvent) => {
      if (
        !authChecked ||
        (authRequired && !authenticated) ||
        document.querySelector("dialog[open], .dialog-backdrop")
      )
        return;
      if (!(event.ctrlKey || event.metaKey) || event.altKey) return;
      const key = event.key.toLowerCase();
      if (key === "n" && !event.shiftKey) {
        event.preventDefault();
        void createSession();
      }
      if (key === "b") {
        event.preventDefault();
        setSidebar((value) => !value);
      }
      if (key === ",") {
        event.preventDefault();
        setSettingsOpen(true);
      }
    };
    window.addEventListener("keydown", keydown);
    return () => window.removeEventListener("keydown", keydown);
  }, [createSession, authChecked, authRequired, authenticated]);

  useEffect(() => {
    if (!authChecked || (authRequired && !authenticated)) return;
    const timer = window.setInterval(() => void refreshSessions(false), 5000);
    return () => window.clearInterval(timer);
  }, [authChecked, authRequired, authenticated, refreshSessions]);

  useEffect(() => {
    saveLanguage(language);
    document.documentElement.lang = language === "zh" ? "zh-CN" : "en";
  }, [language]);

  useEffect(() => {
    let cancelled = false;
    setAuthChecked(false);
    setAccessCheckFailed(false);
    setAuthError("");
    void (async () => {
      try {
        const auth = await api.auth();
        if (cancelled) return;
        setAuthRequired(auth.required);
        setAuthenticated(auth.authenticated || !auth.required);
      } catch (cause) {
        if (cancelled) return;
        setAuthRequired(true);
        setAuthenticated(false);
        setAccessCheckFailed(true);
        setAuthError(errorText(cause));
      } finally {
        if (!cancelled) setAuthChecked(true);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [authRevision]);

  useEffect(() => {
    if (!authChecked || (authRequired && !authenticated)) return;
    let cancelled = false;
    let availableProjects: Project[] = [];
    setLoadingSession(true);
    void (async () => {
      try {
        const caps = await api.capabilities();
        if (cancelled) return;
        setCapabilities(caps);
        if (caps.supportedActions?.includes("projects")) {
          const result = await api.projects();
          if (cancelled) return;
          availableProjects = result.projects;
          setProjects(result.projects);
          const defaultProject =
            result.projects.find(
              (item) => item.id === result.defaultProjectId && !item.archived,
            ) ?? result.projects.find((item) => !item.archived);
          projectRef.current = defaultProject?.id ?? result.defaultProjectId;
          setProjectId(projectRef.current);
        }
        const list = await refreshSessions();
        if (cancelled || !list) return;
        const requested = new URLSearchParams(location.search).get("session");
        const reusable = list.filter(
          (session) =>
            session.status !== "closed" &&
            !session.archived &&
            (!session.projectId ||
              availableProjects.some((project) =>
                project.id === session.projectId && !project.archived,
              )),
        );
        const match =
          list.find((session) => session.sessionId === requested) ??
          reusable.sort((a, b) => b.updatedAt - a.updatedAt)[0];
        if (match) select(match.sessionId, match.projectId);
        else await createSession();
      } catch (cause) {
        if (cancelled) return;
        setStatus("error");
        setError(errorText(cause));
        setAccessCheckFailed(true);
        setAuthenticated(false);
        setAuthRequired(true);
        setCapabilities(null);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [
    authChecked,
    authRequired,
    authenticated,
    createSession,
    refreshSessions,
  ]);

  useEffect(() => {
    if (!capabilities) return;
    diagnosticsRevision.current++;
    dialogRequests.current.clear();
    activeDialogId.current = "";
    earlierPage.current = null;
    setRows(sessionId ? (rowsBySession.current.get(sessionId) ?? []) : []);
    setRuntime(null);
    setHistory(null);
    setQueueItems([]);
    setAuthProviders([]);
    setAuthProvider("");
    setApiKeyDraft("");
    setAuthMessage(null);
    setQueueDraft("");
    setQueueEditId(null);
    setModels([]);
    setLeafId("");
    setHistoryHasMore(false);
    setLoadingEarlier(false);
    setMutationPending(false);
    setDialogBusy(false);
    dialogRevision.current++;
    setHistoryLoading(false);
    sendingRevision.current++;
    sendingRef.current = false;
    setSending(false);
    setLevels([]);
    setSeen(0);
    setError("");
    setConnectionFailed(false);
    setStatus("connecting");
    setLoadingSession(Boolean(sessionId));
    setInterrupted(false);
    setDialog(null);
    setDialogAnswer("");
    setFirstId("");
    setHasMore(false);
    lastSeq.current = 0;
    currentAssistant.current = null;
    currentUser.current = null;
    followingRef.current = true;
    setFollowing(true);
    setDraft(draftBySession.current.get(sessionId) ?? "");
    if (!sessionId) {
      setStatus("ready");
      setLoadingSession(false);
      return;
    }

    let cancelled = false;
    let stream: EventSource | null = null;
    let refreshingTranscript = false;
    let bufferedEvents: MessageEvent[] = [];
    const failConnection = (cause: unknown) => {
      if (cancelled) return;
      if (cause instanceof ApiError && cause.status === 401) {
        cancelled = true;
        clearAccess();
        return;
      }
      setStatus("error");
      setLoadingSession(false);
      setConnectionFailed(true);
      setError(errorText(cause));
    };
    const restoreSnapshotStatus = (
      snapshot: Awaited<ReturnType<typeof snapshotOf>>,
    ) => {
      setMessageStats({
        sessionId,
        stats: latestMessageStats(snapshot.messages),
      });
      if (snapshot.status) {
        setStatus(snapshot.status);
        setRunActive(["running", "awaiting_input"].includes(snapshot.status));
      }
      if (
        snapshot.status ||
        snapshot.pendingDialog ||
        snapshot.pendingDialogs
      ) {
        const requests =
          snapshot.pendingDialogs ??
          (snapshot.pendingDialog ? [snapshot.pendingDialog] : []);
        dialogRequests.current = new Map(
          requests.map((request) => [request.requestId, request as UiDialog]),
        );
        showNextDialog();
      }
      if (dialogRequests.current.size) {
        setStatus("awaiting_input");
      }
    };

    // Direct prompts are persisted by the native harness without user message
    // events. Refresh at run boundaries so every open page sees that input.
    const refreshTranscript = async () => {
      if (refreshingTranscript || cancelled) return;
      refreshingTranscript = true;
      try {
        const snapshot = await snapshotOf(sessionId);
        if (cancelled) return;
        restoreSnapshotStatus(snapshot);
        currentAssistant.current = null;
        currentUser.current = null;
        const restored = prependRows(
          earlierPage.current?.rows ?? [],
          rowsFromMessages(snapshot.messages),
        );
        const restoredIds = new Set(restored.map((row) => row.id));
        setRows((previous) => [
          ...restored,
          ...previous.filter(
            (row) =>
              row.kind === "note" &&
              row.id !== "retry-note" &&
              !restoredIds.has(row.id),
          ),
        ]);
        setPersistedHistory(snapshot.history ?? null);
        setLeafId(snapshot.leafId ?? "");
        setInterrupted(Boolean(snapshot.pendingResume));
        setFirstId(earlierPage.current?.firstId ?? snapshot.firstId ?? "");
        setHasMore(earlierPage.current?.hasMore ?? Boolean(snapshot.hasMore));
        for (const entry of eventsAfterSnapshot(
          snapshot.messages,
          snapshot.events,
        ))
          applyEvent(entry.event, entry.seq);
        lastSeq.current = snapshot.lastSeq;
      } catch (cause) {
        if (cancelled) return;
        if (cause instanceof ApiError && cause.status === 401) {
          cancelled = true;
          clearAccess();
          return;
        }
        setError(errorText(cause));
      } finally {
        refreshingTranscript = false;
        const pending = bufferedEvents;
        bufferedEvents = [];
        if (!cancelled) {
          let anotherBoundary = false;
          for (const raw of pending) {
            if (Number(raw.lastEventId) <= lastSeq.current) continue;
            const type = raw.type;
            handleEvent(raw);
            anotherBoundary ||=
              type === "agent_start" || type === "agent_settled";
          }
          if (anotherBoundary) void refreshTranscript();
        }
      }
    };

    const connect = (after: number) => {
      const next = new EventSource(
        `/api/v1/sessions/${sessionId}/events?after=${after}`,
      );
      stream = next;
      source.current = next;
      next.addEventListener("resync_required", () => {
        next.close();
        if (!cancelled) setConnectionRevision((value) => value + 1);
      });
      let priorStatus = "ready";
      next.onopen = () =>
        setStatus((previous) =>
          previous === "reconnecting" ? priorStatus : previous,
        );
      for (const name of EVENT_TYPES)
        next.addEventListener(name, (raw) => {
          const event = raw as MessageEvent;
          if (refreshingTranscript) {
            bufferedEvents.push(event);
            if (bufferedEvents.length > 512) {
              next.close();
              setConnectionRevision((value) => value + 1);
            }
            return;
          }
          if (Number(event.lastEventId) <= lastSeq.current) return;
          handleEvent(event);
          if (name === "agent_start" || name === "agent_settled")
            void refreshTranscript();
        });
      next.onerror = () =>
        setStatus((previous) => {
          if (previous === "closed") return previous;
          if (previous !== "reconnecting") priorStatus = previous;
          return "reconnecting";
        });
    };

    void (async () => {
      // Reopen first because a bridge restart leaves only the registry entry.
      // Once the native host is ready, runtime state and the bounded transcript
      // snapshot are independent reads; keep their network/RPC latency parallel.
      try {
        await api.openSession(sessionId);
        if (cancelled) return;
        void refreshSessions();
      } catch (cause) {
        failConnection(cause);
        return;
      }

      let after = 0;
      try {
        const snapshotPromise = capabilities.supportedCommands.includes(
          "get_messages",
        )
          ? snapshotOf(sessionId)
          : Promise.resolve(null);
        const [state, snapshot] = await Promise.all([
          api.runtimeState(sessionId),
          snapshotPromise,
        ]);
        if (cancelled) return;
        setRuntime(state);
        setRunActive(Boolean(state.activeOperation));
        setStatus(state.activeOperation ? "running" : "ready");

        if (snapshot) {
          // Native truth for history; the events captured while the snapshot was
          // taken are applied in order, so `after` continues exactly where it ends.
          restoreSnapshotStatus(snapshot);
          setRows((previous) => {
            const restored = rowsFromMessages(snapshot.messages);
            const ids = new Set(restored.map((row) => row.id));
            return [
              ...restored,
              ...previous.filter(
                (row) => row.kind === "note" && !ids.has(row.id),
              ),
            ];
          });
          setPersistedHistory(snapshot.history ?? null);
          setLeafId(snapshot.leafId ?? "");
          setInterrupted(Boolean(snapshot.pendingResume));
          setFirstId(snapshot.firstId ?? "");
          setHasMore(Boolean(snapshot.hasMore));
          for (const entry of eventsAfterSnapshot(
            snapshot.messages,
            snapshot.events,
          )) {
            applyEvent(entry.event, entry.seq);
          }
          lastSeq.current = snapshot.lastSeq;
          after = snapshot.lastSeq;
          setSeen(snapshot.events.length);
        }
      } catch (cause) {
        failConnection(cause);
        return;
      }
      if (cancelled) return;
      connect(after);
      setLoadingSession(false);
      // Slash commands resolve names locally, so keep the catalogs warm.
      if (capabilities.supportedCommands.includes("get_available_models")) {
        void api
          .models(sessionId)
          .then((data) => {
            if (!cancelled) setModels(data.models ?? []);
          })
          .catch(() => {});
      }
      if (
        capabilities.supportedCommands.includes("get_available_thinking_levels")
      ) {
        void api
          .thinkingLevels(sessionId)
          .then((data) => {
            if (!cancelled) setLevels(data.levels ?? []);
          })
          .catch(() => {});
      }
    })();

    return () => {
      cancelled = true;
      stream?.close();
      if (source.current === stream) source.current = null;
    };
  }, [
    applyEvent,
    capabilities,
    clearAccess,
    handleEvent,
    sessionId,
    connectionRevision,
    showNextDialog,
  ]);

  useEffect(() => {
    if (rowsSessionRef.current !== sessionId) {
      rowsSessionRef.current = sessionId;
      return;
    }
    if (!sessionId) return;
    rowsBySession.current.set(sessionId, rows);
    if (rowsBySession.current.size > 24) {
      const oldest = rowsBySession.current.keys().next().value;
      if (oldest && oldest !== sessionId) rowsBySession.current.delete(oldest);
    }
  }, [rows, sessionId]);

  useLayoutEffect(() => {
    if (!followingRef.current || !transcriptRef.current) return;
    const node = transcriptRef.current;
    node.scrollTop = node.scrollHeight - node.clientHeight;
  }, [rows, status]);

  const refreshDiagnostics = useCallback(
    async (id: string) => {
      const revision = ++diagnosticsRevision.current;
      const current = () =>
        activeSessionRef.current === id &&
        diagnosticsRevision.current === revision;
      const jobs: Promise<void>[] = [];
      if (supported.has("get_history")) {
        jobs.push(
          api
            .history(id)
            .then((value) => {
              if (!current()) return;
              setHistory(value);
              setHistoryHasMore(value.hasMore);
            })
            .catch(() => {}),
        );
      }
      if (supported.has("get_queue")) {
        jobs.push(
          api
            .queue(id, showQueueHistory)
            .then((value) => {
              if (current()) setQueueItems(value.items ?? []);
            })
            .catch(() => {}),
        );
      }
      if (supported.has("get_stats")) {
        jobs.push(
          api
            .stats(id)
            .then((rawStats) => {
              if (!current()) return;
              const stats = normalizeRuntimeStats(rawStats);
              if (!stats) return;
              setRuntime((previous) =>
                previous ? { ...previous, stats } : { stats },
              );
            })
            .catch(() => {}),
        );
      }
      if (supported.has("get_auth_status")) {
        jobs.push(
          api
            .authStatus(id)
            .then((value) => {
              if (!current()) return;
              setAuthProviders(value.providers ?? []);
              setAuthProvider((previous) =>
                value.providers?.some(
                  (provider) => provider.provider === previous,
                )
                  ? previous
                  : value.providers?.[0]?.provider || "",
              );
            })
            .catch(() => {}),
        );
      }
      await Promise.all(jobs);
    },
    [supported, showQueueHistory],
  );

  useEffect(() => {
    if (!sessionId) return;
    const timer = window.setTimeout(() => {
      void refreshDiagnostics(sessionId);
    }, 450);
    return () => window.clearTimeout(timer);
  }, [refreshDiagnostics, sessionId, settleTick]);

  function persistedSettlementRows(value: HistoryResponse | null): Row[] {
    if (!value) return [];
    const record = [...value.records]
      .reverse()
      .find((entry) => entry.type === "operation_finished");
    if (!record) return [];

    const outcome = String(record.outcome ?? "settled");
    return [
      {
        kind: "note" as const,
        id: "restored-settlement",
        text: `Run ${outcome} · restored from session history`,
        translation: {
          key: "restoredRun" as const,
          values: { outcome },
        },
      },
    ];
  }

  function historyRecordLabel(record: Record<string, unknown>): string {
    const type = String(record.type ?? "record");
    return valueLabel(language, type).replaceAll("_", " ");
  }

  function historyRecordTime(record: Record<string, unknown>): string {
    const value = record.timestamp ?? record.createdAt ?? record.updatedAt;
    if (typeof value !== "number" && typeof value !== "string") return "";
    const date = new Date(value);
    return Number.isNaN(date.getTime()) ? String(value) : date.toLocaleString();
  }

  function redactHistoryValue(value: unknown, key = ""): unknown {
    if (
      /api.?key|access.?token|refresh.?token|secret|password|credential/i.test(
        key,
      )
    ) {
      return "[redacted]";
    }
    if (Array.isArray(value)) {
      return value.map((item) => redactHistoryValue(item));
    }
    if (value && typeof value === "object") {
      return Object.fromEntries(
        Object.entries(value).map(([entryKey, entryValue]) => [
          entryKey,
          redactHistoryValue(entryValue, entryKey),
        ]),
      );
    }
    return value;
  }

  function historyRecordDetails(record: Record<string, unknown>): string {
    return JSON.stringify(redactHistoryValue(record), null, 2);
  }

  async function refreshSessionDiagnostics() {
    if (sessionId) await refreshDiagnostics(sessionId);
  }

  function setPersistedHistory(value: HistoryResponse | null) {
    setHistory(value);
    setHistoryHasMore(Boolean(value?.hasMore));
  }

  useEffect(() => {
    if (!settleTick) return;
    void refreshSessions();
    // Context usage only moves on a provider call, i.e. on settlement.
    if (!sessionId) return;
    let cancelled = false;
    void api
      .runtimeState(sessionId)
      .then((state) => {
        if (!cancelled) setRuntime(state);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [refreshSessions, sessionId, settleTick]);

  function note(text: string) {
    noteSeq.current += 1;
    const id = `cmd-${noteSeq.current}`;
    setRows((previous) => [...previous, { kind: "note", id, text }]);
  }

  async function selectModel(entry: ModelDescriptor) {
    if (!sessionId || modelPending) return;
    setModelPending(true);
    setError("");
    try {
      await commandData(sessionId, "set_model", {
        modelId: entry.id,
        provider: entry.provider,
      });
      const state = await api.runtimeState(sessionId);
      setRuntime(state);
      note(
        language === "zh"
          ? `模型 → ${entry.name} (${entry.provider})`
          : `Model → ${entry.name} (${entry.provider})`,
      );
    } catch (cause) {
      setError(errorText(cause));
    } finally {
      setModelPending(false);
    }
  }

  async function selectThinkingLevel(level: string) {
    if (!sessionId || thinkingPending) return;
    setThinkingPending(true);
    setError("");
    try {
      await commandData(sessionId, "set_thinking_level", { level });
      const state = await api.runtimeState(sessionId);
      setRuntime(state);
      note(
        language === "zh"
          ? `思考深度 → ${valueLabel(language, level)}`
          : `Thinking level → ${level}`,
      );
    } catch (cause) {
      setError(errorText(cause));
    } finally {
      setThinkingPending(false);
    }
  }

  /**
   * Slash commands are a browser-side affordance over the same RPC commands the
   * Runtime panel uses; nothing here reaches the model as a prompt.
   */
  async function runSlash(raw: string) {
    if (!sessionId) return;
    const targetSession = sessionId;
    const navigation = navigationRevision.current;
    const cursor = lastSeq.current;
    const priorStatus = status;
    const priorActive = runActive;
    const [name = "", ...rest] = raw.trim().split(/\s+/);
    const arg = rest.join(" ").trim();
    updateDraft("");
    setError("");
    try {
      if (name === "/help") {
        note(
          slashCommands
            .filter((command) => command.enabled)
            .map((command) => `${command.usage} — ${command.hint}`)
            .join("\n"),
        );
        return;
      }
      if (name === "/new") {
        await createSession();
        return;
      }
      if (name === "/compact") {
        if (!supported.has("compact")) {
          note("compact is not advertised by this host.");
          return;
        }
        setStatus("running");
        setRunActive(true);
        await commandData(
          sessionId,
          "compact",
          arg ? { customInstructions: arg } : {},
        );
        return;
      }
      if (name === "/continue") {
        await resumePending();
        return;
      }
      if (name === "/abort") {
        if (!supported.has("abort")) {
          note("/abort is not advertised by this host.");
          return;
        }
        if (!busy) {
          note("Nothing is running.");
          return;
        }
        await abort();
        return;
      }
      if (name === "/tools") {
        setPanel(true);
        note(
          `${runtime?.activeTools?.length ?? 0} active tools — toggle them in Runtime.`,
        );
        return;
      }
      if (name === "/model") {
        if (!supported.has("set_model")) {
          note("set_model is not advertised by this host.");
          return;
        }
        const available = models.length
          ? models
          : ((await api.models(sessionId)).models ?? []);
        if (navigation !== navigationRevision.current) return;
        setModels(available);
        const target = matchModel(available, arg);
        if (!target) {
          setPanel(true);
          note(
            arg
              ? language === "zh"
                ? `无法唯一确定模型“${arg}”，请使用 服务商/模型ID 或在运行时设置中选择。`
                : `No unique model matches "${arg}". Use provider/model-id or select it in Runtime.`
              : `Runtime lists ${available.length} models.`,
          );
          return;
        }
        await commandData(sessionId, "set_model", {
          modelId: target.id,
          provider: target.provider,
        });
        const state = await api.runtimeState(sessionId);
        if (navigation !== navigationRevision.current) return;
        setRuntime(state);
        note(`Model → ${target.name} (${target.provider})`);
        return;
      }
      if (name === "/thinking") {
        if (!supported.has("set_thinking_level")) {
          note("set_thinking_level is not advertised by this host.");
          return;
        }
        const level = levels.find(
          (entry) => entry.toLowerCase() === arg.toLowerCase(),
        );
        if (!level) {
          setPanel(true);
          note(
            arg
              ? `No thinking level "${arg}". Available: ${levels.join(", ")}.`
              : `Available levels: ${levels.join(", ")}.`,
          );
          return;
        }
        await commandData(sessionId, "set_thinking_level", { level });
        const state = await api.runtimeState(sessionId);
        if (navigation !== navigationRevision.current) return;
        setRuntime(state);
        note(`Thinking level → ${level}`);
        return;
      }
      note(`Unknown command ${name}. Try /help.`);
    } catch (cause) {
      if (!draftBySession.current.get(targetSession))
        updateDraft(raw, targetSession);
      if (navigation !== navigationRevision.current) return;
      if (name === "/compact" && cursor === lastSeq.current) {
        setStatus((current) =>
          current === "reconnecting" ? current : priorStatus,
        );
        setRunActive(priorActive);
      }
      const text = errorText(cause);
      setError(text);
      note(`${name} failed: ${text}`);
    }
  }

  async function uploadFiles(files: FileList | null) {
    if (!files || !projectId || !sessionId) return;
    setUploading(true);
    setError("");
    try {
      const uploaded: Attachment[] = [];
      for (const file of Array.from(files)) {
        const result = await api.upload(projectId, file);
        uploaded.push({
          ...result,
          previewUrl: file.type.startsWith("image/")
            ? URL.createObjectURL(file)
            : undefined,
        });
      }
      setAttachments((current) => [...current, ...uploaded]);
    } catch (cause) {
      setError(`${t("uploadFailed")}: ${errorText(cause)}`);
    } finally {
      setUploading(false);
      if (fileInputRef.current) fileInputRef.current.value = "";
    }
  }

  function removeAttachment(path: string) {
    setAttachments((current) => {
      const removed = current.find((file) => file.path === path);
      if (removed?.previewUrl) URL.revokeObjectURL(removed.previewUrl);
      return current.filter((file) => file.path !== path);
    });
  }

  async function submit(event?: FormEvent) {
    event?.preventDefault();
    const message = draft.trim();
    if (
      (!message && attachments.length === 0) ||
      !sessionId ||
      sendingRef.current
    )
      return;
    if (message.startsWith("/") && attachments.length === 0) {
      void runSlash(message);
      return;
    }
    if (!supported.has("prompt")) return;
    const attachedText = attachments.length
      ? `\n\n${t("attachedFiles")}:\n${attachments.map((file) => `- ${file.path}`).join("\n")}`
      : "";
    const submissionMessage = `${message}${attachedText}`.trim();
    const targetSession = sessionId;
    const navigation = navigationRevision.current;
    const submission = ++sendingRevision.current;
    const wasBusy = busy;
    const editingQueueId = queueEditId;
    const shouldQueue = wasBusy || Boolean(editingQueueId);
    const clearAttachments = () => {
      attachments.forEach((file) => {
        if (file.previewUrl) URL.revokeObjectURL(file.previewUrl);
      });
      setAttachments([]);
    };
    sendingRef.current = true;
    setSending(true);
    updateDraft("");
    setError("");
    setInterrupted(false);
    const key = `local-${++localMessageSeq.current}`;
    if (!shouldQueue)
      setRows((previous) => [
        ...previous,
        { kind: "text", id: key, role: "user", text: submissionMessage },
      ]);
    if (!shouldQueue) {
      setStatus("running");
      setRunActive(true);
    }
    try {
      if (shouldQueue) {
        if (!supported.has("queue_message")) throw new Error(t("runBusy"));
        if (editingQueueId)
          await api.cancelQueued(targetSession, editingQueueId);
        const queued = await api.queueMessage(
          targetSession,
          submissionMessage,
          queueKind,
        );
        if (navigation === navigationRevision.current) {
          setQueueItems((previous) => [
            ...previous.filter((item) => item.entryId !== editingQueueId),
            {
              entryId: queued.entryId,
              queue: queued.queue || queueKind,
              text: submissionMessage,
              status: "queued",
            },
          ]);
          setQueueEditId(null);
          void refreshDiagnostics(targetSession);
        }
        clearAttachments();
      } else {
        await commandData(targetSession, "prompt", {
          message: submissionMessage,
        });
        clearAttachments();
      }
    } catch (cause) {
      if (!draftBySession.current.get(targetSession))
        updateDraft(message, targetSession);
      if (navigation === navigationRevision.current) {
        if (!wasBusy && !editingQueueId) {
          setStatus((current) =>
            current === "reconnecting" ? current : "ready",
          );
          setRunActive(false);
        }
        setRows((previous) => previous.filter((row) => row.id !== key));
        setError(errorText(cause));
      }
    } finally {
      if (submission === sendingRevision.current) {
        sendingRef.current = false;
        setSending(false);
      }
    }
  }

  async function resumePending() {
    if (!sessionId || !supported.has("resume_pending")) return;
    const navigation = navigationRevision.current;
    const cursor = lastSeq.current;
    const priorStatus = status;
    const priorActive = runActive;
    const priorInterrupted = interrupted;
    setInterrupted(false);
    setError("");
    setStatus("running");
    setRunActive(true);
    try {
      const data = await commandData<{ resumed?: boolean }>(
        sessionId,
        "resume_pending",
      );
      if (navigation !== navigationRevision.current) return;
      if (data.resumed === false) {
        if (cursor === lastSeq.current) {
          setStatus((current) =>
            current === "reconnecting" ? current : priorStatus,
          );
          setRunActive(priorActive);
        }
        note("Nothing left to resume on this branch.");
      }
    } catch (cause) {
      if (navigation !== navigationRevision.current) return;
      if (cursor === lastSeq.current) {
        setStatus((current) =>
          current === "reconnecting" ? current : priorStatus,
        );
        setRunActive(priorActive);
        setInterrupted(priorInterrupted);
      }
      setError(errorText(cause));
    }
  }

  /** Page backwards through the branch, prepending entries older than the
   * current first row. The bridge forwards `get_messages` unchanged, so this is
   * the same native projection the first snapshot used. */
  async function loadEarlier() {
    if (
      !sessionId ||
      !firstId ||
      loadingEarlier ||
      !supported.has("get_messages")
    )
      return;
    const navigation = navigationRevision.current;
    setLoadingEarlier(true);
    try {
      const page = await api.messages(sessionId, {
        beforeId: firstId,
        limit: 400,
      });
      if (navigation !== navigationRevision.current) return;
      const older = rowsFromMessages(page.messages);
      earlierPage.current = {
        rows: prependRows(older, earlierPage.current?.rows ?? []),
        firstId: page.firstId ?? "",
        hasMore: page.hasMore,
      };
      setRows((previous) => prependRows(older, previous));
      setFirstId(page.firstId ?? "");
      setHasMore(page.hasMore);
    } catch (cause) {
      if (navigation !== navigationRevision.current) return;
      setError(errorText(cause));
    } finally {
      if (navigation === navigationRevision.current) setLoadingEarlier(false);
    }
  }

  async function refreshOAuth(provider: string) {
    const navigation = navigationRevision.current;
    try {
      await api.refreshAuth(provider);
      if (navigation !== navigationRevision.current) return;
      setAuthMessage({ key: "oauthRefreshed", values: { provider } });
      await refreshSessionDiagnostics();
    } catch (cause) {
      if (navigation !== navigationRevision.current) return;
      setError(errorText(cause));
    }
  }

  async function startOAuth(provider: string) {
    if (!sessionId || !supported.has("oauth")) return;
    const navigation = navigationRevision.current;
    try {
      let result = await api.oauth(sessionId, provider, "begin");
      if (navigation !== navigationRevision.current) return;
      if (result.status === "interaction") {
        const display = result.display ?? {};
        const url = typeof display.url === "string" ? display.url : "";
        if (url) window.open(url, "rpi-oauth", "noopener,noreferrer");
        const instructions =
          typeof display.instructions === "string"
            ? display.instructions
            : Array.isArray(display.instructions)
              ? display.instructions.join("\n")
              : t("oauthInstructions");
        const value = await requestText({
          kind: "prompt",
          title: t("oauthSignIn"),
          message: `${instructions}${url ? `\n\n${url}` : ""}`,
        });
        if (!value?.trim()) return;
        result = await api.oauth(sessionId, provider, "exchange", {
          value: value.trim(),
        });
      }
      if (navigation !== navigationRevision.current) return;
      setAuthMessage({
        key: result.status === "authenticated" ? "oauthSaved" : "oauthStatus",
        values: { provider, status: result.status },
      });
      await refreshSessionDiagnostics();
    } catch (cause) {
      if (navigation !== navigationRevision.current) return;
      setError(errorText(cause));
    }
  }

  async function saveApiKey() {
    if (!sessionId || !authProvider || !apiKeyDraft.trim()) return;
    const navigation = navigationRevision.current;
    const credential = credentialRevision.current;
    try {
      const result = await api.setApiKey(
        sessionId,
        authProvider,
        apiKeyDraft.trim(),
      );
      if (
        navigation !== navigationRevision.current ||
        credential !== credentialRevision.current
      )
        return;
      setApiKeyDraft("");
      setAuthMessage({
        key: "keySaved",
        values: { provider: result.provider },
      });
      await refreshSessionDiagnostics();
    } catch (cause) {
      if (
        navigation !== navigationRevision.current ||
        credential !== credentialRevision.current
      )
        return;
      setError(errorText(cause));
    }
  }

  async function clearQueue() {
    if (!sessionId || !supported.has("clear_queue")) return;
    const navigation = navigationRevision.current;
    try {
      await api.clearQueue(sessionId);
      if (navigation !== navigationRevision.current) return;
      await refreshSessionDiagnostics();
    } catch (cause) {
      if (navigation !== navigationRevision.current) return;
      setError(errorText(cause));
    }
  }

  async function loadOlderHistory() {
    const firstSeq = historyData?.firstSeq;
    if (!sessionId || firstSeq == null || historyLoading || !historyHasMore)
      return;
    const navigation = navigationRevision.current;
    setHistoryLoading(true);
    try {
      const page = await api.history(sessionId, {
        beforeSeq: firstSeq,
        limit: 100,
      });
      if (navigation !== navigationRevision.current) return;
      setHistory((current) => {
        if (!current) return page;
        const existing = new Set(
          current.records.map((record) => String(record.id)),
        );
        return {
          ...page,
          total: current.total,
          records: page.records
            .filter((record) => !existing.has(String(record.id)))
            .concat(current.records),
          hasMore: page.hasMore,
        };
      });
      setHistoryHasMore(page.hasMore);
    } catch (cause) {
      if (navigation !== navigationRevision.current) return;
      setError(errorText(cause));
    } finally {
      if (navigation === navigationRevision.current) setHistoryLoading(false);
    }
  }

  async function sendQueuedNow(entryId: string) {
    const targetSession = sessionId;
    const item = pendingQueueItems.find((queued) => queued.entryId === entryId);
    if (
      !targetSession ||
      busy ||
      queueRunning ||
      !item?.text?.trim() ||
      !supported.has("prompt") ||
      !supported.has("cancel_queued") ||
      queueRunSubmission.current.has(targetSession)
    )
      return;
    const navigation = navigationRevision.current;
    const priorStatus = status;
    const priorActive = runActive;
    queueRunSubmission.current.add(targetSession);
    setQueueRunning(true);
    setStatus("starting");
    setRunActive(true);
    setInterrupted(false);
    setError("");
    try {
      // The host owns queue delivery. Send only the selected item now and
      // leave all other queue entries in their existing durable order.
      await api.cancelQueued(targetSession, item.entryId);
      if (navigation !== navigationRevision.current) return;
      setQueueItems((current) =>
        current.filter((queued) => queued.entryId !== item.entryId),
      );
      await commandData(targetSession, "prompt", { message: item.text.trim() });
      if (navigation === navigationRevision.current) {
        setQueueOpen(true);
        await refreshDiagnostics(targetSession);
      }
    } catch (cause) {
      if (navigation !== navigationRevision.current) return;
      setStatus((current) =>
        current === "reconnecting" ? current : priorStatus,
      );
      setRunActive(priorActive);
      setError(errorText(cause));
      await refreshDiagnostics(targetSession);
    } finally {
      queueRunSubmission.current.delete(targetSession);
      if (navigation === navigationRevision.current) setQueueRunning(false);
    }
  }

  async function queuePrompt() {
    if (!sessionId || !queueDraft.trim() || !supported.has("queue_message"))
      return;
    if (queueSubmission.current.has(sessionId)) return;
    const message = queueDraft.trim();
    const draftRevision = queueDraftRevision.current;
    const editingId = queueEditId;
    queueSubmission.current.add(sessionId);
    const navigation = navigationRevision.current;
    try {
      if (editingId) await api.cancelQueued(sessionId, editingId);
      const queued = await api.queueMessage(sessionId, message, queueKind);
      if (navigation !== navigationRevision.current) return;
      setQueueItems((previous) => [
        ...previous.filter((item) => item.entryId !== editingId),
        {
          entryId: queued.entryId,
          queue: queued.queue || queueKind,
          text: message,
          status: "queued",
        },
      ]);
      if (queueDraftRevision.current === draftRevision) setQueueDraft("");
      setQueueEditId(null);
      await refreshSessionDiagnostics();
    } catch (cause) {
      if (navigation !== navigationRevision.current) return;
      setError(errorText(cause));
    } finally {
      queueSubmission.current.delete(sessionId);
    }
  }

  useEffect(() => {
    if (queueEditId) queueInputRef.current?.focus();
  }, [queueEditId]);

  function isEditableQueueItem(item: QueueItem) {
    return (
      !item.status || item.status === "queued" || item.status === "pending"
    );
  }

  async function editQueued(item: QueueItem) {
    if (!isEditableQueueItem(item) || !sessionId) return;
    const navigation = navigationRevision.current;
    const previous = queueItems;
    const previousDraft = draft;
    queueDraftRevision.current++;
    setQueueEditId(null);
    setQueueDraft("");
    setQueueItems((items) => items.filter((queued) => queued.entryId !== item.entryId));
    updateDraft(item.text);
    setQueueKind(item.queue);
    setQueueOpen(false);
    setInspector(false);
    requestAnimationFrame(() => composerInputRef.current?.focus());
    try {
      await api.cancelQueued(sessionId, item.entryId);
      if (navigation === navigationRevision.current) await refreshSessionDiagnostics();
    } catch (cause) {
      if (navigation !== navigationRevision.current) return;
      setQueueItems(previous);
      setQueueOpen(true);
      updateDraft(previousDraft);
      setError(errorText(cause));
    }
  }

  function cancelQueueEdit() {
    queueDraftRevision.current++;
    setQueueEditId(null);
    setQueueDraft("");
    updateDraft("");
  }

  async function cancelQueued(entryId: string) {
    const navigation = navigationRevision.current;
    const previous = queueItems;
    if (queueEditId === entryId) cancelQueueEdit();
    setQueueItems((items) => items.filter((item) => item.entryId !== entryId));
    try {
      await api.cancelQueued(sessionId, entryId);
      if (navigation !== navigationRevision.current) return;
      await refreshSessionDiagnostics();
    } catch (cause) {
      if (navigation !== navigationRevision.current) return;
      setQueueItems(previous);
      setError(errorText(cause));
    }
  }

  async function forkCurrent(entryId?: string) {
    if (!sessionId || !actions.has("fork")) return;
    const navigation = navigationRevision.current;
    const defaultTitle = t("forkOf", {
      title: current?.title ?? sessionId.slice(0, 8),
    });
    const title = await requestText({
      kind: "prompt",
      title: t("forkName"),
      defaultValue: defaultTitle,
    });
    if (title === null) return;
    try {
      const created = await api.fork(sessionId, entryId, title);
      await refreshSessions();
      if (navigation !== navigationRevision.current) return;
      select(created.sessionId);
    } catch (cause) {
      if (navigation !== navigationRevision.current) return;
      setError(errorText(cause));
    }
  }

  async function cloneCurrent() {
    if (!sessionId || !actions.has("clone")) return;
    const navigation = navigationRevision.current;
    const title = await requestText({
      kind: "prompt",
      title: t("cloneName"),
      defaultValue: t("cloneOf", {
        title: current?.title ?? sessionId.slice(0, 8),
      }),
    });
    if (title === null) return;
    try {
      const created = await api.clone(sessionId, title);
      await refreshSessions();
      if (navigation !== navigationRevision.current) return;
      select(created.sessionId);
    } catch (cause) {
      if (navigation !== navigationRevision.current) return;
      setError(errorText(cause));
    }
  }

  async function rewindTo(entryId: string) {
    if (!sessionId || !actions.has("history_mutation") || mutationPending)
      return;
    const confirmed = await requestText({
      kind: "confirm",
      title: t("rewind"),
      message: t("confirmRewind"),
    });
    if (confirmed === null) return;
    const navigation = navigationRevision.current;
    setMutationPending(true);
    try {
      const result = await api.navigate(sessionId, entryId);
      if (navigation !== navigationRevision.current) return;
      if (result.outcome === "failed") {
        const error = result.error;
        throw new Error(
          typeof error === "string"
            ? error
            : typeof error === "object" && error
              ? String(
                  (error as { message?: unknown }).message ??
                    "navigation failed",
                )
              : "navigation failed",
        );
      }
      setMutationPending(false);
      navigationRevision.current++;
      diagnosticsRevision.current++;
      setConnectionRevision((value) => value + 1);
    } catch (cause) {
      if (navigation !== navigationRevision.current) return;
      setError(errorText(cause));
    } finally {
      if (navigation === navigationRevision.current) setMutationPending(false);
    }
  }

  async function answerDialog(answer: unknown, cancelled = false) {
    if (!sessionId || !dialog || dialogBusy) return;
    const navigation = navigationRevision.current;
    const request = dialogRevision.current;
    setDialogBusy(true);
    try {
      await commandData(sessionId, "ui_dialog_response", {
        requestId: dialog.requestId,
        ...(cancelled ? { cancelled: true } : { answer }),
      });
      if (navigation !== navigationRevision.current) return;
      // ui_dialog_closed is authoritative; a later dialog can arrive before
      // this command acknowledgement and must remain visible.
    } catch (cause) {
      if (
        navigation !== navigationRevision.current ||
        request !== dialogRevision.current
      )
        return;
      setError(errorText(cause));
    } finally {
      if (
        navigation === navigationRevision.current &&
        request === dialogRevision.current
      )
        setDialogBusy(false);
    }
  }

  async function abort() {
    if (!sessionId || !supported.has("abort")) return;
    const navigation = navigationRevision.current;
    try {
      await commandData(sessionId, "abort");
    } catch (cause) {
      if (navigation !== navigationRevision.current) return;
      setError(errorText(cause));
    }
  }

  async function submitLogin(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!passwordDraft.trim() || authBusy) return;
    setAuthBusy(true);
    setAuthError("");
    try {
      const result = await api.login(passwordDraft);
      setPasswordDraft("");
      setAuthenticated(result.authenticated);
    } catch (cause) {
      setAuthError(errorText(cause));
    } finally {
      setAuthBusy(false);
    }
  }

  async function logout() {
    try {
      await api.logout();
      clearAccess();
    } catch (cause) {
      setError(errorText(cause));
    }
  }

  function select(id: string, selectedProjectId?: string) {
    const nextProject =
      selectedProjectId ??
      sessions.find((entry) => entry.sessionId === id)?.projectId;
    if (
      activeSessionRef.current !== id ||
      (nextProject && projectRef.current !== nextProject)
    ) {
      navigationRevision.current++;
      diagnosticsRevision.current++;
    }
    activeSessionRef.current = id;
    if (nextProject) {
      projectRef.current = nextProject;
      setProjectId(nextProject);
    }
    setLoadingSession(Boolean(id));
    rowsSessionRef.current = id;
    setRows(id ? (rowsBySession.current.get(id) ?? []) : []);
    followingRef.current = true;
    setFollowing(true);
    setSessionId(id);
    setPanel(false);
    if (window.innerWidth < 768) setSidebar(false);
    const url = new URL(location.href);
    url.searchParams.set("session", id);
    history.replaceState(null, "", url);
  }

  function selectProject(selected: Project) {
    if (projectRef.current === selected.id && activeSessionRef.current) return;
    if (activeSessionRef.current || projectRef.current !== selected.id) {
      navigationRevision.current++;
      diagnosticsRevision.current++;
    }
    projectRef.current = selected.id;
    setProjectId(selected.id);
    activeSessionRef.current = "";
    setSessionId("");
    const url = new URL(location.href);
    url.searchParams.delete("session");
    history.replaceState(null, "", url);
  }

  async function updateSessionMetadata(
    session: SessionSummary,
    patch: { pinned?: boolean; archived?: boolean; tags?: string[] },
  ) {
    const id = session.sessionId;
    const navigation = navigationRevision.current;
    const revision = (metadataRevisions.current.get(id) ?? 0) + 1;
    metadataRevisions.current.set(id, revision);
    const task = (metadataQueues.current.get(id) ?? Promise.resolve()).then(
      async () => {
        try {
          await api.sessionMetadata(session.sessionId, patch);
          if (metadataRevisions.current.get(id) !== revision) return;
          const list = await refreshSessions();
          if (!list || metadataRevisions.current.get(id) !== revision) return;
          if (
            navigation === navigationRevision.current &&
            patch.archived &&
            list.find((entry) => entry.sessionId === session.sessionId)
              ?.archived &&
            session.sessionId === activeSessionRef.current
          ) {
            const next = list.find(
              (entry) =>
                entry.projectId === session.projectId && !entry.archived,
            );
            if (next) select(next.sessionId, next.projectId);
            else {
              activeSessionRef.current = "";
              setSessionId("");
              const url = new URL(location.href);
              url.searchParams.delete("session");
              history.replaceState(null, "", url);
            }
          }
        } catch (cause) {
          if (
            metadataRevisions.current.get(id) === revision &&
            navigation === navigationRevision.current
          )
            setError(errorText(cause));
        }
      },
    );
    metadataQueues.current.set(id, task);
    await task;
    if (metadataQueues.current.get(id) === task) {
      metadataQueues.current.delete(id);
      metadataRevisions.current.delete(id);
    }
  }

  async function updateProjectMetadata(project: Project, archived: boolean) {
    try {
      const result = await api.projectMetadata(project.id, { archived });
      setProjects((previous) =>
        previous.map((item) =>
          item.id === result.project.id ? result.project : item,
        ),
      );
      if (archived && projectRef.current === project.id) {
        const next = projects.find(
          (item) => item.id !== project.id && !item.archived,
        );
        if (next) selectProject(next);
        else {
          projectRef.current = "";
          setProjectId("");
          activeSessionRef.current = "";
          setSessionId("");
          const url = new URL(location.href);
          url.searchParams.delete("session");
          history.replaceState(null, "", url);
        }
      }
    } catch (cause) {
      setError(errorText(cause));
    }
  }

  if (!authChecked || (authRequired && !authenticated)) {
    return (
      <LoginScreen
        checking={!authChecked}
        accessError={accessCheckFailed}
        onRetry={() => setAuthRevision((value) => value + 1)}
        language={language}
        theme={theme}
        onLanguage={setLanguage}
        onTheme={setTheme}
        password={passwordDraft}
        onPassword={(value) => {
          setPasswordDraft(value);
          setAuthError("");
        }}
        busy={authBusy}
        error={authError}
        onSubmit={submitLogin}
      />
    );
  }

  return (
    <div
      className="workspace"
      style={
        {
          "--sidebar-width": `${sidebarWidth}px`,
          "--workbench-width": `${workbenchWidth}px`,
        } as CSSProperties
      }
    >
      {dialog && (
        <DialogOverlay
          key={dialog.requestId}
          dialog={dialog}
          answer={dialogAnswer}
          busy={dialogBusy}
          language={language}
          onAnswer={setDialogAnswer}
          onSubmit={(value) => void answerDialog(value)}
          onCancel={() => void answerDialog(null, true)}
        />
      )}
      {textRequest?.kind === "prompt" && (
        <PromptDialog
          language={language}
          title={textRequest.title}
          message={textRequest.message}
          defaultValue={textRequest.defaultValue}
          onClose={() => resolveTextRequest(null)}
          onSubmit={(value) => resolveTextRequest(value)}
        />
      )}
      {textRequest?.kind === "confirm" && (
        <ConfirmDialog
          language={language}
          title={textRequest.title}
          message={textRequest.message}
          onClose={() => resolveTextRequest(null)}
          onConfirm={() => resolveTextRequest("confirmed")}
        />
      )}
      {projectDialog && (
        <ProjectDialog
          language={language}
          root={projectsRoot}
          onClose={() => setProjectDialog(false)}
          onCreated={async (selected) => {
            const navigation = navigationRevision.current;
            const result = await api.projects();
            if (navigation !== navigationRevision.current) return;
            setProjects(result.projects);
            setProjectsRoot(result.projectsRoot ?? "");
            await createSession(selected.id);
          }}
        />
      )}
      {projectRenameTarget && (
        <ProjectRenameDialog
          language={language}
          project={projectRenameTarget}
          onClose={() => setProjectRenameTarget(null)}
          onSave={async (name) => {
            const result = await api.renameProject(
              projectRenameTarget.id,
              name,
            );
            setProjects((previous) =>
              previous.map((project) =>
                project.id === result.project.id ? result.project : project,
              ),
            );
          }}
        />
      )}
      {renameTarget && (
        <RenameDialog
          language={language}
          session={renameTarget}
          onClose={() => setRenameTarget(null)}
          onSave={async (name) => {
            await api.rename(renameTarget.sessionId, name);
            await refreshSessions();
          }}
        />
      )}
      {tagsTarget && (
        <TagsDialog
          language={language}
          session={tagsTarget}
          onClose={() => setTagsTarget(null)}
          onSave={async (tags) => {
            await api.sessionMetadata(tagsTarget.sessionId, { tags });
            await refreshSessions();
          }}
        />
      )}
      {settingsOpen && (
        <Modal
          language={language}
          title={language === "zh" ? "设置" : "Settings"}
          onClose={() => setSettingsOpen(false)}
        >
          <div className="project-form workspace-preferences">
            <label>{language === "zh" ? "外观" : "Appearance"}</label>
            <div className="project-mode">
              <button
                aria-pressed={theme === "light"}
                onClick={() => setTheme("light")}
              >
                <Sun size={16} />
                {language === "zh" ? "浅色" : "Light"}
              </button>
              <button
                aria-pressed={theme === "dark"}
                onClick={() => setTheme("dark")}
              >
                <Moon size={16} />
                {language === "zh" ? "深色" : "Dark"}
              </button>
            </div>
            <label htmlFor="settings-language">{t("language")}</label>
            <MenuSelect
              label={t("language")}
              value={language}
              onChange={(value) => setLanguage(value as Language)}
              options={[
                { value: "zh", label: "简体中文" },
                { value: "en", label: "English" },
              ]}
            />
            <div className="shortcut-list">
              <span>{t("newSession")}</span>
              <kbd>Ctrl / ⌘ N</kbd>
              <span>{language === "zh" ? "搜索会话" : "Search chats"}</span>
              <kbd>Ctrl / ⌘ K</kbd>
              <span>{t("closeSidebar")}</span>
              <kbd>Ctrl / ⌘ B</kbd>
            </div>
          </div>
        </Modal>
      )}
      {sidebar && (
        <>
          <button
            className="sidebar-backdrop mobile-only"
            aria-label={t("dismissSidebar")}
            onClick={() => setSidebar(false)}
          />
          <aside className="sidebar open">
            <div className="sidebar-brand">
              <Mark /> <span>rpi</span>{" "}
              <span className="brand-caption">{t("workspace")}</span>
              <button
                className="icon-button mobile-only"
                aria-label={t("closeSidebar")}
                onClick={() => setSidebar(false)}
              >
                <X size={16} />
              </button>
            </div>
            <ProjectSidebar
              projects={projects}
              sessions={sidebarSessions}
              draftSessionIds={draftSessionIds}
              activeProjectId={projectId}
              sessionId={sessionId}
              language={language}
              creating={creatingSession}
              canManage={actions.has("session_organization")}
              canCreate={actions.has("create_project")}
              onSelectProject={selectProject}
              onSelectSession={(session) =>
                select(session.sessionId, session.projectId)
              }
              onNewSession={(id) => void createSession(id)}
              onNewProject={() => setProjectDialog(true)}
              onRename={setRenameTarget}
              onRenameProject={setProjectRenameTarget}
              onProjectMetadata={updateProjectMetadata}
              onEditTags={setTagsTarget}
              onMetadata={updateSessionMetadata}
            />
            <div className="sidebar-bottom">
              <div className="utility-nav">
                <button onClick={() => setSettingsOpen(true)}>
                  <Settings2 size={14} />
                  {language === "zh" ? "设置" : "Settings"}
                  <kbd>Ctrl ,</kbd>
                </button>
                <button
                  disabled={!sessionId}
                  className={panel ? "active" : ""}
                  onClick={() => {
                    setPanel(true);
                    setInspector(false);
                  }}
                >
                  <Settings2 size={14} /> {t("runtimeSettings")}
                </button>
              </div>
              <div className="account-panel">
                <span className="user-avatar">U</span>
                <span className="account-name">{t("workspaceUser")}</span>
                <button
                  className="icon-button"
                  aria-label={
                    language === "zh" ? "切换外观" : "Toggle appearance"
                  }
                  onClick={() => setTheme(theme === "light" ? "dark" : "light")}
                  title={t(
                    theme === "light" ? "switchToDark" : "switchToLight",
                  )}
                >
                  {theme === "light" ? <Moon size={14} /> : <Sun size={14} />}
                </button>
                {authRequired && (
                  <button
                    type="button"
                    className="icon-button account-button"
                    onClick={() => void logout()}
                    title={t("signOut")}
                    aria-label={t("signOut")}
                  >
                    <LogOut size={15} />
                  </button>
                )}
              </div>
              <div className="local-label">
                <span
                  className="workspace-security"
                  title={t("loopback")}
                  aria-label={t("loopback")}
                >
                  <ShieldCheck size={14} />
                </span>
                <MenuSelect
                  className="language-select"
                  label={t("language")}
                  value={language}
                  onChange={(value) => setLanguage(value as Language)}
                  options={[
                    { value: "en", label: t("english") },
                    { value: "zh", label: t("chinese") },
                  ]}
                />
                <button
                  className="icon-button"
                  aria-label={t("refreshSessions")}
                  title={t("refreshSessions")}
                  onClick={() => void refreshSessions()}
                >
                  <RefreshCw size={13} />
                </button>
              </div>
            </div>
          </aside>
          <Splitter
            label={t("resizeSidebar")}
            value={sidebarWidth}
            min={200}
            max={360}
            onChange={setSidebarWidth}
          />
        </>
      )}

      <section className="main-shell">
        <header className="toolbar">
          <div className="toolbar-left">
            <button
              className="icon-button"
              aria-label={t("toggleSidebar")}
              onClick={() => setSidebar(!sidebar)}
            >
              <PanelLeft size={17} />
            </button>
            <div
              className="breadcrumb"
              aria-label={`${project?.name ?? "rpi"} · ${current?.title ?? t("newSession")}`}
            >
              <span
                className="breadcrumb-project"
                title={project?.name ?? "rpi"}
              >
                {project?.name ?? "rpi"}
              </span>
              <span className="breadcrumb-separator" aria-hidden="true">
                ·
              </span>
              <h1 title={current?.title ?? t("newSession")}>
                {current?.title ?? t("newSession")}
              </h1>
            </div>
            <button
              type="button"
              className="mobile-session-switcher"
              aria-label={t("switchSession")}
              aria-expanded={mobileSessionPicker}
              onClick={() => {
                setMobileSessionQuery("");
                setMobileSessionPicker(true);
              }}
            >
              <span>
                <small>{project?.name ?? "rpi"}</small>
                <strong>{current?.title ?? t("newSession")}</strong>
                {activeSessionRunning && (
                  <small className="mobile-session-status">
                    <span className="status-dot" /> {t("running")}
                  </small>
                )}
              </span>
              <ChevronDown size={15} aria-hidden="true" />
            </button>
          </div>
          <div className="toolbar-actions">
            {!activeSessionRunning && (
              <span
                className={`connection ${status === "error" ? "offline" : ""}`}
                title={connectionLabel}
              >
                <span className="status-dot" /> {connectionLabel}
              </span>
            )}
            {actions.has("fork") && (
              <button
                disabled={!sessionId}
                className="icon-button toolbar-fork"
                title={t("forkSession")}
                aria-label={t("forkSession")}
                onClick={() => void forkCurrent()}
              >
                <GitBranch size={16} />
              </button>
            )}
            {actions.has("clone") && (
              <button
                disabled={!sessionId}
                className="icon-button toolbar-clone"
                title={t("cloneSession")}
                aria-label={t("cloneSession")}
                onClick={() => void cloneCurrent()}
              >
                <Copy size={16} />
              </button>
            )}
            <button
              className="icon-button"
              aria-label={t("toggleInspector")}
              title={t("toggleInspector")}
              onClick={() => {
                setInspector(!inspector);
                setPanel(false);
              }}
            >
              <Settings2 size={17} />
            </button>
            {project && actions.has("files") && (
              <button
                className={`icon-button ${workbenchOpen && !panel && !inspector ? "selected" : ""}`}
                aria-label={t("toggleFiles")}
                aria-pressed={workbenchOpen && !panel && !inspector}
                onClick={() => {
                  setWorkbenchOpen(!workbenchOpen || panel || inspector);
                  setPanel(false);
                  setInspector(false);
                }}
              >
                <Files size={17} />
              </button>
            )}
          </div>
        </header>

        {mobileSessionPicker && (
          <div
            className="mobile-session-backdrop"
            role="presentation"
            onClick={() => setMobileSessionPicker(false)}
          >
            <section
              className="mobile-session-sheet"
              role="dialog"
              aria-modal="true"
              aria-label={t("switchSession")}
              onClick={(event) => event.stopPropagation()}
            >
              <div className="mobile-session-sheet-heading">
                <h2>{t("switchSession")}</h2>
                <button
                  type="button"
                  className="icon-button"
                  aria-label={t("closePanel")}
                  onClick={() => setMobileSessionPicker(false)}
                >
                  <X size={16} />
                </button>
              </div>
              <input
                className="mobile-session-search"
                value={mobileSessionQuery}
                onChange={(event) => setMobileSessionQuery(event.target.value)}
                placeholder={t("search")}
                aria-label={t("search")}
              />
              <div className="mobile-session-list">
                {mobileSessionResults.length > 0 ? (
                  mobileSessionResults.map((entry) => {
                    const entryProject = projects.find(
                      (item) => item.id === entry.projectId,
                    );
                    const selected = entry.sessionId === sessionId;
                    const entryRunning = [
                      "running",
                      "starting",
                      "awaiting_input",
                    ].includes(entry.status ?? "");
                    return (
                      <button
                        type="button"
                        className={`mobile-session-option${selected ? " selected" : ""}`}
                        key={entry.sessionId}
                        onClick={() => {
                          select(entry.sessionId, entry.projectId);
                          setMobileSessionPicker(false);
                        }}
                      >
                        <span
                          className={`mobile-session-option-status${entryRunning ? " running" : ""}`}
                        />
                        <span className="mobile-session-option-copy">
                          <strong>{entry.title || t("newSession")}</strong>
                          <small>
                            {entryProject?.name ?? "rpi"}
                            {entryRunning && ` · ${t("running")}`}
                          </small>
                        </span>
                        {selected && <Check size={15} />}
                      </button>
                    );
                  })
                ) : (
                  <p className="mobile-session-empty">
                    {t("noMatchingSessions")}
                  </p>
                )}
              </div>
            </section>
          </div>
        )}

        {interrupted && (
          <div className="notice-banner">
            <AlertTriangle size={14} />
            <span>{t("interruptedRun")}</span>
            {supported.has("resume_pending") && (
              <button
                className="banner-action"
                onClick={() => void resumePending()}
                disabled={busy}
              >
                <Play size={13} /> {t("continue")}
              </button>
            )}
            <button
              className="icon-button"
              aria-label={t("dismiss")}
              onClick={() => setInterrupted(false)}
            >
              <X size={14} />
            </button>
          </div>
        )}

        {error && (
          <div className="error-banner">
            <X size={14} />
            <span>{error}</span>
            {connectionFailed && (
              <button
                className="banner-action"
                onClick={() => setConnectionRevision((value) => value + 1)}
              >
                {language === "zh" ? "重试连接" : "Retry connection"}
              </button>
            )}
            <button
              className="icon-button"
              aria-label={t("dismiss")}
              onClick={() => setError("")}
            >
              <X size={14} />
            </button>
          </div>
        )}

        <main className="main-content chat-layout">
          <div
            className={`conversation-column ${rows.length === 0 && !loadingSession && !creatingSession ? "empty-conversation" : ""}`}
          >
            <div
              className="transcript"
              ref={transcriptRef}
              onScroll={(event) => {
                const node = event.currentTarget;
                const nearBottom =
                  node.scrollHeight - node.scrollTop - node.clientHeight < 80;
                followingRef.current = nearBottom;
                setFollowing(nearBottom);
              }}
            >
              <div className="message-list">
                {hasMore && (
                  <div className="load-earlier">
                    <button
                      type="button"
                      onClick={() => void loadEarlier()}
                      disabled={loadingEarlier}
                    >
                      <Loader2
                        size={13}
                        className={loadingEarlier ? "spin" : undefined}
                      />
                      {t("loadEarlier")}
                    </button>
                  </div>
                )}

                {loadingSession && rows.length === 0 && (
                  <div
                    className="session-loading"
                    role="status"
                    aria-label={t("loadingConversation")}
                  >
                    <span className="sr-only">{t("loadingConversation")}</span>
                    <div className="session-skeleton" aria-hidden="true">
                      <div className="skeleton-message skeleton-assistant">
                        <div className="skeleton-heading">
                          <span className="skeleton-avatar" />
                          <span className="skeleton-line skeleton-name" />
                        </div>
                        <div className="skeleton-body">
                          <span className="skeleton-line skeleton-wide" />
                          <span className="skeleton-line skeleton-medium" />
                          <span className="skeleton-line skeleton-short" />
                        </div>
                      </div>
                      <div className="skeleton-message skeleton-user">
                        <div className="skeleton-heading">
                          <span className="skeleton-line skeleton-user-name" />
                          <span className="skeleton-avatar" />
                        </div>
                        <div className="skeleton-user-bubble">
                          <span className="skeleton-line skeleton-medium" />
                        </div>
                      </div>
                      <div className="skeleton-message skeleton-assistant">
                        <div className="skeleton-heading">
                          <span className="skeleton-avatar" />
                          <span className="skeleton-line skeleton-name" />
                        </div>
                        <div className="skeleton-body">
                          <span className="skeleton-line skeleton-wide" />
                          <span className="skeleton-line skeleton-wide" />
                          <span className="skeleton-line skeleton-medium" />
                        </div>
                      </div>
                    </div>
                  </div>
                )}

                {rows.length === 0 && !loadingSession && !creatingSession && (
                  <div className="welcome">
                    <div className="welcome-symbol">
                      <Mark large />
                      <span>{project?.name ?? "rpi workspace"}</span>
                    </div>
                    <h2>{t("welcomeTitle")}</h2>
                    <p>{t("welcomeDescription")}</p>
                    {!sessionId && (
                      <button
                        className="start-project-chat"
                        disabled={creatingSession}
                        onClick={() => void createSession()}
                      >
                        {creatingSession ? (
                          <Loader2 className="spin" size={15} />
                        ) : (
                          <ArrowUp size={15} />
                        )}
                        {language === "zh"
                          ? "在此项目中开始会话"
                          : "Start a chat in this project"}
                      </button>
                    )}
                    <div className="suggestions">
                      <button
                        onClick={() =>
                          updateDraft(t("explainProjectPrompt"))
                        }
                      >
                        <Sparkles size={17} /> {t("explainProject")}
                      </button>
                      <button
                        onClick={() =>
                          updateDraft(t("reviewChangesPrompt"))
                        }
                      >
                        <Code2 size={17} /> {t("reviewChanges")}
                      </button>
                      <button
                        onClick={() => updateDraft(t("findTodoPrompt"))}
                      >
                        <Bot size={17} /> {t("findTodo")}
                      </button>
                    </div>
                  </div>
                )}

                {rows.map((row) => (
                  <RowView row={row} key={row.id} language={language} />
                ))}

                {busy && rows.length > 0 && (
                  <div className="working-indicator">
                    <Loader2 size={14} className="spin" />{" "}
                    {dialog ? t("waitingForAnswer") : t("waiting")}
                  </div>
                )}
              </div>
            </div>
            {!following && rows.length > 0 && (
              <button
                type="button"
                className="jump-to-bottom"
                aria-label={t("jumpToBottom")}
                onClick={() => {
                  followingRef.current = true;
                  setFollowing(true);
                  transcriptRef.current?.scrollTo({
                    top: transcriptRef.current.scrollHeight,
                    behavior: "smooth",
                  });
                }}
              >
                <ArrowDown size={14} aria-hidden="true" />
                <span>{t("jumpToBottom")}</span>
              </button>
            )}

            <form className="composer-area" onSubmit={submit}>
              {slashMatches.length > 0 && (
                <div className="slash-menu">
                  {slashMatches.map((command) => (
                    <button
                      type="button"
                      key={command.name}
                      className="slash-item"
                      disabled={!command.enabled}
                      onClick={() => void runSlash(command.name)}
                    >
                      <code>{command.usage}</code>
                      <span className="muted">{command.hint}</span>
                    </button>
                  ))}
                </div>
              )}
              {pendingQueueItems.length > 0 && (
                <div
                  className={`composer-queue-panel${queueOpen ? " is-open" : ""}`}
                  aria-label={t("queue")}
                >
                  <button
                    type="button"
                    className="composer-queue-heading"
                    aria-expanded={queueOpen}
                    onClick={() => setQueueOpen((value) => !value)}
                  >
                    <span>
                      <ListTodo size={13} />
                      <strong>{t("queue")}</strong>
                      <em>{pendingQueueItems.length}</em>
                      {!queueOpen && pendingQueueItems[0]?.text && (
                        <span className="composer-queue-preview">
                          {pendingQueueItems[0].text}
                        </span>
                      )}
                    </span>
                    <ChevronDown size={14} aria-hidden="true" />
                  </button>
                  <div className="composer-queue-list">
                      {pendingQueueItems.map((item) => (
                        <div className="composer-queue-item" key={item.entryId}>
                          <span>{item.text || t("nonTextMessage")}</span>
                          {isEditableQueueItem(item) && (
                            <span className="queue-item-actions">
                              {item.text?.trim() &&
                                !busy &&
                                supported.has("prompt") &&
                                supported.has("cancel_queued") && (
                                  <button
                                    type="button"
                                    className="icon-button queue-send-now"
                                    aria-label={`${t("sendNow")}: ${item.text}`}
                                    title={t("sendNow")}
                                    onClick={() =>
                                      void sendQueuedNow(item.entryId)
                                    }
                                    disabled={queueRunning}
                                  >
                                    {queueRunning ? (
                                      <Loader2 size={13} className="spin" />
                                    ) : (
                                      <Play size={13} />
                                    )}
                                  </button>
                                )}
                              <button
                                type="button"
                                className="icon-button"
                                aria-label={t("editQueued")}
                                title={t("editQueued")}
                                onClick={() => editQueued(item)}
                              >
                                <SquarePen size={13} />
                              </button>
                              <button
                                type="button"
                                className="icon-button"
                                aria-label={t("cancelQueued")}
                                title={t("cancelQueued")}
                                onClick={() => void cancelQueued(item.entryId)}
                              >
                                <X size={13} />
                              </button>
                            </span>
                          )}
                        </div>
                      ))}
                    </div>
                </div>
              )}
              <div className="composer">
                <ProjectContext
                  project={project}
                  tick={settleTick}
                  language={language}
                  loadGit={actions.has("git_diff")}
                  onOpen={() => {
                    setSidebar(true);
                    requestAnimationFrame(() =>
                      document
                        .querySelector<HTMLElement>(
                          ".selected-project .project-name",
                        )
                        ?.focus(),
                    );
                  }}
                />
                <input
                  ref={fileInputRef}
                  type="file"
                  multiple
                  hidden
                  onChange={(event) => void uploadFiles(event.target.files)}
                />
                {attachments.length > 0 && (
                  <div
                    className="composer-attachments"
                    aria-label={t("attachedFiles")}
                  >
                    {attachments.map((file) => (
                      <div className="composer-attachment" key={file.path}>
                        {file.previewUrl ? (
                          <img src={file.previewUrl} alt="" />
                        ) : (
                          <FileText size={13} />
                        )}
                        <span title={file.path}>{file.name}</span>
                        <button
                          type="button"
                          className="icon-button"
                          aria-label={`${t("removeAttachment")}: ${file.name}`}
                          title={t("removeAttachment")}
                          onClick={() => removeAttachment(file.path)}
                        >
                          <X size={12} />
                        </button>
                      </div>
                    ))}
                  </div>
                )}
                <textarea
                  ref={composerInputRef}
                  value={draft}
                  onChange={(event) => {
                    updateDraft(event.target.value, sessionId);
                  }}
                  onKeyDown={(event) => {
                    if (
                      event.key === "Enter" &&
                      !event.shiftKey &&
                      !event.nativeEvent.isComposing &&
                      event.nativeEvent.keyCode !== 229
                    ) {
                      event.preventDefault();
                      void submit();
                    }
                  }}
                  placeholder={t("sendPlaceholder")}
                  disabled={!sessionId}
                  aria-label={t("sendPlaceholder")}
                  aria-keyshortcuts="Shift+Enter"
                />
                <div className="composer-footer">
                  <div className="composer-options">
                    <button
                      type="button"
                      className="composer-attach"
                      aria-label={t("attachFiles")}
                      title={t("attachFiles")}
                      disabled={!sessionId || !projectId || uploading}
                      onClick={() => fileInputRef.current?.click()}
                    >
                      {uploading ? (
                        <Loader2 className="spin" size={14} />
                      ) : (
                        <Paperclip size={14} />
                      )}
                      <span className="sr-only">
                        {uploading ? t("uploading") : t("attachFiles")}
                      </span>
                    </button>
                    <div className="composer-runtime-controls">
                      {supported.has("set_model") && (
                        <ModelPicker
                          language={language}
                          runtime={runtime}
                          models={models}
                          supported={supported}
                          pending={modelPending}
                          onSelect={(entry) => void selectModel(entry)}
                        />
                      )}
                      {supported.has("set_thinking_level") && (
                        <ThinkingPicker
                          language={language}
                          runtime={runtime}
                          levels={levels}
                          supported={supported}
                          pending={thinkingPending}
                          onSelect={(level) => void selectThinkingLevel(level)}
                        />
                      )}
                    </div>
                    <span
                      className="composer-shortcut-hint"
                      title={t("shiftEnterNewline")}
                    >
                      <kbd>Shift</kbd>
                      <span aria-hidden="true">+</span>
                      <kbd>Enter</kbd>
                      <span>{t("shiftEnterNewline")}</span>
                    </span>
                    {queueEditId && (
                      <button
                        type="button"
                        className="composer-queue-editing"
                        onClick={cancelQueueEdit}
                        title={t("cancelQueueEdit")}
                      >
                        {t("editQueued")} <X size={11} />
                      </button>
                    )}
                  </div>
                  <div className="composer-controls">
                    <button
                      type={
                        busy && supported.has("abort") ? "button" : "submit"
                      }
                      className={`send-button${busy && supported.has("abort") ? " composer-action-stop" : ""}`}
                      aria-label={
                        busy && supported.has("abort") ? t("abort") : t("send")
                      }
                      title={
                        busy && supported.has("abort") ? t("abort") : t("send")
                      }
                      disabled={
                        busy && supported.has("abort")
                          ? false
                          : (!draft.trim() && attachments.length === 0) ||
                            !sessionId ||
                            sending
                      }
                      onClick={
                        busy && supported.has("abort")
                          ? () => void abort()
                          : undefined
                      }
                    >
                      {busy && supported.has("abort") ? (
                        <span className="stop-indicator" aria-hidden="true" />
                      ) : (
                        <ArrowUp size={17} />
                      )}
                    </button>
                  </div>
                </div>
                <details
                  className="composer-metrics"
                  aria-label={`${t("inputTokens")}, ${t("outputTokens")}, ${t("totalTokens")}, ${t("cacheHit")}, ${t("context")}, ${t("cachedTokens")}, ${t("toolCalls")}, ${t("operations")}, ${t("cost")}, ${t("eventsObserved")}`}
                >
                  <summary className="composer-metrics-summary">
                    <span
                      className="composer-metric"
                      title={t("inputTokens")}
                      aria-label={`${t("inputTokens")}: ${inputTokenCount(displayStats) == null ? "—" : formatTokens(inputTokenCount(displayStats)!)}`}
                    >
                      <ArrowDown size={12} aria-hidden="true" />
                      <strong>
                        {inputTokenCount(displayStats) == null
                          ? "—"
                          : formatTokens(inputTokenCount(displayStats)!)}
                      </strong>
                    </span>
                    <span
                      className="composer-metric"
                      title={t("outputTokens")}
                      aria-label={`${t("outputTokens")}: ${outputTokenCount(displayStats) == null ? "—" : formatTokens(outputTokenCount(displayStats)!)}`}
                    >
                      <ArrowUp size={12} aria-hidden="true" />
                      <strong>
                        {outputTokenCount(displayStats) == null
                          ? "—"
                          : formatTokens(outputTokenCount(displayStats)!)}
                      </strong>
                    </span>
                    <span
                      className="composer-metric"
                      title={t("cacheHit")}
                      aria-label={`${t("cacheHit")}: ${cacheHitRatio(displayStats) == null ? "—" : `${Math.round(cacheHitRatio(displayStats)! * 100)}%`}`}
                    >
                      <Database size={12} aria-hidden="true" />
                      <strong>
                        {cacheHitRatio(displayStats) == null
                          ? "—"
                          : `${Math.round(cacheHitRatio(displayStats)! * 100)}%`}
                      </strong>
                    </span>
                    <span
                      className="composer-metric"
                      title={`${t("context")}: ${runtime?.context ? `${formatTokens(runtime.context.tokens)} / ${formatTokens(runtime.context.window)} (${runtime.context.percent}%)` : "—"}`}
                      aria-label={`${t("context")}: ${runtime?.context ? `${formatTokens(runtime.context.tokens)} / ${formatTokens(runtime.context.window)}` : "—"}`}
                    >
                      <Gauge size={12} aria-hidden="true" />
                      <strong>
                        {runtime?.context
                          ? `${formatTokens(runtime.context.tokens)} / ${formatTokens(runtime.context.window)}`
                          : "—"}
                      </strong>
                    </span>
                    <span className="composer-metrics-more">
                      {t("details")}
                    </span>
                  </summary>
                  <div className="composer-metrics-detail">
                    <span
                      className="composer-metric"
                      title={t("cachedTokens")}
                      aria-label={`${t("cachedTokens")}: ${cachedTokenCount(displayStats) == null ? "—" : formatTokens(cachedTokenCount(displayStats)!)}`}
                    >
                      <Database size={12} aria-hidden="true" />
                      <strong>
                        {cachedTokenCount(displayStats) == null
                          ? "—"
                          : formatTokens(cachedTokenCount(displayStats)!)}
                      </strong>
                    </span>
                    <span
                      className="composer-metric"
                      title={t("totalTokens")}
                      aria-label={`${t("totalTokens")}: ${totalTokenCount(displayStats) == null ? "—" : formatTokens(totalTokenCount(displayStats)!)}`}
                    >
                      <Sigma size={12} aria-hidden="true" />
                      <strong>
                        {totalTokenCount(displayStats) == null
                          ? "—"
                          : formatTokens(totalTokenCount(displayStats)!)}
                      </strong>
                    </span>

                    <span
                      className="composer-metric"
                      title={t("toolCalls")}
                      aria-label={`${t("toolCalls")}: ${rows.filter((row) => row.kind === "tool").length}`}
                    >
                      <Wrench size={12} aria-hidden="true" />
                      <strong>
                        {rows.filter((row) => row.kind === "tool").length}
                      </strong>
                    </span>
                    <span
                      className="composer-metric"
                      title={t("operations")}
                      aria-label={`${t("operations")}: ${runtime?.stats?.operationCount ?? historyData?.total ?? 0}`}
                    >
                      <History size={12} aria-hidden="true" />
                      <strong>
                        {runtime?.stats?.operationCount ??
                          historyData?.total ??
                          0}
                      </strong>
                    </span>
                    <span
                      className="composer-metric"
                      title={t("cost")}
                      aria-label={`${t("cost")}: ${costTotal(displayStats) == null ? "—" : `$${costTotal(displayStats)!.toFixed(4)}`}`}
                    >
                      <CircleDollarSign size={12} aria-hidden="true" />
                      <strong>
                        {costTotal(displayStats) == null
                          ? "—"
                          : `$${costTotal(displayStats)!.toFixed(4)}`}
                      </strong>
                    </span>
                    <span
                      className="composer-metric"
                      title={t("eventsObserved")}
                      aria-label={`${t("eventsObserved")}: ${seen}`}
                    >
                      <Activity size={12} aria-hidden="true" />
                      <strong>{seen}</strong>
                    </span>
                  </div>
                </details>
              </div>
            </form>
          </div>

          {(panel ||
            inspector ||
            (workbenchOpen && project && actions.has("files"))) && (
            <Splitter
              label={t("resizeWorkbench")}
              value={workbenchWidth}
              min={300}
              max={900}
              reverse
              onChange={setWorkbenchWidth}
            />
          )}
          {workbenchOpen &&
            project &&
            actions.has("files") &&
            !panel &&
            !inspector && (
              <Workbench
                key={project.id}
                project={project}
                language={language}
                tick={settleTick}
                onClose={() => setWorkbenchOpen(false)}
              />
            )}
          {panel && sessionId && (
            <RuntimePanel
              language={language}
              sessionId={sessionId}
              supported={supported}
              runtime={runtime}
              onChange={setRuntime}
              onError={setError}
              onClose={() => setPanel(false)}
            />
          )}

          {inspector && (
            <aside className="inspector">
              <div className="panel-heading">
                <h2>{t("inspector")}</h2>
                <button
                  className="icon-button"
                  onClick={() => setInspector(false)}
                  aria-label={t("closePanel")}
                  title={t("closePanel")}
                >
                  <X size={15} />
                </button>
              </div>
              <div className="stat-row">
                <span>{t("protocol")}</span>
                <strong>{capabilities?.protocol ?? "—"}</strong>
              </div>
              <div className="stat-row">
                <span>{t("settlement")}</span>
                <strong>agent_settled</strong>
              </div>
              <div className="stat-row">
                <span>{t("session")}</span>
                <strong>{sessionId.slice(0, 8) || "—"}</strong>
              </div>
              <div className="stat-row">
                <span>{t("leaf")}</span>
                <strong>{leafId ? leafId.slice(0, 8) : "—"}</strong>
              </div>
              <div className="stat-row">
                <span>{t("eventsObserved")}</span>
                <strong>{seen}</strong>
              </div>
              <div className="stat-row">
                <span>{t("context")}</span>
                <strong>
                  {runtime?.context
                    ? `${runtime.context.percent}% / ${formatTokens(runtime.context.window)}`
                    : "—"}
                </strong>
              </div>
              <div className="stat-row">
                <span>{t("toolCalls")}</span>
                <strong>
                  {rows.filter((row) => row.kind === "tool").length}
                </strong>
              </div>
              <div className="stat-row">
                <span>{t("operations")}</span>
                <strong>{historyData?.total ?? 0}</strong>
              </div>
              {displayStats && (
                <>
                  <div className="stat-row">
                    <span>{t("tokens")}</span>
                    <strong>
                      {totalTokenCount(displayStats) == null
                        ? "—"
                        : formatTokens(totalTokenCount(displayStats)!)}
                    </strong>
                  </div>
                  <div className="stat-row">
                    <span>{t("inputTokens")}</span>
                    <strong>
                      {inputTokenCount(displayStats) == null
                        ? "—"
                        : formatTokens(inputTokenCount(displayStats)!)}
                    </strong>
                  </div>
                  <div className="stat-row">
                    <span>{t("outputTokens")}</span>
                    <strong>
                      {outputTokenCount(displayStats) == null
                        ? "—"
                        : formatTokens(outputTokenCount(displayStats)!)}
                    </strong>
                  </div>
                  <div className="stat-row">
                    <span>{t("cachedTokens")}</span>
                    <strong>
                      {cachedTokenCount(displayStats) == null
                        ? "—"
                        : formatTokens(cachedTokenCount(displayStats)!)}
                    </strong>
                  </div>
                  <div className="stat-row">
                    <span>{t("cost")}</span>
                    <strong>
                      {costTotal(displayStats) == null
                        ? "—"
                        : `$${costTotal(displayStats)!.toFixed(4)}`}
                    </strong>
                  </div>
                  <div className="stat-row">
                    <span>{t("cacheHit")}</span>
                    <strong>
                      {cacheHitRatio(displayStats) == null
                        ? "—"
                        : `${Math.round(cacheHitRatio(displayStats)! * 100)}%`}
                    </strong>
                  </div>
                  <div className="stats-breakdown">
                    <div className="muted">
                      {displayStats.provider ?? t("unknownProvider")} ·{" "}
                      {displayStats.modelId ?? t("unknownModel")}
                    </div>
                    {Object.entries(displayStats.operationCounts ?? {}).map(
                      ([name, value]) => {
                        const maximum = Math.max(
                          1,
                          ...Object.values(displayStats.operationCounts ?? {}),
                        );
                        return (
                          <div className="stats-bar-row" key={name}>
                            <span>{valueLabel(language, name)}</span>
                            <span className="stats-bar-track">
                              <span
                                className="stats-bar-fill"
                                style={{ width: `${(value / maximum) * 100}%` }}
                              />
                            </span>
                            <strong>{value}</strong>
                          </div>
                        );
                      },
                    )}
                  </div>
                </>
              )}
              {actions.has("provider_auth") &&
                supported.has("get_auth_status") && (
                  <section className="diagnostic-section">
                    <h3 className="panel-heading">
                      <span>
                        <ShieldCheck size={14} /> {t("providerAuth")}
                      </span>
                    </h3>
                    <div className="auth-provider-list">
                      {authProviders.map((provider) => (
                        <div
                          className="auth-provider-row"
                          key={provider.provider}
                        >
                          <button
                            type="button"
                            className={`auth-provider ${authProvider === provider.provider ? "selected" : ""}`}
                            onClick={() => {
                              credentialRevision.current++;
                              setAuthProvider(provider.provider);
                              setApiKeyDraft("");
                              setAuthMessage(null);
                            }}
                          >
                            <strong>{provider.provider}</strong>
                            <span>
                              {provider.ready
                                ? valueLabel(language, provider.source)
                                : t("notConfigured")}
                              {provider.oauth && provider.source !== "oauth"
                                ? " · OAuth"
                                : ""}
                            </span>
                          </button>
                          {provider.oauth && (
                            <button
                              className="icon-button"
                              title={t("refresh")}
                              aria-label={`${t("refresh")} ${provider.provider} OAuth`}
                              onClick={() =>
                                void refreshOAuth(provider.provider)
                              }
                            >
                              <RefreshCw size={14} />
                            </button>
                          )}
                        </div>
                      ))}
                    </div>
                    {supported.has("oauth") &&
                      authProvider &&
                      authProviders.find(
                        (item) => item.provider === authProvider,
                      )?.oauthAvailable && (
                        <button
                          type="button"
                          className="text-button"
                          onClick={() => void startOAuth(authProvider)}
                        >
                          {t("oauthSignIn")}
                        </button>
                      )}
                    {supported.has("set_api_key") && authProvider && (
                      <div className="auth-compose">
                        <input
                          type="password"
                          autoComplete="new-password"
                          value={apiKeyDraft}
                          onChange={(event) => {
                            credentialRevision.current++;
                            setApiKeyDraft(event.target.value);
                          }}
                          placeholder={t("apiKeyFor", {
                            provider: authProvider,
                          })}
                          aria-label={t("apiKeyFor", {
                            provider: authProvider,
                          })}
                          onKeyDown={(event) => {
                            if (event.key === "Enter") void saveApiKey();
                          }}
                        />
                        <button
                          type="button"
                          disabled={!apiKeyDraft.trim()}
                          onClick={() => void saveApiKey()}
                        >
                          {t("saveKey")}
                        </button>
                      </div>
                    )}
                    {authMessage && (
                      <p className="muted">
                        {t(authMessage.key, {
                          ...authMessage.values,
                          ...(authMessage.values.status
                            ? {
                                status: valueLabel(
                                  language,
                                  authMessage.values.status,
                                ),
                              }
                            : {}),
                        })}
                      </p>
                    )}
                  </section>
                )}
              {actions.has("queue") && supported.has("get_queue") && (
                <section className="diagnostic-section">
                  <h3 className="panel-heading">
                    <span>
                      <ListTodo size={14} /> {t("queue")}
                    </span>
                    <span className="panel-heading-actions">
                      <strong>{queueItems.length}</strong>
                      {actions.has("queue_status") && (
                        <button
                          type="button"
                          className="text-button"
                          onClick={() => setShowQueueHistory((value) => !value)}
                        >
                          {t(showQueueHistory ? "queuePending" : "history")}
                        </button>
                      )}
                      {supported.has("clear_queue") &&
                        queueItems.some((item) =>
                          isEditableQueueItem(item),
                        ) && (
                          <button
                            type="button"
                            className="text-button"
                            onClick={() => void clearQueue()}
                          >
                            {t("clear")}
                          </button>
                        )}
                    </span>
                  </h3>
                  {queueItems.map((item) => (
                    <div className="queue-item" key={item.entryId}>
                      <div>
                        <strong>
                          {valueLabel(language, item.queue)} ·{" "}
                          {valueLabel(language, item.status ?? "queued")}
                        </strong>
                        <span>{item.text || t("nonTextMessage")}</span>
                        {item.runId && (
                          <small>
                            {t("runLabel")} {item.runId.slice(0, 10)}
                          </small>
                        )}
                      </div>
                      {isEditableQueueItem(item) && (
                        <span className="queue-item-actions">
                          {item.text?.trim() &&
                            !busy &&
                            supported.has("prompt") &&
                            supported.has("cancel_queued") && (
                              <button
                                type="button"
                                className="icon-button queue-send-now"
                                title={t("sendNow")}
                                aria-label={`${t("sendNow")}: ${item.text}`}
                                onClick={() =>
                                  void sendQueuedNow(item.entryId)
                                }
                                disabled={queueRunning}
                              >
                                {queueRunning ? (
                                  <Loader2 size={13} className="spin" />
                                ) : (
                                  <Play size={13} />
                                )}
                              </button>
                            )}
                          <button
                            type="button"
                            className="icon-button"
                            title={t("editQueued")}
                            aria-label={t("editQueued")}
                            onClick={() => editQueued(item)}
                          >
                            <SquarePen size={13} />
                          </button>
                          <button
                            type="button"
                            className="icon-button"
                            title={t("cancelQueued")}
                            aria-label={t("cancelQueued")}
                            onClick={() => void cancelQueued(item.entryId)}
                          >
                            <X size={13} />
                          </button>
                        </span>
                      )}
                    </div>
                  ))}
                  {supported.has("queue_message") && !queueEditId && (
                    <div className="queue-compose">
                      <input
                        ref={queueInputRef}
                        value={queueDraft}
                        onChange={(event) => {
                          queueDraftRevision.current++;
                          setQueueDraft(event.target.value);
                        }}
                        placeholder={t("queuePlaceholder")}
                        aria-label={t("queuePlaceholder")}
                        onKeyDown={(event) => {
                          if (event.key === "Enter") void queuePrompt();
                        }}
                      />
                      <MenuSelect
                        label={t("queueKind")}
                        value={queueKind}
                        onChange={setQueueKind}
                        options={[
                          { value: "followUp", label: t("followUp") },
                          { value: "steer", label: t("steer") },
                          { value: "nextTurn", label: t("nextTurn") },
                        ]}
                      />
                      <button
                        type="button"
                        onClick={() => void queuePrompt()}
                        disabled={!queueDraft.trim()}
                      >
                        {t("queue")}
                      </button>
                    </div>
                  )}
                </section>
              )}
              {actions.has("history") && historyData && (
                <section className="diagnostic-section">
                  <h3 className="panel-heading">
                    <span>
                      <History size={14} /> {t("history")}
                    </span>
                    <span className="panel-heading-actions">
                      <strong>{historyData.total}</strong>
                      {historyHasMore && (
                        <button
                          type="button"
                          className="text-button"
                          disabled={historyLoading}
                          onClick={() => void loadOlderHistory()}
                        >
                          {t(historyLoading ? "loading" : "older")}
                        </button>
                      )}
                    </span>
                  </h3>
                  {persistedSettlementRows(historyData).length > 0 && (
                    <p className="muted">{t("settlementsRestored")}</p>
                  )}
                  {historyData.openOperations &&
                    historyData.openOperations.length > 0 && (
                      <div className="history-open-operations">
                        <div className="history-subheading">
                          <Loader2 size={12} /> {t("activeOperations")}
                        </div>
                        {historyData.openOperations.map((operation, index) => (
                          <details
                            className="history-item operation-open"
                            key={String(
                              operation.id ?? operation.runId ?? index,
                            )}
                            open
                          >
                            <summary>
                              <strong>
                                {historyRecordLabel({
                                  ...operation,
                                  type: operation.type ?? "operation",
                                })}
                              </strong>
                              <span>{t("toolRunning")}</span>
                            </summary>
                            <pre>{historyRecordDetails(operation)}</pre>
                          </details>
                        ))}
                      </div>
                    )}
                  <div className="history-list history-timeline">
                    {historyData.records
                      .slice()
                      .reverse()
                      .map((record) => {
                        const target =
                          typeof record.sourceLeafId === "string"
                            ? record.sourceLeafId
                            : null;
                        const recordId = String(record.id ?? record.seq);
                        const time = historyRecordTime(record);
                        const runId =
                          typeof record.runId === "string"
                            ? record.runId.slice(0, 12)
                            : null;
                        return (
                          <details
                            className="history-item history-timeline-item"
                            key={recordId}
                          >
                            <summary>
                              <div>
                                <strong>{historyRecordLabel(record)}</strong>
                                <span>
                                  {t("sequenceLabel")}{" "}
                                  {String(record.seq ?? "—")}
                                  {time ? ` · ${time}` : ""}
                                </span>
                                {runId && (
                                  <span>
                                    {t("runLabel")} {runId}
                                  </span>
                                )}
                              </div>
                              <div className="history-summary-status">
                                {Boolean(record.outcome) && (
                                  <span>
                                    {valueLabel(
                                      language,
                                      String(record.outcome),
                                    )}
                                  </span>
                                )}
                                {Boolean(record.error) &&
                                  typeof record.error === "object" && (
                                    <span className="error-text">
                                      {String(
                                        (record.error as { message?: unknown })
                                          .message ?? "error",
                                      )}
                                    </span>
                                  )}
                              </div>
                            </summary>
                            <div className="history-detail">
                              <pre>{historyRecordDetails(record)}</pre>
                              {target && actions.has("history_mutation") && (
                                <div className="history-actions">
                                  <button
                                    title={t("forkHere")}
                                    disabled={mutationPending}
                                    onClick={() => void forkCurrent(target)}
                                  >
                                    <GitBranch size={12} /> {t("fork")}
                                  </button>
                                  <button
                                    title={t("rewindHere")}
                                    disabled={mutationPending}
                                    onClick={() => void rewindTo(target)}
                                  >
                                    <RotateCcw size={12} /> {t("rewind")}
                                  </button>
                                </div>
                              )}
                            </div>
                          </details>
                        );
                      })}
                  </div>
                </section>
              )}
              <div className="stat-row">
                <span>{t("activeTools")}</span>
                <strong>{runtime?.activeTools?.length ?? 0}</strong>
              </div>
              <h3 className="panel-heading">{t("capabilities")}</h3>
              <div className="tool-stack capability-list">
                {capabilities?.supportedCommands.map((command) => (
                  <span className="badge" key={command}>
                    {command}
                  </span>
                ))}
              </div>
            </aside>
          )}
        </main>
      </section>
    </div>
  );
}

function DialogOverlay({
  dialog,
  answer,
  busy,
  language,
  onAnswer,
  onSubmit,
  onCancel,
}: {
  dialog: UiDialog;
  answer: string;
  busy: boolean;
  language: Language;
  onAnswer: (value: string) => void;
  onSubmit: (value: unknown) => void;
  onCancel: () => void;
}) {
  const t = (key: Parameters<typeof translate>[1]) => translate(language, key);
  const ui = dialog.ui;
  const options = ui.options ?? [];
  const structured =
    ui.purpose !== "permission" &&
    (ui.kind === "selector" || ui.kind === "input");
  const multiple = structured && !!ui.allowMultiple;
  const comment = structured && !!ui.allowComment;
  const [selected, setSelected] = useState<number[]>([]);
  const optionValue = (index: number) =>
    options[index].value ??
    options[index].permission ??
    options[index].title ??
    String(index);
  const freeform =
    Boolean(ui.allowFreeform ?? ui.freeform) || options.length === 0;
  const canSubmit = selected.length > 0 || (freeform && !!answer.trim());
  const submit = () => {
    if (!canSubmit || busy) return;
    onSubmit(
      structured
        ? {
            values: selected.length
              ? selected.map((index) => String(optionValue(index)))
              : [answer],
            ...(answer.trim() && (ui.kind === "input" || comment)
              ? { text: answer }
              : {}),
          }
        : answer,
    );
  };
  return (
    <div className="dialog-backdrop" role="presentation">
      <form
        className="dialog-card"
        role="dialog"
        aria-modal="true"
        aria-labelledby="native-dialog-title"
        onSubmit={(event) => {
          event.preventDefault();
          submit();
        }}
      >
        <div className="dialog-heading">
          <strong>
            {ui.purpose === "permission" ? t("permission") : t("question")}
          </strong>
          <button
            type="button"
            className="icon-button"
            onClick={onCancel}
            aria-label={t("cancel")}
          >
            <X size={16} />
          </button>
        </div>
        <h2 id="native-dialog-title">
          {ui.question ?? ui.message ?? t("question")}
        </h2>
        {ui.context && <pre className="dialog-context">{ui.context}</pre>}
        {options.length > 0 && (
          <div className="dialog-options">
            {options.map((option, index) => {
              const value =
                option.value ??
                option.permission ??
                option.title ??
                String(index);
              return (
                <button
                  type="button"
                  key={`${option.title ?? "option"}-${index}`}
                  disabled={busy}
                  aria-pressed={
                    multiple || comment ? selected.includes(index) : undefined
                  }
                  onClick={() => {
                    if (multiple)
                      setSelected((previous) =>
                        previous.includes(index)
                          ? previous.filter((item) => item !== index)
                          : [...previous, index],
                      );
                    else if (comment) setSelected([index]);
                    else
                      onSubmit(
                        structured ? { values: [String(value)] } : value,
                      );
                  }}
                >
                  <span>
                    {option.title ?? String(value)}
                    {option.description && <small>{option.description}</small>}
                  </span>
                </button>
              );
            })}
          </div>
        )}
        {(freeform || comment) && (
          <>
            <label htmlFor="dialog-answer">{t("answer")}</label>
            <textarea
              id="dialog-answer"
              value={answer}
              onChange={(event) => onAnswer(event.target.value)}
              autoFocus
              rows={4}
              placeholder={ui.suggest}
            />
          </>
        )}
        {(freeform || multiple || comment) && (
          <div className="dialog-actions">
            <button type="button" onClick={onCancel} disabled={busy}>
              {t("cancel")}
            </button>
            <button
              type="submit"
              className="primary"
              disabled={busy || !canSubmit}
            >
              {t("submit")}
            </button>
          </div>
        )}
      </form>
    </div>
  );
}

function groupModels(models: ModelDescriptor[]): [string, ModelDescriptor[]][] {
  const order: string[] = [];
  const map = new Map<string, ModelDescriptor[]>();
  for (const model of models) {
    const list = map.get(model.provider);
    if (list) list.push(model);
    else {
      order.push(model.provider);
      map.set(model.provider, [model]);
    }
  }
  return order.map((provider) => [provider, map.get(provider) ?? []]);
}

function Dropdown({
  label,
  className,
  disabled = false,
  align = "start",
  trigger,
  children,
}: {
  label: string;
  className?: string;
  disabled?: boolean;
  align?: "start" | "end";
  trigger: ReactNode;
  children: ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const anchor = useRef<HTMLDivElement | null>(null);
  const popover = useRef<HTMLDivElement | null>(null);
  const [position, setPosition] = useState({ top: 0, left: 0, width: 220 });
  useEffect(() => {
    if (!open) return;
    const closeOnPointer = (event: PointerEvent) => {
      const target = event.target as Element;
      if (
        !target.closest("[data-dropdown]") &&
        !target.closest(".dropdown-popover")
      )
        setOpen(false);
    };
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpen(false);
    };
    const closeOnScroll = () => setOpen(false);
    document.addEventListener("pointerdown", closeOnPointer);
    document.addEventListener("keydown", closeOnEscape);
    window.addEventListener("resize", closeOnScroll);
    return () => {
      document.removeEventListener("pointerdown", closeOnPointer);
      document.removeEventListener("keydown", closeOnEscape);
      window.removeEventListener("resize", closeOnScroll);
    };
  }, [open]);
  useEffect(() => {
    if (!open) return;
    const frame = requestAnimationFrame(() => {
      const box = anchor.current?.getBoundingClientRect();
      const menu = popover.current;
      if (!box || !menu) return;
      const width = Math.max(box.width, 224);
      const height = menu.getBoundingClientRect().height;
      const viewportBottom = window.innerHeight - 8;
      const below = box.bottom + 6;
      const top =
        below + height <= viewportBottom
          ? below
          : Math.max(8, box.top - height - 6);
      const left = align === "end" ? box.right - width : box.left;
      setPosition({
        top: Math.min(top, Math.max(8, viewportBottom - height)),
        left: Math.max(8, Math.min(left, window.innerWidth - width - 8)),
        width,
      });
    });
    return () => cancelAnimationFrame(frame);
  }, [align, open]);
  function toggle() {
    if (!open && anchor.current) {
      const box = anchor.current.getBoundingClientRect();
      const width = Math.max(box.width, 224);
      const left = align === "end" ? box.right - width : box.left;
      setPosition({
        top: Math.max(8, Math.min(box.bottom + 6, window.innerHeight - 220)),
        left: Math.max(8, Math.min(left, window.innerWidth - width - 8)),
        width,
      });
    }
    setOpen((value) => !value);
  }
  return (
    <div className="dropdown-anchor" data-dropdown ref={anchor}>
      <button
        type="button"
        className={className}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-label={label}
        title={label}
        disabled={disabled}
        onClick={toggle}
      >
        {trigger}
      </button>
      {open &&
        createPortal(
          <div
            ref={popover}
            className="dropdown-popover"
            role="listbox"
            aria-label={label}
            onClick={(event) => {
              if ((event.target as Element).closest('[role="option"]')) {
                setOpen(false);
              }
            }}
            style={{
              position: "fixed",
              top: position.top,
              left: position.left,
              width: position.width,
            }}
          >
            {children}
          </div>,
          document.body,
        )}
    </div>
  );
}

function ModelPicker({
  language,
  runtime,
  models,
  supported,
  pending,
  className = "composer-model",
  icon,
  onSelect,
}: {
  language: Language;
  runtime: RuntimeState | null;
  models: ModelDescriptor[];
  supported: Set<string>;
  pending: boolean;
  className?: string;
  icon?: ReactNode;
  onSelect: (model: ModelDescriptor) => void;
}) {
  const t = (key: MessageKey) => translate(language, key);
  const currentKey = runtime?.modelId
    ? modelKey({ id: runtime.modelId, provider: runtime.provider })
    : "";
  const current = models.find((entry) => modelKey(entry) === currentKey);
  return (
    <Dropdown
      label={t("chooseModel")}
      className={className}
      disabled={!supported.has("set_model") || pending || models.length === 0}
      trigger={
        <>
          {icon}
          <span>{current?.name ?? runtime?.modelId ?? t("model")}</span>
          <ChevronDown size={11} />
        </>
      }
    >
      {models.length === 0 ? (
        <p className="dropdown-empty muted">{t("noModels")}</p>
      ) : (
        groupModels(models).map(([provider, entries]) => (
          <div className="dropdown-group" key={provider}>
            <div className="dropdown-group-label">{provider}</div>
            {entries.map((entry) => {
              const key = modelKey(entry);
              const selected = key === currentKey;
              return (
                <button
                  key={key}
                  role="option"
                  aria-selected={selected}
                  className="dropdown-item"
                  onClick={() => onSelect(entry)}
                >
                  <span>{entry.name}</span>
                  {selected && <Check size={13} />}
                </button>
              );
            })}
          </div>
        ))
      )}
    </Dropdown>
  );
}

function ThinkingPicker({
  language,
  runtime,
  levels,
  supported,
  pending,
  className = "composer-thinking",
  icon,
  onSelect,
}: {
  language: Language;
  runtime: RuntimeState | null;
  levels: string[];
  supported: Set<string>;
  pending: boolean;
  className?: string;
  icon?: ReactNode;
  onSelect: (level: string) => void;
}) {
  const t = (key: MessageKey) => translate(language, key);
  const current = normalizeLevel(runtime?.thinkingLevel);
  return (
    <Dropdown
      label={t("chooseThinking")}
      className={className}
      disabled={
        !supported.has("set_thinking_level") || pending || levels.length === 0
      }
      trigger={
        <>
          {icon}
          <span>
            {current ? valueLabel(language, current) : t("thinkingLevel")}
          </span>
          <ChevronDown size={11} />
        </>
      }
    >
      {levels.length === 0 ? (
        <p className="dropdown-empty muted">{t("noModels")}</p>
      ) : (
        levels.map((level) => {
          const selected = level === current;
          return (
            <button
              key={level}
              role="option"
              aria-selected={selected}
              className="dropdown-item"
              onClick={() => onSelect(level)}
            >
              <span>{valueLabel(language, level)}</span>
              {selected && <Check size={13} />}
            </button>
          );
        })
      )}
    </Dropdown>
  );
}

function toolIcon(name: string) {
  switch (name.trim().toLowerCase()) {
    case "read":
    case "read_file":
    case "readfile":
    case "cat":
      return <FileText size={14} />;
    case "edit":
    case "write":
    case "write_file":
    case "writefile":
    case "apply_patch":
      return <SquarePen size={14} />;
    case "grep":
    case "glob":
    case "find":
    case "search":
    case "search_files":
    case "file_search":
    case "rg":
      return <Search size={14} />;
    case "list":
    case "ls":
    case "list_files":
    case "list_directory":
      return <Folder size={14} />;
    case "web_fetch":
    case "web_search":
    case "fetch":
    case "http_get":
    case "browser":
    case "open_url":
      return <Globe size={14} />;
    case "git":
    case "git_status":
    case "git_log":
    case "git_diff":
      return <GitBranch size={14} />;
    case "plan":
      return <ClipboardList size={14} />;
    case "todo":
      return <ListTodo size={14} />;
    default:
      return <Wrench size={14} />;
  }
}

function ImagePreview({ image }: { image: ImageBlock }) {
  const source =
    image.url ??
    (image.data
      ? image.data.startsWith("data:")
        ? image.data
        : `data:${image.mimeType || "image/png"};base64,${image.data}`
      : "");
  const isSafeImage =
    source.startsWith("data:image/") ||
    source.startsWith("/") ||
    source.startsWith("https://") ||
    source.startsWith("http://");
  if (
    !source ||
    !isSafeImage ||
    (image.mimeType && !image.mimeType.startsWith("image/"))
  )
    return null;
  return (
    <figure className="transcript-image">
      <img src={source} alt={image.alt ?? ""} loading="lazy" />
    </figure>
  );
}

const RowView = memo(function RowView({
  row,
  language,
}: {
  row: Row;
  language: Language;
}) {
  const t = (key: MessageKey) => translate(language, key);
  if (row.kind === "note") {
    const values = row.translation?.values;
    const isRunStatus =
      row.translation?.key === "liveRun" ||
      row.translation?.key === "restoredRun";
    return (
      <p className={`transcript-note muted${isRunStatus ? " run-status" : ""}`}>
        {row.translation
          ? translate(
              language,
              row.translation.key,
              values?.outcome
                ? {
                    ...values,
                    outcome: valueLabel(language, String(values.outcome)),
                    ...(values.aborted
                      ? {
                          aborted: ` · ${valueLabel(language, String(values.aborted))}`,
                        }
                      : {}),
                  }
                : values,
            )
          : row.text}
      </p>
    );
  }

  if (row.kind === "image") {
    return <ImagePreview image={row.image} />;
  }
  if (row.kind === "thinking") {
    return (
      <details className="thinking-row">
        <summary>
          <Sparkles size={13} /> {t("thinking")}
        </summary>
        <MarkdownBody text={row.text} language={language} />
      </details>
    );
  }

  if (row.kind === "tool") {
    const plan = row.name === "plan" ? planInfo(row.args, row.result) : null;
    if (plan) return <PlanCard plan={plan} language={language} />;
    const todos = row.name === "todo" ? todoItems(row.result) : null;
    if (todos) return <TodoCard items={todos} language={language} />;
    if (isShellTool(row.name))
      return <ShellCard row={row} language={language} />;
    const edits = editInfo(row.name, row.args);
    if (edits) return <EditCard row={row} edits={edits} language={language} />;
    const resultText = typeof row.result === "string" ? row.result : null;
    const readResultText =
      row.name === "read" && row.status !== "error"
        ? readOutput(row.result)
        : null;
    const readResult = readResultText !== null;
    const readPath = row.name === "read" ? pathOfArgs(row.args) : "";
    const showArgs = row.result === undefined && row.args !== undefined;
    return (
      <details
        className={`tool-card tool-${row.status} ${row.status === "error" ? "failed" : ""}`}
        open={row.status === "error"}
      >
        <summary>
          {toolIcon(row.name)}
          <strong>{row.name}</strong>
          {row.timing && (
            <ToolElapsed timing={row.timing} language={language} />
          )}
          <span className="tool-status">
            {toolStatusLabel(row.status, language)}
          </span>
        </summary>
        <div className="tool-detail">
          {row.images?.map((image, index) => (
            <ImagePreview key={`${row.id}-image-${index}`} image={image} />
          ))}
          {showArgs ? (
            <>
              <h4>{t("input")}</h4>
              <pre>{safeJson(row.args)}</pre>
            </>
          ) : readResult ? (
            <ReadCode
              path={readPath}
              text={readResultText ?? ""}
              language={language}
            />
          ) : resultText !== null ? (
            isMarkdownText(resultText) ? (
              <MarkdownBody text={resultText} language={language} />
            ) : (
              <pre className="tool-result-pre">{resultText}</pre>
            )
          ) : row.result !== undefined ? (
            <pre>{safeJson(row.result)}</pre>
          ) : (
            <pre className="muted">{t("noOutput")}</pre>
          )}
        </div>
      </details>
    );
  }

  return <MessageRow row={row} language={language} />;
});

function MessageRow({
  row,
  language,
}: {
  row: Extract<Row, { kind: "text" }>;
  language: Language;
}) {
  const t = (key: MessageKey) => translate(language, key);
  const [copiedMarkdown, setCopiedMarkdown] = useState(false);

  async function copyMarkdown() {
    try {
      await navigator.clipboard.writeText(row.text);
      setCopiedMarkdown(true);
      window.setTimeout(() => setCopiedMarkdown(false), 1600);
    } catch {
      // Clipboard access can be denied; the message remains selectable.
    }
  }

  return (
    <article className={`message ${row.role}`}>
      <div className="message-heading">
        {row.role === "assistant" ? (
          <Mark />
        ) : (
          <span className="user-avatar">{t("you").slice(0, 1)}</span>
        )}
        <strong>{row.role === "assistant" ? "rpi" : t("you")}</strong>
      </div>
      <div className="message-body">
        {row.role === "assistant" ? (
          <MarkdownBody text={row.text} language={language} />
        ) : (
          <div className="prose plain-message">
            <p>{row.text}</p>
          </div>
        )}
      </div>
      {row.role === "assistant" && (
        <div className="message-actions">
          <button
            type="button"
            className="message-copy message-copy-markdown"
            onClick={() => void copyMarkdown()}
            aria-label={copiedMarkdown ? t("copied") : t("copyMarkdown")}
            title={copiedMarkdown ? t("copied") : t("copyMarkdown")}
          >
            {copiedMarkdown ? <Check size={12} /> : <Copy size={12} />}
            <span>{copiedMarkdown ? t("copied") : t("copyMarkdown")}</span>
          </button>
        </div>
      )}
    </article>
  );
}

function PlanCard({ plan, language }: { plan: PlanInfo; language: Language }) {
  const t = (key: MessageKey) => translate(language, key);
  return (
    <details className="tool-card plan-card" open={plan.active}>
      <summary>
        <ClipboardList size={14} />
        <strong>{t("plan")}</strong>
        <span className={`tool-status ${plan.active ? "" : "done"}`}>
          {plan.active ? t("planActive") : t("planDone")}
        </span>
      </summary>
      <div className="tool-detail plan-detail">
        {plan.outline.length > 0 && (
          <ol className="plan-outline">
            {plan.outline.map((step, index) => (
              <li key={`${index}-${step.slice(0, 16)}`}>
                <span className="plan-outline-index">{index + 1}</span>
                <span>{step}</span>
              </li>
            ))}
          </ol>
        )}
        <MarkdownBody text={plan.plan} language={language} />
      </div>
    </details>
  );
}

function TodoCard({
  items,
  language,
}: {
  items: TodoItem[];
  language: Language;
}) {
  const t = (key: MessageKey, values?: Record<string, string | number>) =>
    translate(language, key, values);
  const done = items.filter((item) => item.done).length;
  return (
    <details className="tool-card todo-card" open>
      <summary>
        <ListTodo size={14} />
        <strong>{t("todos")}</strong>
        <span className="tool-status">
          {t("todoDone", { done, total: items.length })}
        </span>
      </summary>
      <div className="tool-detail todo-detail">
        <ul className="todo-list">
          {items.map((item, index) => (
            <li
              key={`${index}-${item.text.slice(0, 16)}`}
              className={item.done ? "done" : ""}
            >
              <span className="todo-check" aria-hidden="true">
                {item.done ? <Check size={12} /> : null}
              </span>
              <span>{item.text}</span>
            </li>
          ))}
        </ul>
      </div>
    </details>
  );
}

function compactToolPath(path: string): string {
  const normalized = path.replace(/\\/g, "/");
  const parts = normalized.split("/").filter(Boolean);
  if (parts.length <= 2 && normalized.length <= 42) return normalized;
  return `…/${parts.slice(-2).join("/")}`;
}

function EditCard({
  row,
  edits,
  language,
}: {
  row: ToolRow;
  edits: EditInfo;
  language: Language;
}) {
  const t = (key: MessageKey, values?: Record<string, string | number>) =>
    translate(language, key, values);
  return (
    <details
      className={`tool-card edit-card tool-${row.status} ${row.status === "error" ? "failed" : ""}`}
      open={row.status === "error"}
    >
      <summary>
        <SquarePen size={14} />
        <span className="tool-action">edit</span>
        <code
          className="shell-command edit-summary-path"
          title={edits.path || row.name}
        >
          {compactToolPath(edits.path || row.name)}
        </code>
        {row.timing && <ToolElapsed timing={row.timing} language={language} />}
        <span className="tool-status">
          {toolStatusLabel(row.status, language)}
        </span>
      </summary>
      <div className="tool-detail edit-detail">
        {edits.path && <div className="edit-path">{edits.path}</div>}
        <details className="edit-content" open={row.status === "error"}>
          <summary>
            <span className="edit-content-label">{t("editDiff")}</span>
            <span className="edit-content-count">
              {t(edits.edits.length === 1 ? "editChange" : "editChanges", {
                count: edits.edits.length,
              })}
            </span>
          </summary>
          <div className="edit-content-body">
            {edits.edits.map((edit, index) => {
              const oldLines = edit.oldText ? edit.oldText.split("\\n") : [];
              const newLines = edit.newText ? edit.newText.split("\\n") : [];
              return (
                <div
                  className="edit-hunk"
                  key={`${index}-${edit.oldText.slice(0, 12)}-${edit.newText.slice(0, 12)}`}
                >
                  {oldLines.map((line, lineIndex) => (
                    <div className="edit-line remove" key={`o-${lineIndex}`}>
                      <span className="edit-marker" aria-hidden="true">
                        -
                      </span>
                      <code>{line || " "}</code>
                    </div>
                  ))}
                  {newLines.map((line, lineIndex) => (
                    <div className="edit-line add" key={`n-${lineIndex}`}>
                      <span className="edit-marker" aria-hidden="true">
                        +
                      </span>
                      <code>{line || " "}</code>
                    </div>
                  ))}
                </div>
              );
            })}
            {typeof row.result === "string" && row.result.trim() && (
              <div className="edit-result">{row.result}</div>
            )}
          </div>
        </details>
      </div>
    </details>
  );
}

function ShellCard({ row, language }: { row: ToolRow; language: Language }) {
  const t = (key: MessageKey) => translate(language, key);
  const { command, meta } = shellCommand(row.args);
  const output = shellOutput(row.result);
  const resultObj =
    row.result && typeof row.result === "object" && !Array.isArray(row.result)
      ? (row.result as Record<string, unknown>)
      : null;
  const exitCode =
    resultObj && typeof resultObj.exitCode === "number"
      ? String(resultObj.exitCode)
      : resultObj && typeof resultObj.code === "number"
        ? String(resultObj.code)
        : "";
  const hasStderr =
    resultObj &&
    typeof resultObj.stderr === "string" &&
    !!resultObj.stderr.trim();
  const metaParts = [
    meta,
    exitCode ? `exit ${exitCode}` : "",
    hasStderr ? "stderr" : "",
  ].filter(Boolean);
  const [copied, setCopied] = useState(false);
  useEffect(() => {
    if (!copied) return;
    const timer = window.setTimeout(() => setCopied(false), 1600);
    return () => window.clearTimeout(timer);
  }, [copied]);
  async function copyCommand() {
    if (!command) return;
    try {
      await navigator.clipboard.writeText(command);
      setCopied(true);
    } catch {
      // Clipboard access can be denied; the command stays selectable.
    }
  }
  return (
    <details
      className={`tool-card shell-card tool-${row.status} ${row.status === "error" ? "failed" : ""}`}
      open={row.status === "error"}
    >
      <summary>
        <Terminal size={14} />
        <span className="tool-action">Ran</span>
        <code className="shell-command" title={command || row.name}>
          {command ? command.replace(/\s+/g, " ").trim() : row.name}
        </code>
        {row.timing && <ToolElapsed timing={row.timing} language={language} />}
        <span className="tool-status">
          {toolStatusLabel(row.status, language)}
        </span>
      </summary>
      <div className="tool-detail shell-detail">
        <div className="shell-window">
          <div className="shell-command-heading">
            <span className="shell-branch" aria-hidden="true">
              └
            </span>
            <span className="shell-title">{row.name}</span>
            {command && (
              <button
                type="button"
                className="shell-copy"
                onClick={() => void copyCommand()}
              >
                {copied ? <Check size={12} /> : <Copy size={12} />}
                {copied ? t("copied") : t("copyCommand")}
              </button>
            )}
          </div>
          <div className="shell-body">
            {command && (
              <pre className="shell-input">
                <code>{command}</code>
              </pre>
            )}
            {output ? (
              <pre className="shell-output">{output}</pre>
            ) : (
              <pre className="shell-empty muted">{t("noOutput")}</pre>
            )}
          </div>
        </div>
        {metaParts.length > 0 && (
          <p className="shell-meta">{metaParts.join(" · ")}</p>
        )}
      </div>
    </details>
  );
}

function ToolElapsed({
  timing,
  language,
}: {
  timing: ToolTiming;
  language: Language;
}) {
  const [, tick] = useState(0);
  useEffect(() => {
    if (!timing.running) return;
    const interval = window.setInterval(() => tick((value) => value + 1), 100);
    return () => window.clearInterval(interval);
  }, [timing]);
  const milliseconds = elapsedMs(timing);
  const seconds = milliseconds / 1000;
  const duration =
    seconds < 60
      ? `${seconds.toFixed(1)}s`
      : seconds < 3600
        ? `${Math.floor(seconds / 60)}m ${Math.floor(seconds % 60)}s`
        : `${Math.floor(seconds / 3600)}h ${Math.floor(seconds / 60) % 60}m ${Math.floor(seconds % 60)}s`;
  return (
    <span
      className="tool-elapsed"
      aria-label={translate(language, "toolElapsed", { duration })}
    >
      {duration}
    </span>
  );
}

function toolStatusLabel(
  status: Row extends never ? never : string,
  language: Language,
) {
  switch (status) {
    case "pending":
      return translate(language, "toolPending");
    case "running":
      return translate(language, "toolRunning");
    case "done":
      return translate(language, "toolDone");
    case "error":
      return translate(language, "toolError");
    default:
      return status;
  }
}

function RuntimePanel({
  language,
  sessionId,
  supported,
  runtime,
  onChange,
  onError,
  onClose,
}: {
  language: Language;
  sessionId: string;
  supported: Set<string>;
  runtime: RuntimeState | null;
  onChange: (value: RuntimeState) => void;
  onError: (message: string) => void;
  onClose: () => void;
}) {
  const zh = language === "zh";
  const t = (key: MessageKey, values?: Record<string, string | number>) =>
    translate(language, key, values);
  const [models, setModels] = useState<ModelDescriptor[]>([]);
  const [levels, setLevels] = useState<string[]>([]);
  const context = runtime?.context ?? null;
  const [catalog, setCatalog] = useState<ToolCatalog | null>(null);
  const [pending, setPending] = useState("");
  const mounted = useRef(true);
  const settingsRevision = useRef(0);
  const settingsSession = useRef(sessionId);
  settingsSession.current = sessionId;
  useEffect(() => {
    mounted.current = true;
    settingsRevision.current++;
    setPending("");
    setModels([]);
    setLevels([]);
    setCatalog(null);
    return () => {
      mounted.current = false;
      settingsRevision.current++;
    };
  }, [sessionId]);

  useEffect(() => {
    let cancelled = false;
    setLevels([]);
    setCatalog(null);
    void (async () => {
      try {
        const jobs: Promise<void>[] = [];
        if (supported.has("get_available_models"))
          jobs.push(
            api.models(sessionId).then((data) => {
              if (!cancelled) setModels(data.models ?? []);
            }),
          );
        if (supported.has("get_available_thinking_levels"))
          jobs.push(
            api.thinkingLevels(sessionId).then((data) => {
              if (!cancelled) setLevels(data.levels ?? []);
            }),
          );
        if (supported.has("get_available_tools"))
          jobs.push(
            api.tools(sessionId).then((data) => {
              if (!cancelled) setCatalog(data);
            }),
          );
        await Promise.all(jobs);
      } catch (cause) {
        if (!cancelled) onError(errorText(cause));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [onError, sessionId, supported, runtime?.modelId, runtime?.provider]);

  async function run(
    label: string,
    action: () => Promise<RuntimeState | void>,
    applied?: (state: RuntimeState | void) => void,
  ) {
    setPending(label);
    const revision = settingsRevision.current;
    const current = () =>
      mounted.current &&
      settingsSession.current === sessionId &&
      settingsRevision.current === revision;
    try {
      const next = await action();
      if (current()) {
        applied?.(next);
        if (next) onChange(next);
      }
    } catch (cause) {
      if (current()) onError(errorText(cause));
    } finally {
      if (current()) setPending("");
    }
  }

  const model = models.find(
    (entry) =>
      entry.id === runtime?.modelId && entry.provider === runtime?.provider,
  );
  const level = normalizeLevel(runtime?.thinkingLevel);
  const active = new Set(catalog?.activeTools ?? runtime?.activeTools ?? []);

  return (
    <aside className="inspector">
      <div className="panel-heading">
        <h2>{zh ? "运行时设置" : "Runtime settings"}</h2>
        <button
          className="icon-button"
          onClick={onClose}
          aria-label={t("closePanel")}
          title={t("closePanel")}
        >
          <X size={15} />
        </button>
      </div>

      <label htmlFor="model">{zh ? "模型" : "Model"}</label>
      <MenuSelect
        label={zh ? "模型" : "Model"}
        value={
          runtime?.modelId
            ? modelKey({ id: runtime.modelId, provider: runtime.provider })
            : ""
        }
        disabled={
          !supported.has("set_model") || pending !== "" || models.length === 0
        }
        onChange={(value) => {
          const next = models.find((entry) => modelKey(entry) === value);
          if (!next) return;
          void run("model", async () => {
            await commandData(sessionId, "set_model", {
              modelId: next.id,
              provider: next.provider,
            });
            return api.runtimeState(sessionId);
          });
        }}
        options={[
          { value: "", label: "—" },
          ...models.map((entry) => ({
            value: modelKey(entry),
            label: `${entry.name} (${entry.provider})`,
          })),
        ]}
      />
      {model?.contextWindow ? (
        <p className="muted">
          {t("context")} {Math.round(model.contextWindow / 1000)}k
        </p>
      ) : null}

      <label htmlFor="thinking">{zh ? "思考深度" : "Thinking level"}</label>
      <MenuSelect
        label={zh ? "思考深度" : "Thinking level"}
        value={level}
        disabled={
          !supported.has("set_thinking_level") ||
          pending !== "" ||
          levels.length === 0
        }
        onChange={(next) =>
          void run("thinking", async () => {
            await commandData(sessionId, "set_thinking_level", { level: next });
            return api.runtimeState(sessionId);
          })
        }
        options={[
          { value: "", label: "—" },
          ...levels.map((entry) => ({
            value: entry,
            label: valueLabel(language, entry),
          })),
        ]}
      />

      <h3 className="panel-heading">{zh ? "上下文" : "Context"}</h3>
      {context ? (
        <>
          <div
            className="context-bar"
            title={`${context.tokens} / ${context.window}`}
          >
            <span
              className={
                context.compactionEnabled && context.tokens > context.threshold
                  ? "context-fill full"
                  : "context-fill"
              }
              style={{
                width: `${Math.min(100, Math.max(0, context.percent))}%`,
              }}
            />
          </div>
          <p className="muted">
            {formatTokens(context.tokens)} / {formatTokens(context.window)} ·{" "}
            {context.percent}%{context.estimated ? t("estimated") : ""}
          </p>
          <p className="muted">
            {context.compactionEnabled
              ? t("contextCompacts", {
                  threshold: formatTokens(context.threshold),
                  keep: formatTokens(context.keepRecentTokens),
                })
              : t("compactionDisabled")}
          </p>
        </>
      ) : (
        <p className="muted">
          {zh ? "暂无用量数据" : "No usage reported yet."}
        </p>
      )}

      <h3 className="panel-heading">{zh ? "启用的工具" : "Active tools"}</h3>
      {catalog ? (
        <div className="tool-stack">
          {catalog.tools.map((tool) => (
            <label key={tool} className="stat-row">
              <span>{tool}</span>
              <input
                type="checkbox"
                checked={active.has(tool)}
                disabled={!supported.has("set_active_tools") || pending !== ""}
                onChange={(event) => {
                  const next = new Set(active);
                  if (event.target.checked) next.add(tool);
                  else next.delete(tool);
                  const tools = [...next];
                  void run(
                    "tools",
                    async () => {
                      await commandData(sessionId, "set_active_tools", {
                        tools,
                      });
                      return api.runtimeState(sessionId);
                    },
                    (state) =>
                      setCatalog({
                        tools: catalog.tools,
                        activeTools: state?.activeTools ?? tools,
                      }),
                  );
                }}
              />
            </label>
          ))}
        </div>
      ) : (
        <p className="muted">{t("noToolCatalog")}</p>
      )}
      {pending && (
        <p className="muted">
          <Loader2 size={12} className="spin" /> {t("applying")}
        </p>
      )}
    </aside>
  );
}
