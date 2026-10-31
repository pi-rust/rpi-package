import { test as base, expect, type Page, type Route } from "@playwright/test";

// Independent, fully mocked host, following session-loading/timeline-loading.
// EVERY /api/** request terminates here. Mutation-shaped requests are admitted
// only by a one-shot expectation; no prompt/resume/upload reaches a real host.
const A = "continue-a";
const B = "continue-b";
const projectId = "continue-project";
const titles = { [A]: "Continue Alpha", [B]: "Continue Beta" };
const original = "Original question: inspect the unfinished implementation";
const instruction = "Continue the previous unfinished task.";
const chineseInstruction = "继续上一个未完成的任务。";
type Json = Record<string, unknown>;
type Reply = { json: unknown; status?: number };
type Call = { id: string; kind: string; body: Json; ordinal: number };

function gate<T>() {
  let release!: (value: T) => void;
  const promise = new Promise<T>((resolve) => {
    release = resolve;
  });
  return { promise, release };
}
const envelope = (command: string, data: unknown) => ({
  type: "response",
  command,
  success: true,
  data,
});
const rejected = (command: string, error: string): Reply => ({
  json: { type: "response", command, success: false, error },
});
function snapshot(id: string, extra: Json = {}): Json {
  return {
    sessionId: id,
    status: "ready",
    lastSeq: 10,
    messages: [{ role: "user", entryId: `${id}-question`, content: original }],
    events: [],
    total: 1,
    truncated: false,
    hasMore: false,
    firstId: `${id}-question`,
    lastId: `${id}-question`,
    leafId: `${id}-leaf`,
    pendingResume: false,
    pendingDialogs: [],
    latestSettlement: null,
    ...extra,
  };
}
function failed(id = A, outcome = "failed", pendingResume = false): Json {
  return snapshot(id, {
    pendingResume,
    latestSettlement: {
      runId: `${id}-failed-run`,
      outcome,
      pendingResume,
      error: "Mock previous run failed",
      aborted: false,
    },
  });
}
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

class MockHost {
  warmupAfter: Record<string, number> = {};
  commands = [...reads, "prompt", "resume_pending", "abort"];
  snapshots: Record<string, Json> = { [A]: snapshot(A), [B]: snapshot(B) };
  snapshotFailure: Reply | undefined;
  unexpected: string[] = [];
  calls: Call[] = [];
  uploads = 0;
  private uploadPlanned = false;
  private closing = false;
  private streams = new Map<string, ReturnType<typeof gate<string>>>();
  private heldReads = new Map<string, ReturnType<typeof gate<void>>>();
  private mutations: Array<{
    id: string;
    kind: string;
    reply: Reply;
    arrived: ReturnType<typeof gate<void>>;
    wait: ReturnType<typeof gate<void>>;
    finished: ReturnType<typeof gate<void>>;
    used: boolean;
  }> = [];

