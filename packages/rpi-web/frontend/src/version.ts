/** Injected by `vite.config.ts` from `packages/rpi-web/Cargo.toml`. */
declare const __RPI_WEB_VERSION__: string;

/** The installed Web build, shown beside the logo so a page can be identified. */
export const WEB_VERSION: string =
  typeof __RPI_WEB_VERSION__ === "string" ? __RPI_WEB_VERSION__ : "";
