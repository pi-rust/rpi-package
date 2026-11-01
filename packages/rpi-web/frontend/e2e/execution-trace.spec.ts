import {
  test as base,
  expect,
  type Locator,
  type Page,
  type Route,
} from "@playwright/test";
import { chooseMenuOption } from "./controls";

// An independent, fail-closed host: no prompt, mutation, real SSE or native host.
// POST open and run-timings/query are the existing read-only Web contracts.
const A = "trace-a",
  B = "trace-b",
  projectId = "trace-project";
const titles = { [A]: "Trace Alpha", [B]: "Trace Beta" };
const epoch = 1_700_000_000_000;
type Json = Record<string, unknown>;
type Call = { id: string; kind: string; body: Json };
type Bridge = Window & {
  __traceEmit: (
    id: string,
    type: string,
    data: Json,
    lastEventId: string,
  ) => number;
  __traceClose: () => void;
  __traceConnections: (id: string) => number;
};
const reads = [
  "get_state",
  "get_messages",
  "get_queue",
  "get_stats",
  "get_history",
  "get_auth_status",
  "get_available_models",
  "get_available_thinking_levels",
];
const envelope = (command: string, data: unknown) => ({
  type: "response",
  command,
  success: true,
  data,
});
function gate() {
  let release!: () => void;
  const promise = new Promise<void>((resolve) => {
    release = resolve;
  });
  return { promise, release };
}
function user(id: string, content = `Loaded question ${id}`): Json {
  return { role: "user", entryId: `${id}-user`, timestamp: epoch, content };
}
function assistant(id: string, extra: Json = {}): Json {
  return {
    role: "assistant",
    entryId: `${id}-assistant`,
    timestamp: epoch + 100,
    model: "historical-model",
    provider: "historical-provider",
    stopReason: "stop",
    content: [{ type: "text", text: `Loaded answer ${id}` }],
    ...extra,
  };
}
function history(id = A): Json[] {
  return [
    user(id),
    assistant(id, {
      content: [
        { type: "text", text: `Loaded answer ${id}` },
        {
          type: "toolCall",
          id: `${id}-read`,
          name: "read",
          arguments: { path: "notes.txt" },
          webTiming: { elapsedMs: 25, running: false },
        },
      ],
    }),
    {
      role: "toolResult",
      entryId: `${id}-result`,
      timestamp: epoch + 200,
      toolCallId: `${id}-read`,
      toolName: "read",
      content: [{ type: "text", text: "Fixture file contents" }],
      isError: false,
      webTiming: { elapsedMs: 25, running: false },
    },
  ];
}
function snapshot(
  id: string,
  messages: Json[] = history(id),
  extra: Json = {},
): Json {
  return {
    sessionId: id,
    status: "ready",
    lastSeq: 10,
    messages,
    events: [],
    total: messages.length,
    truncated: false,
    hasMore: false,
    firstId: messages[0]?.entryId ?? null,
    lastId: messages.at(-1)?.entryId ?? null,
    leafId: `${id}-leaf`,
    pendingResume: false,
    pendingDialogs: [],
    latestSettlement: null,
    ...extra,
  };
}

class MockHost {
  snapshots: Record<string, Json> = { [A]: snapshot(A), [B]: snapshot(B) };
  older: Record<string, Json> = {};
  timings: Json[] = [];
  snapshotUnauthorized = false;
  calls: Call[] = [];
  unexpected: string[] = [];
  private closing = false;
  private holds = new Map<string, ReturnType<typeof gate>>();
  constructor(readonly page: Page) {}
  count(id: string, kind: string) {
    return this.calls.filter((c) => c.id === id && c.kind === kind).length;
  }
  hold(id: string, kind: string) {
    const ticket = gate();
    this.holds.set(`${id}:${kind}`, ticket);
    return {
      release: () => {
        this.holds.delete(`${id}:${kind}`);
        ticket.release();
      },
    };
  }
  private summary(id: string) {
    return {
      sessionId: id,
      projectId,
      title: titles[id as keyof typeof titles],
      status: this.snapshots[id].status,
      createdAt: epoch,
      updatedAt: epoch + (id === A ? 100 : 0),
      lastSeq: this.snapshots[id].lastSeq,
      latestSettlement: this.snapshots[id].latestSettlement,
      error: null,
      pinned: false,
      archived: false,
    };
  }
  async install() {
    await this.page.addInitScript(() => {
      if (!localStorage.getItem("rpi-web-language"))
        localStorage.setItem("rpi-web-language", "en");
      const sources = new Set<MockEventSource>();
      class MockEventSource extends EventTarget {
        static CONNECTING = 0;
        static OPEN = 1;
        static CLOSED = 2;
        CONNECTING = 0;
        OPEN = 1;
        CLOSED = 2;
        readyState = 0;
        url: string;
        withCredentials = false;
        sessionId: string;
        onopen: ((event: Event) => void) | null = null;
        onerror: ((event: Event) => void) | null = null;
        onmessage: ((event: MessageEvent) => void) | null = null;
        constructor(url: string | URL) {
          super();
          this.url = new URL(String(url), location.href).href;
          this.sessionId =
            new URL(this.url).pathname.match(
              /\/sessions\/([^/]+)\/events$/,
            )?.[1] ?? "";
          sources.add(this);
          queueMicrotask(() => {
            if (this.readyState === 2) return;
            this.readyState = 1;
            const event = new Event("open");
            this.dispatchEvent(event);
            this.onopen?.(event);
          });
        }
        close() {
          this.readyState = 2;
          sources.delete(this);
        }
      }
      Object.defineProperty(window, "EventSource", {
        configurable: true,
        value: MockEventSource,
      });
      const bridge = window as unknown as Bridge;
      bridge.__traceConnections = (id) =>
        [...sources].filter((s) => s.sessionId === id && s.readyState === 1)
          .length;
      bridge.__traceEmit = (id, type, data, lastEventId) => {
        let delivered = 0;
        for (const source of sources) {
          if (source.sessionId !== id || source.readyState !== 1) continue;
          source.dispatchEvent(
            new MessageEvent(type, { data: JSON.stringify(data), lastEventId }),
          );
          delivered++;
        }
        return delivered;
      };
      bridge.__traceClose = () => {
        for (const source of [...sources]) source.close();
      };
    });
    await this.page.route("**/api/**", async (route: Route) => {
      const request = route.request(),
        path = new URL(request.url()).pathname,
        method = request.method();
      const reject = async (reason = "") => {
        this.unexpected.push(`${method} ${path} ${reason}`.trim());
        await route.fulfill({
          status: 501,
          json: { error: "Unexpected API in execution trace mock" },
        });
      };
      if (method === "GET" && path === "/api/v1/auth")
        return route.fulfill({
          json: { required: false, authenticated: true },
        });
      if (method === "GET" && path === "/api/v1/capabilities")
        return route.fulfill({
          json: {
            protocol: "rpi",
            supportedCommands: reads,
            supportedActions: [
              "projects",
              "session_metadata",
              "history",
              "queue",
              "stats",
              "provider_auth",
            ],
            parityComplete: false,
          },
        });
      if (method === "GET" && path === "/api/v1/projects")
        return route.fulfill({
          json: {
            projects: [
              {
                id: projectId,
                name: "Trace project",
                path: "/mock/trace",
                archived: false,
              },
            ],
            defaultProjectId: projectId,
            projectsRoot: "/mock",
          },
        });
      if (method === "GET" && path === "/api/v1/sessions")
        return route.fulfill({
          json: { sessions: [this.summary(A), this.summary(B)] },
        });
      const match = path.match(
        /^\/api\/v1\/sessions\/([^/]+)\/(open|snapshot|commands|run-timings\/query)$/,
      );
      // In particular, real EventSource /events is never admitted, even on GET.
      if (!match || ![A, B].includes(match[1])) return reject();
      const [, id, action] = match;
      if (method !== (action === "snapshot" ? "GET" : "POST"))
        return reject("wrong method");
      const body =
        action === "commands" || action === "run-timings/query"
          ? (request.postDataJSON() as Json)
          : {};
      const kind = action === "commands" ? String(body.type) : action;
      if (action === "commands" && !reads.includes(kind))
        return reject(`unplanned mutation ${kind}`);
      this.calls.push({ id, kind, body });
      // Capture before a gate: late A replies exercise the real navigation guard.
      const frame = structuredClone(this.snapshots[id]);
      try {
        const hold = this.holds.get(`${id}:${kind}`);
        if (hold) await hold.promise;
        if (kind === "open")
          return await route.fulfill({ json: this.summary(id) });
        if (kind === "snapshot")
          return await route.fulfill(
            this.snapshotUnauthorized
              ? { status: 401, json: { error: "Mock access expired" } }
              : { json: frame },
          );
        if (kind === "run-timings/query")
          return await route.fulfill({
            json: {
              timings: this.timings,
              complete: true,
              cursor: null,
              scannedBytes: 0,
              totalBytes: 0,
            },
          });
        const data: Record<string, unknown> = {
          get_state: {
            sessionId: id,
            modelId: "runtime-current-model",
            provider: "runtime-current-provider",
            thinkingLevel: "high",
            activeOperation: null,
            activeTools: [],
            queue: { steeringCount: 0, followUpCount: 0 },
          },
          get_messages: this.older[id] ?? {
            messages: [],
            total: 0,
            hasMore: false,
            firstId: null,
          },
          get_queue: { items: [], count: 0 },
          get_stats: { messageCount: 3, totalTokens: 0, costTotal: 0 },
          get_history: {
            records: [],
            total: 0,
            limit: 40,
            hasMore: false,
            openOperations: [],
          },
          get_auth_status: { providers: [] },
          get_available_models: {
            models: [
              {
                id: "runtime-current-model",
                name: "Current model",
                provider: "runtime-current-provider",
              },
            ],
          },
          get_available_thinking_levels: { levels: ["high"] },
        };
        return await route.fulfill({ json: envelope(kind, data[kind]) });
      } catch (error) {
        if (!this.closing && !request.failure()) throw error;
      }
    });
  }
  async emit(id: string, seq: number, event: Json) {
    await expect
      .poll(() =>
        this.page.evaluate(
          (id) => (window as unknown as Bridge).__traceConnections(id),
          id,
        ),
      )
      .toBe(1);
    const delivered = await this.page.evaluate(
      ({ id, seq, event }) =>
        (window as unknown as Bridge).__traceEmit(
          id,
          String(event.type),
          { ...event, seq },
          String(seq),
        ),
      { id, seq, event },
    );
    expect(delivered).toBe(1);
    await rendered(this.page);
  }
  async boundary(id: string, seq: number, event: Json, extra: Json = {}) {
    const before = this.count(id, "snapshot");
    // App refreshes at start/settlement. Advance the mocked durable cursor first.
    this.snapshots[id] = {
      ...this.snapshots[id],
      lastSeq: seq,
      status: event.type === "agent_start" ? "running" : "ready",
      events: [{ seq, event: { ...event, seq } }],
      ...extra,
    };
    const response = this.page.waitForResponse(
      (r) => new URL(r.url()).pathname === `/api/v1/sessions/${id}/snapshot`,
    );
    await this.emit(id, seq, event);
    await response;
    await expect.poll(() => this.count(id, "snapshot")).toBe(before + 1);
    await rendered(this.page);
  }
  async dispose() {
    this.closing = true;
    for (const hold of this.holds.values()) hold.release();
    this.holds.clear();
    if (!this.page.isClosed())
      await this.page.evaluate(() =>
        (window as unknown as Bridge).__traceClose?.(),
      );
    await this.page.unrouteAll({ behavior: "wait" });
  }
}
const test = base.extend<{ host: MockHost }>({
  host: async ({ page }, use) => {
    const host = new MockHost(page);
    await host.install();
    try {
      await use(host);
    } finally {
      await host.dispose();
      expect(
        host.unexpected,
        "every API terminates in the mock; no unplanned mutation or real SSE",
      ).toEqual([]);
    }
  },
});
const pane = (page: Page) => page.getByTestId("execution-trace");
const toggle = (page: Page, zh = false) =>
  page.getByRole("button", {
    name: zh ? "执行轨迹" : "Execution trace",
    exact: true,
  });
