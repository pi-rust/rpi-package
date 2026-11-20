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

export interface ProjectFileMatch {
  path: string;
  /** Linked worktree the file lives in; undefined for the project root. */
  root?: string;
}

export interface ProjectPathLookup {
  /** Exactly one file matches the wanted path. */
  match: ProjectFileMatch | null;
  /** Several plausible files; the user picks instead of being refused. */
  candidates: ProjectFileMatch[];
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
    /** Linked worktrees to search as well, as reported by `/projects`. */
    roots?: string[];
    signal?: AbortSignal;
    maxDirs?: number;
    maxDepth?: number;
    maxCandidates?: number;
  } = {},
): Promise<ProjectPathLookup> {
  const target = wanted.replace(/\\/g, "/").replace(/^(?:\.\/)+/, "");
  if (!target || target.startsWith("/") || target.split("/").includes(".."))
    return { match: null, candidates: [] };
  const suffix = `/${target}`;
  // A message can name the wrong directory (`src/x.pdf` for `docs/x.pdf`), so
  // the file name alone is the fallback search key.
  const basename = target.split("/").pop() ?? target;
  const nameSuffix = `/${basename}`;
  const byName: ProjectFileMatch[] = [];
  const maxDirs = options.maxDirs ?? 64;
  const maxDepth = options.maxDepth ?? 5;
  const maxCandidates = options.maxCandidates ?? 8;
  const seen = new Set<string>();
  // Each checkout is searched on its own so a match keeps its root.
  const roots = [undefined, ...(options.roots ?? [])];
  const queue = roots.map((root) => ({ root, path: "", depth: 0 }));
  const matches: ProjectFileMatch[] = [];
  let listed = 0;
  while (
    queue.length > 0 &&
    listed < maxDirs &&
    matches.length < maxCandidates &&
    byName.length < maxCandidates
  ) {
    const next = queue.shift()!;
    const key = `${next.root ?? ""}\u0000${next.path}`;
    if (seen.has(key)) continue;
    seen.add(key);
    listed += 1;
    let entries;
    try {
      entries = (await api.files(projectId, next.path, next.root)).entries ?? [];
    } catch {
      continue;
    }
    if (options.signal?.aborted) return { match: null, candidates: [] };
    for (const entry of entries) {
      const path = entry.path.replace(/\\/g, "/");
      if (entry.kind === "file") {
        const found: ProjectFileMatch = next.root
          ? { path, root: next.root }
          : { path };
        if (path.endsWith(suffix)) matches.push(found);
        else if (
          path.endsWith(nameSuffix) &&
          !byName.some((item) => item.path === path && item.root === next.root)
        )
          byName.push(found);
        continue;
      }
      if (next.depth < maxDepth && !SKIPPED.has(entry.name))
        queue.push({ root: next.root, path, depth: next.depth + 1 });
    }
  }
  const found = matches.length > 0 ? matches : byName;
  if (found.length === 1) return { match: found[0], candidates: [] };
  // Project-root matches first, then shortest path, so the likeliest is on top.
  const candidates = found
    .sort(
      (a, b) =>
        Number(Boolean(a.root)) - Number(Boolean(b.root)) ||
        a.path.length - b.path.length ||
        a.path.localeCompare(b.path),
    )
    .slice(0, maxCandidates);
  return { match: null, candidates };
}
