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
    error?: string | null;
    runId?: string;
    seq?: number;
  } | null;
}

export interface AuthState {
  required: boolean;
  authenticated: boolean;
}

export interface Project {
  id: string;
  name: string;
  path: string;
}
export interface FileEntry {
  name: string;
  path: string;
  kind: "directory" | "file";
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
export interface SnapshotFrame {
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

export interface SeqEvent {
  seq: number;
  type: string;
  [key: string]: unknown;
}

export class ApiError extends Error {
  constructor(
    message: string,
    readonly status: number,
  ) {
    super(message);
  }
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(`/api/v1${path}`, {
    ...init,
    headers: {
      "x-rpi-web": "1",
      ...(init?.body ? { "content-type": "application/json" } : {}),
      ...init?.headers,
    },
  });
  if (response.status === 204) return undefined as T;
  const value: unknown = await response.json().catch(() => null);
  if (!response.ok) {
    const error = value as { error?: string } | null;
    throw new ApiError(
      error?.error ?? `HTTP ${response.status}`,
      response.status,
    );
  }
  return value as T;
}

export function sendCommand<T = Record<string, unknown>>(
  sessionId: string,
  type: string,
  payload: Record<string, unknown> = {},
): Promise<HostResponse<T>> {
  return request<HostResponse<T>>(`/sessions/${sessionId}/commands`, {
    method: "POST",
    body: JSON.stringify({ type, ...payload }),
  });
}

/** Unwrap a host response, turning `success: false` into a thrown error. */
export async function commandData<T>(
  sessionId: string,
  type: string,
  payload: Record<string, unknown> = {},
): Promise<T> {
  const response = await sendCommand<T>(sessionId, type, payload);
  if (!response.success) throw new Error(response.error ?? `${type} failed`);
  return (response.data ?? {}) as T;
}

export const api = {
  projects: () =>
    request<{
      projects: Project[];
      defaultProjectId: string;
      projectsRoot?: string;
    }>("/projects"),
  createProject: (name: string, initGit: boolean, path?: string) =>
    request<{ project: Project }>("/projects", {
      method: "POST",
      body: JSON.stringify({ name, initGit, ...(path ? { path } : {}) }),
    }),
  sessionMetadata: (
    id: string,
    metadata: { pinned?: boolean; archived?: boolean },
  ) =>
    request<{ sessionId: string; pinned: boolean; archived: boolean }>(
      `/sessions/${id}/metadata`,
      { method: "POST", body: JSON.stringify(metadata) },
    ),
  files: (projectId: string, path = "") =>
    request<{ path: string; entries: FileEntry[] }>(
      `/projects/${projectId}/files?${new URLSearchParams({ path })}`,
    ),
  file: (projectId: string, path: string) =>
    request<{ path: string; content: string }>(
      `/projects/${projectId}/file?${new URLSearchParams({ path })}`,
    ),
  gitStatus: (projectId: string) =>
    request<GitStatus>(`/projects/${projectId}/git/status`),
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

  listSessions: () => request<{ sessions: SessionSummary[] }>("/sessions"),
  createSession: (projectId?: string) =>
    request<{ sessionId: string; status: string }>("/sessions", {
      method: "POST",
      body: JSON.stringify({ projectId }),
    }),
  deleteSession: (id: string) =>
    request<void>(`/sessions/${id}`, { method: "DELETE" }),
  getSession: (id: string) => request<SessionSummary>(`/sessions/${id}`),
  snapshot: (id: string) => request<SnapshotFrame>(`/sessions/${id}/snapshot`),
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
  ) => commandData<HistoryResponse>(id, "get_history", params),
  authStatus: (id: string) =>
    commandData<{ providers: AuthProviderStatus[] }>(id, "get_auth_status"),
  oauth: (id: string, provider: string, action: string, input = {}) =>
    commandData<{
      provider: string;
      status: string;
      display?: Record<string, unknown>;
    }>(id, "oauth", { provider, action, input }),
  setApiKey: (id: string, provider: string, apiKey: string) =>
    commandData<{ provider: string; ready: boolean; requiresRestart: boolean }>(
      id,
      "set_api_key",
      { provider, apiKey },
    ),

  queue: (id: string, includeCompleted = false) =>
    commandData<{ items: QueueItem[]; count: number }>(id, "get_queue", {
      includeCompleted,
    }),
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
  openSession: (id: string) =>
    request<SessionSummary>(`/sessions/${id}/open`, { method: "POST" }),
  runtimeState: (id: string) => commandData<RuntimeState>(id, "get_state"),
  models: (id: string) =>
    commandData<{ models: ModelDescriptor[] }>(id, "get_available_models"),
  thinkingLevels: (id: string) =>
    commandData<{ levels: string[] }>(id, "get_available_thinking_levels"),
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