const nodes = (page: Page, name: string) =>
  pane(page)
    .locator(".trace-node")
    .filter({
      has: page.locator(".trace-step-name", {
        hasText: new RegExp(
          `^${name}${name === "LLM response" || name === "LLM 响应" ? "(?: · .+)?" : ""}$`,
        ),
      }),
    });
async function rendered(page: Page) {
  await page.evaluate(
    () =>
      new Promise<void>((resolve) =>
        requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
      ),
  );
}
async function ready(page: Page, host: MockHost, id = A, catalogs = 1) {
  await expect
    .poll(() => host.count(id, "get_available_thinking_levels"))
    .toBeGreaterThanOrEqual(catalogs);
  await expect
    .poll(() =>
      page.evaluate(
        (id) => (window as unknown as Bridge).__traceConnections(id),
        id,
      ),
    )
    .toBe(1);
  await rendered(page);
}
async function open(page: Page, host: MockHost, id = A) {
  await page.goto(`/?session=${id}`, { waitUntil: "domcontentloaded" });
  await ready(page, host, id);
}
async function show(page: Page, zh = false) {
  await toggle(page, zh).click();
  await expect(pane(page)).toBeVisible();
  await rendered(page);
}
const details = (page: Page) => page.getByTestId("trace-details");
async function expand(node: Locator) {
  await expect(node).toHaveCount(1);
  await node.click();
  await expect(node).toHaveAttribute("aria-pressed", "true");
  await expect(node).toHaveAttribute("data-selected", "true");
}
function value(node: Locator, label: string) {
  return details(node.page())
    .locator("dl > div")
    .filter({
      has: node.page().locator("dt", { hasText: new RegExp(`^${label}$`) }),
    })
    .locator("dd");
}
async function select(page: Page, id: string) {
  await page
    .getByRole("button", {
      name: new RegExp(`^${titles[id as keyof typeof titles]}`),
    })
    .click();
  await expect(page).toHaveURL(new RegExp(`session=${id}`));
}
async function start(page: Page, host: MockHost) {
  await open(page, host);
  await show(page);
  await host.boundary(A, 11, { type: "agent_start", runId: "live-run" });
}
const liveGroup = (page: Page) =>
  pane(page).getByRole("region", { name: "Observed run", exact: true });

// 1: Opening is a local projection, not a trace API or a transcript scan.
test("closed by default; loaded user, LLM, correlated tool and native run timing", async ({
  page,
  host,
}) => {
  host.timings = [
    {
      runId: "history-run",
      finalEntryId: `${A}-assistant`,
      outcome: "completed",
      startedAt: epoch,
      finishedAt: epoch + 1500,
      elapsedMs: 1500,
    },
  ];
  await open(page, host);
  await expect(pane(page)).toHaveCount(0);
  await expect(toggle(page)).toHaveAttribute("aria-pressed", "false");
  await expect(toggle(page)).toHaveAttribute("title", "Execution trace");
  await expect.poll(() => host.count(A, "run-timings/query")).toBe(1);
  expect(host.calls.find((c) => c.kind === "run-timings/query")?.body).toEqual({
    entryIds: [`${A}-user`, `${A}-assistant`, `${A}-result`],
  });
  // Initial diagnostics are deliberately deferred; finish them before measuring
  // whether opening the local projection itself causes another request.
  await expect.poll(() => host.count(A, "get_auth_status")).toBe(1);
  await rendered(page);
  const before = [...host.calls];
  await show(page);
  await expect(toggle(page)).toHaveAttribute("aria-pressed", "true");
  await expect(nodes(page, "User message \\(loaded excerpt\\)")).toHaveCount(1);
  await expand(nodes(page, "LLM response"));
  await expect(value(nodes(page, "LLM response"), "Model")).toHaveText(
    "historical-model",
  );
  await expect(value(nodes(page, "LLM response"), "Provider")).toHaveText(
    "historical-provider",
  );
  await expect(value(nodes(page, "LLM response"), "LLM elapsed")).toHaveText(
    "Unknown",
  );
  await expand(nodes(page, "Tool · read"));
  await expect(details(page)).toContainText("Fixture file contents");
  await expect(value(nodes(page, "Tool · read"), "Elapsed")).toHaveText(
    "25 ms",
  );
  await expand(nodes(page, "Run settlement"));
  await expect(value(nodes(page, "Run settlement"), "Elapsed")).toHaveText(
    "1.50 s",
  );
  expect(host.calls).toEqual(before);
  expect(host.count(A, "get_messages")).toBe(0);
});

test("partial history shows explicit unknown metadata rather than current runtime model", async ({
  page,
  host,
}) => {
  host.snapshots[A] = snapshot(
    A,
    [
      user(A),
      assistant(A, { model: undefined, provider: undefined, usage: undefined }),
    ],
    { truncated: true },
  );
  await open(page, host);
  await show(page);
  await expect(pane(page)).toContainText("run boundaries may be unknown");
  await expand(nodes(page, "LLM response"));
  for (const label of ["Model", "Provider", "LLM elapsed"])
    await expect(value(nodes(page, "LLM response"), label)).toHaveText(
      "Unknown",
    );
  for (const label of [
    "Input",
    "Output",
    "Cache read",
    "Cache write",
    "Total tokens",
    "Cost",
  ])
    await expect(value(nodes(page, "LLM response"), label)).toHaveText("—");
  await expect(pane(page)).not.toContainText("runtime-current-model");
  await expect(pane(page)).not.toContainText("runtime-current-provider");
  await expect(nodes(page, "Run settlement")).toHaveCount(0);
});

test("native usage preserves zero values and separate cache and cost metadata", async ({
  page,
  host,
}) => {
  host.snapshots[A] = snapshot(A, [
    assistant(A, {
      usage: {
        input: 0,
        output: 0,
        cacheRead: 42,
        cacheWrite: 7,
        totalTokens: 49,
        cost: { input: 0, output: 0, cacheRead: 0.125, total: 0.125 },
      },
    }),
  ]);
  await open(page, host);
  await show(page);
  await expand(nodes(page, "LLM response"));
  for (const [label, expected] of Object.entries({
    Input: "0",
    Output: "0",
    "Cache read": "42",
    "Cache write": "7",
    "Total tokens": "49",
    Cost: "0.125",
  }))
    await expect(value(nodes(page, "LLM response"), label)).toHaveText(
      expected,
    );
});

