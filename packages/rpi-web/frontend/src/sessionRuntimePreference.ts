// Per-session memory of the model/thinking this browser last used in a
// session, so a reopened native host (idle retirement, crash) or a later page
// load can restore the session's last round instead of the startup default.
// Distinct from `modelPreference.ts`, which is a per-project default applied
// to NEW sessions only.
export interface SessionRuntimeChoice {
  provider?: string;
  modelId?: string;
  thinkingLevel?: string;
}

const fallback = new Map<string, SessionRuntimeChoice | null>();
const unpersisted = new Set<string>();
// Sessions are cheap records; keep the store bounded so long-lived browsers
// do not accumulate unbounded entries.
const MAX_SESSIONS = 100;
const PREFIX = "rpi-web-session-runtime:";
// localStorage is origin-scoped; include the origin in the memory fallback too.
const key = (sessionId: string) =>
  `${PREFIX}${encodeURIComponent(location.origin)}:${encodeURIComponent(sessionId)}`;

export function readSessionRuntimePreference(
  sessionId: string,
): SessionRuntimeChoice | null {
  if (!sessionId) return null;
  const storageKey = key(sessionId);
  // A failed write must win over any stale persisted value in this tab.
  // Otherwise read storage afresh so other tabs' updates are honored.
  if (unpersisted.has(storageKey)) return fallback.get(storageKey) ?? null;
  let choice: SessionRuntimeChoice | null = null;
  try {
    const value = JSON.parse(localStorage.getItem(storageKey) ?? "null");
    if (value && typeof value === "object") {
      const provider =
        typeof value.provider === "string" && value.provider
          ? value.provider
          : undefined;
      const modelId =
        typeof value.modelId === "string" && value.modelId
          ? value.modelId
          : undefined;
      const thinkingLevel =
        typeof value.thinkingLevel === "string" && value.thinkingLevel
          ? value.thinkingLevel
          : undefined;
      if (provider || modelId || thinkingLevel)
        choice = { provider, modelId, thinkingLevel };
    }
  } catch {
    return fallback.get(storageKey) ?? null;
  }
  fallback.set(storageKey, choice);
  return choice;
}

function prune() {
  const entries: { storageKey: string; updatedAt: number }[] = [];
  for (let index = 0; index < localStorage.length; index++) {
    const storageKey = localStorage.key(index);
    if (!storageKey || !storageKey.startsWith(PREFIX)) continue;
    let updatedAt = 0;
    try {
      updatedAt =
        JSON.parse(localStorage.getItem(storageKey) ?? "{}").updatedAt ?? 0;
    } catch {
      /* unparsable records sort oldest and are evicted first */
    }
    entries.push({ storageKey, updatedAt });
  }
  if (entries.length <= MAX_SESSIONS) return;
  entries.sort((a, b) => a.updatedAt - b.updatedAt);
  for (const stale of entries.slice(0, entries.length - MAX_SESSIONS))
    localStorage.removeItem(stale.storageKey);
}

export function writeSessionRuntimePreference(
  sessionId: string,
  value: SessionRuntimeChoice | null,
) {
  if (!sessionId) return;
  const storageKey = key(sessionId);
  if (value && !value.provider && !value.modelId && !value.thinkingLevel)
    value = null;
  // Keep this tab usable even when persistence is unavailable (including clear).
  fallback.set(storageKey, value);
  try {
    if (value)
      localStorage.setItem(
        storageKey,
        JSON.stringify({ ...value, updatedAt: Date.now() }),
      );
    else localStorage.removeItem(storageKey);
    unpersisted.delete(storageKey);
    prune();
  } catch {
    unpersisted.add(storageKey);
    /* Optional persistence: session selection still succeeds. */
  }
}
