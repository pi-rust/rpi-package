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
const levels = ["off", "low", "high"];
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
/**
 * Mock host with per-session model/thinking state that SURVIVES only while the
 * mock host process lives. `resetHost()` simulates the bridge retiring the
 * native host: the next request reopens it from a transcript that carries no
 * model/thinking entries, so the session restarts on the startup defaults —
 * the exact drift the session runtime memory exists to repair.
 */
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
  resetHost() {
    for (const id of Object.keys(this.states)) {
      this.states[id] = {
        provider: "native",
        modelId: "default",
        thinkingLevel: "off",
      };
    }
    // The retired process is gone; its command history is gone with it.
    this.calls = [];
  }
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
          return json(envelope(type, { levels }));
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
  writes(session?: string) {
    return this.calls.filter(
      (call) =>
        (call.type === "set_model" || call.type === "set_thinking_level") &&
        (!session || call.session === session),
    );
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
    Object.entries(localStorage).filter(
      ([key]) =>
        key.startsWith("rpi-web-session-runtime:") ||
        key.startsWith("rpi-web-model-preference:"),
    ),
  );
}

async function chooseThinking(page: Page, level: string) {
  await runtime(page);
  await chooseMenuOption(page, "Thinking level", { label: level });
  await page.keyboard.press("Escape");
}

test("a historical session restores its last model and thinking after a host reopen", async ({
  page,
  host,
}) => {
  await start(page);
  // Round one: explicit choices land in the per-session memory.
  await choose(page);
  await expect.poll(() => host.states.old.modelId).toBe("special");
  await chooseThinking(page, "high");
  await expect.poll(() => host.states.old.thinkingLevel).toBe("high");
  await expect
    .poll(async () => (await stored(page)).length)
    .toBeGreaterThanOrEqual(2);

  // The bridge retires the host; the reopened child starts on defaults.
  host.resetHost();
  await page.reload();
  await ready(page);
  // The drift is repaired before the composer is usable: the reopened state
  // matches the remembered last round again.
  await expect.poll(() => host.states.old.modelId).toBe("special");
  await expect.poll(() => host.states.old.thinkingLevel).toBe("high");
  const repairs = host.writes("old").map((call) => call.type);
  expect(repairs).toContain("set_model");
  expect(repairs).toContain("set_thinking_level");
});

test("an unchanged session entry triggers no repair writes", async ({
  page,
  host,
}) => {
  await start(page);
  await choose(page);
  await expect.poll(() => host.states.old.modelId).toBe("special");
  const writesBefore = host.writes("old").length;
  await page.reload();
  await ready(page);
  await expect.poll(() => host.states.old.modelId).toBe("special");
  expect(host.writes("old").length).toBe(writesBefore);
});

test("a repair failure latches an actionable banner and keeps the memory", async ({
  page,
  host,
}) => {
  await start(page);
  await choose(page);
  await expect.poll(() => host.states.old.modelId).toBe("special");
  host.resetHost();
  host.failNext = true;
  await page.reload();
  await ready(page);
  // The failed model repair surfaces a banner naming the drifted choice.
  await expect(page.getByRole("alert")).toContainText("special");
  // The remembered choice is kept for the next entry.
  await expect
    .poll(async () => (await stored(page)).length)
    .toBeGreaterThanOrEqual(2);
  // A later entry retries the repair and succeeds.
  await page.reload();
  await ready(page);
  await expect.poll(() => host.states.old.modelId).toBe("special");
});

test("repair happens before the first prompt after a host reopen", async ({
  page,
  host,
}) => {
  await start(page);
  await chooseThinking(page, "high");
  await expect.poll(() => host.states.old.thinkingLevel).toBe("high");
  host.resetHost();
  const hold = gate();
  host.holdNext = hold;
  await page.reload();
  await expect.poll(() => host.writes("old").length).toBeGreaterThan(0);
  hold.release();
  await ready(page);
  const input = page.getByRole("textbox", {
    name: "Send a prompt, or type / for commands…",
    exact: true,
  });
  await input.fill("Prompt after reopen");
  await input.press("Enter");
  await expect
    .poll(() =>
      host.calls.some(
        (call) => call.session === "old" && call.type === "prompt",
      ),
    )
    .toBe(true);
  const commands = host.calls
    .filter((call) => call.session === "old")
    .map((call) => call.type);
  expect(commands.indexOf("set_thinking_level")).toBeLessThan(
    commands.indexOf("prompt"),
  );
});

test("new sessions use the web-set default; history sessions keep their own last round", async ({
  page,
  host,
}) => {
  await start(page);
  // Round one in `old`: explicit choices update both the per-session memory
  // and the per-project default preference.
  await choose(page);
  await expect.poll(() => host.states.old.modelId).toBe("special");
  await chooseThinking(page, "high");
  await expect.poll(() => host.states.old.thinkingLevel).toBe("high");

  // A new session starts on the web-set default (the project preference).
  await newSession(page);
  await ready(page, "new2");
  expect(host.states.new2.modelId).toBe("special");
  expect(host.states.new2.thinkingLevel).toBe("high");

  // Round two in `new2`: switch to different model/thinking.
  await chooseMenuOption(page, "Choose model", { label: "Native default" });
  await expect.poll(() => host.states.new2.modelId).toBe("default");
  await chooseThinking(page, "off");
  await expect.poll(() => host.states.new2.thinkingLevel).toBe("off");

  // Yet another new session starts on the updated web-set default.
  await newSession(page);
  await ready(page, "new3");
  expect(host.states.new3.modelId).toBe("default");
  expect(host.states.new3.thinkingLevel).toBe("off");

  // Back to the historical session: it follows its OWN last round
  // (special + high), not another session's choices — and because the mock
  // host never retired, no repair writes are needed.
  await page.goto("/?session=old");
  await ready(page);
  expect(host.states.old.modelId).toBe("special");
  expect(host.states.old.thinkingLevel).toBe("high");
  const oldWrites = host.writes("old").map((call) => call.type);
  expect(oldWrites).toEqual(["set_model", "set_thinking_level"]);
});

test("session memory persists across reload and repairs model-only drift", async ({
  page,
  host,
}) => {
  await start(page);
  await choose(page);
  await expect.poll(() => host.states.old.modelId).toBe("special");
  // Only the model was chosen; thinking stays at the host default.
  host.resetHost();
  await page.reload();
  await ready(page);
  await expect.poll(() => host.states.old.modelId).toBe("special");
  expect(host.states.old.thinkingLevel).toBe("off");
  const repairs = host.writes("old").map((call) => call.type);
  expect(repairs).toEqual(["set_model"]);
});