test("desktop trace has no horizontal overflow and produces an inspection screenshot", async ({
  page,
  host,
}, info) => {
  await page.setViewportSize({ width: 1440, height: 1000 });
  host.snapshots[A] = snapshot(
    A,
    comparisonHistory([
      { name: "functions.read", ms: 125 },
      { name: "functions.bash", ms: 3250 },
      { name: "functions.search", ms: 780 },
    ]),
  );
  host.timings = [
    {
      runId: "desktop-native-run",
      finalEntryId: "compare-result-2",
      outcome: "completed",
      startedAt: epoch,
      finishedAt: epoch + 5000,
      elapsedMs: 5000,
    },
  ];
  await open(page, host);
  await show(page);
  await expand(nodes(page, "LLM response"));
  await expect(pane(page)).toHaveClass(/inspector execution-trace/);
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
  ).toBe(true);
  expect(
    await pane(page).evaluate((el) => el.scrollWidth <= el.clientWidth),
  ).toBe(true);
  await page.screenshot({ path: info.outputPath("trace-desktop.png") });
  await page.screenshot({
    path: info.outputPath("langfuse-final-desktop.png"),
  });
});

test("Chinese 390px trace remains bounded and produces a mobile screenshot", async ({
  page,
  host,
}, info) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.addInitScript(() =>
    localStorage.setItem("rpi-web-language", "zh"),
  );
  await open(page, host);
  await show(page, true);
  await expect(toggle(page, true)).toHaveAttribute("title", "执行轨迹");
  await expect(
    pane(page).getByRole("heading", { name: "执行轨迹", exact: true }),
  ).toBeVisible();
  await expect(
    pane(page).getByRole("button", { name: "关闭", exact: true }),
  ).toBeFocused();
  await expand(nodes(page, "LLM 响应"));
  await expect(value(nodes(page, "LLM 响应"), "模型")).toHaveText(
    "historical-model",
  );
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
  ).toBe(true);
  expect(
    await pane(page).evaluate((el) => el.scrollWidth <= el.clientWidth),
  ).toBe(true);
  await page.screenshot({ path: info.outputPath("trace-mobile-zh.png") });
  await page.screenshot({
    path: info.outputPath("langfuse-final-mobile-zh.png"),
  });
});

test("pane-scoped Escape and Close restore focus; outside Escape does not close it", async ({
  page,
  host,
}) => {
  await open(page, host);
  await show(page);
  await expect(
    pane(page).getByRole("button", { name: "Close", exact: true }),
  ).toBeFocused();
  await page.locator(".composer textarea").focus();
  await page.keyboard.press("Escape");
  await expect(pane(page)).toBeVisible();
  await pane(page).getByRole("button", { name: "Close", exact: true }).focus();
  await page.keyboard.press("Escape");
  await expect(pane(page)).toHaveCount(0);
  await expect(toggle(page)).toBeFocused();
  await show(page);
  await pane(page).getByRole("button", { name: "Close", exact: true }).click();
  await expect(toggle(page)).toBeFocused();
  await expect(toggle(page)).toHaveAttribute("aria-pressed", "false");
});

test("trace, inspector and advertised runtime panel are mutually exclusive", async ({
  page,
  host,
}) => {
  await open(page, host);
  await show(page);
  await page
    .getByRole("button", { name: "Toggle inspector", exact: true })
    .click();
  await expect(pane(page)).toHaveCount(0);
  await expect(
    page.locator("aside.inspector:not(.execution-trace)"),
  ).toBeVisible();
  await show(page);
  await expect(
    page.locator("aside.inspector:not(.execution-trace)"),
  ).toHaveCount(0);
  await page
    .getByRole("button", { name: "Runtime settings", exact: true })
    .click();
  await expect(pane(page)).toHaveCount(0);
  await expect(
    page.getByRole("heading", { name: "Runtime settings", exact: true }),
  ).toBeVisible();
  await show(page);
  await expect(
    page.getByRole("heading", { name: "Runtime settings", exact: true }),
  ).toHaveCount(0);
  // Files capability is intentionally absent: no Workbench filesystem reads.
});

test("details are inert plain text with secrets, thinking and image data removed", async ({
  page,
  host,
}) => {
  const html = '<img src=x onerror="alert(1)"><script>alert(2)</script>';
  host.snapshots[A] = snapshot(A, [
    assistant(A, {
      content: [
        {
          type: "text",
          text: `${html} <think>PRIVATE_THOUGHT</think> Bearer forbidden-bearer sk-forbidden-key`,
        },
        { type: "thinking", thinking: "PRIVATE_REASONING" },
        { type: "image", data: "PRIVATE_IMAGE", mimeType: "image/png" },
        {
          type: "toolCall",
          id: "unsafe-call",
          name: "read",
          arguments: {
            path: "notes.txt",
            apiKey: "PRIVATE_KEY",
            password: "PRIVATE_PASSWORD",
            reasoning: "PRIVATE_REASONING",
            image: "PRIVATE_IMAGE",
            text: "data:image/png;base64,QUJDREVGRw==",
          },
        },
      ],
    }),
    {
      role: "toolResult",
      entryId: "unsafe-result",
      timestamp: epoch + 200,
      toolCallId: "unsafe-call",
      toolName: "read",
      isError: true,
      content: [
        {
          type: "text",
          text: `${html} token=PRIVATE_TOKEN data:image/png;base64,QUJDREVGRw==\nAuthorization: Basic UFJJVkFURV9CQVNJQw==\nCookie: session=PRIVATE_SESSION; csrf=PRIVATE_CSRF`,
        },
      ],
    },
  ]);
  let dialogs = 0;
  page.on("dialog", async (dialog) => {
    dialogs++;
    await dialog.dismiss();
  });
  await open(page, host);
  await show(page);
  for (const detail of await pane(page).locator(".trace-node").all()) {
    await expand(detail);
    for (const tab of ["Overview", "Input", "Output"]) {
      await details(page)
        .getByRole("button", { name: tab, exact: true })
        .click();
      await expect(details(page).locator("img, script, iframe")).toHaveCount(0);
      for (const forbidden of [
        "PRIVATE_",
        "forbidden-bearer",
        "sk-forbidden-key",
        "UFJJVkFURV9CQVNJQw==",
        "QUJDREVGRw==",
        "base64,",
      ])
        await expect(details(page)).not.toContainText(forbidden);
    }
  }
  await expand(nodes(page, "Tool · read"));
  await expect(
    details(page).locator("pre").filter({ hasText: html }).first(),
  ).toBeVisible();
  await expect(pane(page)).toContainText("[redacted]");
  await expect(nodes(page, "Tool · read").locator(".trace-status")).toHaveText(
    "Failed",
  );
  await expect(pane(page).locator(".trace-error").first()).toBeVisible();
  await expect(pane(page).locator("img, script, iframe")).toHaveCount(0);
  for (const forbidden of [
    "PRIVATE_",
    "forbidden-bearer",
    "sk-forbidden-key",
    "UFJJVkFURV9CQVNJQw==",
    "QUJDREVGRw==",
    "base64,",
  ])
    await expect(pane(page)).not.toContainText(forbidden);
  await expect(page.getByRole("alertdialog")).toHaveCount(0);
  expect(dialogs).toBe(0);
});

test("live start/accept and message lifecycle merge once; parallel tools correlate by call ID", async ({
  page,
  host,
}) => {
  await start(page, host);
  await host.emit(A, 12, {
    type: "operation_accepted",
    runId: "live-run",
    operationId: "operation-1",
  });
  const message = assistant("live", {
    stopReason: undefined,
    content: [{ type: "text", text: "Partial" }],
  });
  await host.emit(A, 13, { type: "message_start", message });
  await host.emit(A, 14, {
    type: "message_update",
    message: { ...message, content: [{ type: "text", text: "Updated" }] },
  });
  await host.emit(A, 15, {
    type: "message_end",
    message: {
      ...message,
      stopReason: "toolUse",
      content: [
        { type: "text", text: "Final live answer" },
        {
          type: "toolCall",
          id: "parallel-read",
          name: "read",
          arguments: { path: "a.txt" },
        },
        {
          type: "toolCall",
          id: "parallel-find",
          name: "find",
          arguments: { pattern: "needle" },
        },
      ],
    },
  });
  await host.emit(A, 16, {
    type: "tool_execution_start",
    toolCallId: "parallel-read",
    toolName: "read",
    args: { path: "a.txt" },
  });
  await host.emit(A, 17, {
    type: "tool_execution_start",
    toolCallId: "parallel-find",
    toolName: "find",
  });
  await host.emit(A, 18, {
    type: "tool_execution_end",
    toolCallId: "parallel-find",
    toolName: "find",
    result: { content: [{ type: "text", text: "Found needle" }] },
    isError: false,
  });
  await host.emit(A, 19, {
    type: "tool_execution_end",
    toolCallId: "parallel-read",
    toolName: "read",
    result: { content: [{ type: "text", text: "Read a.txt" }] },
    isError: false,
  });
  await expect(liveGroup(page)).toHaveCount(1);
  await expect(
    liveGroup(page).locator(".trace-step-name", {
      hasText: /^LLM response(?: · .+)?$/,
    }),
  ).toHaveCount(1);
  await expect(liveGroup(page).locator(".trace-node")).toHaveCount(3);
  const read = liveGroup(page)
    .locator(".trace-node")
    .filter({ hasText: "Tool · read" });
  const find = nodes(page, "Tool · find");
  await expand(read);
  await expect(details(page)).toContainText("Read a.txt");
  await expect(details(page)).not.toContainText("Found needle");
  await expand(find);
  await expect(details(page)).toContainText("Found needle");
  await expect(details(page)).not.toContainText("Read a.txt");
  await expect(read.locator(".trace-status")).toHaveText("Completed");
});