  constructor(readonly page: Page) {}
  count(id: string, kind: string) {
    return this.calls.filter((call) => call.id === id && call.kind === kind)
      .length;
  }
  mutationsSent() {
    return this.calls.filter((call) =>
      ["prompt", "resume_pending", "abort"].includes(call.kind),
    );
  }
  // Explicitly plan a MOCK command, optionally keeping its HTTP response gated.
  // Callers inspect the recorded body rather than issuing any host operation.
  expectCommand(id: string, kind: string, reply: Reply, hold = false) {
    const ticket = {
      id,
      kind,
      reply,
      arrived: gate<void>(),
      wait: gate<void>(),
      finished: gate<void>(),
      used: false,
    };
    if (!hold) ticket.wait.release();
    this.mutations.push(ticket);
    return {
      arrived: ticket.arrived.promise,
      release: () => ticket.wait.release(),
      finished: ticket.finished.promise,
    };
  }
  expectUpload() {
    this.uploadPlanned = true;
  }
  holdRead(id: string, kind: string) {
    const ticket = gate<void>();
    this.heldReads.set(`${id}:${kind}`, ticket);
    return { release: () => ticket.release() };
  }
  private stream(id: string, ordinal: number) {
    const key = `${id}:${ordinal}`;
    let stream = this.streams.get(key);
    if (!stream) {
      stream = gate<string>();
      this.streams.set(key, stream);
    }
    return stream;
  }
  // Actual held EventSource HTTP response, as in session-loading.spec.ts.
  // Long retry bounds EOF reconnect noise. Update snapshot BEFORE emitting a
  // settlement: App refreshes the transcript at agent_settled boundaries.
  emit(id: string, seq: number, event: Json, ordinal = 1) {
    this.stream(id, ordinal).release(
      `retry: 60000\n\nid: ${seq}\nevent: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`,
    );
  }
  private summary(id: string) {
    const current = this.snapshots[id];
    return {
      sessionId: id,
      projectId,
      title: titles[id as keyof typeof titles],
      status: current.status,
      createdAt: 1_700_000_000_000,
      updatedAt: id === A ? 1_700_000_000_100 : 1_700_000_000_000,
      lastSeq: current.lastSeq,
      error: null,
      latestSettlement: current.latestSettlement,
      pinned: false,
      archived: false,
    };
  }
  async install() {
    // Preserve an explicit test locale, irrespective of init-script ordering.
    await this.page.addInitScript(() => {
      if (!localStorage.getItem("rpi-web-language"))
        localStorage.setItem("rpi-web-language", "en");
    });
    await this.page.route("**/api/**", async (route: Route) => {
      const request = route.request();
      const path = new URL(request.url()).pathname;
      const method = request.method();
      const reject = async (detail = "") => {
        this.unexpected.push(`${method} ${path} ${detail}`.trim());
        await route.fulfill({
          status: 501,
          json: { error: "Unexpected API in continuation mock" },
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
            supportedCommands: this.commands,
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
                name: "Continue project",
                path: "/mock/continue",
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
      if (
        method === "POST" &&
        path === `/api/v1/projects/${projectId}/upload`
      ) {
        if (!this.uploadPlanned) return reject("unplanned upload");
        this.uploadPlanned = false;
        this.uploads++;
        return route.fulfill({
          json: { name: "notes.txt", path: ".rpi/uploads/notes.txt", size: 5 },
        });
      }
      const match = path.match(
        /^\/api\/v1\/sessions\/([^/]+)\/(open|snapshot|commands|events|run-timings\/query)$/,
      );
      if (!match || ![A, B].includes(match[1])) return reject();
      const [, id, action] = match;
      if (method !== (["snapshot", "events"].includes(action) ? "GET" : "POST"))
        return reject("wrong method");
      const body: Json =
        action === "commands" ? (request.postDataJSON() as Json) : {};
      const kind = action === "commands" ? String(body.type) : action;
      const ordinal = this.count(id, kind) + 1;
      this.calls.push({ id, kind, body, ordinal });
      let mutation: (typeof this.mutations)[number] | undefined;
      if (action === "commands" && !reads.includes(kind)) {
        mutation = this.mutations.find((item) => !item.used);
        if (!mutation || mutation.id !== id || mutation.kind !== kind)
          return reject(`unplanned mutation ${JSON.stringify(body)}`);
        mutation.used = true;
        mutation.arrived.release();
      }
      try {
        const hold = this.heldReads.get(`${id}:${kind}`);
        if (hold) await hold.promise;
        if (mutation) {
          await mutation.wait.promise;
          return await route.fulfill(mutation.reply);
        }
        if (kind === "events") {
          const stream = this.stream(id, ordinal);
          if (this.closing) stream.release("");
          return await route.fulfill({
            contentType: "text/event-stream",
            body: await stream.promise,
          });
        }
        if (kind === "open")
          return await route.fulfill({ json: this.summary(id) });
        if (kind === "snapshot")
          return await route.fulfill(
            this.snapshotFailure ?? { json: this.snapshots[id] },
          );
        if (kind === "run-timings/query")
          return await route.fulfill({
            json: {
              timings: [],
              complete: true,
              cursor: null,
              scannedBytes: 0,
              totalBytes: 0,
            },
          });
        const data: Record<string, unknown> = {
          get_state: {
            sessionId: id,
            modelId: "mock-model",
            provider: "offline",
            thinkingLevel: "high",
            activeOperation: null,
            activeTools: [],
            queue: { steeringCount: 0, followUpCount: 0 },
          },
          get_queue: { items: [], count: 0 },
          get_stats: { messageCount: 1, totalTokens: 0, costTotal: 0 },
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
              { id: "mock-model", name: "Mock model", provider: "offline" },
            ],
          },
          get_available_thinking_levels: { levels: ["high"] },
        };
        // Unexpected history reads cannot silently become a successful mutation.
        if (!(kind in data)) return reject(`unplanned read ${kind}`);
        return await route.fulfill({ json: envelope(kind, data[kind]) });
      } catch (error) {
        // Navigation may abort an explicitly held request before its release.
        if (!this.closing && !request.failure()) throw error;
      } finally {
        mutation?.finished.release();
      }
    });
  }
  async dispose() {
    this.closing = true;
    for (const read of this.heldReads.values()) read.release();
    for (const mutation of this.mutations) mutation.wait.release();
    for (const stream of this.streams.values()) stream.release("");
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
        "all API is mocked; unexpected mutations fail",
      ).toEqual([]);
    }
  },
});
const input = (page: Page) => page.locator(".composer textarea");
const controls = (page: Page) => page.locator(".composer-controls");
const continueButton = (page: Page) =>
  controls(page).getByRole("button", {
    name: /^(Continue|继续)$/,
    exact: true,
  });
const sendButton = (page: Page) =>
  controls(page).getByRole("button", { name: "Send message", exact: true });
const stopButton = (page: Page) =>
  controls(page).getByRole("button", { name: "Abort", exact: true });
