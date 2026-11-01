import { useEffect, useId, useRef, useState, type ReactNode } from "react";
import {
  Activity,
  AlertTriangle,
  Bot,
  Check,
  ChevronDown,
  ChevronRight,
  Maximize2,
  Minimize2,
  RotateCcw,
  User,
  Wrench,
  X,
} from "lucide-react";
import type { TraceGroup, TraceNode, TraceView } from "./executionTrace";
import { compareTraceDurations } from "./traceComparison";
import { traceBar, traceLayout, type TraceTreeNode } from "./traceLayout";

type Language = "en" | "zh";
export interface ExecutionTraceProps {
  view: TraceView;
  language: Language;
  onClose: () => void;
}
const labels = {
  en: {
    title: "Execution trace",
    close: "Close",
    expand: "Expand trace",
    compact: "Compact trace",
    scope:
      "Loaded messages and observed events only; not the full prompt or provider API trace.",
    clipped: "Some trace entries were clipped to keep this view bounded.",
    partial:
      "Partial history or observation: run boundaries may be unknown. Missing steps are not inferred.",
    empty: "No loaded messages or observed execution events yet.",
    emptyGroup: "No steps are available in this observed fragment.",
    live: "Observed run",
    history: "Loaded history",
    runId: "Run ID",
    runElapsed: "Run elapsed",
    comparison: "Duration comparison",
    ranking: "Duration ranking",
    comparisonScope: "Loaded steps · longest 5 recorded durations",
    comparisonRule:
      "Bars are relative to the longest recorded step, not shares of run time. Parallel steps are not added; run totals and retry delays are excluded.",
    comparisonEmpty: "No recorded step durations yet. Unknown is not zero.",
    comparisonRecorded: "Recorded",
    comparisonSteps: "steps",
    comparisonUnknown: "unknown",
    untimedLlm: "LLM durations unknown; excluded from ranking",
    slowest: "Longest recorded",
    tiedSlowest: "Tied longest",
    revealStep: "Show step details",
    runningTiming:
      "Running values are from the latest native event, not final durations.",
    zeroTiming:
      "Recorded values are all 0 ms; no slowest step can be distinguished.",
    input: "User message (loaded excerpt)",
    llm: "LLM response",
    tool: "Tool",
    retry: "Native retry",
    settlement: "Run settlement",
    notice: "Notice",
    unknown: "Unknown",
    running: "Running",
    completed: "Completed",
    failed: "Failed",
    stopped: "Stopped",
    declined: "Declined",
    model: "Model",
    provider: "Provider",
    llmElapsed: "LLM elapsed",
    elapsed: "Elapsed",
    inputTokens: "Input",
    outputTokens: "Output",
    cacheRead: "Cache read",
    cacheWrite: "Cache write",
    total: "Total tokens",
    cost: "Cost",
    excerpt: "Loaded excerpt",
    output: "Output summary",
    arguments: "Tool arguments",
    result: "Tool result",
    settlementInput: "Settlement input",
    settlementOutput: "Settlement summary",
    error: "Error",
    observedInput: "Observed input",
    noDetails: "No further details were observed in the available data.",
    attempt: "Attempt",
    maxRetries: "Max retries",
    delay: "Retry delay",
    nativeOnly:
      "Retry metadata comes only from native events; no inferred attempts.",
    missingText:
      "Unknown — not available in the loaded messages or observed events.",
    overview: "Overview",
    inputTab: "Input",
    outputTab: "Output",
    observations: "Observations",
    timeline: "Timeline",
    startUnknown: "Start unknown",
    legend:
      "Solid: validated native bounds. Dashed: duration only, start unknown. Unknown time is not zero. Each execution uses its own origin.",
    details: "Observation details",
    status: "Status",
    nodeId: "Observation ID",
    partialLabel: "Partial observation",
    yes: "Yes",
    no: "No",
  },
  zh: {
    title: "执行轨迹",
    close: "关闭",
    expand: "展开轨迹",
    compact: "收窄轨迹",
    scope: "仅基于已加载消息和已观测事件，不代表完整提示词或提供商 API 轨迹。",
    clipped: "为限制视图大小，部分轨迹条目已被裁剪。",
    partial: "历史或观测不完整：执行边界可能未知，不推断缺失步骤。",
    empty: "尚无已加载消息或已观测执行事件。",
    emptyGroup: "此观测片段中没有可用步骤。",
    live: "已观测执行",
    history: "已加载历史片段",
    runId: "执行 ID",
    runElapsed: "执行耗时",
    comparison: "耗时对比",
    ranking: "耗时排行",
    comparisonScope: "已加载步骤 · 已记录耗时 Top 5",
    comparisonRule:
      "横条相对于最长已记录步骤，并非执行总耗时占比。并行步骤不相加；执行总耗时和重试等待不参与排行。",
    comparisonEmpty: "暂无已记录的步骤耗时，未知不等于 0。",
    comparisonRecorded: "已记录",
    comparisonSteps: "个步骤",
    comparisonUnknown: "未知",
    untimedLlm: "个 LLM 步骤耗时未知，未参与排行",
    slowest: "已知最慢",
    tiedSlowest: "并列最慢",
    revealStep: "查看步骤详情",
    runningTiming: "执行中数值来自最近一次原生事件，尚非最终耗时。",
    zeroTiming: "记录值均为 0 ms，暂无法区分最慢步骤。",
    input: "用户消息（已加载摘录）",
    llm: "LLM 响应",
    tool: "工具",
    retry: "原生重试",
    settlement: "执行结算",
    notice: "提示",
    unknown: "未知",
    running: "执行中",
    completed: "已完成",
    failed: "失败",
    stopped: "已停止",
    declined: "已拒绝",
    model: "模型",
    provider: "提供商",
    llmElapsed: "LLM 耗时",
    elapsed: "耗时",
    inputTokens: "输入",
    outputTokens: "输出",
    cacheRead: "缓存读取",
    cacheWrite: "缓存写入",
    total: "总令牌数",
    cost: "费用",
    excerpt: "已加载摘录",
    output: "输出摘要",
    arguments: "工具参数",
    result: "工具结果",
    settlementInput: "结算输入",
    settlementOutput: "结算摘要",
    error: "错误",
    observedInput: "已观测输入",
    noDetails: "可用数据中未观测到更多详情。",
    attempt: "尝试次数",
    maxRetries: "最大重试次数",
    delay: "重试延迟",
    nativeOnly: "重试元数据仅来自原生事件，不推断尝试次数。",
    missingText: "未知 — 已加载消息或已观测事件中未提供。",
    overview: "概览",
    inputTab: "输入",
    outputTab: "输出",
    observations: "观测步骤",
    timeline: "时间轴",
    startUnknown: "起点未知",
    legend:
      "实线：已验证的原生时间边界。虚线：仅有耗时，起点未知。未知耗时不等于 0。各次执行使用独立起点。",
    details: "观测详情",
    status: "状态",
    nodeId: "观测 ID",
    partialLabel: "观测不完整",
    yes: "是",
    no: "否",
  },
};
type Labels = typeof labels.en;
type DetailTab = "overview" | "input" | "output";
const icons = {
  input: User,
  llm: Bot,
  tool: Wrench,
  retry: RotateCcw,
  settlement: Check,
  notice: AlertTriangle,
};
const duration = (ms: number) =>
  ms < 1000 ? `${ms} ms` : `${(ms / 1000).toFixed(2)} s`;