test("native retry displays actual attempt, error and delay and rejects repeated sequence", async ({
  page,
  host,
}) => {
  await start(page, host);
  const event = {
    type: "retry_scheduled",
    attempt: 2,
    maxRetries: 4,
    delayMs: 1250,
    error: "Mock provider throttled",
  };
  await host.emit(A, 12, event);
  await host.emit(A, 12, event);
  await expand(nodes(page, "Native retry"));
  await expect(value(nodes(page, "Native retry"), "Attempt")).toHaveText("2");
  await expect(value(nodes(page, "Native retry"), "Max retries")).toHaveText(
    "4",
  );
  await expect(value(nodes(page, "Native retry"), "Retry delay")).toHaveText(
    "1.25 s",
  );
  await expect(details(page)).toContainText("Mock provider throttled");
  await expect(details(page)).toContainText("no inferred attempts");
  await expect(nodes(page, "Native retry")).toHaveCount(1);
});

test("host closure clears running trace observations without a fabricated settlement", async ({ page, host }) => {
  await start(page, host);
  await host.emit(A, 12, { type: "message_start", message: assistant("closing", { stopReason: undefined }) });
  await host.emit(A, 13, { type: "tool_execution_start", toolCallId: "closing-read", toolName: "read" });
  await expect(liveGroup(page).locator(".trace-status--running")).not.toHaveCount(0);
  await host.emit(A, 14, { type: "host_closed" });
  await expect(liveGroup(page).locator(".trace-group-select .trace-status")).toHaveText("Unknown");
  await expect(liveGroup(page).locator(".trace-status--running")).toHaveCount(0);
  await expect(liveGroup(page).locator(".trace-node")).toHaveCount(2);
  await expect(nodes(page, "Run settlement")).toHaveCount(0);
});

for (const outcome of ["failed", "stopped"] as const) {
  test(`agent_end is not settlement; native ${outcome} settles the observed run`, async ({
    page,
    host,
  }) => {
    await start(page, host);
    await host.emit(A, 12, { type: "agent_end", messages: [] });
    await expect(
      liveGroup(page).locator(".trace-group-select .trace-status"),
    ).toHaveText("Running");
    await expect(nodes(page, "Run settlement")).toHaveCount(0);
    const event = {
      type: "agent_settled",
      runId: "live-run",
      outcome,
      aborted: outcome === "stopped",
      summary: `Mock ${outcome} settlement`,
      error: outcome === "failed" ? "Mock failure" : undefined,
    };
    await host.boundary(A, 13, event, { latestSettlement: event });
    await expect(
      liveGroup(page).locator(".trace-group-select .trace-status"),
    ).toHaveText(outcome === "failed" ? "Failed" : "Stopped");
    await expand(nodes(page, "Run settlement"));
    await expect(details(page)).toContainText(`Mock ${outcome} settlement`);
  });
}

test("snapshot/live overlap and boundary refresh retain one LLM/tool and latest settlement", async ({
  page,
  host,
}) => {
  await start(page, host);
  const message = assistant("overlap", {
    content: [
      { type: "text", text: "Durable overlap answer" },
      {
        type: "toolCall",
        id: "overlap-call",
        name: "find",
        arguments: { pattern: "overlap" },
      },
    ],
  });
  const result = {
    role: "toolResult",
    entryId: "overlap-result",
    timestamp: epoch + 200,
    toolName: "find",
    toolCallId: "overlap-call",
    content: [{ type: "text", text: "Overlap result" }],
    isError: false,
  };
  await host.emit(A, 12, { type: "message_start", message });
  await host.emit(A, 13, { type: "message_end", message });
  await host.emit(A, 14, {
    type: "tool_execution_end",
    toolName: "find",
    toolCallId: "overlap-call",
    result: { content: result.content },
    isError: false,
  });
  const settled = {
    type: "agent_settled",
    runId: "live-run",
    outcome: "completed",
    summary: "Latest native settlement",
  };
  await host.boundary(A, 15, settled, {
    messages: [...history(), message, result],
    latestSettlement: settled,
    events: [
      { seq: 13, event: { type: "message_end", seq: 13, message } },
      {
        seq: 14,
        event: {
          type: "tool_execution_end",
          seq: 14,
          toolName: "find",
          toolCallId: "overlap-call",
          result: { content: result.content },
        },
      },
      { seq: 15, event: { ...settled, seq: 15 } },
    ],
  });
  await host.emit(A, 14, {
    type: "tool_execution_end",
    toolName: "find",
    toolCallId: "overlap-call",
    result: "STALE RESULT",
  });
  await expect(nodes(page, "LLM response")).toHaveCount(2); // One historical and one live.
  await expect(nodes(page, "Tool · find")).toHaveCount(1);
  await expect(nodes(page, "Run settlement")).toHaveCount(1);
  await expand(nodes(page, "Tool · find"));
  await expect(details(page)).toContainText("Overlap result");
  await expect(pane(page)).not.toContainText("STALE RESULT");
  await expand(nodes(page, "Run settlement"));
  await expect(details(page)).toContainText("Latest native settlement");
  await expect(
    liveGroup(page).locator(".trace-group-select .trace-status"),
  ).toHaveText("Completed");
});

test("A to held B to A clears live state and never leaks A into loading B", async ({
  page,
  host,
}) => {
  await start(page, host);
  await host.emit(A, 12, {
    type: "message_end",
    message: assistant("transient", {
      content: [{ type: "text", text: "A live transient only" }],
    }),
  });
  await expand(nodes(page, "LLM response").last());
  await expect(details(page)).toContainText("A live transient only");
  const held = host.hold(B, "snapshot");
  await select(page, B);
  await expect.poll(() => host.count(B, "snapshot")).toBe(1);
  await expect(pane(page)).toBeVisible();
  await expect(pane(page)).not.toContainText("A live transient only");
  await expect(pane(page)).not.toContainText(`Loaded answer ${A}`);
  await expect(pane(page).locator(".trace-node")).toHaveCount(0);
  // The current authoritative history has no replay of the old live fragment.
  host.snapshots[A] = snapshot(A);
  const catalogs = host.count(A, "get_available_thinking_levels") + 1;
  await select(page, A);
  await ready(page, host, A, catalogs);
  held.release();
  await rendered(page);
  await expand(nodes(page, "LLM response"));
  await expect(details(page)).toContainText(`Loaded answer ${A}`);
  await expect(pane(page)).not.toContainText("A live transient only");
  await expect(pane(page)).not.toContainText(`Loaded answer ${B}`);
  await expect(liveGroup(page)).toHaveCount(0);
});

test("explicit older pagination merges stable identities; opening the pane never scans", async ({
  page,
  host,
}) => {
  // Fill the ten-question rail up front: this case isolates explicit pagination
  // and proves opening the independent trace does not initiate history reads.
  const loaded = [
    user(A),
    ...Array.from({ length: 9 }, (_, index) => user(`window-${index}`)),
    assistant(A),
  ];
  host.snapshots[A] = snapshot(A, loaded, {
    total: 13,
    hasMore: true,
    truncated: true,
  });
  host.older[A] = {
    messages: [
      user("older", "Older loaded question"),
      assistant("older"),
      loaded[0],
    ],
    total: 13,
    firstId: "older-user",
    hasMore: false,
  };
  await open(page, host);
  expect(host.count(A, "get_messages")).toBe(0);
  await show(page);
  expect(host.count(A, "get_messages")).toBe(0);
  // The expanded trace is a read-only workbench over the transcript.
  // Return to its compact form to use the existing explicit paging control.
  await pane(page)
    .getByRole("button", { name: "Compact trace", exact: true })
    .click();
  await page
    .getByRole("button", { name: "Load earlier messages", exact: true })
    .click();
  await expect.poll(() => host.count(A, "get_messages")).toBe(1);
  await expect(pane(page).locator(".trace-node")).toHaveCount(13);
  await expect(nodes(page, "LLM response")).toHaveCount(2);
  await expect(nodes(page, "User message \\(loaded excerpt\\)")).toHaveCount(
    11,
  );
  expect(host.calls.find((c) => c.kind === "get_messages")?.body).toMatchObject(
    { beforeId: `${A}-user`, limit: 48 },
  );
  await expand(nodes(page, "User message \\(loaded excerpt\\)").first());
  await expect(details(page)).toContainText("Older loaded question");
  await expect(
    page.getByRole("button", { name: "Load earlier messages", exact: true }),
  ).toHaveCount(0);
});