const userRows = (page: Page) => page.locator(".message.user .message-body");
async function rendered(page: Page) {
  await page.evaluate(
    () =>
      new Promise<void>((resolve) => {
        requestAnimationFrame(() => requestAnimationFrame(() => resolve()));
      }),
  );
}
async function received(host: MockHost, id: string, kind: string, count = 1) {
  await expect.poll(() => host.count(id, kind)).toBeGreaterThanOrEqual(count);
}
async function open(page: Page, host: MockHost, id = A) {
  const opening = host.count(id, "open") + 1;
  host.warmupAfter[id] = host.count(id, "get_available_thinking_levels") + 1;
  // HMR sockets must not hold initial navigation at the load event.
  await page.goto(`/?session=${id}`, { waitUntil: "domcontentloaded" });
  await received(host, id, "open", opening);
}
async function ready(page: Page, host: MockHost, id = A) {
  await received(
    host,
    id,
    "get_available_thinking_levels",
    host.warmupAfter[id] ?? 1,
  );
  await expect(userRows(page)).toHaveText(original);
  await rendered(page);
}
async function select(page: Page, id: string) {
  // Wait for THIS navigation's metadata, not a previous visit's call count.
  const hydration = page.waitForResponse((response) => {
    const request = response.request();
    return (
      new URL(response.url()).pathname === `/api/v1/sessions/${id}/commands` &&
      request.method() === "POST" &&
      request.postDataJSON()?.type === "get_available_thinking_levels"
    );
  });
  await page
    .getByRole("button", {
      name: new RegExp(`^${titles[id as keyof typeof titles]}`),
    })
    .click();
  await expect(page).toHaveURL(new RegExp(`session=${id}`));
  await hydration;
  await expect(userRows(page)).toHaveText(original);
  await rendered(page);
}
async function noContinue(page: Page) {
  await expect(continueButton(page)).toHaveCount(0);
}
async function noAutomaticMutation(page: Page, host: MockHost) {
  await rendered(page);
  expect(host.mutationsSent()).toEqual([]);
}
function commandBodies(host: MockHost, kind: string) {
  return host.calls
    .filter((call) => call.kind === kind)
    .map((call) => call.body);
}
// Exercise React's real ingress handlers in one browser task, before a network
// reply can settle. No app internals, replacement EventSource or backend calls.
async function submitBurst(page: Page) {
  await input(page).evaluate((textarea) => {
    const form = textarea.closest("form")!;
    // Click while this is still Continue/Send; never click the newly rendered
    // Stop button after a discrete event has flushed React's busy state.
    form.querySelector<HTMLButtonElement>(".composer-controls button")!.click();
    form.dispatchEvent(
      new Event("submit", { bubbles: true, cancelable: true }),
    );
    textarea.dispatchEvent(
      new KeyboardEvent("keydown", {
        key: "Enter",
        bubbles: true,
        cancelable: true,
      }),
    );
    textarea.dispatchEvent(
      new KeyboardEvent("keydown", {
        key: "Enter",
        bubbles: true,
        cancelable: true,
      }),
    );
  });
}

test("pending native work exposes a visible desktop composer Continue, independently of banner dismissal", async ({
  page,
  host,
}) => {
  host.snapshots[A] = snapshot(A, { pendingResume: true });
  await open(page, host);
  await ready(page, host);
  await expect(continueButton(page)).toBeEnabled();
  await expect(continueButton(page)).toBeVisible();
  await expect(continueButton(page)).toHaveText("");
  await expect(continueButton(page)).toHaveAttribute("title", "Continue");
  await expect(continueButton(page).locator("svg")).toHaveCount(1);
  const bounds = await continueButton(page).boundingBox();
  expect(bounds!.width).toBeCloseTo(32, 0);
  expect(bounds!.height).toBeCloseTo(32, 0);
  await expect(
    page
      .locator(".notice-banner")
      .getByRole("button", { name: "Continue", exact: true }),
  ).toHaveCount(0);
  await noAutomaticMutation(page, host);
  await page
    .locator(".notice-banner")
    .getByRole("button", { name: "Dismiss", exact: true })
    .click();
  await expect(page.locator(".notice-banner")).toHaveCount(0);
  await expect(continueButton(page)).toBeEnabled();
  host.expectCommand(A, "resume_pending", {
    json: envelope("resume_pending", { resumed: true }),
  });
  await page.screenshot({
    path: test.info().outputPath("continue-desktop.png"),
  });
  await continueButton(page).click();
  await received(host, A, "resume_pending");
  expect(commandBodies(host, "resume_pending")).toEqual([
    { type: "resume_pending" },
  ]);
  expect(commandBodies(host, "prompt")).toEqual([]);
  await expect(stopButton(page)).toBeVisible();
});

test("failed, error and interrupted restored settlements offer an explicit continuation, never an automatic resend", async ({
  page,
  host,
}) => {
  for (const outcome of ["failed", "error", "interrupted"]) {
    host.snapshots[A] = failed(A, outcome);
    await open(page, host);
    await ready(page, host);
    await expect(continueButton(page)).toBeEnabled();
    await expect(continueButton(page)).toHaveText("");
    await expect(continueButton(page)).toHaveAttribute(
      "aria-label",
      "Continue",
    );
    await expect(input(page)).toHaveValue("");
    await expect(userRows(page)).toHaveText(original);
    await noAutomaticMutation(page, host);
  }
});

test("a gated live failure changes Stop to Continue after the authoritative snapshot refresh without sending anything", async ({
  page,
  host,
}) => {
  host.snapshots[A] = snapshot(A, { status: "running" });
  await open(page, host);
  await ready(page, host);
  await expect(stopButton(page)).toBeEnabled();
  await noContinue(page);
  await received(host, A, "events");
  host.snapshots[A] = { ...failed(A), lastSeq: 11 };
  host.emit(A, 11, {
    type: "agent_settled",
    runId: `${A}-failed-run`,
    outcome: "failed",
    pendingResume: false,
    error: "Live mocked failure",
    summary: "Live failure observed",
  });
  await received(host, A, "snapshot", 2);
  await expect(
    page.getByText("Live failure observed", { exact: true }),
  ).toBeVisible();
  await expect(continueButton(page)).toBeEnabled();
  await expect(stopButton(page)).toHaveCount(0);
  await expect(input(page)).toHaveValue("");
  await noAutomaticMutation(page, host);
});