const knownNumber = (value: number | undefined, precise = false) =>
  value === undefined
    ? "—"
    : value.toLocaleString(
        undefined,
        precise ? { maximumSignificantDigits: 8 } : undefined,
      );
const nodeName = (node: TraceNode, t: Labels) =>
  `${t[node.kind]}${node.name ? ` · ${node.name}` : node.kind === "llm" && node.model ? ` · ${node.model}` : ""}`;
const nodeKey = (id: string) => `node:${id}`;
const groupKey = (id: string) => `group:${id}`;
const treeCount = (roots: TraceTreeNode[]): number =>
  roots.reduce((sum, tree) => sum + 1 + treeCount(tree.children), 0);

function Status({ status, t }: { status: TraceNode["status"]; t: Labels }) {
  return (
    <span className={`trace-status trace-status--${status}`}>{t[status]}</span>
  );
}

function DurationComparison({
  view,
  t,
  onReveal,
}: {
  view: TraceView;
  t: Labels;
  onReveal: (id: string) => void;
}) {
  const comparison = compareTraceDurations(view);
  return (
    <section className="trace-comparison" aria-label={t.comparison}>
      <h3>{t.comparison}</h3>
      <p className="trace-muted">{t.comparisonScope}</p>
      {comparison.totalCount > 0 && (
        <p className="trace-comparison-coverage">
          {t.comparisonRecorded} {comparison.knownCount}/{comparison.totalCount}{" "}
          {t.comparisonSteps} · {comparison.unknownCount} {t.comparisonUnknown}
        </p>
      )}
      {comparison.unknownLlmCount > 0 && (
        <p className="trace-muted">
          {comparison.unknownLlmCount} {t.untimedLlm}
        </p>
      )}
      {comparison.knownCount === 0 ? (
        <p className="trace-muted">{t.comparisonEmpty}</p>
      ) : (
        <>
          <ol className="trace-duration-list">
            {comparison.steps.map(
              ({ node, groupSource, groupIndex, ratio }) => {
                const slowest =
                  comparison.maxMs > 0 && node.elapsedMs === comparison.maxMs;
                const name = nodeName(node, t);
                const group = `${groupSource === "live" ? t.live : t.history} #${groupIndex + 1}`;
                const Icon = icons[node.kind];
                return (
                  <li key={node.id}>
                    <button
                      type="button"
                      className={`trace-duration-step${slowest ? " trace-duration-step--slowest" : ""}`}
                      aria-label={`${name} · ${duration(node.elapsedMs)} · ${group} · ${t.revealStep}`}
                      onClick={() => onReveal(node.id)}
                    >
                      <span className="trace-duration-heading">
                        <span className="trace-duration-name">
                          <Icon size={13} aria-hidden="true" />
                          {name}
                        </span>
                        <strong>{duration(node.elapsedMs)}</strong>
                      </span>
                      <span className="trace-duration-bar" aria-hidden="true">
                        <span style={{ width: `${ratio * 100}%` }} />
                      </span>
                      <span className="trace-duration-caption">
                        <span>{group}</span>
                        <Status status={node.status} t={t} />
                        {slowest && (
                          <span className="trace-slowest-label">
                            {comparison.slowestCount > 1
                              ? t.tiedSlowest
                              : t.slowest}
                          </span>
                        )}
                      </span>
                    </button>
                  </li>
                );
              },
            )}
          </ol>
          {comparison.maxMs === 0 && (
            <p className="trace-muted">{t.zeroTiming}</p>
          )}
          {comparison.steps.some((step) => step.node.status === "running") && (
            <p className="trace-muted">{t.runningTiming}</p>
          )}
        </>
      )}
      <p className="trace-comparison-rule">{t.comparisonRule}</p>
    </section>
  );
}