test("large loaded history is clipped to 300 nodes with bounded text and a visible warning", async ({
  page,
  host,
}) => {
  const messages = Array.from({ length: 305 }, (_, i) =>
    assistant(`bounded-${i}`, {
      timestamp: epoch + i,
      content: [
        {
          type: "text",
          text:
            i === 304
              ? `LATEST ${"bounded word ".repeat(400)} NEVER_RETAIN_TAIL`
              : `Bounded answer ${i}`,
        },
      ],
    }),
  );
  host.snapshots[A] = snapshot(A, messages, { total: 305, truncated: true });
  await open(page, host);
  await show(page);
  await expect(pane(page).locator(".trace-node")).toHaveCount(300);
  await expect(pane(page)).toContainText(
    "Some trace entries were clipped to keep this view bounded.",
  );
  const last = pane(page).locator(".trace-node").last();
  await expand(last);
  await expect(details(page)).toContainText("LATEST");
  expect(
    await details(page)
      .locator("pre")
      .evaluate((el) => el.textContent?.length ?? 0),
  ).toBeLessThanOrEqual(2000);
  await expect(pane(page)).not.toContainText("NEVER_RETAIN_TAIL");
  expect(host.count(A, "get_messages")).toBe(0);
});

test("a snapshot-only failure stays visible without assigning older history to that execution", async ({
  page,
  host,
}) => {
  host.snapshots[A] = snapshot(A, history(A), {
    latestSettlement: {
      runId: "snapshot-failed-run",
      outcome: "failed",
      error: "Snapshot-only native failure",
      finalText: "Native final summary",
    },
  });
  await open(page, host);
  await show(page);
  const groups = pane(page).locator(".trace-group");
  await expect(groups).toHaveCount(2);
  await expect(
    groups.first().locator(".trace-group-select .trace-status"),
  ).toHaveText("Unknown");
  await expect(
    groups.last().locator(".trace-group-select .trace-status"),
  ).toHaveText("Failed");
  await expand(nodes(page, "Run settlement"));
  await expect(details(page)).toContainText("snapshot-failed-run");
  await expect(details(page)).toContainText("Snapshot-only native failure");
  await expect(details(page)).toContainText("Native final summary");
});

test("same-session resync clears the previous observation before a held replacement snapshot", async ({
  page,
  host,
}) => {
  await start(page, host);
  await host.emit(A, 12, {
    type: "retry_scheduled",
    attempt: 1,
    maxRetries: 3,
    delayMs: 40,
    error: "OLD_OBSERVED_RETRY",
  });
  await expand(nodes(page, "Native retry"));
  await expect(details(page)).toContainText("OLD_OBSERVED_RETRY");
  const held = host.hold(A, "snapshot");
  host.snapshots[A] = snapshot(A, [
    assistant("new-branch", {
      content: [{ type: "text", text: "RESEEDED_HISTORY" }],
    }),
  ]);
  const before = host.count(A, "snapshot");
  await host.emit(A, 13, { type: "resync_required" });
  await expect.poll(() => host.count(A, "snapshot")).toBe(before + 1);
  await expect(pane(page)).not.toContainText("OLD_OBSERVED_RETRY");
  await expect(liveGroup(page)).toHaveCount(0);
  held.release();
  await ready(page, host, A, 2);
  await expand(nodes(page, "LLM response"));
  await expect(details(page)).toContainText("RESEEDED_HISTORY");
  await expect(pane(page)).not.toContainText("OLD_OBSERVED_RETRY");
});

test("authentication loss clears the trace rather than retaining it behind a login screen", async ({
  page,
  host,
}) => {
  await start(page, host);
  await host.emit(A, 12, {
    type: "retry_scheduled",
    error: "PRIVATE_OBSERVATION",
  });
  await expand(nodes(page, "Native retry"));
  await expect(details(page)).toContainText("PRIVATE_OBSERVATION");
  host.snapshotUnauthorized = true;
  await host.emit(A, 13, { type: "resync_required" });
  await expect(page.locator('input[type="password"]')).toBeVisible();
  await expect(pane(page)).toHaveCount(0);
  await expect(page.locator("body")).not.toContainText("PRIVATE_OBSERVATION");
});

test("small recorded costs are not rounded to a misleading zero", async ({
  page,
  host,
}) => {
  host.snapshots[A] = snapshot(A, [
    assistant(A, { usage: { input: 1, cost: { total: 0.000000123 } } }),
  ]);
  await open(page, host);
  await show(page);
  await expand(nodes(page, "LLM response"));
  await expect(value(nodes(page, "LLM response"), "Cost")).toHaveText(
    "0.000000123",
  );
});

const comparison = (page: Page, zh = false) =>
  pane(page).getByRole("region", {
    name: zh ? "耗时对比" : "Duration comparison",
    exact: true,
  });
async function openRanking(page: Page) {
  const ranking = pane(page).locator("details.trace-ranking");
  if ((await ranking.getAttribute("open")) === null)
    await ranking.locator(":scope > summary").click();
  await expect(ranking).toHaveAttribute("open", "");
}
function comparisonHistory(
  items: Array<{ name: string; ms?: number }>,
): Json[] {
  return [
    assistant(A, {
      content: [
        { type: "text", text: `Loaded answer ${A}` },
        ...items.map(({ name }, i) => ({
          type: "toolCall",
          id: `compare-call-${i}`,
          name,
          arguments: { path: `${name}.txt` },
        })),
      ],
    }),
    ...items.map(({ name, ms }, i) => ({
      role: "toolResult",
      entryId: `compare-result-${i}`,
      toolCallId: `compare-call-${i}`,
      toolName: name,
      timestamp: epoch + 200 + i,
      isError: false,
      content: [{ type: "text", text: `Result of ${name}` }],
      ...(ms === undefined
        ? {}
        : { webTiming: { elapsedMs: ms, running: false } }),
    })),
  ];
}

test("duration comparison highlights and locates the longest recorded step without ranking run totals", async ({
  page,
  host,
}, info) => {
  host.snapshots[A] = snapshot(
    A,
    comparisonHistory([
      { name: "read", ms: 80 },
      { name: "slow-bash", ms: 4000 },
      { name: "search", ms: 1000 },
      { name: "untimed" },
    ]),
  );
  host.timings = [
    {
      runId: "inclusive-total",
      finalEntryId: "compare-result-3",
      outcome: "completed",
      startedAt: epoch,
      finishedAt: epoch + 90000,
      elapsedMs: 90000,
    },
  ];
  await open(page, host);
  await show(page);
  await openRanking(page);
  const chart = comparison(page),
    bars = chart.locator(".trace-duration-step");
  await expect(chart).toContainText("Recorded 3/5 steps · 2 unknown");
  await expect(chart).toContainText(
    "1 LLM durations unknown; excluded from ranking",
  );
  await expect(bars).toHaveCount(3);
  await expect(bars.nth(0)).toContainText("Tool · slow-bash");
  await expect(bars.nth(0)).toContainText("4.00 s");
  await expect(bars.nth(0)).toContainText("Longest recorded");
  await expect(bars.nth(1)).toContainText("Tool · search");
  await expect(bars.nth(2)).toContainText("Tool · read");
  expect(
    await chart
      .locator(".trace-duration-bar > span")
      .evaluateAll((els) => els.map((el) => (el as HTMLElement).style.width)),
  ).toEqual(["100%", "25%", "2%"]);
  await expect(chart).not.toContainText("90.00 s");
  await expect(nodes(page, "Run settlement")).toContainText("90.00 s");
  await expect(chart).toContainText("Parallel steps are not added");
  await page.screenshot({
    path: info.outputPath("duration-comparison-desktop.png"),
  });
  const transcript = page.locator(".transcript");
  const before = await transcript.evaluate((el) => el.scrollTop);
  await bars.first().focus();
  await page.keyboard.press("Enter");
  await expect(nodes(page, "Tool · slow-bash")).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await expect(nodes(page, "Tool · slow-bash")).toBeFocused();
  await expect(details(page)).toContainText("Result of slow-bash");
  expect(await transcript.evaluate((el) => el.scrollTop)).toBe(before);
  expect(host.count(A, "get_messages")).toBe(0);
});

test("duration comparison never invents a zero for unknown timings", async ({
  page,
  host,
}) => {
  host.snapshots[A] = snapshot(A, comparisonHistory([{ name: "untimed" }]));
  await open(page, host);
  await show(page);
  await openRanking(page);
  await expect(comparison(page)).toContainText(
    "Recorded 0/2 steps · 2 unknown",
  );
  await expect(comparison(page)).toContainText(
    "No recorded step durations yet. Unknown is not zero.",
  );
  await expect(comparison(page).locator(".trace-duration-step")).toHaveCount(0);
  await expect(comparison(page)).not.toContainText("Longest recorded");
});

test("duration comparison preserves zero measurements and honestly marks ties", async ({
  page,
  host,
}) => {
  host.snapshots[A] = snapshot(
    A,
    comparisonHistory([
      { name: "tie-one", ms: 100 },
      { name: "tie-two", ms: 100 },
      { name: "zero", ms: 0 },
    ]),
  );
  await open(page, host);
  await show(page);
  await openRanking(page);
  const chart = comparison(page);
  await expect(chart.locator(".trace-slowest-label")).toHaveText([
    "Tied longest",
    "Tied longest",
  ]);
  await expect(chart.locator(".trace-duration-step").last()).toContainText(
    "0 ms",
  );
  await expect(
    chart.locator(".trace-duration-bar > span").last(),
  ).toHaveAttribute("style", "width: 0%;");
});

