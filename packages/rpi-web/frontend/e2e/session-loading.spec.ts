import {
  test as base,
  expect,
  type Page,
  type Request,
  type Route,
} from "@playwright/test";

// All API traffic, including command POSTs, terminates here. No prompt, model,
// queue mutation, filesystem write, or real host is involved in these tests.
const A = "loading-a";
const B = "loading-b";
const titles = { [A]: "Loading Alpha", [B]: "Loading Beta" };
const diagnostics = [
  "get_queue",
  "get_stats",
  "get_history",
  "get_auth_status",
];
const catalogs = ["get_available_models", "get_available_thinking_levels"];
type Json = Record<string, unknown>;
type Reply = { json: unknown; status?: number };
type Call = {
  id: string;
  kind: string;
  ordinal: number;
  url: URL;
  request: Request;
};

function latch() {
  let release!: () => void;
  const promise = new Promise<void>((resolve) => {
    release = resolve;
  });
  return { promise, release };
}

function deferred<T>() {
  let release!: (value: T) => void;
  const promise = new Promise<T>((resolve) => {
    release = resolve;
  });
  return { promise, release };
}

function snapshot(id: string, extra: Json = {}): Json {
  return {
    sessionId: id,
    status: "ready",
    lastSeq: 10,
    messages: [
      { role: "user", entryId: `${id}-question`, content: `History ${id}` },
    ],
    events: [],
    total: 1,
    truncated: false,
    hasMore: false,
    firstId: `${id}-question`,
    lastId: `${id}-question`,
    leafId: `${id}-leaf`,
    pendingResume: false,
    pendingDialogs: [],
    ...extra,
  };
}

function state(id: string, extra: Json = {}): Json {
  return {
    sessionId: id,
    modelId: `${id}-model`,
    provider: "offline",
    thinkingLevel: "high",
    activeOperation: null,
    activeTools: [],
    queue: { steeringCount: 0, followUpCount: 0 },
    ...extra,
  };
}

function frame(seq: number, event: Json) {
  return `id: ${seq}\nevent: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`;
}

const question = {
  type: "ui_dialog_request",
  requestId: "live-question",
  toolCallId: "ask-live",
  ui: {
    kind: "selector",
    question: "Keep the live question?",
    options: [{ title: "Yes" }],
  },
};

class MockHost {
  calls: Call[] = [];
  unexpected: string[] = [];
  snapshots: Record<string, Json> = { [A]: snapshot(A), [B]: snapshot(B) };
  snapshotFailure: Reply | undefined;
  states: Record<string, Json> = { [A]: state(A), [B]: state(B) };
  commands = [
    "get_state",
    "get_messages",
    ...diagnostics,
    ...catalogs,
    "set_model",
    "set_thinking_level",
    "prompt",
    "abort",
  ];
  private holds = new Map<
    string,
    {
      arrived: ReturnType<typeof latch>;
      release: () => void;
      wait: Promise<void>;
      finished: ReturnType<typeof latch>;
      reply?: Reply;
    }
  >();
  private streams = new Map<string, ReturnType<typeof deferred<string>>>();
  private closing = false;

  constructor(readonly page: Page) {}

  count(id: string, kind: string) {
    return this.calls.filter((call) => call.id === id && call.kind === kind)
      .length;
  }

  kinds(id: string) {
    return this.calls.filter((call) => call.id === id).map((call) => call.kind);
  }

  hold(id: string, kind: string, ordinal = 1, reply?: Reply) {
    const gate = latch();
    const ticket = {
      arrived: latch(),
      release: gate.release,
      wait: gate.promise,
      finished: latch(),
      reply,
    };
    this.holds.set(`${id}:${kind}:${ordinal}`, ticket);
    return ticket;
  }

  private stream(id: string, ordinal: number) {
    const key = `${id}:${ordinal}`;
    let value = this.streams.get(key);
    if (!value) {
      value = deferred<string>();
      this.streams.set(key, value);
    }
    return value;
  }

