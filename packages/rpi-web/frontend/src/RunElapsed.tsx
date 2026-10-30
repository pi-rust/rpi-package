import type { RunTiming } from "./api";
import { translate, type Language } from "./i18n";

export function RunElapsed({
  timing,
  language,
}: {
  timing: RunTiming;
  language: Language;
}) {
  const seconds = Math.floor(timing.elapsedMs / 1000);
  const h = Math.floor(seconds / 3600),
    m = Math.floor(seconds / 60) % 60,
    s = seconds % 60;
  const duration =
    language === "zh"
      ? [h ? `${h} 小时` : "", m ? `${m} 分` : "", `${s} 秒`]
          .filter(Boolean)
          .join(" ")
      : [h ? `${h}h` : "", m ? `${m}m` : "", `${s}s`].filter(Boolean).join(" ");
  const label = translate(language, "workedFor", { duration });
  return (
    <div
      className={`run-elapsed run-${timing.outcome}`}
      data-run-id={timing.runId}
      data-entry-id={timing.finalEntryId}
    >
      <span className="run-elapsed-line" aria-hidden="true" />
      <span title={translate(language, "runElapsedSource")}>
        {label}
        {timing.outcome !== "completed" && (
          <span className="run-elapsed-outcome">
            {" "}
            ·{" "}
            {translate(
              language,
              timing.outcome === "aborted"
                ? "toolRunAborted"
                : timing.outcome === "failed"
                  ? "toolRunFailed"
                  : "toolRunDeclined",
            )}
          </span>
        )}
      </span>
      <span className="run-elapsed-line" aria-hidden="true" />
    </div>
  );
}