test("duration comparison does not call all-zero observations a bottleneck", async ({
  page,
  host,
}) => {
  host.snapshots[A] = snapshot(A, comparisonHistory([{ name: "zero", ms: 0 }]));
  await open(page, host);
  await show(page);
  await openRanking(page);
  await expect(comparison(page)).toContainText("Recorded values are all 0 ms");
  await expect(comparison(page).locator(".trace-slowest-label")).toHaveCount(0);
  await expect(comparison(page).locator(".trace-duration-step")).toContainText(
    "0 ms",
  );
});

test("duration comparison reranks running steps only from native timing updates", async ({
  page,
  host,
}) => {
  await start(page, host);
  await host.emit(A, 12, {
    type: "tool_execution_start",
    toolCallId: "growing",
    toolName: "growing",
    webTiming: { elapsedMs: 50, running: true },
  });
  await host.emit(A, 13, {
    type: "tool_execution_start",
    toolCallId: "initial-slow",
    toolName: "initial-slow",
    webTiming: { elapsedMs: 1500, running: true },
  });
  await openRanking(page);
  const chart = comparison(page);
  await expect(chart.locator(".trace-duration-step").first()).toContainText(
    "Tool · initial-slow",
  );
  await expect(chart).toContainText("latest native event, not final durations");
  await host.emit(A, 14, {
    type: "tool_execution_end",
    toolCallId: "growing",
    toolName: "growing",
    result: "Done",
    webTiming: { elapsedMs: 4000, running: false },
  });
  await expect(chart.locator(".trace-duration-step").first()).toContainText(
    "Tool · growing",
  );
  await expect(chart.locator(".trace-duration-step").first()).toContainText(
    "4.00 s",
  );
  await expect(chart.locator(".trace-duration-step")).toHaveCount(3);
  await host.emit(A, 15, {
    type: "retry_scheduled",
    attempt: 1,
    delayMs: 90000,
    error: "A native backoff",
  });
  await expect(chart.locator(".trace-duration-step")).toHaveCount(3);
  await expect(chart).not.toContainText("90.00 s");
});

test("duration comparison shows only five ranked steps and keeps missing coverage", async ({
  page,
  host,
}) => {
  host.snapshots[A] = snapshot(
    A,
    comparisonHistory(
      Array.from({ length: 8 }, (_, i) => ({
        name: `tool-${i}`,
        ms: 100 + i * 100,
      })),
    ),
  );
  await open(page, host);
  await show(page);
  await openRanking(page);
  await expect(comparison(page).locator(".trace-duration-step")).toHaveCount(5);
  await expect(
    comparison(page).locator(".trace-duration-step").first(),
  ).toContainText("Tool · tool-7");
  await expect(comparison(page)).toContainText(
    "Recorded 8/9 steps · 1 unknown",
  );
  await select(page, B);
  await ready(page, host, B);
  await openRanking(page);
  await expect(comparison(page).locator(".trace-duration-step")).toHaveCount(1);
  await expect(comparison(page)).not.toContainText("tool-7");
});

test("duration comparison is immediately readable in Chinese at 390px", async ({
  page,
  host,
}, info) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.addInitScript(() =>
    localStorage.setItem("rpi-web-language", "zh"),
  );
  host.snapshots[A] = snapshot(
    A,
    comparisonHistory([
      { name: "functions.bash", ms: 3250 },
      { name: "functions.read", ms: 120 },
      { name: `functions.${"long-native-tool-".repeat(20)}`, ms: 40 },
      { name: "untimed" },
    ]),
  );
  await open(page, host);
  await show(page, true);
  await openRanking(page);
  const chart = comparison(page, true);
  await expect(
    chart.getByRole("heading", { name: "耗时对比", exact: true }),
  ).toBeVisible();
  await chart.locator(".trace-duration-step").first().scrollIntoViewIfNeeded();
  await expect(chart.locator(".trace-duration-step").first()).toBeInViewport();
  await expect(chart.locator(".trace-duration-step").first()).toContainText(
    "已知最慢",
  );
  await expect(chart.locator(".trace-duration-step").first()).toContainText(
    "3.25 s",
  );
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
  ).toBe(true);
  expect(
    await pane(page).evaluate((el) => el.scrollWidth <= el.clientWidth),
  ).toBe(true);
  await page.screenshot({
    path: info.outputPath("duration-comparison-mobile-zh.png"),
  });
});

test("selected details are independent of row buttons and tabs render only observed plain text", async ({
  page,
  host,
}) => {
  await open(page, host);
  await show(page);
  const tool = nodes(page, "Tool · read");
  await expand(tool);
  await expect(details(page)).toHaveCount(1);
  await expect(
    pane(page).locator(
      "button .trace-node-details, button [data-testid='trace-details']",
    ),
  ).toHaveCount(0);
  await expect(tool.locator("pre, dl, .trace-node-details")).toHaveCount(0);
  await expect(tool).not.toContainText("Fixture file contents");
  await expect(tool).not.toContainText("notes.txt");
  await expect(
    details(page).getByRole("button", { name: "Overview", exact: true }),
  ).toHaveAttribute("aria-pressed", "true");
  await expect(details(page).locator("pre")).toHaveText(
    "Fixture file contents",
  );
  await expect(details(page)).not.toContainText("notes.txt");
  await details(page)
    .getByRole("button", { name: "Input", exact: true })
    .click();
  await expect(details(page).locator("pre")).toHaveText("path: notes.txt");
  await expect(details(page)).not.toContainText("Fixture file contents");
  await details(page)
    .getByRole("button", { name: "Output", exact: true })
    .click();
  await expect(details(page).locator("pre")).toHaveText(
    "Fixture file contents",
  );
  await expect(details(page)).not.toContainText("notes.txt");
  await expand(nodes(page, "User message \\(loaded excerpt\\)"));
  await expect(details(page).locator("pre")).toHaveText(`Loaded question ${A}`);
  await expand(nodes(page, "LLM response"));
  await expect(details(page).locator("pre")).toHaveText(`Loaded answer ${A}`);
  await details(page)
    .getByRole("button", { name: "Input", exact: true })
    .click();
  await expect(details(page)).toContainText(
    "Unknown — not available in the loaded messages or observed events.",
  );
  await expect(details(page).locator("pre")).toHaveCount(0);
  expect(host.count(A, "get_messages")).toBe(0);
});

test("missing output remains explicitly unknown in Output, not an invented empty result", async ({
  page,
  host,
}) => {
  host.snapshots[A] = snapshot(A, [
    assistant(A, { content: [], model: undefined, provider: undefined }),
  ]);
  await open(page, host);
  await show(page);
  const llm = nodes(page, "LLM response");
  await expand(llm);
  for (const label of ["Model", "Provider", "LLM elapsed"])
    await expect(value(llm, label)).toHaveText("Unknown");
  await details(page)
    .getByRole("button", { name: "Output", exact: true })
    .click();
  await expect(
    details(page).getByRole("region", { name: "Output summary", exact: true }),
  ).toContainText(
    "Unknown — not available in the loaded messages or observed events.",
  );
  await expect(details(page).locator("pre")).toHaveCount(0);
  await expect(details(page)).not.toContainText("runtime-current-model");
});

test("tool call IDs create exact LLM children while an orphan stays a root; carets support keyboard", async ({
  page,
  host,
}) => {
  host.snapshots[A] = snapshot(A, [
    ...comparisonHistory([
      { name: "read", ms: 80 },
      { name: "find", ms: 120 },
    ]),
    {
      role: "toolResult",
      entryId: "orphan-result",
      toolCallId: "no-assistant-call",
      toolName: "orphan",
      content: [{ type: "text", text: "Orphan output" }],
      isError: false,
    },
  ]);
  await open(page, host);
  await show(page);
  const llm = nodes(page, "LLM response");
  const branch = llm.locator("xpath=../..");
  const children = branch.locator(
    ":scope > div[id] > .trace-branch > .trace-observation > .trace-node",
  );
  await expect(children.locator(".trace-step-name")).toHaveText([
    "Tool · read",
    "Tool · find",
  ]);
  const roots = pane(page).locator(
    ".trace-group > div[id] > .trace-branch > .trace-observation > .trace-node",
  );
  await expect(roots.locator(".trace-step-name")).toHaveText([
    "LLM response · historical-model",
    "Tool · orphan",
  ]);
  const caret = branch.locator(":scope > .trace-observation .trace-caret");
  await expand(nodes(page, "Tool · read"));
  await caret.focus();
  await page.keyboard.press("Enter");
  await expect(caret).toHaveAttribute("aria-expanded", "false");
  await expect(nodes(page, "Tool · read")).toBeHidden();
  await expect(nodes(page, "Tool · orphan")).toBeVisible();
  await expect(llm).toHaveAttribute("aria-pressed", "true");
  await page.keyboard.press("Space");
  await expect(caret).toHaveAttribute("aria-expanded", "true");
  await expect(nodes(page, "Tool · read")).toBeVisible();
  await expand(nodes(page, "Tool · orphan"));
  await expect(details(page)).toContainText("Orphan output");
});

