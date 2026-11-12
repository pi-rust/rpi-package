import { test as base, expect, type Page, type Route } from "@playwright/test";

// The bridge retires an idle native host after `sessionIdleTimeoutSeconds`,
// measured from requests that target the session. The session-list poll does
// not refresh that timer, so the client has to touch the selected session
// itself while the user is composing. These tests use a fake clock, so the
// real timeout never runs.
const A = "keepalive-a";
const B = "keepalive-b";
type Json = Record<string, unknown>;

const commands = [
  "get_state",
  "get_messages",
  "get_queue",
  "get_stats",
  "get_history",
  "get_auth_status",
  "get_available_models",
  "get_available_thinking_levels",
];

function summary(id: string, title: string): Json {
  return {
    sessionId: id,
    projectId: "keepalive-project",
    title,
    tags: [],
    status: "ready",
    createdAt: 1_700_000_000_000,
    updatedAt: 1_700_000_000_100,
    lastSeq: 3,
    error: null,
    latestSettlement: null,
    pinned: false,
    archived: false,
  };
}

function snapshot(id: string): Json {
  return {
    sessionId: id,
    status: "ready",
    lastSeq: 3,
    messages: [
      { role: "user", entryId: `${id}-question`, content: "Hello" },
      { role: "assistant", entryId: `${id}-answer`, content: "Hi there" },
    ],
    events: [],
    total: 2,
    truncated: false,
    hasMore: false,
    firstId: `${id}-question`,
    lastId: `${id}-answer`,
    leafId: `${id}-leaf`,
    pendingResume: false,
    pendingDialogs: [],
  };
}

/** Counts the per-session reads the keepalive makes, separately from the list. */
class KeepaliveHost {
  sessionReads: string[] = [];
  listReads = 0;
  /** `sessionIdleTimeoutSeconds` as advertised by `/capabilities`. */
  idleTimeout: number | null = 6;

  async install(page: Page) {
    await page.addInitScript(() =>
      localStorage.setItem("rpi-web-language", "en"),
    );
    await page.route("**/api/v1/**", async (route: Route) => {
      const request = route.request();
      const path = new URL(request.url()).pathname;
      const json = (value: unknown) => route.fulfill({ json: value });
      if (path === "/api/v1/auth")
        return json({ required: false, authenticated: true });
      if (path === "/api/v1/capabilities")
        return json({
          protocol: "rpi",
          supportedCommands: commands,
          supportedActions: ["projects", "history", "queue", "stats"],
          sessionIdleTimeoutSeconds: this.idleTimeout,
          parityComplete: false,
        });
      if (path === "/api/v1/projects")
        return json({
          projects: [
            {
              id: "keepalive-project",
              name: "Keepalive project",
              path: "/mock/keepalive",
              archived: false,
            },
          ],
          defaultProjectId: "keepalive-project",
          projectsRoot: "/mock",
        });
      if (path === "/api/v1/sessions" && request.method() === "GET") {
        this.listReads++;
        return json({ sessions: [summary(A, "First session"), summary(B, "Second session")] });
      }
      const single = path.match(/^\/api\/v1\/sessions\/([^/]+)$/);
      if (single && request.method() === "GET") {
        this.sessionReads.push(single[1]);
        return json(summary(single[1], "Session"));
      }
      const match = path.match(
        /^\/api\/v1\/sessions\/([^/]+)\/(open|snapshot|commands|events)$/,
      );
      if (!match) return route.fulfill({ status: 404, json: { error: "unexpected" } });
      const id = match[1];
      const kind = match[2];
      if (kind === "open") return json(summary(id, "Session"));
      if (kind === "snapshot") return json(snapshot(id));
      if (kind === "events")
        return route.fulfill({
          contentType: "text/event-stream",
          body: "retry: 60000\n\n",
        });
      const body = request.postDataJSON() as Json;
      const data: Record<string, unknown> = {
        get_state: {
          sessionId: id,
          modelId: "mock-model",
          provider: "offline",
          thinkingLevel: "off",
          activeOperation: null,
          activeTools: [],
          queue: { steeringCount: 0, followUpCount: 0 },
        },
        get_queue: { items: [], count: 0 },
        get_stats: {
          messageCount: 2,
          totalTokens: 1,
          cachedTokens: 0,
          uncachedTokens: 1,
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
          models: [{ id: "mock-model", name: "Mock", provider: "offline" }],
        },
        get_available_thinking_levels: { levels: ["off", "high"] },
      };
      return json({
        type: "response",
        command: String(body.type),
        success: true,
        data: data[String(body.type)],
      });
    });
  }
}

const test = base.extend<{ host: KeepaliveHost }>({
  // eslint-disable-next-line no-empty-pattern
  host: async ({}, use) => {
    await use(new KeepaliveHost());
  },
  page: async ({ page, host }, use) => {
    await page.clock.install();
    await host.install(page);
    await use(page);
  },
});

test("the selected session is touched well inside the advertised idle timeout", async ({
  page,
  host,
}) => {
  await page.goto("/");
  await expect(page.getByText("First session").first()).toBeVisible();
  await expect.poll(() => host.sessionReads.length).toBeGreaterThan(0);

  // Ten seconds is five thirds of the advertised six-second timeout: without a
  // keepalive the host would already have been retired at this point.
  const before = host.sessionReads.length;
  await page.clock.runFor(10_000);
  await expect
    .poll(() => host.sessionReads.length - before)
    .toBeGreaterThanOrEqual(4);
  // Only the selected session is kept alive; probing every listed session
  // would spawn hosts the user never opened.
  expect(new Set(host.sessionReads)).toEqual(new Set([A]));
});

test("a hidden tab does not claim to keep the host alive", async ({ page, host }) => {
  await page.goto("/");
  await expect(page.getByText("First session").first()).toBeVisible();
  await page.clock.runFor(2_000);
  const hidden = await page.evaluate(() => {
    Object.defineProperty(document, "hidden", { value: true, configurable: true });
    return document.hidden;
  });
  expect(hidden).toBe(true);
  const before = host.sessionReads.length;
  await page.clock.runFor(10_000);
  expect(host.sessionReads.length).toBe(before);
  // Becoming visible again probes immediately rather than waiting for the next
  // interval, so a retirement during the hidden period surfaces right away.
  await page.evaluate(() => {
    Object.defineProperty(document, "hidden", { value: false, configurable: true });
    document.dispatchEvent(new Event("visibilitychange"));
  });
  await expect.poll(() => host.sessionReads.length).toBeGreaterThan(before);
});

test("no keepalive is run when the server retains hosts until explicit close", async ({
  page,
  host,
}) => {
  host.idleTimeout = null;
  await page.goto("/");
  await expect(page.getByText("First session").first()).toBeVisible();
  await page.clock.runFor(30_000);
  expect(host.sessionReads).toEqual([]);
});
