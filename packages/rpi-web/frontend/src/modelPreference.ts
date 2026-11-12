// Browser-local UI preference only; never inferred from native runtime/history.
export interface ModelPreference {
  provider: string;
  modelId: string;
}
// Begin an explicit choice before I/O; call the returned completion only after
// set_model succeeds. Its boolean guards UI writes after session navigation.
export type ModelSelected = (model: {
  provider: string;
  id: string;
}) => () => boolean;

const fallback = new Map<string, ModelPreference | null>();
const unpersisted = new Set<string>();
// localStorage is origin-scoped; include the origin in the memory fallback too.
const key = (projectId: string) =>
  `rpi-web-model-preference:${encodeURIComponent(location.origin)}:${encodeURIComponent(projectId)}`;

export function readModelPreference(projectId: string): ModelPreference | null {
  if (!projectId) return null;
  const storageKey = key(projectId);
  // A failed write must win over any stale persisted value in this tab.
  // Otherwise read storage afresh so other tabs' choices and clears are honored.
  if (unpersisted.has(storageKey)) return fallback.get(storageKey) ?? null;
  let preference: ModelPreference | null = null;
  try {
    const value = JSON.parse(localStorage.getItem(storageKey) ?? "null");
    if (
      value &&
      typeof value.provider === "string" &&
      value.provider &&
      typeof value.modelId === "string" &&
      value.modelId
    )
      preference = { provider: value.provider, modelId: value.modelId };
  } catch {
    return fallback.get(storageKey) ?? null;
  }
  fallback.set(storageKey, preference);
  return preference;
}

export function writeModelPreference(
  projectId: string,
  value: ModelPreference | null,
) {
  if (!projectId) return;
  const storageKey = key(projectId);
  // Keep this tab usable even when persistence is unavailable (including clear).
  fallback.set(storageKey, value);
  try {
    if (value) localStorage.setItem(storageKey, JSON.stringify(value));
    else localStorage.removeItem(storageKey);
    unpersisted.delete(storageKey);
  } catch {
    unpersisted.add(storageKey);
    /* Optional persistence: native session selection still succeeds. */
  }
}