test("live successful settlement clears an older failure instead of exposing Continue", async ({
  page,
  host,
}) => {
  host.snapshots[A] = { ...failed(A), status: "running" };
  await open(page, host);
  await ready(page, host);
  await expect(stopButton(page)).toBeEnabled();
  await received(host, A, "events");
  host.snapshots[A] = snapshot(A, {
    lastSeq: 11,
    latestSettlement: { runId: "success-run", outcome: "completed" },
  });
  host.emit(A, 11, {
    type: "agent_settled",
    runId: "success-run",
    outcome: "completed",
    pendingResume: false,
    summary: "Live success observed",
  });
  await received(host, A, "snapshot", 2);
  await expect(
    page.getByText("Live success observed", { exact: true }),
  ).toBeVisible();
  await expect(stopButton(page)).toHaveCount(0);
  await expect(sendButton(page)).toBeDisabled();
  await noContinue(page);
  await noAutomaticMutation(page, host);
});

test("capabilities distinguish unsupported pending work from a failed run with prompt fallback", async ({
  page,
  host,
}) => {
  host.commands = [...reads, "prompt", "abort"];
  host.snapshots[A] = snapshot(A, { pendingResume: true });
  host.snapshots[B] = failed(B, "failed", true);
  await open(page, host);
  await ready(page, host);
  await noContinue(page);
  await expect(sendButton(page)).toBeDisabled();
  await noAutomaticMutation(page, host);
  await select(page, B);
  await expect(continueButton(page)).toBeEnabled();
  host.expectCommand(B, "prompt", { json: envelope("prompt", {}) });
  await continueButton(page).click();
  await received(host, B, "prompt");
  expect(commandBodies(host, "prompt")).toEqual([
    { type: "prompt", message: instruction },
  ]);
  expect(commandBodies(host, "resume_pending")).toEqual([]);
  // Nor may a failed settlement advertise an unsupported prompt fallback.
  host.commands = [...reads, "abort"];
  host.snapshots[A] = failed(A);
  await open(page, host);
  await ready(page, host);
  await noContinue(page);
});

for (const outcome of ["completed", "declined", null]) {
  test(`${outcome ?? "ordinary idle"} history never implies unfinished work`, async ({
    page,
    host,
  }) => {
    host.snapshots[A] = snapshot(A, {
      latestSettlement: outcome
        ? { outcome, error: "Stale non-failure text" }
        : null,
    });
    await open(page, host);
    await ready(page, host);
    await noContinue(page);
    await expect(sendButton(page)).toBeDisabled();
    await input(page).press("Enter");
    await noAutomaticMutation(page, host);
  });
}

for (const settlement of [
  { outcome: "aborted", aborted: true },
  { outcome: "stopped" },
  { outcome: "settled", aborted: true },
]) {
  test(`manually stopped snapshot ${JSON.stringify(settlement)} offers icon-only continuation without resending the original`, async ({
    page,
    host,
  }) => {
    host.snapshots[A] = snapshot(A, { latestSettlement: settlement });
    await open(page, host);
    await ready(page, host);
    await expect(continueButton(page)).toBeEnabled();
    await expect(continueButton(page)).toHaveText("");
    await noAutomaticMutation(page, host);
    host.expectCommand(A, "prompt", { json: envelope("prompt", {}) });
    await continueButton(page).click();
    await received(host, A, "prompt");
    expect(commandBodies(host, "prompt")).toEqual([
      { type: "prompt", message: instruction },
    ]);
    expect(commandBodies(host, "resume_pending")).toEqual([]);
  });
}

test("manual Stop waits for native settlement, then empty Enter continues while a draft still sends normally", async ({
  page,
  host,
}) => {
  host.snapshots[A] = snapshot(A, { status: "running" });
  await open(page, host);
  await ready(page, host);
  host.expectCommand(A, "abort", { json: envelope("abort", {}) });
  await stopButton(page).click();
  await received(host, A, "abort");
  await expect(stopButton(page)).toBeEnabled();
  await noContinue(page);
  expect(commandBodies(host, "prompt")).toEqual([]);
  await received(host, A, "events");
  host.snapshots[A] = snapshot(A, {
    lastSeq: 11,
    latestSettlement: { outcome: "aborted", aborted: true },
  });
  host.emit(A, 11, {
    type: "agent_settled",
    runId: "manual-stop",
    outcome: "aborted",
    aborted: true,
    pendingResume: false,
    summary: "Stopped by user",
  });
  await received(host, A, "snapshot", 2);
  await expect(continueButton(page)).toBeEnabled();
  await expect(stopButton(page)).toHaveCount(0);
  expect(commandBodies(host, "prompt")).toEqual([]);
  await input(page).fill("A different task");
  await noContinue(page);
  await expect(sendButton(page)).toBeEnabled();
  await input(page).fill("");
  await expect(continueButton(page)).toBeEnabled();
  const ticket = host.expectCommand(
    A,
    "prompt",
    rejected("prompt", "Mock stopped continuation denied"),
    true,
  );
  await input(page).press("Enter");
  await ticket.arrived;
  ticket.release();
  await ticket.finished;
  await expect(page.locator(".error-banner")).toContainText(
    "Mock stopped continuation denied",
  );
  await expect(continueButton(page)).toBeEnabled();
  await expect(input(page)).toHaveValue("");
  await expect(userRows(page)).toHaveText(original);
  expect(commandBodies(host, "prompt")).toEqual([
    { type: "prompt", message: instruction },
  ]);
});

