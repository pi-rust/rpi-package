/** Injected by `vite.config.ts` from `packages/rpi-web/Cargo.toml`. */
declare const __RPI_WEB_VERSION__: string;

/** The build this bundle came from; the running host may be newer. */
export const WEB_VERSION: string =
  typeof __RPI_WEB_VERSION__ === "string" ? __RPI_WEB_VERSION__ : "";

/**
 * The version to show beside the logo.
 *
 * `/api/v1/capabilities` reports what the service is actually running, which is
 * authoritative across a stale dev server or a rebuilt extension; the bundled
 * constant is the offline fallback for older hosts.
 */
export function displayedVersion(
  capabilities: { version?: string } | null | undefined,
): string {
  const served = capabilities?.version?.trim();
  return served || WEB_VERSION;
}