  // This releases an actual held EventSource HTTP response, not a synthetic DOM
  // event or replacement EventSource. A long retry prevents EOF reconnect noise.
  emit(id: string, events: Array<{ seq: number; event: Json }>, ordinal = 1) {
    this.stream(id, ordinal).release(
      "retry: 60000\n\n" +
        events.map(({ seq, event }) => frame(seq, event)).join(""),
    );
  }

  async install() {
    await this.page.addInitScript(() =>
      localStorage.setItem("rpi-web-language", "en"),
    );
    await this.page.route("**/api/v1/**", async (route: Route) => {
      const request = route.request();
      const url = new URL(request.url());
      const path = url.pathname;
      const session = (id: string) => ({
        sessionId: id,
        projectId: "loading-project",
        title: titles[id as keyof typeof titles],
        status: "ready",
        createdAt: 1_700_000_000_000,
        updatedAt: 1_700_000_000_100,
        lastSeq: 10,
        error: null,
        latestSettlement: null,
        pinned: false,
        archived: false,
      });
      if (path === "/api/v1/auth" && request.method() === "GET")
        return route.fulfill({
          json: { required: false, authenticated: true },
        });
      if (path === "/api/v1/capabilities")
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
      if (path === "/api/v1/projects" && request.method() === "GET")
        return route.fulfill({
          json: {
            projects: [
              {
                id: "loading-project",
                name: "Loading project",
                path: "/mock/loading",
                archived: false,
              },
            ],
            defaultProjectId: "loading-project",
            projectsRoot: "/mock",
          },
        });
      if (path === "/api/v1/sessions" && request.method() === "GET")
        return route.fulfill({ json: { sessions: [session(A), session(B)] } });

      const match = path.match(
        /^\/api\/v1\/sessions\/([^/]+)\/(open|snapshot|commands|events|run-timings\/query)$/,
      );
      if (!match) return this.reject(route, `${request.method()} ${path}`);
      const id = match[1];
      const body =
        match[2] === "commands" ? (request.postDataJSON() as Json) : null;
      const kind = body ? String(body.type) : match[2];
      if (![A, B].includes(id))
        return this.reject(route, `unknown session ${id}`);
      if (body && !["get_state", ...diagnostics, ...catalogs].includes(kind))
        return this.reject(route, `unexpected command ${kind}`);
      if (
        (["snapshot", "events"].includes(kind) && request.method() !== "GET") ||
        (!["snapshot", "events"].includes(kind) && request.method() !== "POST")
      )
        return this.reject(route, `${request.method()} ${kind}`);
      const ordinal = this.count(id, kind) + 1;
      this.calls.push({ id, kind, ordinal, url, request });
      const ticket = this.holds.get(`${id}:${kind}:${ordinal}`);
      ticket?.arrived.release();
      if (ticket) await ticket.wait;
      try {
        if (kind === "events") {
          const stream = this.stream(id, ordinal);
          if (this.closing) stream.release("");
          const payload = await stream.promise;
          await route.fulfill({
            contentType: "text/event-stream",
            body: payload,
          });
        } else if (ticket?.reply) {
          await route.fulfill(ticket.reply);
        } else if (kind === "open") {
          await route.fulfill({ json: session(id) });
        } else if (kind === "snapshot") {
          await route.fulfill(
            this.snapshotFailure ?? { json: this.snapshots[id] },
          );
        } else if (kind === "run-timings/query") {
          await route.fulfill({
            json: {
              timings: [],
              complete: true,
              cursor: null,
              scannedBytes: 0,
              totalBytes: 0,
            },
          });
        } else {
          const data: Record<string, unknown> = {
            get_state: this.states[id],
            get_queue: { items: [], count: 0 },
            get_stats: {
              messageCount: 1,
              totalTokens: 100,
              cachedTokens: 0,
              uncachedTokens: 100,
              costTotal: 0,
            },
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
                  id: `${id}-model`,
                  name: `Catalog ${id}`,
                  provider: "offline",
                },
              ],
            },
            get_available_thinking_levels: { levels: ["off", "high"] },
          };
          await route.fulfill({
            json: {
              type: "response",
              command: kind,
              success: true,
              data: data[kind],
            },
          });
        }
      } catch (error) {
        // Releasing a route after its fetch was aborted is expected in the
        // navigation tests. Do not swallow unrelated fulfillment errors.
        if (!this.closing && !request.failure()) throw error;
      } finally {
        ticket?.finished.release();
      }
    });
  }

  private async reject(route: Route, reason: string) {
    this.unexpected.push(reason);
    await route.fulfill({
      status: 501,
      json: { error: `Unmocked API: ${reason}` },
    });
  }

  async dispose() {
    this.closing = true;
    for (const ticket of this.holds.values()) ticket.release();
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
      // Release every outstanding route even when an assertion fails.
      await host.dispose();
      expect(
        host.unexpected,
        "no test may fall through to a real API or send a mutation",
      ).toEqual([]);
    }
  },
});