test("rejected Stop does not invent a settled continuation action", async ({
  page,
  host,
}) => {
  host.snapshots[A] = snapshot(A, { status: "running" });
  await open(page, host);
  await ready(page, host);
  host.expectCommand(A, "abort", rejected("abort", "Mock abort denied"));
  await stopButton(page).click();
  await expect(page.locator(".error-banner")).toContainText(
    "Mock abort denied",
  );
  await expect(stopButton(page)).toBeEnabled();
  await noContinue(page);
  expect(commandBodies(host, "prompt")).toEqual([]);
  expect(commandBodies(host, "resume_pending")).toEqual([]);
});

test("an ordinary failed fetch does not turn a completed run into continuation work", async ({
  page,
  host,
}) => {
  host.snapshots[A] = snapshot(A, { status: "running" });
  await open(page, host);
  await ready(page, host);
  await received(host, A, "events");
  host.snapshotFailure = {
    status: 503,
    json: { error: "Mock transcript fetch unavailable" },
  };
  host.emit(A, 11, {
    type: "agent_settled",
    outcome: "completed",
    pendingResume: false,
    summary: "Completed before fetch error",
  });
  await expect(page.locator(".error-banner")).toContainText(
    "Mock transcript fetch unavailable",
  );
  await noContinue(page);
  await expect(stopButton(page)).toHaveCount(0);
  await noAutomaticMutation(page, host);
});

test("a draft or attachment changes Continue to Send without substituting the continuation instruction", async ({
  page,
  host,
}) => {
  host.snapshots[A] = failed(A);
  await open(page, host);
  await ready(page, host);
  await input(page).fill("A new local draft");
  await noContinue(page);
  await expect(sendButton(page)).toBeEnabled();
  await input(page).fill("");
  await expect(continueButton(page)).toBeEnabled();
  host.expectUpload();
  await page.locator('input[type="file"]').setInputFiles({
    name: "notes.txt",
    mimeType: "text/plain",
    buffer: Buffer.from("hello"),
  });
  await expect(page.locator(".composer-attachment")).toContainText("notes.txt");
  expect(host.uploads).toBe(1);
  await noContinue(page);
  await expect(sendButton(page)).toBeEnabled();
  host.expectCommand(A, "prompt", { json: envelope("prompt", {}) });
  await sendButton(page).click();
  await received(host, A, "prompt");
  const body = commandBodies(host, "prompt")[0];
  expect(body.message).toContain(".rpi/uploads/notes.txt");
  expect(body.message).not.toContain(instruction);
  expect(body.message).not.toContain(original);
  expect(commandBodies(host, "resume_pending")).toEqual([]);
});

test("empty Enter continues, whereas Shift+Enter and IME Enter cannot submit", async ({
  page,
  host,
}) => {
  host.snapshots[A] = failed(A);
  await open(page, host);
  await ready(page, host);
  await input(page).press("Shift+Enter");
  await expect(input(page)).toHaveValue("\n");
  await input(page).fill("");
  await input(page).dispatchEvent("keydown", {
    key: "Enter",
    code: "Enter",
    isComposing: true,
  });
  await input(page).dispatchEvent("keydown", {
    key: "Enter",
    code: "Enter",
    keyCode: 229,
  });
  await noAutomaticMutation(page, host);
  host.expectCommand(A, "prompt", { json: envelope("prompt", {}) });
  await input(page).press("Enter");
  await received(host, A, "prompt");
  expect(commandBodies(host, "prompt")).toEqual([
    { type: "prompt", message: instruction },
  ]);
});

test("native resume is synchronously deduplicated across Enter, form, button and slash; rejection restores Continue", async ({
  page,
  host,
}) => {
  host.snapshots[A] = failed(A, "interrupted", true);
  await open(page, host);
  await ready(page, host);
  for (const slash of [false, true]) {
    if (slash) await input(page).fill("/continue");
    const count = host.count(A, "resume_pending");
    const ticket = host.expectCommand(
      A,
      "resume_pending",
      rejected("resume_pending", "Mock resume denied"),
      true,
    );
    await submitBurst(page);
    await ticket.arrived;
    await rendered(page);
    expect(host.count(A, "resume_pending")).toBe(count + 1);
    await expect(stopButton(page)).toBeVisible();
    ticket.release();
    await ticket.finished;
    await expect(page.locator(".error-banner")).toContainText(
      "Mock resume denied",
    );
    await expect(continueButton(page)).toBeEnabled();
    await expect(input(page)).toHaveValue("");
    await expect(stopButton(page)).toHaveCount(0);
  }
  expect(commandBodies(host, "resume_pending")).toEqual([
    { type: "resume_pending" },
    { type: "resume_pending" },
  ]);
  expect(commandBodies(host, "prompt")).toEqual([]);
  await expect(userRows(page)).toHaveText(original);
});

