import { createContext } from "react";
import type { Project } from "./api";

/** Resolve local image paths against the message's configured project, never the Vite root. */
export const ProjectImageContext = createContext<Project | undefined>(
  undefined,
);

export function projectImageSource(source: string, project?: Project): string {
  if (!project || !source || /^(?:data:|\/\/)/i.test(source)) return source;
  let path = source;
  if (/^https?:\/\//i.test(path)) {
    try {
      const url = new URL(path);
      if (url.origin !== window.location.origin) return source;
      path = url.pathname;
    } catch {
      return "";
    }
  }
  // Existing API and bundled assets already have explicit resource identities.
  if (/^\/(?:api|assets)(?:\/|$)/.test(path)) return source;
  // A native Windows path is accepted only under the explicitly selected project.
  if (/^[a-z]:[\\/]/i.test(path)) {
    const root = project.path.replace(/\\/g, "/").replace(/\/$/, "");
    const normalized = path.replace(/\\/g, "/");
    if (!normalized.toLowerCase().startsWith(root.toLowerCase() + "/"))
      return "";
    path = normalized.slice(root.length + 1);
  } else if (/^[a-z][a-z0-9+.-]*:/i.test(path)) {
    return source; // Keep the caller's existing URL safety transform in charge.
  }
  path = path.split(/[?#]/, 1)[0];
  try {
    path = decodeURIComponent(path);
  } catch {
    return "";
  }
  path = path.replace(/^\//, "").replace(/^(?:\.\/)+/, "");
  if (/[\\:\0]/.test(path) || path.split("/").some((part) => part === ".."))
    return "";
  // The project endpoint intentionally serves raster files only, not active SVG/HTML.
  if (!/\.(?:png|jpe?g|gif|webp|bmp)$/i.test(path)) return source;
  return `/api/v1/projects/${encodeURIComponent(project.id)}/image?${new URLSearchParams({ path })}`;
}