const composer = (page: Page) => page.locator(".composer textarea");
const send = (page: Page) =>
  page.getByRole("button", { name: "Send message", exact: true });
const abort = (page: Page) =>
  page.getByRole("button", { name: "Abort", exact: true });
const model = (page: Page) =>
  page.getByRole("button", { name: "Choose model", exact: true });
const rows = (page: Page) => page.locator(".message.user .message-body");

async function start(page: Page, host: MockHost, id = A) {
  await page.goto(`/?session=${id}`);
  await expect.poll(() => host.count(id, "open")).toBeGreaterThanOrEqual(1);
}

async function select(page: Page, id: string) {
  await page
    .getByRole("button", {
      name: new RegExp(`^${titles[id as keyof typeof titles]}`),
    })
    .click();
  await expect(page).toHaveURL(new RegExp(`session=${id}`));
}

async function received(host: MockHost, id: string, kind: string, count = 1) {
  await expect.poll(() => host.count(id, kind)).toBe(count);
}

async function hydrated(page: Page, host: MockHost, id = A) {
  await received(host, id, "get_available_thinking_levels");
  await expect(model(page)).toContainText(`Catalog ${id}`);
}

function stateReply(id: string, extra: Json = {}): Reply {
  return {
    json: {
      type: "response",
      command: "get_state",
      success: true,
      data: state(id, extra),
    },
  };
}

// Give React/browser microtasks a render boundary, without a speed threshold or
// networkidle (held SSE responses deliberately never become idle).
async function rendered(page: Page) {
  await page.evaluate(
    () =>
      new Promise<void>((resolve) => {
        requestAnimationFrame(() => requestAnimationFrame(() => resolve()));
      }),
  );
}

test("open finishes before a bounded snapshot, and snapshot paints/connects before state", async ({
  page,
  host,
}) => {
  const opening = host.hold(A, "open");
  const history = host.hold(A, "snapshot");
  const metadata = host.hold(A, "get_state");
  await start(page, host);
  await rendered(page);
  expect(host.kinds(A)).toEqual(["open"]);
  opening.release();
  await received(host, A, "snapshot");
  const query = host.calls.find(
    (call) => call.id === A && call.kind === "snapshot",
  )!.url.searchParams;
  expect(query.get("limit")).toBe("40");
  expect(query.get("includeHistory")).toBe("false");
  await rendered(page);
  expect(host.count(A, "get_state")).toBe(0);
  history.release();
  await expect(rows(page)).toHaveText(`History ${A}`);
  await received(host, A, "events");
  await received(host, A, "get_state");
  expect(
    host.calls
      .find((call) => call.kind === "events")!
      .url.searchParams.get("after"),
  ).toBe("10");
  expect(
    host
      .kinds(A)
      .filter((kind) => catalogs.includes(kind) || diagnostics.includes(kind)),
  ).toEqual([]);
  await composer(page).fill("Local draft, never submitted");
  await expect(send(page)).toBeEnabled();
  metadata.release();
  await hydrated(page, host);
});