test("resumed:false returns to idle without a phantom Stop or optimistic prompt", async ({
  page,
  host,
}) => {
  host.snapshots[A] = snapshot(A, { pendingResume: true });
  await open(page, host);
  await ready(page, host);
  const ticket = host.expectCommand(
    A,
    "resume_pending",
    { json: envelope("resume_pending", { resumed: false }) },
    true,
  );
  await continueButton(page).click();
  await ticket.arrived;
  await expect(stopButton(page)).toBeVisible();
  ticket.release();
  await ticket.finished;
  await expect(stopButton(page)).toHaveCount(0);
  await noContinue(page);
  await expect(sendButton(page)).toBeDisabled();
  await expect(input(page)).toHaveValue("");
  await expect(userRows(page)).toHaveText(original);
  expect(commandBodies(host, "prompt")).toEqual([]);
});

test("rejected failure continuation keeps Continue with neither draft injection nor an optimistic user row", async ({
  page,
  host,
}) => {
  host.snapshots[A] = failed(A);
  await open(page, host);
  await ready(page, host);
  const ticket = host.expectCommand(
    A,
    "prompt",
    rejected("prompt", "Mock continue prompt denied"),
    true,
  );
  await submitBurst(page);
  await ticket.arrived;
  await rendered(page);
  expect(commandBodies(host, "prompt")).toEqual([
    { type: "prompt", message: instruction },
  ]);
  ticket.release();
  await ticket.finished;
  await expect(page.locator(".error-banner")).toContainText(
    "Mock continue prompt denied",
  );
  await expect(continueButton(page)).toBeEnabled();
  await expect(input(page)).toHaveValue("");
  await expect(userRows(page)).toHaveText(original);
  await expect(stopButton(page)).toHaveCount(0);
});

test("late rejected native resume cannot pollute another session or resurrect stale state after A to B to A", async ({
  page,
  host,
}) => {
  host.snapshots[A] = snapshot(A, { pendingResume: true });
  await open(page, host);
  await ready(page, host);
  const ticket = host.expectCommand(
    A,
    "resume_pending",
    rejected("resume_pending", "STALE native resume rejection"),
    true,
  );
  await continueButton(page).click();
  await ticket.arrived;
  await select(page, B);
  await received(host, B, "get_state");
  await input(page).fill("Beta draft stays local");
  host.snapshots[A] = snapshot(A, {
    latestSettlement: { outcome: "completed" },
  });
  await select(page, A);
  await received(host, A, "snapshot", 2);
  ticket.release();
  await ticket.finished;
  await rendered(page);
  await noContinue(page);
  await expect(stopButton(page)).toHaveCount(0);
  await expect(input(page)).toHaveValue("");
  await expect(
    page
      .locator(".error-banner")
      .filter({ hasText: "STALE native resume rejection" }),
  ).toHaveCount(0);
  await select(page, B);
  await expect(input(page)).toHaveValue("Beta draft stays local");
  await expect(sendButton(page)).toBeEnabled();
  expect(host.mutationsSent().map((call) => call.id)).toEqual([A]);
});

test("late rejected fallback prompt cannot inject its instruction or failure into a newly opened session", async ({
  page,
  host,
}) => {
  host.snapshots[A] = failed(A);
  await open(page, host);
  await ready(page, host);
  const ticket = host.expectCommand(
    A,
    "prompt",
    rejected("prompt", "STALE continuation rejection"),
    true,
  );
  await continueButton(page).click();
  await ticket.arrived;
  await select(page, B);
  await received(host, B, "get_state");
  ticket.release();
  await ticket.finished;
  await rendered(page);
  await expect(input(page)).toHaveValue("");
  await expect(userRows(page)).toHaveText(original);
  await noContinue(page);
  await expect(stopButton(page)).toHaveCount(0);
  await expect(
    page
      .locator(".error-banner")
      .filter({ hasText: "STALE continuation rejection" }),
  ).toHaveCount(0);
  await select(page, A);
  await received(host, A, "snapshot", 2);
  await expect(continueButton(page)).toBeEnabled();
  await expect(input(page)).toHaveValue("");
  await expect(userRows(page)).toHaveText(original);
  expect(commandBodies(host, "prompt")).toEqual([
    { type: "prompt", message: instruction },
  ]);
});

for (const status of ["running", "awaiting_input", "starting", "closed"]) {
  test(`${status} host cannot expose Continue even with recovery metadata`, async ({
    page,
    host,
  }) => {
    const pendingDialogs =
      status === "awaiting_input"
        ? [
            {
              type: "ui_dialog_request",
              requestId: "continue-dialog",
              toolCallId: "ask-continue",
              ui: {
                kind: "selector",
                question: "Choose before continuing",
                options: [{ title: "Yes" }],
              },
            },
          ]
        : [];
    host.snapshots[A] = {
      ...failed(A, "failed", true),
      status,
      pendingDialogs,
    };
    await open(page, host);
    await ready(page, host);
    await noContinue(page);
    if (status !== "closed") await expect(stopButton(page)).toBeVisible();
    // Slash and form ingress must obey the same lifecycle admission guard as
    // the visible button, including when a dialog overlays the composer.
    await input(page).fill("/continue");
    await page.locator("form.composer-area").dispatchEvent("submit");
    await noAutomaticMutation(page, host);
  });
}

