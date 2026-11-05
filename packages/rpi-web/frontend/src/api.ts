export interface Capabilities {
  protocol: string;
  piCompatibilityTarget?: string;
  supportedCommands: string[];
  supportedActions?: string[];
  semantics?: Record<string, unknown>;
  limits?: Record<string, unknown>;
  parityComplete: boolean;
}

export interface HostResponse<T = Record<string, unknown>> {
  type: "response";
  id?: string | number;
  command: string;
  success: boolean;
  data?: T;
  error?: string;
}

export interface SessionSummary {
  sessionId: string;
  projectId?: string;
  pinned?: boolean;
  archived?: boolean;
  deleting?: boolean;
  tags?: string[];
  title: string | null;
  status: string;
  createdAt: number;
  updatedAt: number;
  lastSeq: number;
  error: string | null;
  parentSessionId?: string | null;
  relation?: "fork" | "clone" | string | null;
  forkEntryId?: string | null;
  latestSettlement?: {
    outcome?: string;
    pendingResume?: boolean;
    error?: string | null;
    runId?: string;
    seq?: number;
  } | null;
}

export interface SessionListFilters {
  q?: string;
  projectId?: string;
  tag?: string;
  status?: string;
  archived?: boolean;
  pinned?: boolean;
}
export interface AuthState {
  required: boolean;
  authenticated: boolean;
}

export interface Project {
  id: string;
  name: string;
  path: string;
  archived?: boolean;
}
export interface FileEntry {
  name: string;
  path: string;
  kind: "directory" | "file";
  size: number;
}
export interface UploadedFile {
  name: string;
  path: string;
  size: number;
}
export interface ChangedFile {
  path: string;
  originalPath?: string | null;
  status: string;
  staged: boolean;
  unstaged: boolean;
}
export interface GitStatus {
  isRepository: boolean;
  branch: string | null;
  isDetached?: boolean;
  isWorktree?: boolean | null;
  repositoryRoot?: string | null;
  worktreePath?: string | null;
  files: ChangedFile[];
}

export interface AuthProviderStatus {
  provider: string;
  ready: boolean;
  source: string;
  oauth: boolean;
  oauthAvailable: boolean;
}
export interface ContextUsage {
  tokens: number;
  window: number;
  threshold: number;
  percent: number;
  reserveTokens: number;
  keepRecentTokens: number;
  compactionEnabled: boolean;
  estimated: boolean;
  trailingTokens: number;
}

export interface RuntimeQueue {
  steering?: string[];
  followUp?: string[];
  steeringCount?: number;
  followUpCount?: number;
}

export interface RuntimeStats {
  messageCount: number;
  cachedTokens: number;
  uncachedTokens: number;
  totalTokens: number;
  inputTokens?: number | null;
  outputTokens?: number | null;
  input?: number | null;
  output?: number | null;
  cacheRead?: number | null;
  cacheWrite?: number | null;
  promptTokens?: number | null;
  completionTokens?: number | null;
  cost?: number | null;
  provider?: string | null;
  modelId?: string | null;
  operationCount?: number;
  retryCount?: number;
  compactionCount?: number;
  resumeCount?: number;
  operationCounts?: Record<string, number>;
  costTotal: number;
  cacheHitRatio?: number | null;
}

export interface QueueItem {
  entryId: string;
  queue: string;
  text: string;
  status?: "queued" | "consumed" | "cancelled" | string;
  completedSeq?: number | null;
  message?: unknown;
  runId?: string | null;
  seq?: number;
}

export interface HistoryResponse {
  records: Record<string, unknown>[];
  total: number;
  limit: number;
  afterSeq?: number | null;
  beforeSeq?: number | null;
  firstSeq?: number | null;
  lastSeq?: number | null;
  hasMore: boolean;
  leafId?: string | null;
  openOperations?: Record<string, unknown>[];
}

export interface RuntimeState {
  modelId?: string | null;
  context?: ContextUsage | null;
  provider?: string | null;
  thinkingLevel?: string | null;
  activeTools?: string[];
  leafId?: string | null;
  sessionId?: string | null;
  activeOperation?: Record<string, unknown> | null;
  queue?: RuntimeQueue | null;
  stats?: RuntimeStats | null;
}

export interface ModelDescriptor {
  id: string;
  name: string;
  provider: string;
  reasoning?: boolean;
  api?: string;
  baseUrl?: string;
  input?: string[];
  contextWindow?: number;
  maxTokens?: number;
}

export interface ToolCatalog {
  tools: string[];
  activeTools: string[];
}