test("hydration precedes catalog warmups, then diagnostics wait 450ms and serialize queue/stats/history/auth", async ({
  page,
  host,
}) => {
  const now = new Date("2026-01-01T00:00:00Z");
  await page.clock.install({ time: now });
  await page.clock.pauseAt(now);
  const metadata = host.hold(A, "get_state");
  const models = host.hold(A, catalogs[0]);
  const thinking = host.hold(A, catalogs[1]);
  const stages = diagnostics.map((kind) => host.hold(A, kind));
  await start(page, host);
  await expect(rows(page)).toBeVisible();
  await received(host, A, "get_state");
  await page.clock.runFor(500);
  expect(
    host
      .kinds(A)
      .filter((kind) => catalogs.includes(kind) || diagnostics.includes(kind)),
  ).toEqual([]);
  metadata.release();
  await received(host, A, catalogs[0]);
  await page.clock.runFor(500);
  expect(host.kinds(A).filter((kind) => diagnostics.includes(kind))).toEqual(
    [],
  );
  models.release();
  await received(host, A, catalogs[1]);
  await page.clock.runFor(500);
  expect(host.kinds(A).filter((kind) => diagnostics.includes(kind))).toEqual(
    [],
  );
  thinking.release();
  await thinking.finished.promise;
  await hydrated(page, host);
  await page.clock.runFor(0);
  await page.clock.runFor(449);
  expect(host.kinds(A).filter((kind) => diagnostics.includes(kind))).toEqual(
    [],
  );
  await page.clock.runFor(1);
  for (let index = 0; index < diagnostics.length; index++) {
    await received(host, A, diagnostics[index]);
    expect(host.kinds(A).filter((kind) => diagnostics.includes(kind))).toEqual(
      diagnostics.slice(0, index + 1),
    );
    stages[index].release();
  }
});

test("known snapshot status replays captured events immediately, without late state clearing busy", async ({
  page,
  host,
}) => {
  host.snapshots[A] = snapshot(A, {
    events: [
      { seq: 11, event: { type: "operation_accepted", runId: "snapshot-run" } },
    ],
    lastSeq: 11,
  });
  const metadata = host.hold(A, "get_state");
  await start(page, host);
  await expect(rows(page)).toHaveText(`History ${A}`);
  await expect(abort(page)).toBeEnabled();
  await received(host, A, "events");
  expect(
    host.calls
      .find((call) => call.kind === "events")!
      .url.searchParams.get("after"),
  ).toBe("11");
  metadata.release();
  await hydrated(page, host);
  await expect(abort(page)).toBeEnabled();
});

test("held live SSE makes the session busy before metadata and stale idle state cannot undo it", async ({
  page,
  host,
}) => {
  const metadata = host.hold(A, "get_state");
  await start(page, host);
  await received(host, A, "events");
  host.emit(A, [
    { seq: 11, event: { type: "operation_accepted", runId: "live-run" } },
  ]);
  await expect(abort(page)).toBeEnabled();
  metadata.release();
  await hydrated(page, host);
  await expect(abort(page)).toBeEnabled();
  await expect(rows(page)).toHaveText(`History ${A}`);
});

test("live SSE dialog survives late idle metadata", async ({ page, host }) => {
  const metadata = host.hold(A, "get_state");
  await start(page, host);
  await received(host, A, "events");
  host.emit(A, [{ seq: 11, event: question }]);
  const dialog = page.getByRole("dialog");
  await expect(
    dialog.getByRole("heading", { name: "Keep the live question?" }),
  ).toBeVisible();
  metadata.release();
  await received(host, A, catalogs[1]);
  await rendered(page);
  await expect(
    dialog.getByRole("heading", { name: "Keep the live question?" }),
  ).toBeVisible();
  await expect(
    dialog.getByRole("button", { name: "Yes", exact: true }),
  ).toBeEnabled();
});