test("initialization and failed connection cannot use stale failure metadata to admit Continue", async ({
  page,
  host,
}) => {
  host.snapshots[A] = failed(A, "failed", true);
  const held = host.holdRead(A, "snapshot");
  await open(page, host);
  await received(host, A, "snapshot");
  await noContinue(page);
  await input(page).press("Enter");
  await noAutomaticMutation(page, host);
  host.snapshotFailure = {
    status: 503,
    json: { error: "Mock initial connection failed" },
  };
  held.release();
  await expect(page.locator(".error-banner")).toContainText(
    "Mock initial connection failed",
  );
  await noContinue(page);
  await expect(sendButton(page)).toBeDisabled();
  await input(page).press("Enter");
  await noAutomaticMutation(page, host);
});

test("390px Chinese composer shows an icon-only continuation with a localized name and sends only the explicit instruction", async ({
  page,
  host,
}) => {
  await page.setViewportSize({ width: 390, height: 844 });
  // Default initialization preserves this explicit locale in either script
  // order; avoid interacting with unrelated settings UI.
  await page.addInitScript(() =>
    localStorage.setItem("rpi-web-language", "zh"),
  );
  host.snapshots[A] = failed(A);
  await open(page, host);
  await ready(page, host);
  const button = continueButton(page);
  await expect(button).toBeEnabled();
  await expect(button).toHaveText("");
  await expect(button).toHaveAttribute("aria-label", "继续");
  await expect(button).toHaveAttribute("title", "继续");
  await expect(button.locator("svg")).toHaveCount(1);
  const bounds = await button.boundingBox();
  expect(bounds).not.toBeNull();
  expect(bounds!.width).toBeCloseTo(32, 0);
  expect(bounds!.height).toBeCloseTo(32, 0);
  expect(bounds!.x).toBeGreaterThanOrEqual(0);
  expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(390);
  expect(bounds!.y).toBeGreaterThanOrEqual(0);
  expect(bounds!.y + bounds!.height).toBeLessThanOrEqual(844);
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
  ).toBe(true);
  await noAutomaticMutation(page, host);
  host.expectCommand(A, "prompt", { json: envelope("prompt", {}) });
  await page.screenshot({
    path: test.info().outputPath("continue-mobile-zh.png"),
  });
  await button.click();
  await received(host, A, "prompt");
  expect(commandBodies(host, "prompt")).toEqual([
    { type: "prompt", message: chineseInstruction },
  ]);
  expect(commandBodies(host, "resume_pending")).toEqual([]);
});

for (const outcome of ["completed", "failed"] as const) {
  test(`captured ${outcome} settlement overrides opposite snapshot continuation metadata in replay order`, async ({
    page,
    host,
  }) => {
    const pendingResume = outcome === "failed";
    const summary = `Captured ${outcome} settlement wins`;
    const message = {
      role: "assistant",
      timestamp: 1_700_000_000_200,
      content: [{ type: "text", text: `Captured ${outcome} response` }],
    };
    // Snapshot messages contain ONLY the original user question: the assistant
    // segment is not committed, so eventsAfterSnapshot must retain this replay.
    // All four events are beyond the fixture's initial lastSeq=10 boundary.
    host.snapshots[A] = snapshot(A, {
      pendingResume: !pendingResume,
      latestSettlement: {
        runId: "older-snapshot-run",
        outcome: pendingResume ? "completed" : "failed",
        pendingResume: !pendingResume,
        aborted: false,
      },
      lastSeq: 14,
      events: [
        {
          seq: 11,
          event: { type: "operation_accepted", runId: "captured-run" },
        },
        { seq: 12, event: { type: "message_start", message } },
        { seq: 13, event: { type: "message_end", message } },
        {
          seq: 14,
          event: {
            type: "agent_settled",
            runId: "captured-run",
            outcome,
            pendingResume,
            aborted: false,
            summary,
          },
        },
      ],
    });
    await open(page, host);
    await ready(page, host);
    await expect(
      page.getByText(`Captured ${outcome} response`, { exact: true }),
    ).toBeVisible();
    await expect(page.getByText(summary, { exact: true })).toBeVisible();
    await expect(stopButton(page)).toHaveCount(0);
    if (pendingResume) await expect(continueButton(page)).toBeEnabled();
    else {
      await noContinue(page);
      await expect(sendButton(page)).toBeDisabled();
    }
    await expect(input(page)).toHaveValue("");
    await noAutomaticMutation(page, host);
  });
}

