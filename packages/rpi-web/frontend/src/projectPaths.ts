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