test("live settlement is not resurrected by stale hydration or replaced with stale runtime metadata", async ({
  page,
  host,
}) => {
  host.snapshots[A] = snapshot(A, { status: "running" });
  host.states[A] = state(A, { modelId: "settled-model" });
  const metadata = host.hold(
    A,
    "get_state",
    1,
    stateReply(A, {
      modelId: "obsolete-model",
      activeOperation: { runId: "obsolete-run" },
    }),
  );
  await start(page, host);
  await expect(abort(page)).toBeEnabled();
  await received(host, A, "events");
  host.snapshots[A] = snapshot(A, { lastSeq: 11 });
  host.emit(A, [
    {
      seq: 11,
      event: {
        type: "agent_settled",
        runId: "obsolete-run",
        outcome: "completed",
        summary: "Live settlement won",
      },
    },
  ]);
  await expect(
    page.getByText("Live settlement won", { exact: true }),
  ).toBeVisible();
  await expect(abort(page)).toHaveCount(0);
  // The settlement refresh is a newer source than the still-held initial read.
  await received(host, A, "get_state", 2);
  await expect(model(page)).toContainText("settled-model");
  metadata.release();
  await received(host, A, catalogs[1]);
  await composer(page).fill("Do not submit");
  await expect(send(page)).toBeEnabled();
  await expect(abort(page)).toHaveCount(0);
  await expect(model(page)).toContainText("settled-model");
});

test("live host_closed cannot be reopened by stale active-operation hydration", async ({
  page,
  host,
}) => {
  const metadata = host.hold(
    A,
    "get_state",
    1,
    stateReply(A, { activeOperation: { runId: "dead-run" } }),
  );
  await start(page, host);
  await received(host, A, "events");
  host.emit(A, [{ seq: 11, event: { type: "host_closed" } }]);
  await expect(page.locator(".connection")).toHaveText("closed");
  metadata.release();
  await metadata.finished.promise;
  await rendered(page);
  await expect(page.locator(".connection")).toHaveText("closed");
  await expect(abort(page)).toHaveCount(0);
  await expect(rows(page)).toHaveText(`History ${A}`);
});

test("legacy status-less snapshot paints but state seeds status before replay and SSE", async ({
  page,
  host,
}) => {
  const legacy = snapshot(A, {
    events: [
      {
        seq: 11,
        event: { type: "operation_accepted", runId: "legacy-replay" },
      },
    ],
    lastSeq: 11,
  });
  delete legacy.status;
  host.snapshots[A] = legacy;
  const metadata = host.hold(A, "get_state");
  await start(page, host);
  await expect(rows(page)).toHaveText(`History ${A}`);
  await received(host, A, "get_state");
  await composer(page).fill("Disabled while status is unknown");
  await expect(send(page)).toBeDisabled();
  await composer(page).press("Enter");
  await expect(composer(page)).toHaveValue("Disabled while status is unknown");
  expect(host.count(A, "events")).toBe(0);
  await expect(abort(page)).toHaveCount(0);
  metadata.release();
  await expect(abort(page)).toBeEnabled();
  await received(host, A, "events");
  expect(
    host.calls
      .find((call) => call.kind === "events")!
      .url.searchParams.get("after"),
  ).toBe("11");
  await hydrated(page, host);
  await expect(abort(page)).toBeEnabled();
});

