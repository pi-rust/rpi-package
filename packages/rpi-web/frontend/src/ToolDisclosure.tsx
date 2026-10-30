import { useEffect, useState, type ReactNode } from "react";
import {
  ChevronRight,
  CircleAlert,
  FileText,
  Globe,
  ListChecks,
  Loader2,
  Pencil,
  Search,
  Terminal,
  Wrench,
} from "lucide-react";
import { translate, type Language } from "./i18n";
import { isShellTool, type ToolStatus } from "./transcript";

function iconForTool(name: string) {
  const tool = name
    .trim()
    .toLowerCase()
    .replace(/^functions\./, "");
  if (isShellTool(tool)) return Terminal;
  if (["read", "read_file", "readfile", "cat"].includes(tool)) return FileText;
  if (
    ["edit", "apply_patch", "write", "write_file", "writefile"].includes(tool)
  )
    return Pencil;
  if (["plan", "todo"].includes(tool)) return ListChecks;
  if (["fetch", "webfetch", "web_fetch", "http_get", "open_url"].includes(tool))
    return Globe;
  if (
    [
      "grep",
      "glob",
      "find",
      "search",
      "search_files",
      "file_search",
      "rg",
      "websearch",
      "web_search",
      "ls",
      "list",
      "list_files",
      "list_directory",
    ].includes(tool)
  )
    return Search;
  return Wrench;
}

export function useToolExpansion(status: ToolStatus) {
  const [open, setOpen] = useState(status === "error");
  // Owned by the stable tool row, not its result-specific renderer. A new
  // failure opens once; later time/output updates preserve the reader's toggle.
  useEffect(() => {
    if (status === "error") setOpen(true);
  }, [status]);
  return { open, setOpen };
}

export type ToolExpansion = ReturnType<typeof useToolExpansion>;

/** One compact, keyboard-accessible activity row, with a single detail toggle. */
export function ToolDisclosure({
  expansion,
  toolName,
  action,
  target,
  status,
  language,
  className = "",
  targetClassName = "",
  timing,
  meta,
  children,
}: {
  expansion: ToolExpansion;
  toolName: string;
  action: string;
  target?: string;
  status: ToolStatus;
  language: Language;
  className?: string;
  targetClassName?: string;
  timing?: ReactNode;
  meta?: ReactNode;
  children: ReactNode;
}) {
  const { open, setOpen } = expansion;
  const Icon = iconForTool(toolName);
  const label = translate(
    language,
    status === "running"
      ? "toolRunning"
      : status === "pending"
        ? "toolPending"
        : status === "error"
          ? "toolError"
          : "toolDone",
  );
  return (
    <details
      className={`tool-card tool-${status} ${status === "error" ? "failed" : ""} ${className}`}
      open={open}
      onToggle={(event) => setOpen(event.currentTarget.open)}
    >
      <summary>
        <span className="tool-status" title={label}>
          {status === "running" || status === "pending" ? (
            <Loader2 size={14} className="spin" aria-hidden="true" />
          ) : status === "error" ? (
            <CircleAlert size={14} aria-hidden="true" />
          ) : (
            <Icon size={14} aria-hidden="true" />
          )}
          <span className="sr-only">{label}</span>
        </span>
        <span className="tool-summary-text">
          <span className="tool-action" title={action}>
            {action}
          </span>
          {target && (
            <code className={`tool-target ${targetClassName}`} title={target}>
              {target.replace(/\s+/g, " ").trim()}
            </code>
          )}
        </span>
        {meta && <span className="tool-summary-meta">{meta}</span>}
        {timing}
        <ChevronRight size={14} className="tool-chevron" aria-hidden="true" />
      </summary>
      {children}
    </details>
  );
}

/** Presentation only: aliases do not change native tool names or arguments. */
export function toolSummary(
  name: string,
  args: unknown,
  status: ToolStatus,
  language: Language,
) {
  const record =
    args && typeof args === "object" && !Array.isArray(args)
      ? (args as Record<string, unknown>)
      : {};
  const field = (...keys: string[]) =>
    keys
      .map((key) => record[key])
      .find((v): v is string => typeof v === "string" && !!v.trim()) ?? "";
  const tool = name
    .trim()
    .toLowerCase()
    .replace(/^functions\./, "");
  const path = field("path", "filePath", "file_path", "directory");
  const active = status === "running" || status === "pending";
  const t = (key: Parameters<typeof translate>[1]) => translate(language, key);
  if (["read", "read_file", "readfile", "cat"].includes(tool))
    return {
      action: t(active ? "toolReading" : "toolRead"),
      target: path || name,
    };
  if (
    [
      "grep",
      "glob",
      "find",
      "search",
      "search_files",
      "file_search",
      "rg",
      "websearch",
      "web_search",
    ].includes(tool)
  )
    return {
      action: t(active ? "toolSearching" : "toolSearched"),
      target:
        [field("query", "pattern", "glob", "search"), path]
          .filter(Boolean)
          .join(" · ") || name,
    };
  if (["ls", "list", "list_files", "list_directory"].includes(tool))
    return {
      action: t(active ? "toolListing" : "toolListed"),
      target: path || name,
    };
  if (["write", "write_file", "writefile"].includes(tool))
    return {
      action: t(active ? "toolWriting" : "toolWrote"),
      target: path || name,
    };
  if (["edit", "apply_patch"].includes(tool))
    return {
      action: t(active ? "toolEditing" : "toolEdited"),
      target: path || name,
    };
  if (["fetch", "webfetch", "web_fetch", "http_get", "open_url"].includes(tool))
    return {
      action: t(active ? "toolFetching" : "toolFetched"),
      target: field("url") || name,
    };
  return {
    action: name,
    target: field("description", "query", "path", "filePath", "url", "action"),
  };
}
