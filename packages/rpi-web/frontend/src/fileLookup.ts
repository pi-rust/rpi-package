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

export interface ProjectPathLookup {
  /** Exactly one file ends with the wanted path. */
  path: string | null;
  /** Several plausible files; the user picks instead of being refused. */
  candidates: string[];
}

/**
 * Find a message-linked path the project does not contain literally.
 *
 * Agents write paths relative to a nested package (`src/App.tsx`) or name a
 * file without its directory. Walk the project breadth-first and collect files
 * whose relative path ends with the wanted suffix, then let the caller decide:
 * one match opens, several become a choice, none stays an explicit error.
 */
export async function lookupProjectPath(
  projectId: string,
  wanted: string,
  options: {
    signal?: AbortSignal;
    maxDirs?: number;
    maxDepth?: number;
    maxCandidates?: number;
  } = {},
): Promise<ProjectPathLookup> {
  const target = wanted.replace(/\\/g, "/").replace(/^(?:\.\/)+/, "");
  if (!target || target.startsWith("/") || target.split("/").includes(".."))
    return { path: null, candidates: [] };
  const suffix = `/${target}`;
  // A message can name the wrong directory (`src/x.pdf` for `docs/x.pdf`), so
  // the file name alone is the fallback search key.
  const basename = target.split("/").pop() ?? target;
  const nameSuffix = `/${basename}`;
  const byName: string[] = [];
  const maxDirs = options.maxDirs ?? 64;
  const maxDepth = options.maxDepth ?? 5;
  const maxCandidates = options.maxCandidates ?? 8;
  const seen = new Set<string>();
  const queue = [{ path: "", depth: 0 }];
  const matches: string[] = [];
  let listed = 0;
  while (
    queue.length > 0 &&
    listed < maxDirs &&
    matches.length < maxCandidates &&
    byName.length < maxCandidates
  ) {
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
    if (options.signal?.aborted) return { path: null, candidates: [] };
    for (const entry of entries) {
      const path = entry.path.replace(/\\/g, "/");
      if (entry.kind === "file") {
        if (path.endsWith(suffix)) matches.push(path);
        else if (path.endsWith(nameSuffix) && !byName.includes(path))
          byName.push(path);
        continue;
      }
      if (next.depth < maxDepth && !SKIPPED.has(entry.name))
        queue.push({ path, depth: next.depth + 1 });
    }
  }
  const found = matches.length > 0 ? matches : byName;
  if (found.length === 1) return { path: found[0], candidates: [] };
  // Shortest paths first: the closest checkout wins the tie-break presentation.
  const candidates = found
    .sort((a, b) => a.length - b.length || a.localeCompare(b))
    .slice(0, maxCandidates);
  return { path: null, candidates };
}
