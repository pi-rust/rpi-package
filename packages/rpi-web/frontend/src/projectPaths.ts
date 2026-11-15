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
