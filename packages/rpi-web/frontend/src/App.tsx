import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type FormEvent,
  type CSSProperties,
} from "react";
import {
  AlertTriangle,
  ArrowUp,
  Bot,
  CircleStop,
  ChevronDown,
  Code2,
  Copy,
  Cpu,
  Files,
  Folder,
  Gauge,
  GitBranch,
  History,
  ListTodo,
  Loader2,
  LogOut,
  Moon,
  PanelLeft,
  Play,
  RefreshCw,
  RotateCcw,
  Settings2,
  ShieldCheck,
  Sparkles,
  Sun,
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
import { LoginScreen } from "./LoginScreen";
import {
  Modal,
  ProjectDialog,
  ProjectSidebar,
  RenameDialog,
} from "./ProjectSidebar";
import {
  rowsFromAssistant,
  rowsFromMessages,
  prependRows,
  upsertTool,
  withoutPrefix,
  eventsAfterSnapshot,
  toolTiming,
  elapsedMs,
  stopToolTimers,
  type ToolTiming,
  textOf,
  type Row,
} from "./transcript";

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
    freeform?: boolean;
  };
};

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

/** 1234 -> 1.2k, 1234567 -> 1.2M. */
function formatTokens(value: number) {
  if (!Number.isFinite(value) || value <= 0) return "0";
  if (value >= 1_000_000) return `${(value / 1_000_000).toFixed(1)}M`;
  if (value >= 1_000)
    return `${(value / 1_000).toFixed(value >= 10_000 ? 0 : 1)}k`;
  return String(Math.round(value));
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
  const [renameTarget, setRenameTarget] = useState<SessionSummary | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);
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
  const activeSessionRef = useRef(sessionId);
  activeSessionRef.current = sessionId;
  const diagnosticsRevision = useRef(0);
  const navigationRevision = useRef(0);
  const sessionListRevision = useRef(0);
  const sessionListRequest = useRef<Promise<SessionSummary[] | null> | null>(
    null,
  );
  const sessionListValue = useRef<SessionSummary[] | null>(null);
  const draftBySession = useRef(new Map<string, string>());
  const transcriptRef = useRef<HTMLDivElement>(null);
  const followingRef = useRef(true);
  const [following, setFollowing] = useState(true);
  const [rows, setRows] = useState<Row[]>([]);
  const earlierPage = useRef<{
    rows: Row[];
    firstId: string;
    hasMore: boolean;
  } | null>(null);
  const [runtime, setRuntime] = useState<RuntimeState | null>(null);
  const [authProviders, setAuthProviders] = useState<AuthProviderStatus[]>([]);
  const [authProvider, setAuthProvider] = useState("");
  const [apiKeyDraft, setApiKeyDraft] = useState("");
  const [authMessage, setAuthMessage] = useState<{
    key: MessageKey;
    values: Record<string, string>;
  } | null>(null);
  const [historyData, setHistory] = useState<HistoryResponse | null>(null);
  const [queueItems, setQueueItems] = useState<QueueItem[]>([]);
  const [showQueueHistory, setShowQueueHistory] = useState(false);
  const [queueDraft, setQueueDraft] = useState("");
  const [queueKind, setQueueKind] = useState("followUp");
  const [draft, setDraft] = useState("");
  const [status, setStatus] = useState("connecting");
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
  const [language, setLanguage] = useState<Language>(() => detectLanguage());
  const [dialog, setDialog] = useState<UiDialog | null>(null);
  const dialogRequests = useRef(new Map<string, UiDialog>());
  const activeDialogId = useRef("");
  const [dialogAnswer, setDialogAnswer] = useState("");
  const [dialogBusy, setDialogBusy] = useState(false);

  const lastSeq = useRef(0);
  const noteSeq = useRef(0);
  const currentAssistant = useRef<string | null>(null);
  const currentUser = useRef<string | null>(null);
  const credentialRevision = useRef(0);
  const dialogRevision = useRef(0);
  const queueSubmission = useRef(new Set<string>());
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
  const current = sessions.find((session) => session.sessionId === sessionId);
  const sidebarSessions = useMemo(
    () =>
      sessions.map((entry) =>
        entry.sessionId === sessionId ? { ...entry, status } : entry,
      ),
    [sessions, sessionId, status],
  );
  const project = projects.find((entry) => entry.id === projectId);
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
                row.id !== `settled-${String(event.runId ?? seq)}`,
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
    draftBySession.current.clear();
  }, []);

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
      if (key === "k") {
        event.preventDefault();
        setSidebar(true);
        requestAnimationFrame(() =>
          document
            .querySelector<HTMLInputElement>(".sidebar-search input")
            ?.focus(),
        );
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
    void (async () => {
      try {
        const caps = await api.capabilities();
        if (cancelled) return;
        setCapabilities(caps);
        if (caps.supportedActions?.includes("projects")) {
          const result = await api.projects();
          if (cancelled) return;
          setProjects(result.projects);
          setProjectsRoot(result.projectsRoot ?? "");
          projectRef.current = result.defaultProjectId;
          setProjectId(result.defaultProjectId);
        }
        const list = await refreshSessions();
        if (cancelled || !list) return;
        const requested = new URLSearchParams(location.search).get("session");
        const reusable = list.filter(
          (session) => session.status !== "closed" && !session.archived,
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
    setRows([]);
    setRuntime(null);
    setHistory(null);
    setQueueItems([]);
    setAuthProviders([]);
    setAuthProvider("");
    setApiKeyDraft("");
    setAuthMessage(null);
    setQueueDraft("");
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
    setRunActive(false);
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
      setConnectionFailed(true);
      setError(errorText(cause));
    };
    const restoreSnapshotStatus = (
      snapshot: Awaited<ReturnType<typeof snapshotOf>>,
    ) => {
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
      // Sequential on purpose: reopen first (a bridge restart leaves only the
      // registry entry), then read state, then history, then follow live events.
      try {
        await api.openSession(sessionId);
        if (cancelled) return;
        void refreshSessions();
      } catch (cause) {
        failConnection(cause);
        return;
      }

      try {
        const state = await api.runtimeState(sessionId);
        if (cancelled) return;
        setRuntime(state);
        setRunActive(Boolean(state.activeOperation));
        setStatus(state.activeOperation ? "running" : "ready");
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
          capabilities.supportedCommands.includes(
            "get_available_thinking_levels",
          )
        ) {
          void api
            .thinkingLevels(sessionId)
            .then((data) => {
              if (!cancelled) setLevels(data.levels ?? []);
            })
            .catch(() => {});
        }
      } catch (cause) {
        failConnection(cause);
        return;
      }

      let after = 0;
      if (capabilities.supportedCommands.includes("get_messages")) {
        try {
          // Native truth for history; the events captured while the snapshot was
          // taken are applied in order, so `after` continues exactly where it ends.
          const snapshot = await snapshotOf(sessionId);
          if (cancelled) return;
          restoreSnapshotStatus(snapshot);
          setRows(rowsFromMessages(snapshot.messages));
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
        } catch (cause) {
          failConnection(cause);
          return;
        }
      }
      if (cancelled) return;
      connect(after);
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
    if (followingRef.current && transcriptRef.current)
      transcriptRef.current.scrollTop = transcriptRef.current.scrollHeight;
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
    return value.records.flatMap((record) => {
      if (record.type !== "operation_finished") return [];
      const outcome = String(record.outcome ?? "settled");
      const error = record.error as { message?: string } | null | undefined;
      const reason = error?.message ? ` · ${shortReason(error.message)}` : "";
      return [
        {
          kind: "note" as const,
          id: `settled-${String(record.runId ?? record.id)}`,
          text: `Run ${outcome}${reason} · restored from session history`,
          translation: {
            key: "restoredRun" as const,
            values: { outcome, reason },
          },
        },
      ];
    });
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
    if (value) {
      setRows((previous) => {
        const existing = new Set(previous.map((row) => row.id));
        return [
          ...previous,
          ...persistedSettlementRows(value).filter(
            (row) => !existing.has(row.id),
          ),
        ];
      });
    }
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
    setDraft("");
    draftBySession.current.delete(targetSession);
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
        const target = matchModel(models, arg);
        if (!target) {
          setPanel(true);
          note(
            arg
              ? language === "zh"
                ? `无法唯一确定模型“${arg}”，请使用 服务商/模型ID 或在运行时设置中选择。`
                : `No unique model matches "${arg}". Use provider/model-id or select it in Runtime.`
              : `Runtime lists ${models.length} models.`,
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
        draftBySession.current.set(targetSession, raw);
      if (navigation !== navigationRevision.current) return;
      setDraft((current) => current || raw);
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

  async function submit(event?: FormEvent) {
    event?.preventDefault();
    const message = draft.trim();
    if (!message || !sessionId || sendingRef.current) return;
    if (message.startsWith("/")) {
      void runSlash(message);
      return;
    }
    if (!supported.has("prompt")) return;
    const targetSession = sessionId;
    const navigation = navigationRevision.current;
    const submission = ++sendingRevision.current;
    const wasBusy = busy;
    sendingRef.current = true;
    setSending(true);
    setDraft("");
    draftBySession.current.delete(sessionId);
    setError("");
    setInterrupted(false);
    const key = `local-${crypto.randomUUID()}`;
    if (!wasBusy)
      setRows((previous) => [
        ...previous,
        { kind: "text", id: key, role: "user", text: message },
      ]);
    if (!wasBusy) {
      setStatus("running");
      setRunActive(true);
    }
    try {
      if (wasBusy) {
        if (!supported.has("queue_message")) throw new Error(t("runBusy"));
        await api.queueMessage(targetSession, message, "followUp");
        if (navigation === navigationRevision.current) {
          note(`${t("queued")}: ${message}`);
          void refreshDiagnostics(targetSession);
        }
      } else {
        await commandData(targetSession, "prompt", { message });
      }
    } catch (cause) {
      if (!draftBySession.current.get(targetSession))
        draftBySession.current.set(targetSession, message);
      if (navigation === navigationRevision.current) {
        if (!wasBusy) {
          setStatus((current) =>
            current === "reconnecting" ? current : "ready",
          );
          setRunActive(false);
        }
        setRows((previous) => previous.filter((row) => row.id !== key));
        setDraft((current) => current || message);
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
        const value = window.prompt(
          `${instructions}${url ? `\n\n${url}` : ""}`,
        );
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

  async function queuePrompt() {
    if (!sessionId || !queueDraft.trim() || !supported.has("queue_message"))
      return;
    if (queueSubmission.current.has(sessionId)) return;
    const message = queueDraft.trim();
    const draftRevision = queueDraftRevision.current;
    queueSubmission.current.add(sessionId);
    const navigation = navigationRevision.current;
    try {
      await api.queueMessage(sessionId, message, queueKind);
      if (navigation !== navigationRevision.current) return;
      if (queueDraftRevision.current === draftRevision) setQueueDraft("");
      await refreshSessionDiagnostics();
    } catch (cause) {
      if (navigation !== navigationRevision.current) return;
      setError(errorText(cause));
    } finally {
      queueSubmission.current.delete(sessionId);
    }
  }

  async function cancelQueued(entryId: string) {
    const navigation = navigationRevision.current;
    try {
      await api.cancelQueued(sessionId, entryId);
      if (navigation !== navigationRevision.current) return;
      await refreshSessionDiagnostics();
    } catch (cause) {
      if (navigation !== navigationRevision.current) return;
      setError(errorText(cause));
    }
  }

  async function forkCurrent(entryId?: string) {
    if (!sessionId || !actions.has("fork")) return;
    const navigation = navigationRevision.current;
    const defaultTitle = t("forkOf", {
      title: current?.title ?? sessionId.slice(0, 8),
    });
    const title = window.prompt(t("forkName"), defaultTitle);
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
    const title = window.prompt(
      t("cloneName"),
      t("cloneOf", { title: current?.title ?? sessionId.slice(0, 8) }),
    );
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
    if (!window.confirm(t("confirmRewind"))) return;
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
    setSessionId(id);
    setPanel(false);
    if (window.innerWidth < 768) setSidebar(false);
    const url = new URL(location.href);
    url.searchParams.set("session", id);
    history.replaceState(null, "", url);
  }

  function selectProject(selected: Project) {
    const next = sessions
      .filter((entry) => entry.projectId === selected.id && !entry.archived)
      .sort((a, b) => b.updatedAt - a.updatedAt)[0];
    if (next) select(next.sessionId, selected.id);
    else {
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
      if (window.innerWidth < 768) setSidebar(false);
    }
  }

  async function updateSessionMetadata(
    session: SessionSummary,
    patch: { pinned?: boolean; archived?: boolean },
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
          dialog={dialog}
          answer={dialogAnswer}
          busy={dialogBusy}
          language={language}
          onAnswer={setDialogAnswer}
          onSubmit={(value) => void answerDialog(value)}
          onCancel={() => void answerDialog(null, true)}
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
            <select
              id="settings-language"
              value={language}
              onChange={(event) => setLanguage(event.target.value as Language)}
            >
              <option value="zh">简体中文</option>
              <option value="en">English</option>
            </select>
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
                <select
                  className="language-select"
                  value={language}
                  aria-label={t("language")}
                  onChange={(event) =>
                    setLanguage(event.target.value as Language)
                  }
                >
                  <option value="en">{t("english")}</option>
                  <option value="zh">{t("chinese")}</option>
                </select>
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
            <div className="breadcrumb">
              <h1>{current?.title ?? t("newSession")}</h1>
              <span>·</span>
              <span>{project?.name ?? "rpi"}</span>
            </div>
          </div>
          <div className="toolbar-actions">
            {runtime?.modelId && (
              <button
                className="badge"
                onClick={() => {
                  setPanel(true);
                  setInspector(false);
                }}
                title={t("changeModel")}
              >
                <Cpu size={12} /> {runtime.modelId}
              </button>
            )}
            {normalizeLevel(runtime?.thinkingLevel) && (
              <button
                className="badge"
                onClick={() => {
                  setPanel(true);
                  setInspector(false);
                }}
                title={t("changeThinking")}
              >
                <Gauge size={12} />{" "}
                {valueLabel(language, normalizeLevel(runtime?.thinkingLevel))}
              </button>
            )}
            <span
              className={`connection ${status === "error" ? "offline" : ""}`}
            >
              <span className="status-dot" />{" "}
              {t(
                (status === "awaiting_input"
                  ? "waitingForAnswer"
                  : status) as Parameters<typeof translate>[1],
              )}
            </span>
            {actions.has("fork") && (
              <button
                disabled={!sessionId}
                className="icon-button"
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
                className="icon-button"
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
            className={`conversation-column ${rows.length === 0 ? "empty-conversation" : ""}`}
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

                {rows.length === 0 && (
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
                        onClick={() => setDraft(t("explainProjectPrompt"))}
                      >
                        <Sparkles size={17} /> {t("explainProject")}
                      </button>
                      <button
                        onClick={() => setDraft(t("reviewChangesPrompt"))}
                      >
                        <Code2 size={17} /> {t("reviewChanges")}
                      </button>
                      <button onClick={() => setDraft(t("findTodoPrompt"))}>
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
                className="jump-to-bottom"
                onClick={() => {
                  followingRef.current = true;
                  setFollowing(true);
                  transcriptRef.current?.scrollTo({
                    top: transcriptRef.current.scrollHeight,
                    behavior: "smooth",
                  });
                }}
              >
                {t("jumpToBottom")}
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
              <div className="composer">
                <textarea
                  value={draft}
                  onChange={(event) => {
                    setDraft(event.target.value);
                    draftBySession.current.set(sessionId, event.target.value);
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
                />
                <div className="composer-footer">
                  <div className="composer-options">
                    <button
                      type="button"
                      className="composer-project"
                      title={project?.path}
                      onClick={() => {
                        setSidebar(true);
                        requestAnimationFrame(() =>
                          document
                            .querySelector<HTMLElement>(
                              ".selected-project .project-name",
                            )
                            ?.focus(),
                        );
                      }}
                    >
                      <Folder size={12} /> {project?.name ?? t("workspace")}
                    </button>
                    {runtime?.modelId && (
                      <button
                        type="button"
                        className="composer-model"
                        onClick={() => {
                          setPanel(true);
                          setInspector(false);
                        }}
                      >
                        <span>{runtime.modelId}</span>
                        <ChevronDown size={11} />
                      </button>
                    )}
                    {normalizeLevel(runtime?.thinkingLevel) && (
                      <button
                        type="button"
                        className="composer-thinking"
                        onClick={() => {
                          setPanel(true);
                          setInspector(false);
                        }}
                      >
                        {valueLabel(
                          language,
                          normalizeLevel(runtime?.thinkingLevel),
                        )}
                        <ChevronDown size={11} />
                      </button>
                    )}
                    {busy && (
                      <span className="composer-queue-hint">
                        {t("queueHint")}
                      </span>
                    )}
                  </div>
                  <div className="composer-controls">
                    <button
                      type="button"
                      className="danger"
                      aria-label={t("abort")}
                      title={t("abort")}
                      onClick={() => void abort()}
                      disabled={!busy || !supported.has("abort")}
                    >
                      <CircleStop size={15} />
                    </button>
                    <button
                      className="send-button"
                      aria-label={t("send")}
                      disabled={!draft.trim() || !sessionId || sending}
                    >
                      <ArrowUp size={17} />
                    </button>
                  </div>
                </div>
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
              {runtime?.stats && (
                <>
                  <div className="stat-row">
                    <span>{t("tokens")}</span>
                    <strong>{formatTokens(runtime.stats.totalTokens)}</strong>
                  </div>
                  <div className="stat-row">
                    <span>{t("cost")}</span>
                    <strong>${runtime.stats.costTotal.toFixed(4)}</strong>
                  </div>
                  <div className="stat-row">
                    <span>{t("cacheHit")}</span>
                    <strong>
                      {runtime.stats.cacheHitRatio == null
                        ? "—"
                        : `${Math.round(runtime.stats.cacheHitRatio * 100)}%`}
                    </strong>
                  </div>
                  <div className="stats-breakdown">
                    <div className="muted">
                      {runtime.stats.provider ?? t("unknownProvider")} ·{" "}
                      {runtime.stats.modelId ?? t("unknownModel")}
                    </div>
                    {Object.entries(runtime.stats.operationCounts ?? {}).map(
                      ([name, value]) => {
                        const maximum = Math.max(
                          1,
                          ...Object.values(
                            runtime.stats?.operationCounts ?? {},
                          ),
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
                        queueItems.some((item) => item.status === "queued") && (
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
                      {item.status === "queued" && (
                        <button
                          className="icon-button"
                          title={t("cancelQueued")}
                          aria-label={t("cancelQueued")}
                          onClick={() => void cancelQueued(item.entryId)}
                        >
                          <X size={13} />
                        </button>
                      )}
                    </div>
                  ))}
                  {supported.has("queue_message") && (
                    <div className="queue-compose">
                      <input
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
                      <select
                        value={queueKind}
                        aria-label={t("queueKind")}
                        onChange={(event) => setQueueKind(event.target.value)}
                      >
                        <option value="followUp">{t("followUp")}</option>
                        <option value="steer">{t("steer")}</option>
                        <option value="nextTurn">{t("nextTurn")}</option>
                      </select>
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
  const freeform =
    Boolean(ui.allowFreeform ?? ui.freeform) || options.length === 0;
  return (
    <div className="dialog-backdrop" role="presentation">
      <form
        className="dialog-card"
        role="dialog"
        aria-modal="true"
        aria-labelledby="native-dialog-title"
        onSubmit={(event) => {
          event.preventDefault();
          if (freeform) onSubmit(answer);
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
                  onClick={() => onSubmit(value)}
                >
                  {option.title ?? String(value)}
                </button>
              );
            })}
          </div>
        )}
        {freeform && (
          <>
            <label htmlFor="dialog-answer">{t("answer")}</label>
            <textarea
              id="dialog-answer"
              value={answer}
              onChange={(event) => onAnswer(event.target.value)}
              autoFocus
              rows={4}
            />
            <div className="dialog-actions">
              <button type="button" onClick={onCancel} disabled={busy}>
                {t("cancel")}
              </button>
              <button
                type="submit"
                className="primary"
                disabled={busy || !answer.trim()}
              >
                {t("submit")}
              </button>
            </div>
          </>
        )}
      </form>
    </div>
  );
}

function RowView({ row, language }: { row: Row; language: Language }) {
  const t = (key: MessageKey) => translate(language, key);
  if (row.kind === "note") {
    const values = row.translation?.values;
    return (
      <p className="transcript-note muted">
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
    const detail = row.result ?? row.args;
    return (
      <details
        className={`tool-card tool-${row.status} ${row.status === "error" ? "failed" : ""}`}
        open={row.status === "error"}
      >
        <summary>
          <Wrench size={14} />
          <strong>{row.name}</strong>
          {row.timing && (
            <ToolElapsed timing={row.timing} language={language} />
          )}
          <span className="tool-status">
            {toolStatusLabel(row.status, language)}
          </span>
        </summary>
        <div className="tool-detail">
          {row.args !== undefined && (
            <>
              <h4>{t("input")}</h4>
              <pre>{safeJson(row.args)}</pre>
            </>
          )}
          {row.result !== undefined && (
            <>
              <h4>{t("result")}</h4>
              <pre>{safeJson(row.result)}</pre>
            </>
          )}
          {detail === undefined && <pre className="muted">{t("noOutput")}</pre>}
        </div>
      </details>
    );
  }

  return (
    <article className={`message ${row.role}`}>
      <div className="message-heading">
        {row.role === "assistant" ? (
          <Mark />
        ) : (
          <span className="user-avatar">U</span>
        )}
        <strong>{row.role === "assistant" ? "rpi" : t("you")}</strong>
      </div>
      {row.role === "assistant" ? (
        <MarkdownBody text={row.text} language={language} />
      ) : (
        <div className="prose plain-message">
          <p>{row.text}</p>
        </div>
      )}
    </article>
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
      <select
        id="model"
        value={
          runtime?.modelId
            ? modelKey({ id: runtime.modelId, provider: runtime.provider })
            : ""
        }
        disabled={
          !supported.has("set_model") || pending !== "" || models.length === 0
        }
        onChange={(event) => {
          const next = models.find(
            (entry) => modelKey(entry) === event.target.value,
          );
          if (!next) return;
          void run("model", async () => {
            await commandData(sessionId, "set_model", {
              modelId: next.id,
              provider: next.provider,
            });
            return api.runtimeState(sessionId);
          });
        }}
      >
        <option value="">—</option>
        {models.map((entry) => (
          <option key={modelKey(entry)} value={modelKey(entry)}>
            {entry.name} ({entry.provider})
          </option>
        ))}
      </select>
      {model?.contextWindow ? (
        <p className="muted">
          {t("context")} {Math.round(model.contextWindow / 1000)}k
        </p>
      ) : null}

      <label htmlFor="thinking">{zh ? "思考深度" : "Thinking level"}</label>
      <select
        id="thinking"
        value={level}
        disabled={
          !supported.has("set_thinking_level") ||
          pending !== "" ||
          levels.length === 0
        }
        onChange={(event) =>
          void run("thinking", async () => {
            const next = event.target.value;
            await commandData(sessionId, "set_thinking_level", { level: next });
            return api.runtimeState(sessionId);
          })
        }
      >
        <option value="">—</option>
        {levels.map((entry) => (
          <option key={entry} value={entry}>
            {valueLabel(language, entry)}
          </option>
        ))}
      </select>

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
