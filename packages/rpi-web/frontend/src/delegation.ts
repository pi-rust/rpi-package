export const isDelegationTool = (name: string) =>
  name
    .trim()
    .toLowerCase()
    .replace(/^functions\./, "") === "delegate_task";

const record = (value: unknown): Record<string, unknown> | null =>
  value && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
const text = (value: unknown) =>
  typeof value === "string" ? value.trim() : "";
const integer = (value: unknown, min: number, max = Number.MAX_SAFE_INTEGER) =>
  typeof value === "number" &&
  Number.isSafeInteger(value) &&
  value >= min &&
  value <= max
    ? value
    : undefined;
const parse = (value: unknown): unknown => {
  if (typeof value !== "string") return value;
  try {
    return JSON.parse(value);
  } catch {
    return value;
  }
};

/** Native content envelopes and live text, decoded with a fixed work/depth bound. */
function outputOf(result: unknown) {
  let value = result;
  for (let depth = 0; depth < 6; depth++) {
    if (typeof value === "string") {
      const source = value.trim();
      if (!/^[\[{]/.test(source))
        return { object: null, plain: source, unknown: false };
      const decoded = parse(source);
      if (decoded === source) return { object: null, plain: "", unknown: true };
      value = decoded;
      continue;
    }
    const object = record(value);
    if (!object) return { object: null, plain: "", unknown: value != null };
    if (object.kind !== undefined) return { object, plain: "", unknown: false };
    if (Array.isArray(object.content)) {
      value = object.content
        .map((block) => {
          const item = record(block);
          return item?.type === "text" ? text(item.text) : "";
        })
        .filter(Boolean)
        .join("\n");
    } else if (typeof object.content === "string") value = object.content;
    else if (typeof object.text === "string") value = object.text;
    else return { object, plain: "", unknown: false };
  }
  return { object: null, plain: "", unknown: true };
}

export interface DelegationInfo {
  task: string;
  role: string;
  report: string;
  status: string;
  structured: boolean;
  unknownResult: boolean;
  turnLimited: boolean;
  turns?: number;
  maxTurns?: number;
  timeoutSeconds?: number;
  childId: string;
  sessionId: string;
}

/** Read-only evidence; never start, follow, cancel or infer success for a child. */
export function delegationInfo(
  args: unknown,
  result: unknown,
): DelegationInfo | null {
  const input = record(parse(args));
  const decoded = outputOf(result);
  const output = decoded.object;
  const structured = output?.kind === "rpi-delegation";
  const native = structured ? output : null;
  const task = text(native?.task) || text(input?.task);
  if (!task) return null;
  return {
    task,
    role: text(native?.role) || text(input?.role),
    report: structured
      ? typeof native?.report === "string" && native.report.trim()
        ? native.report
        : text(native?.error) || text(native?.message)
      : decoded.plain || text(output?.error) || text(output?.message),
    status: text(native?.status),
    structured,
    unknownResult:
      decoded.unknown ||
      Boolean(
        output && !structured && !text(output.error) && !text(output.message),
      ),
    turnLimited: native?.turnLimited === true,
    turns: integer(native?.turns, 0),
    maxTurns:
      integer(native?.maxTurns, 1, 32) ?? integer(input?.maxTurns, 1, 32),
    timeoutSeconds:
      integer(native?.timeoutSeconds, 10, 1800) ??
      integer(input?.timeoutSeconds, 10, 1800),
    childId: text(native?.childId),
    sessionId: text(native?.sessionId),
  };
}

export function delegationRaw(args: unknown, result: unknown): string {
  try {
    return JSON.stringify({ input: args, output: result }, null, 2);
  } catch {
    return String(result ?? "");
  }
}
