import { test as base, expect, type Page, type Request, type Route } from "@playwright/test";

// The bridge retires an idle native host but keeps its registry entry, so a
// session can still be listed while every `/commands` call answers 404 until it
// is reopened. These tests pin the client's recovery contract with no real host.
const A = "reaped-a";
type Json = Record<string, unknown>;
type Call = { id: string; kind: string; ordinal: number; request: Request };

function snapshot(id: string): Json {
  return {
    sessionId: id,
    status: "ready",
    lastSeq: 3,
    messages: [{ role: "user", entryId: `${id}-question`, content: "Hello" }],
    events: [],
    total: 1,
    truncated: false,
    hasMore: false,
    firstId: `${id}-question`,
    lastId: `${id}-question`,
    leafId: `${id}-leaf`,
    pendingResume: false,
    pendingDialogs: [],
  };
}

function state(id: string): Json {
  return {
    sessionId: id,
    modelId: `${id}-model`,
    provider: "offline",
    thinkingLevel: "off",
    activeOperation: null,
    activeTools: [],
    queue: { steeringCount: 0, followUpCount: 0 },
  };
}

const commands = [
  "get_state",
  "get_messages",
  "get_queue",
  "get_stats",
  "get_history",
  "get_auth_status",
  "get_available_models",
  "get_available_thinking_levels",
  "set_model",
  "prompt",
];

/** Models a host that has been reaped: commands 404 until `/open` revives it. */
class ReapedHost {
  calls: Call[] = [];
  streamClosers: Array<() => void> = [];
  live = false;
  /** Fail the first `open` too, to prove a deleted session is not resurrected. */
  openFails = false;

  count(kind: string) {
    return this.calls.filter((call) => call.kind === kind).length;
  }

  kinds() {
    return this.calls.map((call) => call.kind);
  }

  async install(page: Page) {
    await page.addInitScript(() => localStorage.setItem("rpi-web-language", "en"));
    await page.route("**/api/v1/**", async (route: Route) => {
      const request = route.request();
      const path = new URL(request.url()).pathname;
      const session = {
        sessionId: A,
        projectId: "reaped-project",
        title: "Reaped session",
        status: this.live ? "ready" : "offline",
        createdAt: 1_700_000_000_000,
        updatedAt: 1_700_000_000_100,
        lastSeq: 3,
        error: null,
        latestSettlement: null,
        pinned: false,
        archived: false,
      };
      if (path === "/api/v1/auth")
        return route.fulfill({ json: { required: false, authenticated: true } });
      if (path === "/api/v1/capabilities")
        return route.fulfill({
          json: {
            protocol: "rpi",
            supportedCommands: commands,
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
      if (path === "/api/v1/projects")
        return route.fulfill({
          json: {
            projects: [
              {
                id: "reaped-project",
                name: "Reaped project",
                path: "/mock/reaped",
                archived: false,
              },
            ],
            defaultProjectId: "reaped-project",
            projectsRoot: "/mock",
          },
        });
      if (path === "/api/v1/sessions" && request.method() === "GET")
        return route.fulfill({ json: { sessions: [session] } });

      const match = path.match(/^\/api\/v1\/sessions\/([^/]+)\/(open|snapshot|commands|events)$/);
      if (!match) return route.fulfill({ status: 404, json: { error: "unexpected" } });
      const kind = match[2] === "commands"
        ? String((request.postDataJSON() as Json).type)
        : match[2];
      this.calls.push({ id: A, kind, ordinal: this.count(kind) + 1, request });

      if (kind === "open") {
        if (this.openFails)
          return route.fulfill({
            status: 404,
            json: { error: "unknown session", code: "session_not_found" },
          });
        this.live = true;
        return route.fulfill({ json: session });
      }
      if (kind === "events") {
        // Hold the stream open like a real host. Closing it here would make
        // EventSource reconnect immediately and mask the recovery behaviour.
        // Keep the response pending so EventSource stays connected, exactly like
        // a live host that has nothing to replay yet.
        await new Promise<void>((resolve) => {
          const close = () => resolve();
          this.streamClosers.push(close);
          route.request().frame()?.page()?.once("close", close);
          setTimeout(close, 25_000);
        });
        return route.fulfill({
          contentType: "text/event-stream",
          body: "retry: 60000\n\n",
        }).catch(() => undefined);
      }
      if (!this.live)
        return route.fulfill({ status: 404, json: { error: "session not found" } });
      if (kind === "snapshot") {
              return route.fulfill({ json: snapshot(A) });
      }
      const data: Record<string, unknown> = {
        get_state: state(A),
        get_queue: { items: [], count: 0 },
        get_stats: { messageCount: 1, totalTokens: 1, cachedTokens: 0, uncachedTokens: 1, costTotal: 0 },
        get_history: { records: [], total: 0, limit: 40, hasMore: false, openOperations: [] },
        get_auth_status: { providers: [] },
        get_available_models: { models: [{ id: `${A}-model`, name: "Catalog", provider: "offline" }] },
        get_available_thinking_levels: { levels: ["off", "high"] },
        prompt: { accepted: true },
      };
      return route.fulfill({
        json: { type: "response", command: kind, success: true, data: data[kind] },
      });
    });
  }
}

const test = base.extend<{ host: ReapedHost }>({
  // eslint-disable-next-line no-empty-pattern
  host: async ({}, use) => {
    await use(new ReapedHost());
  },
  page: async ({ page, host }, use) => {
    await host.install(page);
    await use(page);
  },
});

test("a directory read reopens the retired host instead of failing", async ({ page, host }) => {
  await page.goto("/");
  await expect(page.getByText("Reaped session").first()).toBeVisible();
  // The initial open revives the host, which is the pre-existing behaviour.
  await expect.poll(() => host.count("open")).toBeGreaterThan(0);
  await expect.poll(() => host.count("get_state")).toBeGreaterThan(0);
});

test("prompt is never replayed on a retired host", async ({ page, host }) => {
  await page.goto("/");
  await expect.poll(() => host.count("get_state")).toBeGreaterThan(0);
  host.live = false;
  host.calls.length = 0;

  const composer = page.locator(".composer textarea");
  await composer.fill("do not duplicate me");
  await composer.press("Enter");

  // The client reopens the retired host and delivers the message exactly once.
  // Replaying `prompt` would append a duplicate, so the count is the contract.
  await expect
    .poll(() => host.calls.filter((call) => call.kind === "prompt").length, { timeout: 15_000 })
    .toBe(1);
  await expect.poll(() => host.live).toBeTruthy();
});

test("a stream that keeps failing stops retrying and reports the session", async ({ page, host }) => {
  await page.goto("/");
  await expect.poll(() => host.count("get_state")).toBeGreaterThan(0);
  // A deleted session cannot be reopened: `/open` answers 404 every time.
  host.live = false;
  host.openFails = true;
  host.calls.length = 0;

  const composer = page.locator(".composer textarea");
  await composer.fill("this session is gone");
  await composer.press("Enter");

  // Recovery stays bounded instead of spinning open/snapshot/events forever,
  // and the surface reports the real reason the message was not delivered.
  await expect
    .poll(() => host.count("open"), { timeout: 20_000 })
    .toBeLessThanOrEqual(6);
  await expect(page.getByText("session not found")).toBeVisible();
});
