import { test as base, expect, type Page } from "@playwright/test";
import { chooseMenuOption } from "./controls";

// Optional public-dev qualification; every API call remains intercepted.
if (process.env.RPI_MODEL_PREFERENCE_BASE_URL)
  base.use({ baseURL: process.env.RPI_MODEL_PREFERENCE_BASE_URL });

type Json = Record<string, unknown>;
const models = [
  { provider: "native", id: "default", name: "Native default" },
  { provider: "chosen", id: "special", name: "Chosen model" },
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
class Host {
  calls: { session: string; type: string; body: Json }[] = [];
  unexpected: string[] = [];
  states: Record<string, Json> = {
    old: { provider: "native", modelId: "default" },
    other: { provider: "native", modelId: "default" },
  };
  projects: Record<string, string> = { old: "p1", other: "p2" };
  failNext = false;
  holdNext: ReturnType<typeof gate> | null = null;
  streams: ReturnType<typeof gate>[] = [];
  constructor(readonly page: Page) {}
  summary(sessionId: string) {
    return {
      sessionId,
      projectId: this.projects[sessionId],
      title: `Session ${sessionId}`,
      status: "ready",
      createdAt: 1,
      updatedAt: 2,
      lastSeq: 0,
      pinned: false,
      archived: false,
    };
  }
  async install() {
    await this.page.route("**/api/**", async (route) => {
      const req = route.request();
      const path = new URL(req.url()).pathname.replace("/api/v1", "");
      const json = (data: unknown) => route.fulfill({ json: data });
      if (req.method() === "GET" && path === "/auth")
        return json({ required: false, authenticated: true });
      if (req.method() === "GET" && path === "/capabilities")
        return json({
          protocol: "rpi",
          supportedCommands: [
            "get_state",
            "get_messages",
            "get_available_models",
            "get_auth_status",
            "set_model",
            "prompt",
          ],
          supportedActions: ["projects", "provider_auth"],
          parityComplete: false,
        });
      if (req.method() === "GET" && path === "/projects")
        return json({
          projects: [1, 2].map((n) => ({
            id: `p${n}`,
            name: `Project ${n}`,
            path: `/mock/${n}`,
            archived: false,
          })),
          defaultProjectId: "p1",
          projectsRoot: "/mock",
        });
      if (path === "/sessions") {
        if (req.method() === "GET")
          return json({
            sessions: Object.keys(this.states).map((id) => this.summary(id)),
          });
        if (req.method() === "POST") {
          const id = `new${Object.keys(this.states).length}`;
          this.projects[id] = req.postDataJSON().projectId;
          this.states[id] = { provider: "native", modelId: "default" };
          this.calls.push({
            session: id,
            type: "create",
            body: req.postDataJSON(),
          });
          return json({ sessionId: id, status: "ready" });
        }
      }
      const match = path.match(
        /^\/sessions\/([^/]+)\/(open|snapshot|events|commands|run-timings\/query)$/,
      );
      if (match && this.states[match[1]]) {
        const [, session, endpoint] = match;
        if (endpoint === "open") return json(this.summary(session));
        if (endpoint === "snapshot")
          return json({
            sessionId: session,
            status: "ready",
            lastSeq: 0,
            messages: [],
            events: [],
            total: 0,
            hasMore: false,
            truncated: false,
            leafId: `${session}-leaf`,
            pendingResume: false,
          });
        if (endpoint === "events") {
          const stream = gate();
          this.streams.push(stream);
          await stream.promise;
          return route.fulfill({
            contentType: "text/event-stream",
            body: "retry: 60000\n\n",
          });
        }
        if (endpoint === "run-timings/query")
          return json({ timings: [], complete: true, cursor: null });
        const body = req.postDataJSON() as Json;
        const type = String(body.type);
        this.calls.push({ session, type, body });
        if (type === "set_model") {
          const fail = this.failNext;
          this.failNext = false;
          const hold = this.holdNext;
          this.holdNext = null;
          if (hold) await hold.promise;
          if (fail)
            return json({
              type: "response",
              command: type,
              success: false,
              error: "Mock model unavailable",
            });
          this.states[session] = {
            provider: body.provider,
            modelId: body.modelId,
          };
          return json(envelope(type, {}));
        }
        if (type === "prompt") return json(envelope(type, {}));
        if (type === "get_state")
          return json(
            envelope(type, {
              sessionId: session,
              ...this.states[session],
              thinkingLevel: "off",
            }),
          );
        if (type === "get_available_models")
          return json(envelope(type, { models }));
        if (type === "get_messages")
          return json(
            envelope(type, { messages: [], total: 0, hasMore: false }),
          );
        if (type === "get_auth_status")
          return json(envelope(type, { providers: [] }));
      }
      this.unexpected.push(`${req.method()} ${path}`);
      return route.fulfill({ status: 501, json: { error: "Unmocked API" } });
    });
  }
  writes() {
    return this.calls.filter((call) => call.type === "set_model");
  }
  async dispose() {
    this.holdNext?.release();
    for (const stream of this.streams) stream.release();
    await this.page.unrouteAll({ behavior: "wait" });
  }
}
const test = base.extend<{ host: Host }>({
  host: [
    async ({ page }, use) => {
      const host = new Host(page);
      await host.install();
      await page.addInitScript(() =>
        localStorage.setItem("rpi-web-language", "en"),
      );
      try {
        await use(host);
      } finally {
        await host.dispose();
        expect(host.unexpected).toEqual([]);
      }
    },
    { auto: true },
  ],
});
async function ready(page: Page, id = "old") {
  await expect(page.locator(".toolbar .breadcrumb h1")).toHaveText(
    `Session ${id}`,
  );
  await expect(
    page.getByRole("textbox", {
      name: "Send a prompt, or type / for commands…",
      exact: true,
    }),
  ).toBeEnabled();
  await expect(
    page.getByRole("button", { name: "Choose model", exact: true }),
  ).toBeEnabled();
}
async function start(page: Page) {
  await page.goto("/?session=old");
  await ready(page);
}
async function runtime(page: Page) {
  await page
    .getByRole("button", { name: "Runtime settings", exact: true })
    .click();
  await expect(page.locator(".inspector")).toBeVisible();
}
async function choose(page: Page) {
  await chooseMenuOption(page, "Choose model", { label: "Chosen model" });
}
async function newSession(page: Page, project = 1) {
  await page
    .getByRole("button", {
      name: `New session in: Project ${project}`,
      exact: true,
    })
    .click();
}
async function stored(page: Page) {
  return page.evaluate(() =>
    Object.entries(localStorage).filter(([key]) =>
      key.startsWith("rpi-web-model-preference:"),
    ),
  );
}

test("explicit composer choice is applied before publishing new sessions, persists across reload, never overrides resume", async ({
  page,
  host,
}) => {
  await start(page);
  expect(host.writes()).toHaveLength(0);
  expect(await stored(page)).toEqual([]);
  await choose(page);
  await expect.poll(() => host.states.old.provider).toBe("chosen");
  await expect.poll(async () => (await stored(page)).length).toBe(1);
  const hold = gate();
  host.holdNext = hold;
  await newSession(page);
  await expect.poll(() => host.writes().length).toBe(2);
  await expect(page).toHaveURL(/session=old/);
  expect(
    host.calls
      .filter((call) => call.session === "new2")
      .map((call) => call.type),
  ).toEqual(["create", "set_model"]);
  hold.release();
  await ready(page, "new2");
  await page.reload();
  await ready(page, "new2");
  expect(host.writes()).toHaveLength(2);
  await newSession(page);
  await ready(page, "new3");
  expect(host.writes().at(-1)?.body).toMatchObject({
    provider: "chosen",
    modelId: "special",
  });
  await page.goto("/?session=old");
  await ready(page);
  expect(host.writes()).toHaveLength(3);
});

test("Runtime and ProviderManager choices share the preference; clear affects only future sessions", async ({
  page,
  host,
}) => {
  await start(page);
  await runtime(page);
  await chooseMenuOption(page, "Model", { label: "Chosen model (chosen)" });
  await expect.poll(async () => (await stored(page)).length).toBe(1);
  await page
    .getByRole("button", {
      name: "Clear model preference (future native defaults)",
      exact: true,
    })
    .click();
  expect(await stored(page)).toEqual([]);
  expect(host.states.old.provider).toBe("chosen");
  await newSession(page);
  await ready(page, "new2");
  expect(host.writes()).toHaveLength(1);
  await runtime(page);
  await page
    .getByRole("button", { name: "Manage providers", exact: true })
    .click();
  await page
    .getByRole("button", { name: "Use model: chosen/special", exact: true })
    .click();
  await expect.poll(async () => (await stored(page)).length).toBe(1);
  await newSession(page);
  await ready(page, "new3");
  expect(host.states.new3.provider).toBe("chosen");
});

test("slash choice is explicit; failures are not remembered and projects are isolated", async ({
  page,
  host,
}) => {
  await start(page);
  host.failNext = true;
  await choose(page);
  await expect(page.locator(".error-banner")).toContainText(
    "Mock model unavailable",
  );
  expect(await stored(page)).toEqual([]);
  await page
    .getByRole("textbox", {
      name: "Send a prompt, or type / for commands…",
      exact: true,
    })
    .fill("/model chosen/special");
  await page
    .getByRole("textbox", {
      name: "Send a prompt, or type / for commands…",
      exact: true,
    })
    .press("Enter");
  await expect.poll(async () => (await stored(page)).length).toBe(1);
  await newSession(page, 2);
  await ready(page, "new2");
  expect(host.states.new2.provider).toBe("native");
  expect(host.writes()).toHaveLength(2);
  await newSession(page, 1);
  await ready(page, "new3");
  expect(host.states.new3.provider).toBe("chosen");
});

test("remembered apply failure keeps a reachable session and actionable notice, retains preference and precedes prompt", async ({
  page,
  host,
}) => {
  await start(page);
  await choose(page);
  await expect.poll(async () => (await stored(page)).length).toBe(1);
  host.failNext = true;
  await newSession(page);
  await ready(page, "new2");
  await expect(page).toHaveURL(/session=new2/);
  await expect(page.getByRole("alert")).toContainText(
    "Could not apply remembered model chosen/special",
  );
  await expect(
    page.getByRole("alert").getByRole("button", { name: "Runtime settings" }),
  ).toBeVisible();
  expect(host.states.new2.provider).toBe("native");
  expect(await stored(page)).toHaveLength(1);
  const input = page.getByRole("textbox", {
    name: "Send a prompt, or type / for commands…",
    exact: true,
  });
  await input.fill("Mock-only prompt after failed preference");
  await input.press("Enter");
  await expect
    .poll(() =>
      host.calls.some(
        (call) => call.session === "new2" && call.type === "prompt",
      ),
    )
    .toBe(true);
  const commands = host.calls
    .filter((call) => call.session === "new2")
    .map((call) => call.type);
  expect(commands.indexOf("set_model")).toBeLessThan(
    commands.indexOf("prompt"),
  );
});

test("navigation during explicit choice remembers the originating project without stale runtime UI", async ({
  page,
  host,
}) => {
  await start(page);
  const hold = gate();
  host.holdNext = hold;
  await choose(page);
  await expect.poll(() => host.writes().length).toBe(1);
  await newSession(page, 2);
  await ready(page, "new2");
  hold.release();
  await expect.poll(async () => (await stored(page)).length).toBe(1);
  await expect(
    page.getByRole("button", { name: "Choose model", exact: true }),
  ).toContainText("Native default");
  await newSession(page, 1);
  await ready(page, "new3");
  expect(host.states.new3.provider).toBe("chosen");
});

test("browser origin isolation and optional storage memory fallback", async ({
  page,
  context,
  host,
}) => {
  await start(page);
  await page.evaluate(() => {
    Storage.prototype.setItem = () => {
      throw new Error("Storage unavailable");
    };
    Storage.prototype.removeItem = () => {
      throw new Error("Storage unavailable");
    };
  });
  await choose(page);
  await expect.poll(() => host.states.old.provider).toBe("chosen");
  await newSession(page);
  await ready(page, "new2");
  expect(host.states.new2.provider).toBe("chosen");
  await runtime(page);
  await page
    .getByRole("button", {
      name: "Clear model preference (future native defaults)",
      exact: true,
    })
    .click();
  await newSession(page);
  await ready(page, "new3");
  expect(host.states.new3.provider).toBe("native");
  const otherOrigin = await context.newPage();
  await otherOrigin.route("**/*", async (route) =>
    route.fulfill({
      contentType: "text/html",
      body: "<p>Isolated mock origin</p>",
    }),
  );
  await otherOrigin.goto("http://model-preference.invalid");
  expect(await otherOrigin.evaluate(() => localStorage.length)).toBe(0);
  await otherOrigin.close();
});

test("cached preference reads honor changes and clears from another tab", async ({
  page,
  context,
  host,
}) => {
  await start(page);
  await choose(page);
  await expect.poll(async () => (await stored(page)).length).toBe(1);
  const storageKey = (await stored(page))[0][0];
  const secondTab = await context.newPage();
  // Load the same origin without mounting the application or contacting a host.
  await secondTab.route("**/*", (route) =>
    route.fulfill({
      contentType: "text/html",
      body: "<p>Preference storage peer</p>",
    }),
  );
  await secondTab.goto(new URL("/preference-peer", page.url()).href);
  await secondTab.evaluate(
    (key) =>
      localStorage.setItem(
        key,
        JSON.stringify({ provider: "native", modelId: "default" }),
      ),
    storageKey,
  );
  await newSession(page);
  await ready(page, "new2");
  expect(host.writes().at(-1)?.body).toMatchObject({
    provider: "native",
    modelId: "default",
  });
  await secondTab.evaluate((key) => localStorage.removeItem(key), storageKey);
  await newSession(page);
  await ready(page, "new3");
  expect(await stored(page)).toEqual([]);
  expect(host.writes()).toHaveLength(2);
  await secondTab.close();
});