function TextDetail({
  title,
  text,
  t,
  error = false,
}: {
  title: string;
  text?: string;
  t: Labels;
  error?: boolean;
}) {
  return (
    <section
      className={`trace-text${error ? " trace-error" : ""}`}
      aria-label={title}
    >
      <h4>{title}</h4>
      {text !== undefined && text !== "" ? (
        <pre>{text}</pre>
      ) : (
        <p className="trace-muted">{t.missingText}</p>
      )}
    </section>
  );
}

function NodeDetails({
  node,
  t,
  tab,
}: {
  node: TraceNode;
  t: Labels;
  tab: DetailTab;
}) {
  const usage = node.usage;
  const inputTitle =
    node.kind === "input"
      ? t.excerpt
      : node.kind === "tool"
        ? t.arguments
        : node.kind === "settlement"
          ? t.settlementInput
          : t.observedInput;
  const outputTitle =
    node.kind === "tool"
      ? t.result
      : node.kind === "settlement"
        ? t.settlementOutput
        : t.output;
  return (
    <div className="trace-node-details">
      {tab === "overview" && (
        <>
          <dl className="trace-metadata">
            <div>
              <dt>{t.nodeId}</dt>
              <dd>{node.id}</dd>
            </div>
            <div>
              <dt>{t.status}</dt>
              <dd>
                <Status status={node.status} t={t} />
              </dd>
            </div>
            {node.runId !== undefined && (
              <div>
                <dt>{t.runId}</dt>
                <dd>{node.runId}</dd>
              </div>
            )}
            <div>
              <dt>{node.kind === "llm" ? t.llmElapsed : t.elapsed}</dt>
              <dd>
                {node.elapsedMs === undefined
                  ? t.unknown
                  : duration(node.elapsedMs)}
              </dd>
            </div>
            {node.kind === "llm" && (
              <>
                <div>
                  <dt>{t.model}</dt>
                  <dd>{node.model ?? t.unknown}</dd>
                </div>
                <div>
                  <dt>{t.provider}</dt>
                  <dd>{node.provider ?? t.unknown}</dd>
                </div>
              </>
            )}
          </dl>
          {node.kind === "llm" && (
            <dl className="trace-usage" aria-label={t.llm}>
              <div>
                <dt>{t.inputTokens}</dt>
                <dd>{knownNumber(usage?.input)}</dd>
              </div>
              <div>
                <dt>{t.outputTokens}</dt>
                <dd>{knownNumber(usage?.output)}</dd>
              </div>
              <div>
                <dt>{t.cacheRead}</dt>
                <dd>{knownNumber(usage?.cached)}</dd>
              </div>
              <div>
                <dt>{t.cacheWrite}</dt>
                <dd>{knownNumber(usage?.cacheWrite)}</dd>
              </div>
              <div>
                <dt>{t.total}</dt>
                <dd>{knownNumber(usage?.total)}</dd>
              </div>
              <div>
                <dt>{t.cost}</dt>
                <dd>{knownNumber(usage?.cost, true)}</dd>
              </div>
            </dl>
          )}
          {node.kind === "retry" && (
            <>
              <p className="trace-muted">{t.nativeOnly}</p>
              <dl className="trace-metadata">
                <div>
                  <dt>{t.attempt}</dt>
                  <dd>{knownNumber(node.attempt)}</dd>
                </div>
                <div>
                  <dt>{t.maxRetries}</dt>
                  <dd>{knownNumber(node.maxRetries)}</dd>
                </div>
                <div>
                  <dt>{t.delay}</dt>
                  <dd>
                    {node.delayMs === undefined
                      ? t.unknown
                      : duration(node.delayMs)}
                  </dd>
                </div>
              </dl>
            </>
          )}
        </>
      )}
      {tab === "input" && (
        <TextDetail title={inputTitle} text={node.input} t={t} />
      )}
      {(tab === "output" ||
        (tab === "overview" && node.output !== undefined)) && (
        <TextDetail title={outputTitle} text={node.output} t={t} />
      )}
      {tab === "overview" && node.kind === "input" && (
        <TextDetail title={inputTitle} text={node.input} t={t} />
      )}
      {tab !== "input" && node.error !== undefined && (
        <TextDetail title={t.error} text={node.error} t={t} error />
      )}
      {tab === "overview" &&
        node.input === undefined &&
        node.output === undefined &&
        node.error === undefined && (
          <p className="trace-muted">{t.noDetails}</p>
        )}
    </div>
  );
}