export interface UiDialogPayload {
  purpose?: string;
  question?: string;
  message?: string;
  context?: string;
  kind?: string;
  options?: { title?: string; value?: unknown; permission?: string }[];
  allowFreeform?: boolean;
  freeform?: boolean;
}
export interface RunTiming {
  runId: string;
  lane: string;
  finalEntryId: string;
  startedAt: number;
  finishedAt: number;
  elapsedMs: number;
  outcome: "completed" | "aborted" | "failed" | "declined";
}
export interface RunTimingsPage {
  timings: RunTiming[];
  complete: boolean;
  cursor: string | null;
  scannedBytes: number;
  totalBytes: number;
}

export interface MessageSummary {
  entryId: string;
  role: "user";
  preview: string;
  timestamp: number;
  hasImages: boolean;
}
export interface MessageSummaryPage {
  filtered: true;
  role: "user";
  messages: MessageSummary[];
  firstId: string | null;
  lastId: string | null;
  nextBeforeId: string | null;
  hasMore: boolean;
  leafId: string | null;
}
export interface MessageWindowPage {
  messages: unknown[];
  firstId: string | null;
  lastId: string | null;
  nextBeforeId: string | null;
  nextAfterId: string | null;
  hasMoreBefore: boolean;
  hasMoreAfter: boolean;
  leafId: string | null;
}
export interface SnapshotFrame {
  supportedCommands?: string[] | null;
  sessionId: string;
  status?: string;
  messages: unknown[];
  total: number;
  truncated: boolean;
  lastSeq: number;
  leafId?: string | null;
  pendingResume?: boolean;
  pendingDialog?: {
    requestId: string;
    toolCallId?: string;
    ui: UiDialogPayload;
  } | null;
  pendingDialogs?: NonNullable<SnapshotFrame["pendingDialog"]>[];
  latestSettlement?: Record<string, unknown> | null;
  firstId?: string | null;
  lastId?: string | null;
  hasMore?: boolean;
  history?: HistoryResponse;
  events: { seq: number; event: SeqEvent }[];
}

export type ToolProgramDirection = "oldestFirst" | "newestFirst";
export interface ToolProgramCursor {
  sessionId: string;
  lane: string;
  leafId: string;
  direction: ToolProgramDirection;
  childId?: string | null;
  afterEntryId: string;
  afterSeq: number;
}
export interface ToolProgramChildSummary {
  version: number;
  childId: string;
  seq: number;
  origin: {
    runId: string;
    assistantEntryId: string;
    toolIndex: number;
  };
  originVerification: "nativeCheckpoint" | "forkCopy";
  rootCallId: string;
  rootToolName: string;
  toolName: string;
  ordinal: number;
  parentChildId?: string | null;
  state: "unknown";
}
export type ToolProgramPhaseKind =
  | "requested"
  | "prepared"
  | "update"
  | "final"
  | "closed";
export interface ToolProgramBodyDescriptor {
  descriptorId: string;
  version: number;
  bodyId: string;
  childId: string;
  kind: "requested" | "prepared" | "update" | "final";
  codec: string;
  encoding: "base64";
  byteLength: number;
  chunkBytes: number;
  chunks: number;
  sha256: string;
}
export interface ToolProgramPhase {
  entryId: string;
  seq: number;
  version: number;
  childId: string;
  index: number;
  phase: ToolProgramPhaseKind;
  descriptor?: ToolProgramBodyDescriptor | null;
  isError?: boolean | null;
  cancelled?: boolean | null;
  failed?: boolean | null;
  interrupted?: boolean | null;
}
export interface ToolProgramPhasePage {
  leafId: string;
  childId: string;
  phases: ToolProgramPhase[];
  nextCursor?: ToolProgramCursor | null;
  scanned: number;
  scanExhausted: boolean;
  state: "unknown";
}
export interface ToolProgramObservation {
  childId: string;
  latestPhase?: ToolProgramPhaseKind | null;
  finalPhase?: ToolProgramPhase | null;
  closedPhase?: ToolProgramPhase | null;
  outcome:
    | "unknown"
    | "finalObserved"
    | "succeeded"
    | "failed"
    | "cancelled"
    | "interrupted";
}
export interface ToolProgramSnapshot {
  sessionId: string;
  lane: string;
  page: {
    leafId?: string | null;
    children: ToolProgramChildSummary[];
    nextCursor?: ToolProgramCursor | null;
    scanExhausted: boolean;
  };
  phases: ToolProgramPhasePage[];
  observations: ToolProgramObservation[];
  startSeq: number;
  lastSeq: number;
  events: unknown[];
  resyncRequired: boolean;
}
export interface ToolProgramBodyChunk {
  index: number;
  byteOffset: number;
  byteLength: number;
  data: string;
  sha256: string;
}
export interface ToolProgramBodyPage {
  descriptor: ToolProgramBodyDescriptor;
  leafId: string;
  byteOffset: number;
  byteLength: number;
  totalBytes: number;
  chunks: ToolProgramBodyChunk[];
  nextCursor?: ToolProgramBodyCursor["body"] | null;
}
export interface ToolProgramBodyCursor {
  direction: "oldestFirst";
  childId: string;
  phaseEntryId: string;
  body: { sessionId: string; lane: string; leafId: string; descriptorId: string; nextIndex: number };
}
export interface ToolProgramBodyResponse {
  childId: string;
  phaseEntryId: string;
  body: ToolProgramBodyPage;
  nextCursor?: ToolProgramBodyCursor | null;
}
export interface ToolProgramChildEvent {
  type: "tool_program_child_begin" | "tool_program_child_phase" | "tool_program_child_resync_required";
  [key: string]: unknown;
}
export interface ToolProgramStreamItem {
  type: "update" | "resyncRequired";
  seq: number;
  event?: ToolProgramChildEvent;
  observation?: ToolProgramObservation | null;
  disposition?: "applied" | "duplicate" | "outsideWindow" | "resyncRequired";
  reason?: string;
}