test("ranking starts closed and revealing a ranked child unfolds both ancestors and focuses its row", async ({
  page,
  host,
}) => {
  await open(page, host);
  await show(page);
  const ranking = pane(page).locator("details.trace-ranking");
  await expect(ranking).not.toHaveAttribute("open", "");
  await expect(ranking.locator(":scope > summary")).toHaveText(
    "Duration ranking",
  );
  await expect(comparison(page)).toBeHidden();
  const llmCaret = nodes(page, "LLM response")
    .locator("..")
    .locator(".trace-caret");
  const groupCaret = pane(page).locator(".trace-group-row .trace-caret");
  await llmCaret.click();
  await groupCaret.click();
  await expect(nodes(page, "Tool · read")).toBeHidden();
  await openRanking(page);
  await openRanking(page); // Idempotent: a second helper call must not close it.
  await comparison(page).locator(".trace-duration-step").first().click();
  await expect(groupCaret).toHaveAttribute("aria-expanded", "true");
  await expect(llmCaret).toHaveAttribute("aria-expanded", "true");
  const tool = nodes(page, "Tool · read");
  await expect(tool).toHaveAttribute("aria-pressed", "true");
  await expect(tool).toBeFocused();
  await expect(tool.locator(".trace-node-details")).toHaveCount(0);
  await expect(details(page)).toContainText("Fixture file contents");
});

test("native bounded run is solid, tool duration is start-unknown, and LLM time stays unknown", async ({
  page,
  host,
}) => {
  host.snapshots[A] = snapshot(A, history(), {
    latestSettlement: { runId: "bounds-run", outcome: "completed" },
  });
  host.timings = [
    {
      runId: "bounds-run",
      finalEntryId: `${A}-assistant`,
      outcome: "completed",
      startedAt: epoch,
      finishedAt: epoch + 1500,
      elapsedMs: 1500,
    },
  ];
  await open(page, host);
  await show(page);
  const group = pane(page)
    .locator(".trace-group")
    .filter({ has: page.locator('.trace-row-name[title="bounds-run"]') });
  const rootBar = group.locator(".trace-group-select .trace-time-bar");
  await expect(rootBar).toHaveAttribute("data-timing-mode", "positioned");
  await expect(rootBar).toHaveCSS("border-top-style", "none");
  await expect(rootBar).toHaveAttribute("style", /width: 100%/);
  const tool = nodes(page, "Tool · read");
  await expect(tool.locator(".trace-time-bar")).toHaveAttribute(
    "data-timing-mode",
    "duration",
  );
  await expect(tool.locator(".trace-time-bar")).toHaveCSS(
    "border-top-style",
    "dashed",
  );
  await expect(tool).toContainText("25 ms · Start unknown");
  const llm = nodes(page, "LLM response");
  await expect(llm.locator(".trace-time-bar")).toHaveAttribute(
    "data-timing-mode",
    "unknown",
  );
  await expect(llm.locator(".trace-time-bar")).toHaveAttribute(
    "style",
    /width: 0%/,
  );
  await expect(llm.locator(".trace-time-label")).toHaveText("Unknown");
  await expand(llm);
  await expect(value(llm, "LLM elapsed")).toHaveText("Unknown");
  await group.locator(".trace-group-select").click();
  await expect(value(llm, "Run elapsed")).toHaveText("1.50 s");
  await expect(value(llm, "Run ID")).toHaveText("bounds-run");
});

test("selected observation survives live updates but navigation invalidates it", async ({
  page,
  host,
}) => {
  await start(page, host);
  const tool = nodes(page, "Tool · read");
  await expand(tool);
  const selectedId = await tool.getAttribute("data-trace-node-id");
  await details(page)
    .getByRole("button", { name: "Input", exact: true })
    .click();
  await host.emit(A, 12, {
    type: "message_end",
    message: assistant("later-live", {
      content: [{ type: "text", text: "New unrelated live output" }],
    }),
  });
  await expect(tool).toHaveAttribute("data-trace-node-id", selectedId!);
  await expect(tool).toHaveAttribute("aria-pressed", "true");
  await expect(
    details(page).getByRole("button", { name: "Input", exact: true }),
  ).toHaveAttribute("aria-pressed", "true");
  await expect(details(page).locator("pre")).toHaveText("path: notes.txt");
  await expect(details(page)).not.toContainText("New unrelated live output");
  await host.emit(A, 13, {
    type: "tool_execution_end",
    toolCallId: `${A}-read`,
    toolName: "read",
    result: {
      content: [{ type: "text", text: "Updated selected tool result" }],
    },
    webTiming: { elapsedMs: 90, running: false },
  });
  await expect(tool).toHaveAttribute("aria-pressed", "true");
  await expect(
    details(page).getByRole("button", { name: "Input", exact: true }),
  ).toHaveAttribute("aria-pressed", "true");
  await expect(details(page).locator("pre")).toHaveText("path: notes.txt");
  await details(page)
    .getByRole("button", { name: "Output", exact: true })
    .click();
  await expect(details(page).locator("pre")).toHaveText(
    "content: Updated selected tool result",
  );
  await select(page, B);
  await ready(page, host, B);
  await expect(
    pane(page).locator('.trace-node[aria-pressed="true"]'),
  ).toHaveCount(0);
  await expect(
    pane(page).locator('.trace-group-select[aria-pressed="true"]'),
  ).toHaveCount(1);
  await expect(details(page).locator(".trace-node-details")).toHaveCount(0);
  await expect(details(page)).not.toContainText("notes.txt");
  await expect(details(page)).not.toContainText("Updated selected tool result");
  await expand(nodes(page, "LLM response"));
  await expect(details(page)).toContainText(`Loaded answer ${B}`);
  await expect(details(page)).not.toContainText("New unrelated live output");
});

test("wide desktop uses two columns, compact stacks, and long later roots stay selectable at 390px", async ({
  page,
  host,
}) => {
  const longModel = `historical-${"long-model-label-".repeat(14)}`;
  host.snapshots[A] = snapshot(A, [
    ...history(),
    ...Array.from({ length: 35 }, (_, i) =>
      assistant(`root-${i}`, {
        model: longModel,
        timestamp: epoch + 300 + i,
        content: [{ type: "text", text: `Later root output ${i}` }],
      }),
    ),
  ]);
  await page.setViewportSize({ width: 1440, height: 1000 });
  await open(page, host);
  await show(page);
  const outline = pane(page).locator(".trace-outline");
  const wide = await pane(page).boundingBox();
  const left = await outline.boundingBox();
  const right = await details(page).boundingBox();
  expect(right!.x).toBeGreaterThan(left!.x + left!.width - 2);
  expect(Math.abs(right!.y - left!.y)).toBeLessThan(2);
  await pane(page)
    .getByRole("button", { name: "Compact trace", exact: true })
    .click();
  await expect(pane(page)).not.toHaveClass(/execution-trace--wide/);
  const compact = await pane(page).boundingBox();
  expect(compact!.width).toBeLessThan(wide!.width);
  const top = await outline.boundingBox();
  const bottom = await details(page).boundingBox();
  expect(bottom!.y).toBeGreaterThanOrEqual(top!.y + top!.height - 1);
  await pane(page)
    .getByRole("button", { name: "Expand trace", exact: true })
    .click();
  await page.setViewportSize({ width: 390, height: 844 });
  const last = pane(page).locator(".trace-node").last();
  await expand(last);
  await expect(last).toBeInViewport();
  await expect(details(page)).toContainText("Later root output 34");
  await expect(value(last, "Model")).toHaveText(longModel);
  await expect(last.locator(".trace-row-name")).toHaveAttribute(
    "title",
    `LLM response · ${longModel}`,
  );
  await expect(
    pane(page).locator('.trace-node[aria-pressed="true"]'),
  ).toHaveCount(1);
  const mobileTop = await outline.boundingBox();
  const mobileBottom = await details(page).boundingBox();
  expect(mobileBottom!.y).toBeGreaterThanOrEqual(
    mobileTop!.y + mobileTop!.height - 1,
  );
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
  ).toBe(true);
  for (const region of [pane(page), outline, details(page)])
    expect(
      await region.evaluate((el) => el.scrollWidth <= el.clientWidth),
    ).toBe(true);
  expect(host.count(A, "get_messages")).toBe(0);
});

