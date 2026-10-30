import type { ReactNode } from "react";
import { MarkdownBody } from "./MarkdownBody";
import { ToolDisclosure, type ToolExpansion } from "./ToolDisclosure";
import { translate, valueLabel, type Language, type MessageKey } from "./i18n";
import type { Row } from "./transcript";
import { delegationRaw, type DelegationInfo } from "./delegation";
import "./delegation-card.css";

type ToolRow = Extract<Row, { kind: "tool" }>;

/** Read-only native evidence. No child/session navigation or lifecycle actions. */
export function DelegationCard({
  row,
  info,
  expansion,
  language,
  timing,
}: {
  row: ToolRow;
  info: DelegationInfo;
  expansion: ToolExpansion;
  language: Language;
  timing?: ReactNode;
}) {
  const t = (key: MessageKey) => translate(language, key);
  const waiting = row.status === "pending" || row.status === "running";
  const metadata: Array<[MessageKey, string | number | undefined]> = [
    ["delegateMaxTurns", info.maxTurns],
    [
      "delegateTimeout",
      info.timeoutSeconds === undefined ? undefined : `${info.timeoutSeconds}s`,
    ],
    ["delegateChildId", info.childId || undefined],
    ["delegateSessionId", info.sessionId || undefined],
  ];
  return (
    <ToolDisclosure
      expansion={expansion}
      toolName={row.name}
      action={t("delegateTask")}
      target={info.task}
      meta={info.role || undefined}
      status={row.status}
      doneLabel={t("delegateCallDone")}
      language={language}
      className="delegation-card"
      timing={timing}
    >
      <div className="tool-detail delegation-detail">
        <section>
          <h4>{t("delegateTaskLabel")}</h4>
          <p className="delegation-task">{info.task}</p>
        </section>
        <dl className="delegation-overview">
          {info.role && (
            <div>
              <dt>{t("delegateRole")}</dt>
              <dd className="delegation-role">{info.role}</dd>
            </div>
          )}
          {info.status && (
            <div>
              <dt>{t("delegateChildStatus")}</dt>
              <dd className="delegation-status">
                {valueLabel(language, info.status)}
              </dd>
            </div>
          )}
          {info.turns !== undefined && (
            <div>
              <dt>{t("delegateTurns")}</dt>
              <dd className="delegation-turns">{info.turns}</dd>
            </div>
          )}
        </dl>
        {info.turnLimited && (
          <p className="delegation-limit">{t("delegateTurnLimit")}</p>
        )}
        {info.report.trim() ? (
          <section
            className={`delegation-report ${row.status === "error" ? "delegation-error" : ""}`}
          >
            <h4>
              {t(
                row.status === "error"
                  ? "delegateError"
                  : waiting
                    ? "delegateProgress"
                    : info.structured
                      ? "delegateReport"
                      : "output",
              )}
            </h4>
            <MarkdownBody text={info.report} language={language} />
          </section>
        ) : (
          <p className="muted delegation-empty">
            {t(
              info.unknownResult
                ? "delegateUnknown"
                : waiting
                  ? "delegateWaiting"
                  : "delegateNoReport",
            )}
          </p>
        )}
        {metadata.some(([, value]) => value !== undefined) && (
          <details className="delegation-execution">
            <summary>{t("delegateExecution")}</summary>
            <dl>
              {metadata
                .filter(([, value]) => value !== undefined)
                .map(([key, value]) => (
                  <div key={key}>
                    <dt>{t(key)}</dt>
                    <dd>{value}</dd>
                  </div>
                ))}
            </dl>
          </details>
        )}
        <details className="delegation-raw">
          <summary>{t("delegateRaw")}</summary>
          <pre>{delegationRaw(row.args, row.result)}</pre>
        </details>
      </div>
    </ToolDisclosure>
  );
}
