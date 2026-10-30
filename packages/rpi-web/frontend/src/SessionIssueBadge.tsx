import { CircleAlert, RotateCcw } from "lucide-react";
import type { SessionSummary } from "./api";
import { translate, type Language } from "./i18n";

/** Only explicit summary evidence: offline/closed alone is not a failed run. */
export function sessionIssue(
  session: SessionSummary,
): "failed" | "resume" | null {
  if (["running", "starting", "awaiting_input"].includes(session.status))
    return null;
  const settlement = session.latestSettlement;
  if (settlement?.pendingResume === true) return "resume";
  if (session.status === "error") return "failed";
  // User stops and declined operations are not abnormal interruptions.
  if (["aborted", "declined"].includes(settlement?.outcome ?? "")) return null;
  if (
    ["failed", "interrupted"].includes(settlement?.outcome ?? "") ||
    session.error?.trim()
  )
    return "failed";
  return null;
}

export function SessionIssueBadge({
  session,
  language,
}: {
  session: SessionSummary;
  language: Language;
}) {
  const issue = sessionIssue(session);
  if (!issue) return null;
  const label = translate(
    language,
    issue === "resume" ? "sessionNeedsResume" : "sessionInterrupted",
  );
  const hint = translate(
    language,
    issue === "resume" ? "sessionNeedsResumeHint" : "sessionInterruptedHint",
  );
  const Icon = issue === "resume" ? RotateCcw : CircleAlert;
  return (
    <span
      className={`session-issue session-issue-${issue}`}
      title={hint}
      role="img"
      aria-label={`${label}. ${hint}`}
    >
      <Icon size={11} aria-hidden="true" />
      <span>{label}</span>
    </span>
  );
}
