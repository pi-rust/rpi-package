import type { Project } from "./api";

export interface ProjectFileLink {
  path: string;
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
  // Preserve filesystem roots: D:/ must not become a drive-relative D: path.
  const normalizedRoot = project.path.replace(/\\/g, "/");
  const root =
    normalizedRoot.replace(/\/+$/, "") +
    (/^(?:[a-z]:)?\/+$/i.test(normalizedRoot) ? "/" : "");
  // Unix roots are case-sensitive. Only a drive-qualified Windows root is folded.
  if (windows || path.startsWith("/")) {
    if (windows !== /^[a-z]:\//i.test(root)) return null;
    const prefix = root.endsWith("/") ? root : root + "/";
    if (
      !(windows
        ? path.toLowerCase().startsWith(prefix.toLowerCase())
        : path.startsWith(prefix))
    )
      return null;
    path = path.slice(prefix.length);
  }
  const parts = path.split("/");
  if (parts.some((part) => part === ".." || /[:]/.test(part))) return null;
  path = parts.filter((part) => part && part !== ".").join("/");
  if (!path || source.endsWith("/")) return null;
  return { path, line, endLine };
}

/** Inline code needs a recognizable filename/path, not an arbitrary identifier. */
export function inlineFileLink(
  source: string,
  project?: Project,
): ProjectFileLink | null {
  const link = projectFileLink(source, project);
  if (!link) return null;
  return /[/\\]|(?:^|\/)\.?[^\s/]+\.[\p{L}\d_-]+(?:[:#].*)?$/u.test(source) ||
    /^\.[\w-]+$/.test(source)
    ? link
    : null;
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
