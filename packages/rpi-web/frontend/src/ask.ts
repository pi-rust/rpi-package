import type { Row } from "./transcript";

type ToolRow = Extract<Row, { kind: "tool" }>;

export const isAskTool = (name: string) =>
  ["ask_user", "ask"].includes(
    name
      .trim()
      .toLowerCase()
      .replace(/^functions\./, ""),
  );

const record = (value: unknown): Record<string, unknown> | null =>
  value && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
const text = (value: unknown) =>
  typeof value === "string" ? value.trim() : "";
const field = (value: Record<string, unknown>, ...keys: string[]) =>
  keys.map((key) => text(value[key])).find(Boolean) ?? "";
const json = (value: unknown): unknown => {
  if (typeof value !== "string" || !/^[\s]*[\[{]/.test(value)) return value;
  try {
    return JSON.parse(value);
  } catch {
    return value;
  }
};
const strings = (value: unknown): string[] =>
  Array.isArray(value)
    ? value.filter((item): item is string => typeof item === "string")
    : typeof value === "string"
      ? [value]
      : [];

export interface AskQuestion {
  id: string;
  question: string;
  header: string;
  context: string;
  options: { title: string; description: string }[];
}
export interface AskAnswer {
  id: string;
  question: string;
  values: string[];
  text: string;
  confirmed?: boolean;
}
export interface AskInfo {
  questions: AskQuestion[];
  answers: AskAnswer[];
  confirm: boolean;
  cancelled: boolean;
  summary: string;
}

/** Read-only projection: do not interpret transport frames or submit answers here. */
export function askInfo(args: unknown, result: unknown): AskInfo | null {
  const input = record(json(args));
  if (!input) return null;
  const confirm = input.type === "confirm";
  const rawQuestions = confirm
    ? [input]
    : Array.isArray(input.questions)
      ? input.questions
      : [input];
  if (!rawQuestions.length || rawQuestions.length > 3) return null;
  const questions: AskQuestion[] = [];
  for (const [index, raw] of rawQuestions.entries()) {
    const item = record(raw);
    if (!item) return null;
    const question = confirm
      ? field(item, "summary", "question")
      : field(item, "question", "prompt") || field(input, "question", "prompt");
    if (!question) return null;
    const options: AskQuestion["options"] = [];
    if (confirm) {
      options.push(
        { title: "Yes", description: "" },
        { title: "No", description: "" },
      );
    } else if (Array.isArray(item.options)) {
      for (const value of item.options) {
        const option = record(value);
        const title =
          typeof value === "string"
            ? value.trim()
            : option
              ? field(option, "title", "label", "text", "value", "name")
              : "";
        if (title)
          options.push({
            title,
            description: option ? text(option.description) : "",
          });
      }
    }
    questions.push({
      id: confirm
        ? "confirm"
        : field(item, "id", "questionId") || `q${index + 1}`,
      question,
      header: field(item, "header") || field(input, "header"),
      context: field(item, "context") || field(input, "context"),
      options,
    });
  }
  const payload = json(result);
  let output = record(payload);
  let summary = "";
  if (output && Array.isArray(output.content)) {
    summary = output.content
      .map((block) => {
        const item = record(block);
        return item?.type === "text" ? text(item.text) : "";
      })
      .filter(Boolean)
      .join("\n");
  } else if (output) summary = text(output.content) || text(output.text);
  else if (typeof payload === "string") summary = payload;
  const decoded = record(json(summary));
  if (decoded) {
    output = decoded;
    summary = "";
  }
  // JSON which is not a recognized answer remains available in the raw disclosure.
  if (/^\s*[\[{]/.test(summary)) summary = "";
  const details = record(output?.details);
  summary ||=
    field(output ?? {}, "error", "message") || field(details ?? {}, "error");
  const rawAnswers = details?.answers ?? output?.answers;
  const answers: AskAnswer[] = [];
  const add = (raw: unknown, key = "") => {
    const answer = record(raw);
    if (!answer) {
      const values = strings(raw);
      if (values.length)
        answers.push({ id: key, question: "", values, text: "" });
      return;
    }
    const confirmed =
      typeof answer.confirmed === "boolean" ? answer.confirmed : undefined;
    const values = strings(answer.values ?? answer.value ?? answer.selected);
    const answerText = field(answer, "text", "answer", "comment");
    if (!values.length && !answerText && confirmed === undefined) return;
    answers.push({
      id: field(answer, "id", "questionId") || key,
      question: text(answer.question),
      values,
      text: answerText,
      confirmed,
    });
  };
  if (Array.isArray(rawAnswers)) rawAnswers.forEach((value) => add(value));
  else if (record(rawAnswers))
    Object.entries(rawAnswers as Record<string, unknown>).forEach(
      ([id, value]) => add(value, id),
    );
  else if (output?.answer !== undefined) add(output.answer);
  else if (
    output &&
    (output.values !== undefined ||
      output.value !== undefined ||
      output.selected !== undefined ||
      typeof output.confirmed === "boolean")
  )
    add(output);
  return {
    questions,
    answers,
    confirm,
    cancelled:
      output?.cancelled === true ||
      output?.status === "cancelled" ||
      details?.cancelled === true,
    summary,
  };
}

export function answerFor(
  info: AskInfo,
  question: AskQuestion,
): AskAnswer | undefined {
  const byId = info.answers.filter((answer) => answer.id === question.id);
  if (
    byId.length === 1 &&
    info.questions.filter((item) => item.id === question.id).length === 1
  )
    return byId[0];
  const byQuestion = info.answers.filter(
    (answer) => !answer.id && answer.question === question.question,
  );
  if (
    byQuestion.length === 1 &&
    info.questions.filter((item) => item.question === question.question)
      .length === 1
  )
    return byQuestion[0];
  if (
    info.questions.length === 1 &&
    info.answers.length === 1 &&
    !info.answers[0].id &&
    !info.answers[0].question
  )
    return info.answers[0];
}

/** Raw data is explicitly opt-in and preserves unsupported/error payloads. */
export function askRaw(row: ToolRow) {
  try {
    return JSON.stringify({ input: row.args, output: row.result }, null, 2);
  } catch {
    return String(row.result ?? "");
  }
}