export interface SeqEvent {
  seq: number;
  type: string;
  [key: string]: unknown;
}

export class ApiError extends Error {
  constructor(
    message: string,
    readonly status: number,
    readonly code?: string,
  ) {
    super(message);
  }
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(`/api/v1${path}`, {
    ...init,
    headers: {
      "x-rpi-web": "1",
      ...(init?.body && !(init.body instanceof FormData)
        ? { "content-type": "application/json" }
        : {}),
      ...init?.headers,
    },
  });
  if (response.status === 204) return undefined as T;
  const value: unknown = await response.json().catch((cause) => {
    if (init?.signal?.aborted) throw cause;
    return null;
  });
  if (!response.ok) {
    const error = value as { error?: unknown; code?: unknown } | null;
    throw new ApiError(
      typeof error?.error === "string" ? error.error : `HTTP ${response.status}`,
      response.status,
      typeof error?.code === "string" ? error.code : undefined,
    );
  }
  return value as T;
}

export function sendCommand<T = Record<string, unknown>>(
  sessionId: string,
  type: string,
  payload: Record<string, unknown> = {},
  signal?: AbortSignal,
): Promise<HostResponse<T>> {
  return request<HostResponse<T>>(`/sessions/${sessionId}/commands`, {
    method: "POST",
    body: JSON.stringify({ type, ...payload }),
    signal,
  });
}

/** Unwrap a host response, turning `success: false` into a thrown error. */
export async function commandData<T>(
  sessionId: string,
  type: string,
  payload: Record<string, unknown> = {},
  signal?: AbortSignal,
): Promise<T> {
  const response = await sendCommand<T>(sessionId, type, payload, signal);
  if (!response.success) throw new Error(response.error ?? `${type} failed`);
  return (response.data ?? {}) as T;
}

const gitRequests = new Map<string, Promise<GitStatus>>();
function gitStatus(projectId: string) {
  const existing = gitRequests.get(projectId);
  if (existing) return existing;
  const job = request<GitStatus>(`/projects/${projectId}/git/status`).finally(() => {
    if (gitRequests.get(projectId) === job) gitRequests.delete(projectId);
  });
  gitRequests.set(projectId, job);
  return job;
}

