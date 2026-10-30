import { useId, useLayoutEffect, useRef, useState } from "react";
import { translate, type Language } from "./i18n";

/** Keep the exact command selectable/copyable; limit only its visual preview. */
export function ShellCommand({
  command,
  language,
}: {
  command: string;
  language: Language;
}) {
  const id = useId();
  const codeRef = useRef<HTMLElement>(null);
  const [expanded, setExpanded] = useState(false);
  const [long, setLong] = useState(false);
  useLayoutEffect(() => {
    const code = codeRef.current;
    if (!code) return;
    const measure = () => {
      // Closed outer tool details have no layout. Measure again on opening,
      // and when wrapping changes (mobile/sidebar/font/layout changes).
      if (!code.getBoundingClientRect().width) return;
      const lineHeight = Number.parseFloat(getComputedStyle(code).lineHeight);
      setLong(code.scrollHeight > lineHeight * 4 + 1);
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(code);
    return () => observer.disconnect();
  }, [command, expanded]);
  return (
    <>
      <pre
        className={`shell-input shell-command-preview${expanded ? " expanded" : ""}`}
      >
        <code ref={codeRef} id={id} className="shell-command-code">
          {command}
        </code>
      </pre>
      {long && (
        <button
          type="button"
          className="shell-command-toggle"
          aria-expanded={expanded}
          aria-controls={id}
          onClick={() => setExpanded((value) => !value)}
        >
          {translate(
            language,
            expanded ? "collapseCommand" : "showFullCommand",
          )}
        </button>
      )}
    </>
  );
}
