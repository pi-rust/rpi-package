import { useEffect, useState } from "react";
import { Check, Copy } from "lucide-react";
import { translate, type Language } from "./i18n";
import { ZoomableImage } from "./ZoomableImage";
import { renderMermaid } from "./mermaidRenderer";

export function MermaidBlock({
  source,
  language,
}: {
  source: string;
  language: Language;
}) {
  const [dark, setDark] = useState(
    () => document.documentElement.dataset.theme === "dark",
  );
  const [sourceMode, setSourceMode] = useState(false);
  const [copied, setCopied] = useState(false);
  const [result, setResult] = useState<{
    key: string;
    url?: string;
    error?: boolean;
  }>({ key: "" });
  const key = `${dark}:${source}`;
  const current = result.key === key ? result : undefined;
  const t = (name: Parameters<typeof translate>[1]) =>
    translate(language, name);

  useEffect(() => {
    const update = () =>
      setDark(document.documentElement.dataset.theme === "dark");
    const observer = new MutationObserver(update);
    observer.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["data-theme"],
    });
    update();
    return () => observer.disconnect();
  }, []);
  useEffect(() => {
    let cancelled = false;
    // An incomplete streamed fence must not render on every token.
    const timer = setTimeout(() => {
      void renderMermaid(source, dark, () => cancelled).then(
        (url) => {
          if (!cancelled && url) setResult({ key, url });
        },
        () => {
          if (!cancelled) setResult({ key, error: true });
        },
      );
    }, 200);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [source, dark, key]);
  useEffect(() => {
    if (!copied) return;
    const timer = setTimeout(() => setCopied(false), 1600);
    return () => clearTimeout(timer);
  }, [copied]);
  async function copy() {
    try {
      await navigator.clipboard.writeText(source);
      setCopied(true);
    } catch {
      /* Source remains selectable. */
    }
  }

  return (
    <div
      className="markdown-code-block mermaid-block"
      data-theme={dark ? "dark" : "light"}
    >
      <div className="markdown-code-heading">
        <span>Mermaid</span>
        <div className="mermaid-actions">
          <button
            type="button"
            className="markdown-copy"
            aria-pressed={sourceMode}
            onClick={() => setSourceMode((value) => !value)}
          >
            {t(sourceMode ? "mermaidViewDiagram" : "mermaidViewSource")}
          </button>
          <button
            type="button"
            className="markdown-copy"
            onClick={() => void copy()}
            aria-label={t(copied ? "copied" : "copyCode")}
          >
            {copied ? <Check size={12} /> : <Copy size={12} />}
            {t(copied ? "copied" : "copyCode")}
          </button>
        </div>
      </div>
      {current?.error && (
        <p className="mermaid-status" role="status">
          {t("mermaidError")}
        </p>
      )}
      {sourceMode || current?.error ? (
        <pre className="mermaid-source">
          <code>{source}</code>
        </pre>
      ) : current?.url ? (
        <div className="mermaid-diagram">
          <ZoomableImage
            key={current.url}
            src={current.url}
            alt={t("mermaidDiagram")}
            language={language}
          />
        </div>
      ) : (
        <p className="mermaid-status" role="status">
          {t("mermaidLoading")}
        </p>
      )}
    </div>
  );
}