function SelectionDetails({
  node,
  group,
  count,
  t,
}: {
  node?: TraceNode;
  group?: TraceGroup;
  count: number;
  t: Labels;
}) {
  const [tab, setTab] = useState<DetailTab>("overview");
  const bodyRef = useRef<HTMLDivElement>(null);
  const title = node
    ? nodeName(node, t)
    : group
      ? group.source === "live"
        ? t.live
        : t.history
      : t.title;
  return (
    <section
      className="trace-details"
      data-testid="trace-details"
      aria-label={t.details}
    >
      <header className="trace-details-heading">
        <span className="trace-muted">{t.details}</span>
        <h3 title={title}>{title}</h3>
        {node && (
          <nav className="trace-tabs" aria-label={t.details}>
            {(["overview", "input", "output"] as const).map((value) => (
              <button
                key={value}
                type="button"
                aria-pressed={tab === value}
                onClick={() => {
                  setTab(value);
                  if (bodyRef.current) bodyRef.current.scrollTop = 0;
                }}
              >
                {value === "overview"
                  ? t.overview
                  : value === "input"
                    ? t.inputTab
                    : t.outputTab}
              </button>
            ))}
          </nav>
        )}
      </header>
      <div className="trace-details-body" ref={bodyRef}>
        {node ? (
          <NodeDetails node={node} t={t} tab={tab} />
        ) : group ? (
          <>
            <dl className="trace-metadata">
              <div>
                <dt>{t.runId}</dt>
                <dd>{group.runId ?? t.unknown}</dd>
              </div>
              <div>
                <dt>{t.status}</dt>
                <dd>
                  <Status status={group.status} t={t} />
                </dd>
              </div>
              <div>
                <dt>{t.runElapsed}</dt>
                <dd>
                  {group.elapsedMs === undefined
                    ? t.unknown
                    : duration(group.elapsedMs)}
                </dd>
              </div>
              <div>
                <dt>{t.observations}</dt>
                <dd>{count}</dd>
              </div>
              <div>
                <dt>{t.partialLabel}</dt>
                <dd>
                  {group.partial || group.source === "history" ? t.yes : t.no}
                </dd>
              </div>
            </dl>
            <p className="trace-muted">{t.scope}</p>
            {(group.partial || group.source === "history") && (
              <p className="trace-warning">{t.partial}</p>
            )}
            {group.nodes.length === 0 && (
              <p className="trace-muted">{t.emptyGroup}</p>
            )}
          </>
        ) : (
          <p className="trace-muted">{t.empty}</p>
        )}
      </div>
    </section>
  );
}