export const api = {
  projects: () =>
    request<{
      projects: Project[];
      defaultProjectId: string;
      projectsRoot?: string;
    }>("/projects"),
  projectFolders: (path = "") =>
    request<{
      root: string;
      path: string;
      folders: { name: string; path: string }[];
    }>(`/project-folders?${new URLSearchParams({ path })}`),
  createProject: (name: string, initGit: boolean, path?: string) =>
    request<{ project: Project }>("/projects", {
      method: "POST",
      body: JSON.stringify({ name, initGit, ...(path ? { path } : {}) }),
    }),
  renameProject: (id: string, name: string) =>
    request<{ project: Project }>(`/projects/${id}/rename`, {
      method: "POST",
      body: JSON.stringify({ name }),
    }),
  projectMetadata: (id: string, metadata: { archived: boolean }) =>
    request<{ project: Project }>(`/projects/${id}/metadata`, {
      method: "POST",
      body: JSON.stringify(metadata),
    }),
  sessionMetadata: (
    id: string,
    metadata: { pinned?: boolean; archived?: boolean; tags?: string[] },
  ) =>
    request<{
      sessionId: string;
      pinned: boolean;
      archived: boolean;
      tags: string[];
    }>(
      `/sessions/${id}/metadata`,
      { method: "POST", body: JSON.stringify(metadata) },
    ),
  files: (projectId: string, path = "") =>
    request<{ path: string; entries: FileEntry[] }>(
      `/projects/${projectId}/files?${new URLSearchParams({ path })}`,
    ),
  file: (projectId: string, path: string, signal?: AbortSignal) =>
    request<{ path: string; content: string }>(
      `/projects/${projectId}/file?${new URLSearchParams({ path })}`,
      { signal },
    ),
  upload: (projectId: string, file: File, signal?: AbortSignal) => {
    const body = new FormData();
    body.append("file", file, file.name);
    return request<{ name: string; path: string; size: number }>(
      `/projects/${projectId}/upload`,
      { method: "POST", body, signal },
    );
  },
  gitStatus: (projectId: string) => gitStatus(projectId),
  diff: (projectId: string, path: string, staged: boolean) =>
    request<{ path: string; patch: string }>(
      `/projects/${projectId}/git/diff?${new URLSearchParams({ path, staged: String(staged) })}`,
    ),
  auth: () => request<AuthState>("/auth"),
  login: (password: string) =>
    request<AuthState>("/auth", {
      method: "POST",
      headers: { "x-rpi-web": "1" },
      body: JSON.stringify({ password }),
    }),
  logout: () =>
    request<AuthState>("/auth", {
      method: "DELETE",
      headers: { "x-rpi-web": "1" },
    }),
  capabilities: () => request<Capabilities>("/capabilities"),
  refreshAuth: (provider: string) =>
    request<{ provider: string; refreshed: boolean }>("/auth/refresh", {
      method: "POST",
      body: JSON.stringify({ provider }),
    }),

  listSessions: (filters: SessionListFilters = {}) => {
    const params = new URLSearchParams();
    for (const [key, value] of Object.entries(filters)) {
      if (value !== undefined && value !== "") params.set(key, String(value));
    }
    const query = params.toString();
    return request<{ sessions: SessionSummary[] }>(
      `/sessions${query ? `?${query}` : ""}`,
    );
  },
  createSession: (projectId?: string) =>
    request<{ sessionId: string; status: string }>("/sessions", {
      method: "POST",
      body: JSON.stringify({ projectId }),
    }),
  deleteSession: (id: string) =>
    request<void>(`/sessions/${id}`, { method: "DELETE" }),
  getSession: (id: string) => request<SessionSummary>(`/sessions/${id}`),
  snapshot: (
    id: string,
    params: { limit?: number; includeHistory?: boolean; signal?: AbortSignal } = {},
  ) => {
    const query = new URLSearchParams();
    if (params.limit !== undefined) query.set("limit", String(params.limit));
    if (params.includeHistory !== undefined)
      query.set("includeHistory", String(params.includeHistory));
    const suffix = query.toString() ? `?${query}` : "";
    return request<SnapshotFrame>(`/sessions/${id}/snapshot${suffix}`, {
      signal: params.signal,
    });
  },
  runTimings: (id: string, entryIds: string[], cursor?: string, signal?: AbortSignal) =>
    request<RunTimingsPage>(`/sessions/${id}/run-timings/query`, {
      method: "POST",
      body: JSON.stringify({ entryIds, ...(cursor ? { cursor } : {}) }),
      signal,
    }),
  messageSummaries: (id: string, params: { beforeId?: string; leafId?: string; limit?: number } = {}, signal?: AbortSignal) => {
    const query = new URLSearchParams({ role: "user", limit: String(params.limit ?? 10) });
    if (params.beforeId) query.set("beforeId", params.beforeId);
    if (params.leafId) query.set("leafId", params.leafId);
    return request<MessageSummaryPage>(`/sessions/${id}/message-summaries?${query}`, { signal });
  },
  messageWindow: (id: string, params: { anchorId?: string; beforeId?: string; afterId?: string; leafId?: string; limit?: number }, signal?: AbortSignal) => {
    const query = new URLSearchParams({ limit: String(params.limit ?? 48) });
    for (const key of ["anchorId", "beforeId", "afterId", "leafId"] as const)
      if (params[key]) query.set(key, params[key]);
    return request<MessageWindowPage>(`/sessions/${id}/message-window?${query}`, { signal });
  },
  toolProgramChildren: (
    id: string,
    query: {
      leafId?: string;
      cursor?: ToolProgramCursor;
      direction?: ToolProgramDirection;
      limit?: number;
      maxBytes?: number;
    } = {},
    signal?: AbortSignal,
  ) =>
    request<ToolProgramSnapshot["page"]>(
      `/sessions/${id}/tool-program/children/query`,
      {
        method: "POST",
        body: JSON.stringify({
          leafId: query.leafId ?? null,
          cursor: query.cursor ?? null,
          direction: query.direction ?? "newestFirst",
          limit: query.limit ?? 8,
          maxBytes: query.maxBytes ?? 32768,
        }),
        signal,
      },
    ),
  toolProgramPhases: (
    id: string,
    query: {
      childId: string;
      leafId?: string;
      cursor?: ToolProgramCursor;
      direction?: ToolProgramDirection;
      limit?: number;
      maxBytes?: number;
    },
    signal?: AbortSignal,
  ) =>
    request<ToolProgramPhasePage>(
      `/sessions/${id}/tool-program/phases/query`,
      {
        method: "POST",
        body: JSON.stringify({
          ...query,
          leafId: query.leafId ?? null,
          cursor: query.cursor ?? null,
          direction: query.direction ?? "newestFirst",
          limit: query.limit ?? 8,
          maxBytes: query.maxBytes ?? 32768,
        }),
        signal,
      },
    ),
  toolProgramBody: (
    id: string,
    query: {
      childId: string;
      phaseEntryId: string;
      descriptorId: string;
      leafId?: string;
      cursor?: ToolProgramBodyCursor;
      limit?: number;
      maxBytes?: number;
    },
    signal?: AbortSignal,
  ) =>
    request<ToolProgramBodyResponse>(
      `/sessions/${id}/tool-program/body/query`,
      {
        method: "POST",
        body: JSON.stringify({
          ...query,
          leafId: query.leafId ?? null,
          cursor: query.cursor ?? null,
          limit: query.limit ?? 8,
          maxBytes: query.maxBytes ?? 65536,
        }),
        signal,
      },
    ),
  toolProgramSnapshot: (
    id: string,
    signal?: AbortSignal,
  ) =>
    request<ToolProgramSnapshot>(`/sessions/${id}/tool-program/snapshot`, {
      method: "POST",
      body: JSON.stringify({
        children: {
          leafId: null,
          cursor: null,
          direction: "newestFirst",
          limit: 8,
          maxBytes: 131072,
        },
        phaseLimit: 8,
      }),
      signal,
    }),
  toolProgramEventsUrl: (id: string) =>
    `/api/v1/sessions/${id}/tool-program/events`,
  messages: (id: string, params: { beforeId?: string; limit?: number }) =>
    commandData<{
      messages: unknown[];
      total: number;
      hasMore: boolean;
      firstId?: string | null;
    }>(id, "get_messages", { ...params }),
  history: (
    id: string,
    params: { afterSeq?: number; beforeSeq?: number; limit?: number } = {},
    signal?: AbortSignal,
  ) => commandData<HistoryResponse>(id, "get_history", params, signal),
  authStatus: (id: string, signal?: AbortSignal) =>
    commandData<{ providers: AuthProviderStatus[] }>(id, "get_auth_status", {}, signal),
  oauth: (id: string, provider: string, action: string, input = {}) =>
    commandData<{
      provider: string;
      status: string;
      display?: Record<string, unknown>;
    }>(id, "oauth", { provider, action, input }),
  setApiKey: (id: string, provider: string, apiKey: string, signal?: AbortSignal) =>
    commandData<{ provider: string; ready: boolean; requiresRestart: boolean }>(
      id,
      "set_api_key",
      { provider, apiKey },
      signal,
    ),
  getProviderModels: (
    id: string,
    input: { api: string; baseUrl: string; apiKey: string },
    signal?: AbortSignal,
  ) =>
    commandData<{
      models: Array<{
        id: string;
        ownedBy?: string | null;
        contextWindow?: number | null;
        maxTokens?: number | null;
        supportsImages?: boolean | null;
      }>;
    }>(id, "get_provider_models", input, signal),
  testProviderConfig: (
    id: string,
    input: { api: string; baseUrl: string; apiKey: string },
    signal?: AbortSignal,
  ) =>
    commandData<{
      ok: boolean;
      status: number;
      latencyMs: number;
      modelCount?: number | null;
    }>(id, "test_provider_config", input, signal),
  updateProviderConfig: (
    id: string,
    input: {
      provider: string;
      api: string;
      baseUrl: string;
      apiKey?: string;
      modelId: string;
      modelName?: string;
      contextWindow?: number;
      maxOutputTokens?: number;
      supportsImages: boolean;
    },
    signal?: AbortSignal,
  ) =>
    commandData<{
      provider: string;
      modelId: string;
      requiresRestart: boolean;
    }>(id, "update_provider_config", input, signal),
  deleteProviderConfig: (id: string, provider: string, signal?: AbortSignal) =>
    commandData<{ provider: string; requiresRestart: boolean }>(
      id,
      "delete_provider_config",
      { provider },
      signal,
    ),
  setProviderConfig: (
    id: string,
    input: {
      provider: string;
      api: string;
      baseUrl: string;
      apiKey: string;
      modelId: string;
      modelName?: string;
      contextWindow?: number;
      maxOutputTokens?: number;
      supportsImages?: boolean;
    },
    signal?: AbortSignal,
  ) =>
    commandData<{
      provider: string;
      modelId: string;
      api: string;
      requiresRestart: boolean;
    }>(id, "set_provider_config", input, signal),

  queue: (id: string, includeCompleted = false, signal?: AbortSignal) =>
    commandData<{ items: QueueItem[]; count: number }>(id, "get_queue", {
      includeCompleted,
    }, signal),
  queueMessage: (id: string, message: string, deliverAs: string) =>
    commandData<{ accepted: boolean; entryId: string; queue: string }>(
      id,
      "queue_message",
      { message, deliverAs },
    ),
  cancelQueued: (id: string, entryId: string) =>
    commandData<{ entryId: string; outcome: string }>(id, "cancel_queued", {
      entryId,
    }),
  clearQueue: (id: string) =>
    commandData<Record<string, unknown>>(id, "clear_queue"),
  navigate: (id: string, targetId?: string) =>
    commandData<Record<string, unknown>>(id, "navigate", { targetId }),
  fork: (id: string, entryId?: string, title?: string) =>
    request<{ sessionId: string; sourceSessionId: string; status: string }>(
      `/sessions/${id}/fork`,
      {
        method: "POST",
        body: JSON.stringify({
          ...(entryId ? { entryId } : {}),
          ...(title ? { title } : {}),
        }),
      },
    ),
  clone: (id: string, title?: string) =>
    request<{ sessionId: string; sourceSessionId: string; status: string }>(
      `/sessions/${id}/clone`,
      {
        method: "POST",
        ...(title ? { body: JSON.stringify({ title }) } : {}),
      },
    ),
  rename: (id: string, title: string) =>
    request<{ sessionId: string; title: string }>(`/sessions/${id}/rename`, {
      method: "POST",
      body: JSON.stringify({ title }),
    }),
  openSession: (id: string, signal?: AbortSignal) =>
    request<SessionSummary>(`/sessions/${id}/open`, { method: "POST", signal }),
  runtimeState: (id: string, signal?: AbortSignal) =>
    commandData<RuntimeState>(id, "get_state", {}, signal),
  stats: (id: string, signal?: AbortSignal) => commandData<unknown>(id, "get_stats", {}, signal),
  models: (id: string, signal?: AbortSignal) =>
    commandData<{ models: ModelDescriptor[] }>(id, "get_available_models", {}, signal),
  thinkingLevels: (id: string, signal?: AbortSignal) =>
    commandData<{ levels: string[] }>(id, "get_available_thinking_levels", {}, signal),
  tools: (id: string) => commandData<ToolCatalog>(id, "get_available_tools"),
};

export function extractText(value: unknown): string {
  if (!value || typeof value !== "object") return "";
  const content = (value as { content?: unknown }).content;
  if (typeof content === "string") return content;
  if (!Array.isArray(content)) return "";
  return content
    .map((block) => {
      if (!block || typeof block !== "object" || !("text" in block)) return "";
      const typed = block as { type?: unknown; text?: unknown };
      if (typed.type !== "text") return "";
      return String(typed.text ?? "");
    })
    .filter(Boolean)
    .join("\n\n");
}

export function roleOf(value: unknown): string {
  if (!value || typeof value !== "object") return "assistant";
  const role = (value as { role?: unknown }).role;
  return typeof role === "string" ? role : "assistant";
}