// Notice regressions use the same fail-closed snapshot/SSE host. Only this
// test-local route admits one explicit model selection, never a real send.
async function noticeModelControl(page: Page, host: MockHost) {
  const flash = { id: "flash", name: "DeepSeek Flash", provider: "deepseek" };
  const initial = {
    id: "runtime-current-model",
    name: "Current model",
    provider: "runtime-current-provider",
  };
  let selections = 0;
  await page.route("**/api/v1/capabilities", async (route) => {
    expect(route.request().method()).toBe("GET");
    await route.fulfill({
      json: {
        protocol: "rpi",
        supportedCommands: [...reads, "set_model"],
        supportedActions: [
          "projects",
          "session_metadata",
          "history",
          "queue",
          "stats",
          "provider_auth",
        ],
        parityComplete: false,
      },
    });
  });
  await page.route(`**/api/v1/sessions/${A}/commands`, async (route) => {
    const request = route.request();
    expect(request.method()).toBe("POST");
    const body = request.postDataJSON() as Json;
    const kind = String(body.type);
    if (!["set_model", "get_available_models", "get_state"].includes(kind)) {
      // All other reads and every unexpected mutation still hit MockHost's
      // original allowlist/rejection; nothing can escape to a native host.
      await route.fallback();
      return;
    }
    host.calls.push({ id: A, kind, body });
    if (kind === "set_model") {
      expect(body).toMatchObject({ modelId: "flash", provider: "deepseek" });
      selections++;
      expect(selections, "one mocked set_model per UI selection").toBe(1);
    }
    const current = selections ? flash : initial;
    const data =
      kind === "get_available_models"
        ? { models: [initial, flash] }
        : kind === "get_state"
          ? {
              sessionId: A,
              modelId: current.id,
              provider: current.provider,
              thinkingLevel: "high",
              activeOperation: null,
              activeTools: [],
              queue: { steeringCount: 0, followUpCount: 0 },
            }
          : flash;
    await route.fulfill({ json: envelope(kind, data) });
  });
  return {
    async choose(zh = false) {
      await chooseMenuOption(page, zh ? "选择模型" : "Choose model", {
        label: flash.name,
      });
      const notice = page.locator("[data-transcript-row]").filter({
        hasText: zh
          ? "模型 → DeepSeek Flash (deepseek)"
          : "Model → DeepSeek Flash (deepseek)",
      });
      await expect(notice).toHaveCount(1);
      expect(selections).toBe(1);
      const id = await notice.getAttribute("data-transcript-row");
      expect(id).toMatch(/^cmd-/);
      return id!;
    },
    check() {
      expect(selections).toBe(1);
      expect(host.count(A, "set_model")).toBe(1);
      expect(
        host.calls.filter(
          (call) =>
            !reads.includes(call.kind) &&
            !["set_model", "open", "snapshot", "run-timings/query"].includes(
              call.kind,
            ),
        ),
      ).toEqual([]);
    },
  };
}
async function noticeOrder(page: Page, expected: string[]) {
  await expect
    .poll(() =>
      page
        .locator("[data-transcript-row]")
        // Completed settlements and chronology boundaries can retain an empty
        // wrapper for bookkeeping; compare only actually rendered rows.
        .evaluateAll((rows) =>
          rows
            .filter((row) => row.childElementCount > 0)
            .map((row) => row.getAttribute("data-transcript-row")),
        ),
    )
    .toEqual(expected);
}

test("local model notice leads new empty-session messages through three growing boundary snapshots in Chinese", async ({
  page,
  host,
}) => {
  await page.addInitScript(() =>
    localStorage.setItem("rpi-web-language", "zh"),
  );
  host.snapshots[A] = snapshot(A, []);
  const model = await noticeModelControl(page, host);
  await open(page, host);
  const notice = await model.choose(true);
  await noticeOrder(page, [notice]);
  const messages: Json[] = [];
  for (let index = 0; index < 3; index++) {
    messages.push(user(`new-${index}`, `New question ${index}`));
    await host.boundary(
      A,
      11 + index,
      {
        type: index === 1 ? "agent_settled" : "agent_start",
        runId: `notice-run-${index}`,
        outcome: "completed",
      },
      { messages: [...messages] },
    );
    await noticeOrder(page, [
      notice,
      ...messages.map((message) => String(message.entryId)),
    ]);
    await expect(page.locator(`[data-transcript-row="${notice}"]`)).toHaveText(
      "模型 → DeepSeek Flash (deepseek)",
    );
  }
  expect(host.count(A, "snapshot")).toBe(4);
  expect(host.count(A, "get_messages")).toBe(0);
  model.check();
});

test("local model notice stays between existing history and future messages while refresh removes a transient retry", async ({
  page,
  host,
}) => {
  const old = [user("old"), assistant("old")];
  host.snapshots[A] = snapshot(A, old);
  const model = await noticeModelControl(page, host);
  await open(page, host);
  const notice = await model.choose();
  await noticeOrder(page, ["old-user", "old-assistant:b0", notice]);
  await host.emit(A, 11, {
    type: "retry_scheduled",
    attempt: 1,
    maxRetries: 3,
    error: "TRANSIENT_RETRY",
  });
  await expect(
    page.locator('[data-transcript-row="retry-note"]'),
  ).toContainText("TRANSIENT_RETRY");
  await host.boundary(
    A,
    12,
    { type: "agent_settled", runId: "notice-retry", outcome: "completed" },
    {
      messages: [...old, user("future"), assistant("future")],
    },
  );
  await noticeOrder(page, [
    "old-user",
    "old-assistant:b0",
    notice,
    "future-user",
    "future-assistant:b0",
  ]);
  await expect(page.locator('[data-transcript-row="retry-note"]')).toHaveCount(
    0,
  );
  await host.boundary(
    A,
    13,
    { type: "agent_start", runId: "notice-next" },
    {
      messages: [...old, user("future"), assistant("future"), user("later")],
    },
  );
  await noticeOrder(page, [
    "old-user",
    "old-assistant:b0",
    notice,
    "future-user",
    "future-assistant:b0",
    "later-user",
  ]);
  expect(host.count(A, "get_messages")).toBe(0);
  model.check();
});

test("same-session resync of the same snapshot preserves one model notice in its original gap", async ({
  page,
  host,
}) => {
  host.snapshots[A] = snapshot(A, []);
  const model = await noticeModelControl(page, host);
  await open(page, host);
  const notice = await model.choose();
  await host.boundary(
    A,
    11,
    { type: "agent_start", runId: "notice-resync" },
    {
      messages: [user("resync"), assistant("resync")],
    },
  );
  const expected = [notice, "resync-user", "resync-assistant:b0"];
  await noticeOrder(page, expected);
  for (let index = 0; index < 2; index++) {
    const before = host.count(A, "snapshot");
    await host.emit(A, 12 + index, { type: "resync_required" });
    await expect.poll(() => host.count(A, "snapshot")).toBe(before + 1);
    await ready(page, host, A, index + 2);
    await noticeOrder(page, expected);
  }
  expect(host.count(A, "get_messages")).toBe(0);
  model.check();
});

test("overlapping older pagination and boundary refresh retain the model notice before future messages", async ({
  page,
  host,
}) => {
  const windowUsers = Array.from({ length: 9 }, (_, index) =>
    user(`page-window-${index}`),
  );
  const current = [user("page"), ...windowUsers, assistant("page")];
  const currentIds = [
    "page-user",
    ...windowUsers.map((row) => row.entryId as string),
    "page-assistant:b0",
  ];
  host.snapshots[A] = snapshot(A, current, {
    hasMore: true,
    firstId: "page-user",
    total: 13,
  });
  const model = await noticeModelControl(page, host);
  await open(page, host);
  const notice = await model.choose();
  await noticeOrder(page, [...currentIds, notice]);
  host.older[A] = {
    messages: [user("older"), assistant("older"), current[0]],
    total: 13,
    hasMore: false,
    firstId: "older-user",
  };
  await page
    .getByRole("button", { name: "Load earlier messages", exact: true })
    .click();
  await expect.poll(() => host.count(A, "get_messages")).toBe(1);
  await noticeOrder(page, [
    "older-user",
    "older-assistant:b0",
    ...currentIds,
    notice,
  ]);
  expect(
    host.calls.find((call) => call.kind === "get_messages")?.body,
  ).toMatchObject({ beforeId: "page-user", limit: 48 });
  await host.boundary(
    A,
    11,
    { type: "agent_settled", runId: "notice-page", outcome: "completed" },
    {
      messages: [...current, user("after-page")],
    },
  );
  await noticeOrder(page, [
    "older-user",
    "older-assistant:b0",
    ...currentIds,
    notice,
    "after-page-user",
  ]);
  expect(host.count(A, "get_messages")).toBe(1);
  model.check();
});

test("navigation from A to B never leaks A's model notice or a held stale snapshot reply", async ({
  page,
  host,
}) => {
  host.snapshots[A] = snapshot(A, [user("a-original")]);
  const model = await noticeModelControl(page, host);
  await open(page, host);
  const notice = await model.choose();
  await noticeOrder(page, ["a-original-user", notice]);
  const held = host.hold(A, "snapshot");
  host.snapshots[A] = snapshot(A, [user("a-late", "STALE_A_REPLY")], {
    lastSeq: 11,
  });
  const before = host.count(A, "snapshot");
  await host.emit(A, 11, { type: "agent_start", runId: "late-a" });
  await expect.poll(() => host.count(A, "snapshot")).toBe(before + 1);
  await select(page, B);
  await ready(page, host, B);
  const expected = [`${B}-user`, `${B}-assistant:b0`, `call-${B}-read`];
  await noticeOrder(page, expected);
  held.release();
  await rendered(page);
  await noticeOrder(page, expected);
  await expect(
    page
      .locator("[data-transcript-row]")
      .filter({ hasText: "Model → DeepSeek Flash (deepseek)" }),
  ).toHaveCount(0);
  await expect(
    page.locator(".transcript-row").filter({ hasText: "STALE_A_REPLY" }),
  ).toHaveCount(0);
  expect(host.count(B, "set_model")).toBe(0);
  expect(host.count(A, "get_messages") + host.count(B, "get_messages")).toBe(0);
  model.check();
});