function TimeBar({
  item,
  group,
  maxMs,
  t,
}: {
  item: TraceNode | TraceGroup;
  group: TraceGroup;
  maxMs: number;
  t: Labels;
}) {
  const bar = traceBar(item, group, maxMs);
  const description =
    bar.mode === "unknown"
      ? t.unknown
      : `${bar.elapsedMs === undefined ? t.unknown : duration(bar.elapsedMs)}${bar.mode === "duration" ? ` · ${t.startUnknown}` : ""}`;
  return (
    <span
      className={`trace-time-cell trace-time-cell--${bar.mode}`}
      title={description}
    >
      <span className="trace-time-track" aria-hidden="true">
        <span
          className="trace-time-bar"
          data-timing-mode={bar.mode}
          style={{ left: `${bar.left * 100}%`, width: `${bar.width * 100}%` }}
        />
      </span>
      <span className="trace-time-label">{description}</span>
    </span>
  );
}

export function ExecutionTrace({
  view,
  language,
  onClose,
}: ExecutionTraceProps) {
  const t = labels[language];
  const rootRef = useRef<HTMLElement>(null);
  const closeRef = useRef<HTMLButtonElement>(null);
  const pendingReveal = useRef<string | null>(null);
  const panelId = useId();
  const [wide, setWide] = useState(true);
  const [selected, setSelected] = useState<string | null>(null);
  const [folded, setFolded] = useState<Set<string>>(() => new Set());
  const layout = traceLayout(view);
  const comparison = compareTraceDurations(view);
  const hasScale =
    layout.maxMs > 0 ||
    view.groups.some(
      (group) =>
        group.elapsedMs === 0 ||
        group.nodes.some(
          (node) =>
            node.elapsedMs === 0 &&
            ["llm", "tool", "settlement"].includes(node.kind),
        ),
    );
  const entries = new Map<
    string,
    { node?: TraceNode; group: TraceGroup; ancestors: string[] }
  >();
  const nativeGroup = new Map<string, TraceGroup>();
  view.groups.forEach((group) =>
    group.nodes.forEach((node) => nativeGroup.set(node.id, group)),
  );
  const indexTree = (
    tree: TraceTreeNode,
    group: TraceGroup,
    ancestors: string[],
  ) => {
    const key = nodeKey(tree.node.id);
    entries.set(key, { node: tree.node, group, ancestors });
    tree.children.forEach((child) =>
      indexTree(child, group, [...ancestors, key]),
    );
  };
  layout.groups.forEach(({ group, roots }) => {
    const key = groupKey(group.id);
    entries.set(key, { group, ancestors: [] });
    roots.forEach((tree) => indexTree(tree, group, [key]));
  });
  const fallback =
    [...layout.groups].reverse().find(({ roots }) => roots.length > 0) ??
    layout.groups[layout.groups.length - 1];
  const selection =
    selected && entries.has(selected)
      ? selected
      : fallback
        ? groupKey(fallback.group.id)
        : null;
  const active = selection ? entries.get(selection) : undefined;
  // Prune removed selection, while keeping the selected ID stable across stream updates.
  useEffect(() => {
    if (selected !== selection) setSelected(selection);
  }, [selected, selection]);
  // A long-lived navigation can observe many more runs than the retained view.
  // Never retain fold IDs after their bounded trace entries have been evicted.
  useEffect(() => {
    setFolded((current) => {
      const retained = new Set([...current].filter((key) => entries.has(key)));
      return retained.size === current.size ? current : retained;
    });
  });
  useEffect(() => {
    closeRef.current?.focus();
  }, []);
  useEffect(() => {
    const id = pendingReveal.current;
    if (!id) return;
    const row = Array.from(
      rootRef.current?.querySelectorAll<HTMLButtonElement>(".trace-node") ?? [],
    ).find((item) => item.dataset.traceNodeId === id);
    if (!entries.has(nodeKey(id))) {
      pendingReveal.current = null;
      return;
    }
    if (row) {
      row.focus({ preventScroll: true });
      const scroller = row.closest<HTMLElement>(".trace-outline-body");
      if (scroller) {
        const bounds = scroller.getBoundingClientRect();
        const target = row.getBoundingClientRect();
        if (target.top < bounds.top)
          scroller.scrollTop += target.top - bounds.top;
        else if (target.bottom > bounds.bottom)
          scroller.scrollTop += target.bottom - bounds.bottom;
      }
      pendingReveal.current = null;
    }
  });
  const revealStep = (id: string) => {
    const key = nodeKey(id);
    const entry = entries.get(key);
    if (!entry) return;
    setFolded((current) => {
      const next = new Set(current);
      entry.ancestors.forEach((ancestor) => next.delete(ancestor));
      return next;
    });
    pendingReveal.current = id;
    setSelected(key);
  };
  const toggle = (key: string) => {
    const closing = !folded.has(key);
    if (closing && active?.ancestors.includes(key)) setSelected(key);
    setFolded((current) => {
      const next = new Set(current);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  };
  const controlsId = (key: string) => `${panelId}-${encodeURIComponent(key)}`;
  const caret = (key: string, name: string) => (
    <button
      type="button"
      className="trace-caret"
      aria-label={name}
      aria-expanded={!folded.has(key)}
      aria-controls={controlsId(key)}
      onClick={() => toggle(key)}
    >
      {folded.has(key) ? (
        <ChevronRight size={13} aria-hidden="true" />
      ) : (
        <ChevronDown size={13} aria-hidden="true" />
      )}
    </button>
  );
  const renderTree = (
    tree: TraceTreeNode,
    group: TraceGroup,
    depth: number,
  ): ReactNode => {
    const node = tree.node;
    const key = nodeKey(node.id);
    const Icon = icons[node.kind];
    const indent = Math.min(depth, 6) * 10;
    const slowest =
      (node.kind === "llm" || node.kind === "tool") &&
      comparison.maxMs > 0 &&
      node.elapsedMs === comparison.maxMs;
    const failed = node.error !== undefined || node.status === "failed";
    return (
      <div className="trace-branch" key={key}>
        <div
          className={`trace-observation trace-kind--${node.kind}${failed ? " trace-observation--error" : ""}${slowest ? " trace-observation--slowest" : ""}`}
        >
          <div className="trace-caret-slot" style={{ left: `${indent + 1}px` }}>
            {tree.children.length > 0 && caret(key, nodeName(node, t))}
          </div>
          <button
            type="button"
            className="trace-node trace-row-select"
            data-trace-node-id={node.id}
            data-selected={selection === key ? "true" : "false"}
            aria-pressed={selection === key}
            onClick={() => setSelected(key)}
          >
            <span
              className="trace-row-name"
              style={{ paddingLeft: `${indent + 25}px` }}
              title={nodeName(node, t)}
            >
              <Icon size={13} aria-hidden="true" />
              <span className="trace-step-name">{nodeName(node, t)}</span>
              <Status status={node.status} t={t} />
              {slowest && (
                <span className="trace-slowest-label">
                  {comparison.slowestCount > 1 ? t.tiedSlowest : t.slowest}
                </span>
              )}
            </span>
            <TimeBar
              item={node}
              group={nativeGroup.get(node.id) ?? group}
              maxMs={layout.maxMs}
              t={t}
            />
          </button>
        </div>
        <div id={controlsId(key)} hidden={folded.has(key)}>
          {tree.children.map((child) => renderTree(child, group, depth + 1))}
        </div>
      </div>
    );
  };
  return (
    <aside
      ref={rootRef}
      className={`inspector execution-trace${wide ? " execution-trace--wide" : ""}`}
      aria-label={t.title}
      data-testid="execution-trace"
      onKeyDown={(event) => {
        if (event.key === "Escape") {
          event.stopPropagation();
          onClose();
        }
      }}
    >
      <div className="panel-heading">
        <h2>
          <Activity size={16} aria-hidden="true" />
          {t.title}
          <span className="trace-muted trace-total">
            {view.groups.reduce((sum, group) => sum + group.nodes.length, 0)}{" "}
            {t.comparisonSteps}
          </span>
        </h2>
        <div className="trace-heading-actions">
          <button
            type="button"
            className="icon-button"
            onClick={() => setWide((value) => !value)}
            aria-label={wide ? t.compact : t.expand}
            title={wide ? t.compact : t.expand}
          >
            {wide ? (
              <Minimize2 size={16} aria-hidden="true" />
            ) : (
              <Maximize2 size={16} aria-hidden="true" />
            )}
          </button>
          <button
            ref={closeRef}
            type="button"
            className="icon-button"
            onClick={onClose}
            aria-label={t.close}
            title={t.close}
          >
            <X size={16} aria-hidden="true" />
          </button>
        </div>
      </div>
      <div className="trace-scope-strip">
        <p className="trace-scope">{t.scope}</p>
        {view.clipped && (
          <p className="trace-warning">
            <AlertTriangle size={12} aria-hidden="true" />
            {t.clipped}
          </p>
        )}
      </div>
      <div className="trace-workbench">
        <section className="trace-outline" aria-label={t.observations}>
          <div className="trace-outline-header">
            <span>{t.observations}</span>
            <span>
              {t.timeline}
              <span className="trace-axis">
                <span>0</span>
                <span>{hasScale ? duration(layout.maxMs) : t.unknown}</span>
              </span>
            </span>
          </div>
          <div className="trace-outline-body">
            {layout.groups.length === 0 && (
              <p className="trace-empty trace-muted">{t.empty}</p>
            )}
            {layout.groups.map(({ group, roots }, index) => {
              const key = groupKey(group.id);
              const name = `${group.source === "live" ? t.live : t.history} #${index + 1}`;
              return (
                <section
                  key={group.id}
                  className="trace-group"
                  aria-label={group.source === "live" ? t.live : t.history}
                >
                  <div
                    className={`trace-observation trace-group-row${group.status === "failed" ? " trace-observation--error" : ""}`}
                  >
                    <div className="trace-caret-slot">{caret(key, name)}</div>
                    <button
                      type="button"
                      className="trace-group-select trace-row-select"
                      aria-pressed={selection === key}
                      data-selected={selection === key ? "true" : "false"}
                      onClick={() => setSelected(key)}
                    >
                      <span className="trace-row-name" title={group.runId}>
                        <Activity size={13} aria-hidden="true" />
                        <span className="trace-step-name">{name}</span>
                        <span className="trace-count">{treeCount(roots)}</span>
                        <Status status={group.status} t={t} />
                      </span>
                      <TimeBar
                        item={group}
                        group={group}
                        maxMs={layout.maxMs}
                        t={t}
                      />
                    </button>
                  </div>
                  <div id={controlsId(key)} hidden={folded.has(key)}>
                    {roots.map((tree) => renderTree(tree, group, 1))}
                    {roots.length === 0 && (
                      <p className="trace-empty trace-muted">{t.emptyGroup}</p>
                    )}
                  </div>
                </section>
              );
            })}
            <details className="trace-ranking">
              <summary>{t.ranking}</summary>
              <DurationComparison view={view} t={t} onReveal={revealStep} />
            </details>
          </div>
          <p className="trace-legend">{t.legend}</p>
        </section>
        <SelectionDetails
          key={selection ?? "empty"}
          node={active?.node}
          group={active?.group}
          count={treeCount(
            layout.groups.find(({ group }) => group.id === active?.group.id)
              ?.roots ?? [],
          )}
          t={t}
        />
      </div>
    </aside>
  );
}

export default ExecutionTrace;
