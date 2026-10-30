import { Check, MessageCircleQuestion } from "lucide-react";
import type { ReactNode } from "react";
import { ToolDisclosure, type ToolExpansion } from "./ToolDisclosure";
import { translate, type Language } from "./i18n";
import type { Row } from "./transcript";
import { answerFor, askRaw, type AskInfo } from "./ask";
import "./ask-card.css";

type ToolRow = Extract<Row, { kind: "tool" }>;

/** Historical evidence only. The existing native dialog owns interactive answers. */
export function AskCard({
  row,
  info,
  expansion,
  language,
  timing,
}: {
  row: ToolRow;
  info: AskInfo;
  expansion: ToolExpansion;
  language: Language;
  timing?: ReactNode;
}) {
  const t = (key: Parameters<typeof translate>[1]) => translate(language, key);
  const waiting = row.status === "pending" || row.status === "running";
  const associated = info.questions.map((question) =>
    answerFor(info, question),
  );
  return (
    <ToolDisclosure
      expansion={expansion}
      toolName={row.name}
      action={t("askUser")}
      target={info.questions.map((question) => question.question).join(" · ")}
      status={row.status}
      language={language}
      className="ask-card"
      timing={timing}
    >
      <div className="tool-detail ask-detail">
        {info.questions.map((question, index) => {
          const answer = associated[index];
          return (
            <section className="ask-question" key={`${question.id}-${index}`}>
              {question.header && (
                <div className="ask-header">{question.header}</div>
              )}
              <h4 className="ask-prompt">
                <MessageCircleQuestion size={14} aria-hidden="true" />
                {question.question}
              </h4>
              {question.context && (
                <p className="ask-context">{question.context}</p>
              )}
              {question.options.length > 0 && (
                <ul className="ask-options" aria-label={t("askChoices")}>
                  {question.options.map((option, optionIndex) => {
                    const selected = Boolean(
                      answer?.values.includes(option.title),
                    );
                    return (
                      <li
                        key={optionIndex}
                        className={selected ? "selected" : ""}
                      >
                        <span className="ask-choice-mark" aria-hidden="true">
                          {selected ? <Check size={12} /> : null}
                        </span>
                        <span>
                          {selected && (
                            <span className="sr-only">
                              {t("askSelected")}
                              {": "}
                            </span>
                          )}
                          {option.title}
                          {option.description && (
                            <small>{option.description}</small>
                          )}
                        </span>
                      </li>
                    );
                  })}
                </ul>
              )}
              {answer && (
                <div className="ask-answer">
                  <strong>{t("answer")}</strong>
                  {info.confirm && answer.confirmed !== undefined && (
                    <p className="ask-confirmation">
                      {t(answer.confirmed ? "askConfirmed" : "askDeclined")}
                    </p>
                  )}
                  {answer.values.length > 0 && (
                    <ul className="ask-values">
                      {answer.values.map((value, i) => (
                        <li key={i}>{value}</li>
                      ))}
                    </ul>
                  )}
                  {answer.text && !answer.values.includes(answer.text) && (
                    <p className="ask-answer-text">{answer.text}</p>
                  )}
                </div>
              )}
            </section>
          );
        })}
        {info.cancelled && <p className="ask-cancelled">{t("askCancelled")}</p>}
        {info.summary && (
          <div
            className={`ask-result ${row.status === "error" ? "ask-error" : ""}`}
          >
            <strong>{t("output")}</strong>
            <p>{info.summary}</p>
          </div>
        )}
        {!info.cancelled && !info.summary && !associated.some(Boolean) && (
          <p className="muted ask-empty">
            {t(waiting ? "askWaiting" : "askNoAnswer")}
          </p>
        )}
        <details className="ask-raw">
          <summary>{t("askRaw")}</summary>
          <pre>{askRaw(row)}</pre>
        </details>
      </div>
    </ToolDisclosure>
  );
}