test("without get_messages capability initialization waits for state and never requests a snapshot", async ({
  page,
  host,
}) => {
  host.commands = host.commands.filter((kind) => kind !== "get_messages");
  const metadata = host.hold(A, "get_state");
  await start(page, host);
  await received(host, A, "get_state");
  await composer(page).fill("Local draft");
  await expect(send(page)).toBeDisabled();
  expect(host.count(A, "snapshot")).toBe(0);
  expect(host.count(A, "events")).toBe(0);
  metadata.release();
  await received(host, A, "events");
  await expect(send(page)).toBeEnabled();
  expect(
    host.calls
      .find((call) => call.kind === "events")!
      .url.searchParams.get("after"),
  ).toBe("0");
  expect(host.count(A, "snapshot")).toBe(0);
});

test("auxiliary state failure keeps known rows and live SSE with an error banner", async ({
  page,
  host,
}) => {
  const metadata = host.hold(A, "get_state", 1, {
    status: 503,
    json: { error: "Auxiliary metadata unavailable" },
  });
  await start(page, host);
  await expect(rows(page)).toHaveText(`History ${A}`);
  await received(host, A, "events");
  metadata.release();
  await expect(page.locator(".error-banner")).toContainText(
    "Auxiliary metadata unavailable",
  );
  host.emit(A, [
    {
      seq: 11,
      event: { type: "operation_accepted", runId: "still-connected" },
    },
  ]);
  await expect(abort(page)).toBeEnabled();
  await expect(rows(page)).toHaveText(`History ${A}`);
  expect(host.count(A, "open")).toBe(1);
  expect(host.count(A, "snapshot")).toBe(1);
});

test("auxiliary state 401 clears access even after known snapshot first paint", async ({
  page,
  host,
}) => {
  const metadata = host.hold(A, "get_state", 1, {
    status: 401,
    json: { error: "Expired fixture access" },
  });
  await start(page, host);
  await expect(rows(page)).toHaveText(`History ${A}`);
  await received(host, A, "events");
  metadata.release();
  await expect(
    page.getByLabel("Workspace password", { exact: true }),
  ).toBeVisible();
  await expect(page.locator(".workspace")).toHaveCount(0);
  expect(host.kinds(A).filter((kind) => diagnostics.includes(kind))).toEqual(
    [],
  );
});

test("snapshot failure uses normal retry with no premature state or SSE", async ({
  page,
  host,
}) => {
  // Any automatic recovery attempt fails too. Clear the fault only on the
  // explicit user retry, without coupling the test to an internal retry count.
  host.snapshotFailure = {
    status: 503,
    json: { error: "Snapshot temporarily unavailable" },
  };
  await start(page, host);
  await expect(
    page.getByRole("button", { name: "Retry connection", exact: true }),
  ).toBeVisible();
  expect(host.count(A, "events")).toBe(0);
  expect(host.count(A, "get_state")).toBe(0);
  await composer(page).fill("Never send while disconnected");
  await expect(send(page)).toBeDisabled();
  host.snapshotFailure = undefined;
  await page
    .getByRole("button", { name: "Retry connection", exact: true })
    .click();
  await expect(rows(page)).toHaveText(`History ${A}`);
  await received(host, A, "events");
  await expect(
    page.getByRole("button", { name: "Retry connection", exact: true }),
  ).toHaveCount(0);
});

test("navigation aborts a held snapshot and cannot reopen the abandoned session", async ({
  page,
  host,
}) => {
  const old = host.hold(A, "snapshot", 1, {
    status: 409,
    json: { error: "Old host is offline" },
  });
  await start(page, host);
  await received(host, A, "snapshot");
  const abandoned = host.calls.find(
    (call) => call.id === A && call.kind === "snapshot",
  )!.request;
  await select(page, B);
  await expect(rows(page)).toHaveText(`History ${B}`);
  await hydrated(page, host, B);
  await expect
    .poll(() => abandoned.failure()?.errorText ?? "")
    .toMatch(/abort|cancel/i);
  old.release();
  await old.finished.promise;
  await rendered(page);
  expect(host.count(A, "open")).toBe(1);
  expect(host.count(A, "get_state")).toBe(0);
  expect(host.count(A, "events")).toBe(0);
  await expect(rows(page)).toHaveText(`History ${B}`);
  await expect(page.locator(".error-banner")).toHaveCount(0);
});

