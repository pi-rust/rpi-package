import { test as base, expect, type Page } from "@playwright/test";
import { chooseMenuOption, clickRowAction, openRuntimePanel } from "./controls";

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
    old: { provider: "native", modelId: "default", thinkingLevel: "off" },
    other: { provider: "native", modelId: "default", thinkingLevel: "off" },
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
            "get_available_thinking_levels",
            "set_thinking_level",
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
      // The runtime panel embeds the read-only resource catalog; both scopes
      // are requested through the same project.
      if (path.match(/^\/projects\/[^/]+\/resources$/))
        return json({
          version: 1,
          success: true,
          operation: "list",
          scope: new URL(req.url()).searchParams.get("scope"),
          kind: null,
          name: null,
          data: { outcome: "observed", resources: [], diagnostics: [] },
        });
      if (path === "/sessions") {
        if (req.method() === "GET")
          return json({
            sessions: Object.keys(this.states).map((id) => this.summary(id)),
          });
        if (req.method() === "POST") {
          const id = `new${Object.keys(this.states).length}`;
          this.projects[id] = req.postDataJSON().projectId;
          this.states[id] = {
            provider: "native",
            modelId: "default",
            thinkingLevel: "off",
          };
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
        if (type === "set_thinking_level") {
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
              error: "Mock thinking level unavailable",
            });
          this.states[session] = {
            ...this.states[session],
            thinkingLevel: body.level,
          };
          return json(envelope(type, {}));
        }
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
            ...this.states[session],
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
            }),
          );
        if (type === "get_available_models")
          return json(envelope(type, { models }));
        if (type === "get_available_thinking_levels")
          return json(envelope(type, { levels: ["off", "low", "high"] }));
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
  thinkingWrites() {
    return this.calls.filter((call) => call.type === "set_thinking_level");
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
  await openRuntimePanel(page);
  await expect(page.locator(".inspector")).toBeVisible();
}
async function choose(page: Page) {
  await chooseMenuOption(page, "Choose model", { label: "Chosen model" });
}
async function newSession(page: Page, project = 1) {
  await clickRowAction(page, `New session in: Project ${project}`);
}
async function stored(page: Page) {
  return page.evaluate(() =>
    Object.entries(localStorage).filter(([key]) =>
      key.startsWith("rpi-web-model-preference:"),
    ),
  );
}

async function chooseThinking(page: Page, level: string) {
  await runtime(page);
  await chooseMenuOption(page, "Thinking level", { label: level });
  await page.keyboard.press("Escape");
}

test("explicit thinking choice is remembered and applied to new sessions only", async ({
  page,
  host,
}) => {
  await start(page);
  expect(host.thinkingWrites()).toHaveLength(0);
  expect(await stored(page)).toEqual([]);
  await chooseThinking(page, "high");
  await expect.poll(() => host.states.old.thinkingLevel).toBe("high");
  await expect.poll(async () => (await stored(page)).length).toBe(1);
  await newSession(page);
  await ready(page, "new2");
  expect(host.states.new2.thinkingLevel).toBe("high");
  await page.goto("/?session=old");
  await ready(page);
  expect(host.states.old.thinkingLevel).toBe("high");
});

test("failed or invalid thinking choices are not remembered", async ({
  page,
  host,
}) => {
  await start(page);
  host.failNext = true;
  await chooseThinking(page, "low");
  await expect(page.locator(".error-banner")).toContainText(
    "Mock thinking level unavailable",
  );
  expect(await stored(page)).toEqual([]);
  await page
    .getByRole("textbox", {
      name: "Send a prompt, or type / for commands…",
      exact: true,
    })
    .fill("/thinking high");
  await page
    .getByRole("textbox", {
      name: "Send a prompt, or type / for commands…",
      exact: true,
    })
    .press("Enter");
  await expect.poll(() => host.states.old.thinkingLevel).toBe("high");
  await expect.poll(async () => (await stored(page)).length).toBe(1);
  await newSession(page, 2);
  await ready(page, "new2");
  expect(host.states.new2.thinkingLevel).toBe("off");
  await newSession(page, 1);
  await ready(page, "new3");
  expect(host.states.new3.thinkingLevel).toBe("high");
});

test("thinking preference survives a remembered apply failure and is clearable", async ({
  page,
  host,
}) => {
  await start(page);
  await chooseThinking(page, "low");
  await expect.poll(async () => (await stored(page)).length).toBe(1);
  host.failNext = true;
  await newSession(page);
  await ready(page, "new2");
  await expect(page.getByRole("alert")).toContainText(
    "Could not apply remembered thinking level low",
  );
  expect(host.states.new2.thinkingLevel).toBe("off");
  expect(await stored(page)).toHaveLength(1);
  await runtime(page);
  await page
    .getByRole("button", {
      name: "Clear thinking preference (future default off)",
      exact: true,
    })
    .click();
  expect(await stored(page)).toEqual([]);
  await newSession(page);
  await ready(page, "new3");
  expect(host.states.new3.thinkingLevel).toBe("off");
});

test("remembered thinking applies before prompt and survives reload and storage failure", async ({
  page,
  host,
}) => {
  await start(page);
  await chooseThinking(page, "high");
  await expect.poll(async () => (await stored(page)).length).toBe(1);
  await page.reload();
  await ready(page);
  const hold = gate();
  host.holdNext = hold;
  await newSession(page);
  await expect.poll(() => host.thinkingWrites().length).toBe(2);
  expect(
    host.calls
      .filter((call) => call.session === "new2")
      .map((call) => call.type),
  ).toEqual(["create", "set_thinking_level"]);
  hold.release();
  await ready(page, "new2");
  const input = page.getByRole("textbox", {
    name: "Send a prompt, or type / for commands…",
    exact: true,
  });
  await input.fill("Prompt after preference");
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
  expect(commands.indexOf("set_thinking_level")).toBeLessThan(
    commands.indexOf("prompt"),
  );
});

test("thinking preference survives unavailable storage and tracks another tab", async ({
  page,
  context,
  host,
}) => {
  await start(page);
  await chooseThinking(page, "high");
  await expect.poll(async () => (await stored(page)).length).toBe(1);
  const key = (await stored(page))[0][0];
  const peer = await context.newPage();
  await peer.route("**/*", (route) =>
    route.fulfill({
      contentType: "text/html",
      body: "<p>Thinking preference storage peer</p>",
    }),
  );
  await peer.goto(new URL("/preference-peer", page.url()).href);
  await peer.evaluate(
    (storageKey) =>
      localStorage.setItem(
        storageKey,
        JSON.stringify({ thinkingLevel: "low" }),
      ),
    key,
  );
  await newSession(page);
  await ready(page, "new2");
  expect(host.states.new2.thinkingLevel).toBe("low");
  await peer.evaluate((storageKey) => localStorage.removeItem(storageKey), key);
  await newSession(page);
  await ready(page, "new3");
  expect(host.states.new3.thinkingLevel).toBe("off");

  await page.evaluate(() => {
    Storage.prototype.setItem = () => {
      throw new Error("Storage unavailable");
    };
    Storage.prototype.removeItem = () => {
      throw new Error("Storage unavailable");
    };
  });
  await page.goto("/?session=old");
  await ready(page);
  await chooseThinking(page, "high");
  await newSession(page);
  await ready(page, "new4");
  expect(host.states.new4.thinkingLevel).toBe("high");
  await peer.close();
});
