import { createContext } from "react";
import type { Project } from "./api";
import { hiddenProjectComponent, matchProjectRoot } from "./projectPaths";

/** Resolve local image paths against the message's configured project, never the Vite root. */
export const ProjectImageContext = createContext<Project | undefined>(
  undefined,
);

export function isProjectImagePath(path: string): boolean {
  return /\.(?:png|jpe?g|gif|webp|bmp)$/i.test(path);
}

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
  // An absolute path (native Windows or Unix) is accepted only inside the
  // selected project or one of its linked worktrees. The matched root travels
  // with the request so the image is read from the checkout that wrote it.
  let root: string | undefined;
  if (/^[a-z]:[\\/]/i.test(path) || path.startsWith("/")) {
    const match = matchProjectRoot(path.replace(/\\/g, "/"), project);
    if (!match) return "";
    path = match.path;
    root = match.root;
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
  // The image endpoint shares the hidden-component boundary with the file
  // endpoint, so an image under one of those directories would only 403.
  if (path.split("/").some(hiddenProjectComponent)) return "";
  // The project endpoint intentionally serves raster files only, not active SVG/HTML.
  if (!isProjectImagePath(path)) return source;
  const query = new URLSearchParams({ path });
  if (root) query.set("root", root);
  return `/api/v1/projects/${encodeURIComponent(project.id)}/image?${query}`;
}
