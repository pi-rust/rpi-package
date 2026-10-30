// Mermaid has process-wide configuration and rendering state: serialize access.
let queue: Promise<unknown> = Promise.resolve();
let sequence = 0;

export function renderMermaid(
  source: string,
  dark: boolean,
  cancelled: () => boolean,
): Promise<string | null> {
  const task = queue.then(async () => {
    if (cancelled()) return null;
    // Diagram configuration is application-owned, not supplied by chat text.
    // Image/HTML/CSS resources are not needed for local flowcharts and must not
    // trigger requests while Mermaid measures a temporary DOM tree.
    // Restrict the first release to flowcharts and sequences. In particular,
    // do not pass shape metadata (YAML) or authored CSS into the renderer:
    // quoted/escaped `img` keys can otherwise cause requests before SVG export.
    const diagram = source.replace(/^\s*%%[^\n]*(?:\n|$)/gm, "").trimStart();
    if (
      !/^(?:flowchart|graph|sequenceDiagram)\b/.test(diagram) ||
      source.length > 20_000 ||
      source.split(/[\n;]/).length > 300 ||
      source.includes("%%{") ||
      /^\s*---(?:\r?\n|$)/.test(source) ||
      /@\s*\{|\b(?:classDef|linkStyle)\b|\bstyle\s+\S/i.test(source) ||
      /\b(?:url\s*\(|img\s*:)|@import|!\[/i.test(source) ||
      [...source.matchAll(/<\s*\/?\s*[a-z][^>]*>/gi)].some(
        ([tag]) => !/^<br\s*\/?>$/i.test(tag),
      )
    ) {
      throw new Error("Unsupported or oversized diagram");
    }
    const { default: mermaid } = await import("mermaid");
    if (cancelled()) return null;
    mermaid.initialize({
      startOnLoad: false,
      securityLevel: "strict",
      secure: Object.keys(mermaid.mermaidAPI.defaultConfig),
      theme: dark ? "dark" : "default",
      htmlLabels: false,
      flowchart: { htmlLabels: false },
      fontFamily: "Arial, sans-serif",
      maxTextSize: 20_000,
      maxEdges: 150,
      suppressErrorRendering: true,
    });
    const scratch = document.createElement("div");
    scratch.className = "mermaid-render-scratch";
    scratch.setAttribute("aria-hidden", "true");
    // Mermaid needs a connected, measurable container, never the chat DOM.
    Object.assign(scratch.style, {
      position: "fixed",
      left: "-100000px",
      top: "0",
      visibility: "hidden",
      pointerEvents: "none",
    });
    document.body.append(scratch);
    try {
      const { svg } = await mermaid.render(
        `rpi-mermaid-${++sequence}`,
        source,
        scratch,
      );
      if (cancelled()) return null;
      if (svg.length > 2_000_000) throw new Error("Diagram too large");
      // SVG is displayed only in browser image mode (not innerHTML/object),
      // disabling scripts, links and external subresources. Preserve a useful
      // intrinsic size for the existing original-size image viewer.
      const doc = new DOMParser().parseFromString(svg, "image/svg+xml");
      const root = doc.documentElement;
      if (root.localName !== "svg") throw new Error("Invalid diagram");
      const box = root
        .getAttribute("viewBox")
        ?.trim()
        .split(/[\s,]+/)
        .map(Number);
      if (
        !box ||
        box.length !== 4 ||
        !box.every(Number.isFinite) ||
        box[2] <= 0 ||
        box[3] <= 0 ||
        box[2] > 16384 ||
        box[3] > 16384
      )
        throw new Error("Diagram dimensions unsupported");
      root.setAttribute("width", String(box[2]));
      root.setAttribute("height", String(box[3]));
      return `data:image/svg+xml;charset=utf-8,${encodeURIComponent(new XMLSerializer().serializeToString(root))}`;
    } finally {
      scratch.remove();
    }
  });
  queue = task.catch(() => undefined);
  return task;
}