test("A to B to A isolates stale state by navigation revision, not just session id", async ({
  page,
  host,
}) => {
  const oldA = host.hold(
    A,
    "get_state",
    1,
    stateReply(A, {
      modelId: "obsolete-alpha",
      activeOperation: { runId: "old-alpha" },
    }),
  );
  const oldB = host.hold(
    B,
    "get_state",
    1,
    stateReply(B, {
      modelId: "obsolete-beta",
      activeOperation: { runId: "old-beta" },
    }),
  );
  await start(page, host);
  await received(host, A, "get_state");
  await select(page, B);
  await expect(rows(page)).toHaveText(`History ${B}`);
  await received(host, B, "get_state");
  await received(host, B, "events");
  await select(page, A);
  await received(host, A, "get_state", 2);
  await hydrated(page, host);
  oldA.release();
  oldB.release();
  await Promise.all([oldA.finished.promise, oldB.finished.promise]);
  await rendered(page);
  await expect(rows(page)).toHaveText(`History ${A}`);
  await expect(model(page)).toContainText(`Catalog ${A}`);
  await expect(abort(page)).toHaveCount(0);
  await expect(page.locator(".error-banner")).toHaveCount(0);
  expect(host.count(B, "events")).toBe(1);
  expect(
    host
      .kinds(B)
      .filter((kind) => catalogs.includes(kind) || diagnostics.includes(kind)),
  ).toEqual([]);
});

test("navigation aborts a diagnostic request and stops the abandoned serialized chain", async ({
  page,
  host,
}) => {
  const queue = host.hold(A, "get_queue", 1, {
    json: {
      type: "response",
      command: "get_queue",
      success: true,
      data: {
        items: [
          {
            entryId: "old-queue",
            queue: "followUp",
            status: "queued",
            text: "Stale Alpha queue",
          },
        ],
        count: 1,
      },
    },
  });
  await start(page, host);
  await received(host, A, "get_queue");
  const oldRequest = host.calls.find(
    (call) => call.id === A && call.kind === "get_queue",
  )!.request;
  await select(page, B);
  await hydrated(page, host, B);
  await expect
    .poll(() => oldRequest.failure()?.errorText ?? "")
    .toMatch(/abort|cancel/i);
  queue.release();
  await queue.finished.promise;
  await received(host, B, "get_auth_status");
  expect(host.kinds(A).filter((kind) => diagnostics.includes(kind))).toEqual([
    "get_queue",
  ]);
  await expect(
    page.getByText("Stale Alpha queue", { exact: true }),
  ).toHaveCount(0);
  await expect(rows(page)).toHaveText(`History ${B}`);
});

test("reselecting the current session preserves expansion and draft without any session API reload", async ({
  page,
  host,
}) => {
  host.snapshots[A] = snapshot(A, {
    messages: [
      {
        role: "user",
        entryId: "expand-question",
        content: "Expansion survives reselection",
      },
      {
        role: "assistant",
        entryId: "expand-thought",
        content: [{ type: "thinking", thinking: "Private fixture reasoning" }],
      },
    ],
  });
  await start(page, host);
  await hydrated(page, host);
  await received(host, A, "get_auth_status");
  await page.locator(".thinking-row summary").click();
  await expect(page.locator(".thinking-row")).toHaveAttribute("open", "");
  await composer(page).fill("Keep this unsent draft");
  const before = host.kinds(A);
  await select(page, A);
  await rendered(page);
  await expect(composer(page)).toHaveValue("Keep this unsent draft");
  await expect(page.locator(".thinking-row")).toHaveAttribute("open", "");
  await expect(send(page)).toBeEnabled();
  expect(host.kinds(A)).toEqual(before);
  await expect(rows(page)).toHaveText("Expansion survives reselection");
});

