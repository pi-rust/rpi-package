import { api } from "./api";

/** Directories that never hold a message-linked source file and are expensive. */
const SKIPPED = new Set([
  "node_modules",
  "target",
  "dist",
  "build",
  "vendor",
  "coverage",
  ".git",
  ".rpi",
  ".venv",
  "venv",
  "__pycache__",
]);

/**
 * Resolve a path an agent wrote relative to a nested package directory.
 *
 * A message may say `src/App.tsx` while the project root is the repository, so
 * the literal path does not exist. Walk the project breadth-first and accept a
 * single file whose relative path ends with `/<wanted>`. Ambiguity returns
 * null: a wrong guess would preview an unrelated file.
 */
export async function resolveProjectPath(
  projectId: string,
  wanted: string,
  options: { signal?: AbortSignal; maxDirs?: number; maxDepth?: number } = {},
): Promise<string | null> {
  const target = wanted.replace(/\\/g, "/").replace(/^(?:\.\/)+/, "");
  if (!target.includes("/")) return null;
  const suffix = `/${target}`;
  const maxDirs = options.maxDirs ?? 48;
  const maxDepth = options.maxDepth ?? 4;
  const seen = new Set<string>();
  const queue = [{ path: "", depth: 0 }];
  const matches: string[] = [];
  let listed = 0;
  while (queue.length > 0 && listed < maxDirs && matches.length < 2) {
    const next = queue.shift()!;
    if (seen.has(next.path)) continue;
    seen.add(next.path);
    listed += 1;
    let entries;
    try {
      entries = (await api.files(projectId, next.path)).entries ?? [];
    } catch {
      continue;
    }
    if (options.signal?.aborted) return null;
    for (const entry of entries) {
      const path = entry.path.replace(/\\/g, "/");
      if (entry.kind === "file") {
        if (path.endsWith(suffix)) matches.push(path);
      } else if (next.depth < maxDepth && !SKIPPED.has(entry.name)) {
        queue.push({ path, depth: next.depth + 1 });
      }
    }
  }
  return matches.length === 1 ? matches[0] : null;
}
