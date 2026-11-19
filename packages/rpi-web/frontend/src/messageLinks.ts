import type { Project } from "./api";
import { hiddenProjectComponent, matchProjectRoot } from "./projectPaths";

export interface ProjectFileLink {
  path: string;
  /** Linked worktree the path belongs to; omitted for the project root. */
  root?: string;
  line?: number;
  endLine?: number;
}

/** Decode exactly once, and never turn a URL into an OS/shell navigation. */
export function projectFileLink(
  source: string,
  project?: Project,
): ProjectFileLink | null {
  if (!project || !source || /[\u0000-\u001f\u007f]/.test(source)) return null;
  let path: string;
  try {
    path = decodeURIComponent(source);
  } catch {
    return null;
  }
  if (/%[\da-f]{2}|[\u0000-\u001f\u007f]/i.test(path) || path !== path.trim())
    return null;
  const target = path.match(
    /(?:#L([1-9]\d*)(?:-L?([1-9]\d*))?|:([1-9]\d*)(?:-([1-9]\d*))?)$/,
  );
  const line = target ? Number(target[1] ?? target[3]) : undefined;
  const endLine = target ? Number(target[2] ?? target[4] ?? line) : undefined;
  if (
    line !== undefined &&
    (!Number.isSafeInteger(line) ||
      !Number.isSafeInteger(endLine) ||
      endLine! < line)
  )
    return null;
  if (target) path = path.slice(0, target.index);
  path = path.replace(/\\/g, "/");
  if (!path || path.startsWith("//") || /[?#]/.test(path)) return null;
  const windows = /^[a-z]:\//i.test(path);
  if (!windows && /^[a-z][a-z\d+.-]*:/i.test(path)) return null;
  // An absolute path names its own checkout: the project root, or one of its
  // linked worktrees (`git worktree add ../name`). Keep the matched root so a
  // preview reads that checkout instead of the same relative path in the main
  // one. Relative paths stay project-relative.
  let root: string | undefined;
  if (windows || path.startsWith("/")) {
    const match = matchProjectRoot(path, project);
    if (!match) return null;
    path = match.path;
    root = match.root;
  }
  const parts = path.split("/");
  if (parts.some((part) => part === ".." || /[:]/.test(part))) return null;
  const components = parts.filter((part) => part && part !== ".");
  // The server refuses these outright, so do not advertise a preview button
  // that can only fail. The text still renders as plain code.
  if (components.some(hiddenProjectComponent)) return null;
  path = components.join("/");
  if (!path || source.endsWith("/")) return null;
  return root ? { path, root, line, endLine } : { path, line, endLine };
}

/**
 * Inline code needs a recognizable path, not an arbitrary identifier.
 *
 * A bare filename (`message-links.spec.ts`) or a git ref (`origin/main`) has no
 * directory of its own, so joining it onto the project root invents a path the
 * agent never wrote. Both stay plain code; only `dir/.../name.ext` links.
 */
export function inlineFileLink(
  source: string,
  project?: Project,
): ProjectFileLink | null {
  const link = projectFileLink(source, project);
  if (!link) return null;
  // Judge the normalized path: a `:12`/`#L12` suffix must not hide the name.
  const name = link.path.split("/").pop() ?? "";
  return link.path.includes("/") && /\.[\p{L}\d_-]+$/u.test(name) ? link : null;
}

export function browserLink(source: string): "web" | "mail" | null {
  if (/[\u0000-\u0020\u007f\\]/.test(source)) return null;
  if (/^https?:\/\//i.test(source)) {
    try {
      return new URL(source).hostname ? "web" : null;
    } catch {
      return null;
    }
  }
  return /^mailto:[^?]+(?:\?.*)?$/i.test(source) ? "mail" : null;
}