test("an old settlement tick does not create a second initial get_state when switching sessions", async ({
  page,
  host,
}) => {
  await start(page, host);
  await hydrated(page, host);
  await received(host, A, "events");
  host.snapshots[A] = snapshot(A, { lastSeq: 11 });
  host.emit(A, [
    {
      seq: 11,
      event: {
        type: "agent_settled",
        runId: "prior-settlement",
        outcome: "completed",
        summary: "Prior settlement observed",
      },
    },
  ]);
  await expect(
    page.getByText("Prior settlement observed", { exact: true }),
  ).toBeVisible();
  const next = host.hold(B, "get_state");
  await select(page, B);
  await expect(rows(page)).toHaveText(`History ${B}`);
  await received(host, B, "get_state");
  await rendered(page);
  expect(host.count(B, "get_state")).toBe(1);
  next.release();
  await hydrated(page, host, B);
  await received(host, B, "get_auth_status");
  expect(host.count(B, "get_state")).toBe(1);
  expect(
    host
      .kinds(B)
      .filter((kind) => ["open", "snapshot", "events"].includes(kind)),
  ).toEqual(["open", "snapshot", "events"]);
});

test("390px mobile paints a known snapshot before metadata and keeps unknown initialization unsendable", async ({
  page,
  host,
}) => {
  await page.setViewportSize({ width: 390, height: 844 });
  const history = host.hold(A, "snapshot");
  const metadata = host.hold(A, "get_state");
  await start(page, host);
  await received(host, A, "snapshot");
  await composer(page).fill("Mobile draft only");
  await expect(send(page)).toBeDisabled();
  history.release();
  await expect(rows(page)).toHaveText(`History ${A}`);
  await expect(send(page)).toBeEnabled();
  await received(host, A, "events");
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
  ).toBe(true);
  metadata.release();
  await hydrated(page, host);
  await expect(composer(page)).toHaveValue("Mobile draft only");
  await expect(send(page)).toBeEnabled();
});

test("a current diagnostic 401 logs out and stops the remaining serialized reads", async ({
  page,
  host,
}) => {
  const queue = host.hold(A, "get_queue", 1, {
    status: 401,
    json: { error: "Access expired" },
  });
  await start(page, host);
  await received(host, A, "get_queue");
  queue.release();
  await expect(
    page.getByRole("heading", { name: "Sign in to rpi" }),
  ).toBeVisible();
  expect(host.kinds(A).filter((kind) => diagnostics.includes(kind))).toEqual([
    "get_queue",
  ]);
});

test("an abandoned metadata 401 cannot sign out the newly selected session", async ({
  page,
  host,
}) => {
  const stale = host.hold(A, "get_state", 1, {
    status: 401,
    json: { error: "Obsolete access failure" },
  });
  await start(page, host);
  await received(host, A, "get_state");
  const failed = page.waitForEvent("requestfailed", {
    predicate: (request) =>
      request ===
      host.calls.find((call) => call.id === A && call.kind === "get_state")
        ?.request,
  });
  await select(page, B);
  await failed;
  stale.release();
  await hydrated(page, host, B);
  await expect(rows(page)).toHaveText(`History ${B}`);
  await expect(page.locator(".workspace")).toBeVisible();
  await expect(page.locator(".error-banner")).toHaveCount(0);
  expect(host.count(A, "open")).toBe(1);
});

test("snapshot 401 does not attempt a redundant session reopen", async ({
  page,
  host,
}) => {
  host.snapshotFailure = {
    status: 401,
    json: { error: "Snapshot access expired" },
  };
  await start(page, host);
  await expect(
    page.getByRole("heading", { name: "Sign in to rpi" }),
  ).toBeVisible();
  expect(host.count(A, "open")).toBe(1);
  expect(host.count(A, "get_state")).toBe(0);
  expect(host.count(A, "events")).toBe(0);
});