test("a held native resume rejection cannot restore Continue after a newer completed settlement", async ({
  page,
  host,
}) => {
  host.snapshots[A] = failed(A, "interrupted", true);
  await open(page, host);
  await ready(page, host);
  const ticket = host.expectCommand(
    A,
    "resume_pending",
    rejected("resume_pending", "Older resume denied after completion"),
    true,
  );
  await continueButton(page).click();
  await ticket.arrived;
  await received(host, A, "events");
  // The settlement refresh must read the same completed, non-pending source as
  // the live event; otherwise this would test stale mock data, not reply order.
  host.snapshots[A] = snapshot(A, {
    lastSeq: 11,
    latestSettlement: {
      runId: "newer-completed-run",
      outcome: "completed",
      pendingResume: false,
    },
  });
  host.emit(A, 11, {
    type: "agent_settled",
    runId: "newer-completed-run",
    outcome: "completed",
    pendingResume: false,
    summary: "New completion before held resume rejection",
  });
  await received(host, A, "snapshot", 2);
  await expect(
    page.getByText("New completion before held resume rejection", {
      exact: true,
    }),
  ).toBeVisible();
  await expect(stopButton(page)).toHaveCount(0);
  await noContinue(page);
  ticket.release();
  await ticket.finished;
  await expect(page.locator(".error-banner")).toContainText(
    "Older resume denied after completion",
  );
  await expect(stopButton(page)).toHaveCount(0);
  await noContinue(page);
  await expect(sendButton(page)).toBeDisabled();
  await expect(input(page)).toHaveValue("");
  await expect(userRows(page)).toHaveText(original);
  expect(commandBodies(host, "resume_pending")).toEqual([
    { type: "resume_pending" },
  ]);
  expect(commandBodies(host, "prompt")).toEqual([]);
});

test("an old A to B to A resume finalizer cannot unlock a newer pending resume submission", async ({
  page,
  host,
}) => {
  host.snapshots[A] = snapshot(A, { pendingResume: true });
  await open(page, host);
  await ready(page, host);
  const old = host.expectCommand(
    A,
    "resume_pending",
    rejected("resume_pending", "STALE old resume finalizer"),
    true,
  );
  await continueButton(page).click();
  await old.arrived;
  await select(page, B);
  // Re-enter A from authoritative native pending metadata, not cached optimism.
  host.snapshots[A] = snapshot(A, { pendingResume: true });
  await select(page, A);
  await expect(continueButton(page)).toBeEnabled();
  const newer = host.expectCommand(
    A,
    "resume_pending",
    rejected("resume_pending", "Current resume denied"),
    true,
  );
  await continueButton(page).click();
  await newer.arrived;
  await received(host, A, "events", 2);
  // Settle the optimistic busy state while the newer HTTP command is held.
  // Continue now depends on sending's lock alone, exposing an old finally that
  // incorrectly clears sending even if the lifecycle already returned to idle.
  host.snapshots[A] = { ...failed(A, "interrupted", true), lastSeq: 11 };
  host.emit(
    A,
    11,
    {
      type: "agent_settled",
      runId: "newer-pending-run",
      outcome: "interrupted",
      pendingResume: true,
      summary: "New pending marker while resume reply is held",
    },
    2,
  );
  await received(host, A, "snapshot", 3);
  await expect(
    page.getByText("New pending marker while resume reply is held", {
      exact: true,
    }),
  ).toBeVisible();
  await expect(stopButton(page)).toHaveCount(0);
  await expect(continueButton(page)).toBeDisabled();
  const oldResponse = page.waitForResponse(
    (response) =>
      new URL(response.url()).pathname === `/api/v1/sessions/${A}/commands` &&
      response.request().postDataJSON()?.type === "resume_pending",
  );
  old.release();
  await old.finished;
  await (await oldResponse).finished();
  await rendered(page);
  await page.locator("form.composer-area").dispatchEvent("submit");
  await input(page).press("Enter");
  await rendered(page);
  expect(host.count(A, "resume_pending")).toBe(2);
  expect(commandBodies(host, "prompt")).toEqual([]);
  await expect(continueButton(page)).toBeDisabled();
  await expect(
    page
      .locator(".error-banner")
      .filter({ hasText: "STALE old resume finalizer" }),
  ).toHaveCount(0);
  newer.release();
  await newer.finished;
  await expect(page.locator(".error-banner")).toContainText(
    "Current resume denied",
  );
  await expect(continueButton(page)).toBeEnabled();
  await expect(stopButton(page)).toHaveCount(0);
  await expect(input(page)).toHaveValue("");
  await expect(userRows(page)).toHaveText(original);
  expect(commandBodies(host, "resume_pending")).toEqual([
    { type: "resume_pending" },
    { type: "resume_pending" },
  ]);
});

test("a rejected fallback continuation cannot clear a newer authoritative running state", async ({
  page,
  host,
}) => {
  host.snapshots[A] = failed(A);
  await open(page, host);
  await ready(page, host);
  const ticket = host.expectCommand(
    A,
    "prompt",
    rejected("prompt", "Older continuation denied"),
    true,
  );
  await continueButton(page).click();
  await ticket.arrived;
  await received(host, A, "events");
  host.snapshots[A] = snapshot(A, { status: "running", lastSeq: 11 });
  host.emit(A, 11, { type: "agent_start", runId: "newer-running-operation" });
  await received(host, A, "snapshot", 2);
  await expect(stopButton(page)).toBeEnabled();
  ticket.release();
  await ticket.finished;
  await expect(page.locator(".error-banner")).toContainText(
    "Older continuation denied",
  );
  await expect(stopButton(page)).toBeEnabled();
  await noContinue(page);
  await expect(input(page)).toHaveValue("");
  expect(commandBodies(host, "prompt")).toEqual([
    { type: "prompt", message: instruction },
  ]);
});
