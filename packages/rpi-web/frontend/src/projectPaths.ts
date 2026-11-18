/**
 * Project-relative path components that `rpi-extcore`'s `workspace::hidden()`
 * rejects before and after canonicalisation. The file and image endpoints
 * answer `403 path is outside the browsable project` for them, so the client
 * must not advertise a preview that can only fail. `.rpi` deliberately holds
 * session transcripts and uploads; keep both sides aligned instead of relaxing
 * the server boundary.
 */
const HIDDEN_COMPONENTS = new Set([
  ".git",
  ".rpi",
  "node_modules",
  "target",
  ".deploy",
]);

export function hiddenProjectComponent(name: string): boolean {
  const lower = name.toLowerCase();
  return HIDDEN_COMPONENTS.has(lower) || lower.startsWith(".env");
}

/** A path resolved against the project root or one of its linked worktrees. */
export interface RootedPath {
  /** Linked worktree that matched; omitted when the project root matched. */
  root?: string;
  /** `/`-separated path relative to the matched root. */
  path: string;
}

/**
 * Every directory a preview may read: the project root first, then the linked
 * worktrees the backend reports (`git worktree list`). An agent usually works
 * in a sibling checkout, so matching the project root alone would leave every
 * file it wrote unopenable.
 */
function projectRoots(project: {
  path: string;
  worktrees?: string[];
}): Array<{ root?: string; path: string }> {
  const roots: Array<{ root?: string; path: string }> = [
    { path: project.path },
  ];
  for (const worktree of project.worktrees ?? []) {
    if (worktree && worktree !== project.path)
      roots.push({ root: worktree, path: worktree });
  }
  return roots;
}

/** The `/`-separated prefix an absolute path must start with to sit inside `root`. */
function rootPrefix(root: string, windows: boolean): string | null {
  const normalized = root.replace(/\\/g, "/");
  // A Windows root never matches a Unix path, and the reverse is true too.
  if (/^[a-z]:\//i.test(normalized) !== windows) return null;
  // Preserve filesystem roots: "/" and "D:/" keep their trailing separator.
  const trimmed =
    normalized.replace(/\/+$/, "") +
    (/^(?:[a-z]:)?\/+$/i.test(normalized) ? "/" : "");
  if (!trimmed) return null;
  return trimmed.endsWith("/") ? trimmed : trimmed + "/";
}

/**
 * Resolve an absolute `source` against the project's browsable roots.
 *
 * The longest matching root wins, so a worktree nested inside the checkout is
 * attributed to the worktree rather than to the enclosing project. Windows
 * roots are compared case-insensitively only when the source is drive
 * qualified; Unix roots stay case-sensitive. Relative sources belong to the
 * caller's root and return `null` here.
 */
export function matchProjectRoot(
  source: string,
  project: { path: string; worktrees?: string[] },
): RootedPath | null {
  const windows = /^[a-z]:\//i.test(source);
  let best: RootedPath | null = null;
  for (const candidate of projectRoots(project)) {
    const prefix = rootPrefix(candidate.path, windows);
    if (!prefix) continue;
    const matches = windows
      ? source.toLowerCase().startsWith(prefix.toLowerCase())
      : source.startsWith(prefix);
    if (!matches) continue;
    const path = source.slice(prefix.length);
    // A shorter remainder means a more specific root; keep that one.
    if (!path || (best && best.path.length <= path.length)) continue;
    best = candidate.root ? { root: candidate.root, path } : { path };
  }
  return best;
}

/**
 * Join a project-relative file path onto the server's canonical project root.
 * `Project.path` comes from the backend (`/api/v1/projects`), not from the
 * browser's OS, so a Windows server still yields a Windows path for a client
 * running on Linux or macOS. No client-side platform sniffing is involved.
 *
 * `relative` is produced by `projectFileLink`, which validates traversal and
 * normalises separators to `/`. Returns "" when the root is not a shape we can
 * join unambiguously; callers must then fall back to the relative path rather
 * than copy a guessed absolute one.
 *
 * `workspace::Project::new` strips a verbatim `\\?\` prefix from the root. A
 * verbatim UNC root may therefore arrive as `UNC\server\share`; reconstructing
 * `\\server\share` would be guesswork until we can confirm the real value on
 * Windows, so that shape is deliberately left unresolved.
 */
export function absoluteProjectPath(root: string, relative: string): string {
  if (!root || !relative) return "";
  const drive = /^[a-z]:[\\/]/i.test(root);
  const unc = /^\\\\/.test(root);
  const unix = root.startsWith("/");
  if (!drive && !unc && !unix) return "";
  // Keep the root's own separator instead of mixing `/` and `\`. On Unix a
  // backslash is a legal filename character, so only split on it when the
  // server root itself uses backslashes.
  const separator = (drive || unc) && root.includes("\\") ? "\\" : "/";
  const parts = relative
    .split(separator === "\\" ? /[\\/]/ : "/")
    .filter((part) => part && part !== ".");
  if (!parts.length) return root;
  const joined = parts.join(separator);
  const prefix = root.replace(/[\\/]+$/, "");
  // A bare filesystem root has no prefix; drive roots keep their trailing
  // separator (D:\ + file, not D:file).
  return prefix ? `${prefix}${separator}${joined}` : `${separator}${joined}`;
}
