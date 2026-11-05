import { test as base, expect, type Page, type Route } from "@playwright/test";

// This suite owns every /api/** request. It never falls through to a host,
// reads credentials/configuration, or submits a prompt. Mutations require a
// per-test permit; unknown requests (including unknown mutations) fail teardown.
type Json = Record<string, unknown>;
type Reply = { json: unknown; status?: number };
type Call = { session: string; type: string; body: Json };
const A = "providers-a";
const B = "providers-b";
const titles = { [A]: "Provider Alpha session", [B]: "Provider Beta session" };
const labels = {
  en: {
    runtime: "Runtime settings",
    manage: "Manage providers",
    manager: "Model providers",
    back: "Back to runtime settings",
    refresh: "Refresh providers",
    add: "Add connection",
    connection: "Connection settings",
    provider: "Provider ID",
    key: "API Key",
    base: "Custom Base URL · local draft",
    save: "Save API Key",
    clear: "Clear local draft",
    close: "Close dialog",
    panel: "Close panel",
    sidebar: "Toggle sidebar",
    use: "Use model: ",
  },
  zh: {
    runtime: "运行时设置",
    manage: "管理供应商",
    manager: "模型供应商",
    back: "返回运行时设置",
    refresh: "刷新供应商",
    add: "添加连接",
    connection: "连接设置",
    provider: "Provider 标识",
    key: "API Key",
    base: "自定义 Base URL · 本地草稿",
    save: "保存 API Key",
    clear: "清除本地草稿",
    close: "关闭对话框",
    panel: "关闭面板",
    sidebar: "切换侧栏",
    use: "使用模型：",
  },
} as const;
type Language = keyof typeof labels;
const models = [
  {
    provider: "alpha",
    id: "shared",
    name: "Shared model",
    contextWindow: 1000,
  },
  { provider: "beta", id: "shared", name: "Shared model", contextWindow: 8000 },
];
const auth = (provider: string) => ({
  provider,
  ready: true,
  source: "environment",
  oauth: false,
  oauthAvailable: false,
});
function envelope(command: string, data: unknown) {
  return { type: "response", command, success: true, data };
}
function latch() {
  let release!: () => void;
  const promise = new Promise<void>((resolve) => {
    release = resolve;
  });
  return { promise, release };
}
const manager = (page: Page, language: Language = "en") =>
  page
    .locator("section.provider-manager")
    .and(
      page.getByRole("region", { name: labels[language].manager, exact: true }),
    );
const dialog = (page: Page, language: Language = "en") =>
  page.getByRole("dialog", { name: labels[language].connection, exact: true });
const card = (page: Page, provider: string, language: Language = "en") =>
  manager(page, language).getByRole("group", { name: provider, exact: true });
const modelButton = (page: Page, provider: string, language: Language = "en") =>
  card(page, provider, language).getByRole("button", {
    name: `${labels[language].use}${provider}/shared`,
    exact: true,
  });

class ProviderHost {
  calls: Call[] = [];
  unexpected: string[] = [];
  models = models.map((model) => ({ ...model }));
  providers = [auth("alpha"), auth("auth-only")];
  commands = [
    "get_state",
    "get_messages",
    "get_available_models",
    "get_auth_status",
    "set_model",
    "set_api_key",
    "set_provider_config",
    "get_provider_models",
    "test_provider_config",
  ];
  actions = ["projects", "provider_auth"];
  states: Record<string, Json> = {
    [A]: {
      sessionId: A,
      provider: "alpha",
      modelId: "shared",
      thinkingLevel: "off",
    },
    [B]: {
      sessionId: B,
      provider: "beta",
      modelId: "shared",
      thinkingLevel: "off",
    },
  };
  private permits = new Map<string, number>();
  private holds: Array<{
    session: string;
    type: string;
    arrived: ReturnType<typeof latch>;
    gate: ReturnType<typeof latch>;
    finished: ReturnType<typeof latch>;
    reply: Reply;
    claimed: boolean;
  }> = [];
  private streams: Array<ReturnType<typeof latch>> = [];
  private closing = false;
  constructor(readonly page: Page) {}
  count(type: string, session?: string) {
    return this.calls.filter(
      (call) => call.type === type && (!session || call.session === session),
    ).length;
  }
  get writes() {
    return this.calls.filter((call) =>
      ["set_model", "set_api_key", "set_provider_config"].includes(
        call.type,
      ),
    );
  }
  permit(
    type: "set_model" | "set_api_key" | "set_provider_config",
    session = A,
  ) {
    const key = `${session}:${type}`;
    this.permits.set(key, (this.permits.get(key) ?? 0) + 1);
  }
  hold(type: string, reply: Reply, session = A) {
    const ticket = {
      session,
      type,
      reply,
      arrived: latch(),
      gate: latch(),
      finished: latch(),
      claimed: false,
    };
    this.holds.push(ticket);
    return {
      arrived: ticket.arrived.promise,
      release: ticket.gate.release,
      finished: ticket.finished.promise,
    };
  }
  session(id: string) {
    return {
      sessionId: id,
      projectId: "provider-project",
      title: titles[id as keyof typeof titles],
      status: "ready",
      createdAt: 1_700_000_000_000,
      updatedAt: 1_700_000_000_100,
      lastSeq: 0,
      error: null,
      latestSettlement: null,
      pinned: false,
      archived: false,
    };
  }
  async install() {
    await this.page.route("**/api/**", async (route: Route) => {
      const request = route.request();
      const path = new URL(request.url()).pathname;
      const method = request.method();
      if (method === "GET") {
        if (path === "/api/v1/auth")
          return route.fulfill({
            json: { required: false, authenticated: true },
          });
        if (path === "/api/v1/capabilities")
          return route.fulfill({
            json: {
              protocol: "rpi",
              piCompatibilityTarget: "1.2.0",
              supportedCommands: this.commands,
              supportedActions: this.actions,
              parityComplete: false,
            },
          });
        if (path === "/api/v1/projects")
          return route.fulfill({
            json: {
              projects: [
                {
                  id: "provider-project",
                  name: "Provider fixture",
                  path: "/mock/providers",
                  archived: false,
                },
              ],
              defaultProjectId: "provider-project",
              projectsRoot: "/mock",
            },
          });
        if (path === "/api/v1/sessions")
          return route.fulfill({
            json: { sessions: [this.session(A), this.session(B)] },
          });
      }
      const match = path.match(
        /^\/api\/v1\/sessions\/([^/]+)\/(open|snapshot|events|commands|run-timings\/query)$/,
      );
      if (!match || ![A, B].includes(match[1]))
        return this.reject(route, `${method} ${path}`);
      const session = match[1];
      const endpoint = match[2];
      const expectedMethod = ["snapshot", "events"].includes(endpoint)
        ? "GET"
        : "POST";
      if (method !== expectedMethod)
        return this.reject(route, `${method} ${path}`);
      const body =
        endpoint === "commands" ? (request.postDataJSON() as Json) : {};
      const type = endpoint === "commands" ? String(body.type) : endpoint;
      const reads = [
        "get_state",
        "get_messages",
        "get_available_models",
        "get_auth_status",
        "get_provider_models",
        "test_provider_config",
      ];
      if (endpoint === "commands" && !reads.includes(type)) {
        const key = `${session}:${type}`;
        const permitted = this.permits.get(key) ?? 0;
        if (
          !["set_model", "set_api_key", "set_provider_config"].includes(type) ||
          permitted === 0
        )
          return this.reject(route, `unpermitted mutation ${session}:${type}`);
        this.permits.set(key, permitted - 1);
      }
      this.calls.push({ session, type, body });
      const ticket = this.holds.find(
        (hold) =>
          !hold.claimed && hold.session === session && hold.type === type,
      );
      if (ticket) {
        ticket.claimed = true;
        ticket.arrived.release();
      }
      try {
        if (ticket) {
          await ticket.gate.promise;
          await route.fulfill(ticket.reply);
        } else if (type === "open") {
          await route.fulfill({ json: this.session(session) });
        } else if (type === "snapshot") {
          await route.fulfill({
            json: {
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
            },
          });
        } else if (type === "events") {
          const stream = latch();
          this.streams.push(stream);
          if (this.closing) stream.release();
          await stream.promise;
          await route.fulfill({
            contentType: "text/event-stream",
            body: "retry: 60000\n\n",
          });
        } else if (type === "run-timings/query") {
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
          if (type === "set_model")
            this.states[session] = {
              ...this.states[session],
              provider: body.provider,
              modelId: body.modelId,
            };
          const data: Record<string, unknown> = {
            get_state: this.states[session],
            get_messages: { messages: [], total: 0, hasMore: false },
            get_available_models: { models: this.models },
            get_auth_status: { providers: this.providers },
            set_model: {},
            set_api_key: {
              provider: body.provider,
              ready: true,
              requiresRestart: true,
            },
            set_provider_config: {
              provider: body.provider,
              modelId: body.modelId,
              api: body.api,
              requiresRestart: true,
            },
            get_provider_models: {
              models: [
                { id: "model-a", ownedBy: "fixture", contextWindow: 128000 },
                { id: "model-b", ownedBy: "fixture", contextWindow: 256000 },
              ],
            },
            test_provider_config: {
              ok: true,
              status: 200,
              latencyMs: 23,
              modelCount: 2,
            },
          };
          await route.fulfill({ json: envelope(type, data[type]) });
        }
      } catch (error) {
        // Aborted fetches are expected when an owner is unmounted or navigated.
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
    for (const hold of this.holds) hold.gate.release();
    for (const stream of this.streams) stream.release();
    await this.page.unrouteAll({ behavior: "wait" });
  }
}
const test = base.extend<{ host: ProviderHost }>({
  host: [
    async ({ page }, use) => {
      const host = new ProviderHost(page);
      await host.install();
      try {
        await use(host);
      } finally {
        await host.dispose();
        expect(
          host.unexpected,
          "all API requests must be explicitly mocked and all writes explicitly permitted",
        ).toEqual([]);
      }
    },
    { auto: true },
  ],
});

async function rendered(page: Page) {
  await page.evaluate(
    () =>
      new Promise<void>((resolve) =>
        requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
      ),
  );
}
async function start(page: Page, language: Language = "en") {
  // Flush the scheduled bootstrap diagnostics before any per-operation hold is
  // armed, so a background auth read cannot consume a manager refresh ticket.
  await page.clock.install();
  await page.addInitScript(
    (value) => localStorage.setItem("rpi-web-language", value),
    language,
  );
  const warmup = page.waitForResponse(
    (response) =>
      response.url().includes(`/sessions/${A}/commands`) &&
      response.request().postDataJSON().type === "get_available_models",
  );
  await page.goto(`/?session=${A}`);
  await (await warmup).finished();
  await rendered(page);
  await page.clock.runFor(500);
  await rendered(page);
  // The composer picker deliberately does not render without set_model support.
  await expect(page.locator(".toolbar .breadcrumb h1")).toHaveText(titles[A]);
}
async function openRuntime(page: Page, language: Language = "en") {
  const entry = page.getByRole("button", {
    name: labels[language].runtime,
    exact: true,
  });
  if (!(await entry.isVisible()))
    await page
      .getByRole("button", { name: labels[language].sidebar, exact: true })
      .click();
  await entry.click();
  await expect(page.locator(".inspector")).toBeVisible();
}
async function openManager(page: Page, language: Language = "en") {
  await openRuntime(page, language);
  await page
    .locator(".inspector")
    .getByRole("button", { name: labels[language].manage, exact: true })
    .click();
  await expect(manager(page, language)).toBeVisible();
}
async function connection(
  page: Page,
  provider: string,
  language: Language = "en",
) {
  await card(page, provider, language)
    .getByRole("button", {
      name: `${labels[language].connection}${language === "zh" ? "：" : ": "}${provider}`,
      exact: true,
    })
    .click();
  await expect(dialog(page, language)).toBeVisible();
  await expect(
    dialog(page, language).getByLabel(labels[language].provider, {
      exact: true,
    }),
  ).toHaveValue(provider);
}
async function selectSession(page: Page, session: typeof A | typeof B) {
  const entry = page.getByRole("button", {
    name: new RegExp(`^${titles[session]}`),
  });
  if (!(await entry.isVisible()))
    await page
      .getByRole("button", { name: "Toggle sidebar", exact: true })
      .click();
  await entry.click();
  await expect(page).toHaveURL(new RegExp(`session=${session}`));
}
async function noSecretEcho(page: Page, ...secrets: string[]) {
  const publicSurface = await page.evaluate(() => ({
    text: document.body.textContent ?? "",
    attributes: [...document.querySelectorAll("*")].flatMap((element) =>
      [...element.attributes]
        .filter((attribute) => attribute.name !== "value")
        .map((attribute) => attribute.value),
    ),
    unmaskedInputs: [
      ...document.querySelectorAll<HTMLInputElement>(
        "input:not([type=password]), textarea",
      ),
    ].map((input) => input.value),
    storage: [
      ...Object.entries(localStorage),
      ...Object.entries(sessionStorage),
    ],
  }));
  for (const secret of secrets)
    expect(JSON.stringify(publicSurface)).not.toContain(secret);
}

for (const language of ["en", "zh"] as const) {
  for (const width of [375, 390, 1440]) {
    test(`provider manager entry/back is localized and bounded: ${language}/${width}`, async ({
      page,
      host,
    }) => {
      await page.setViewportSize({ width, height: 900 });
      await start(page, language);
      await openManager(page, language);
      const section = manager(page, language);
      for (const name of [
        labels[language].back,
        labels[language].refresh,
        labels[language].add,
      ])
        await expect(
          section.getByRole("button", { name, exact: true }),
        ).toBeVisible();
      await expect(card(page, "alpha", language)).toBeVisible();
      await expect(card(page, "beta", language)).toBeVisible();
      await expect(card(page, "auth-only", language)).toBeVisible();
      await expect(modelButton(page, "alpha", language)).toBeDisabled();
      await expect(modelButton(page, "beta", language)).toBeEnabled();
      const box = await section.boundingBox();
      expect(box).not.toBeNull();
      expect(box!.x).toBeGreaterThanOrEqual(0);
      expect(box!.x + box!.width).toBeLessThanOrEqual(width + 1);
      expect(
        await page.evaluate(() => document.documentElement.scrollWidth),
      ).toBeLessThanOrEqual(width + 1);
      await connection(page, "alpha", language);
      await expect(
        dialog(page, language).getByLabel(labels[language].key, {
          exact: true,
        }),
      ).toHaveValue("");
      await dialog(page, language)
        .getByRole("button", { name: labels[language].close, exact: true })
        .click();
      await section
        .getByRole("button", { name: labels[language].back, exact: true })
        .click();
      await expect(section).toHaveCount(0);
      await expect(
        page
          .locator(".inspector")
          .getByRole("button", { name: labels[language].manage, exact: true }),
      ).toBeVisible();
      expect(host.writes).toEqual([]);
    });
  }
}

test("discovery is the deduplicated union of auth providers and model providers", async ({
  page,
  host,
}) => {
  host.providers = [auth("alpha"), auth("alpha"), auth("auth-only")];
  await start(page);
  await openManager(page);
  await expect(manager(page).getByRole("group")).toHaveCount(3);
  await expect(
    card(page, "auth-only").getByRole("button", { name: /^Use model:/ }),
  ).toHaveCount(0);
  await expect(card(page, "beta")).toBeVisible();
  expect(host.writes).toEqual([]);
});

test("empty catalogs explain the empty state and Add connection opens a blank draft without writes", async ({
  page,
  host,
}) => {
  host.models = [];
  host.providers = [];
  await start(page);
  await openManager(page);
  await expect(manager(page).getByRole("group")).toHaveCount(0);
  await expect(manager(page)).toContainText(
    /No (model )?providers|No providers discovered/i,
  );
  await manager(page)
    .getByRole("button", { name: "Add connection", exact: true })
    .click();
  const modal = dialog(page);
  await expect(modal.getByLabel("Provider ID", { exact: true })).toHaveValue(
    "",
  );
  await expect(modal.getByLabel("API Key", { exact: true })).toHaveValue("");
  await expect(
    modal.getByRole("button", { name: "Save provider", exact: true }),
  ).toBeDisabled();
  expect(host.writes).toEqual([]);
});

test("refresh exposes held loading, recovers from an error, and performs reads only", async ({
  page,
  host,
}) => {
  await start(page);
  await openManager(page);
  await expect(card(page, "auth-only")).toBeVisible();
  const held = host.hold("get_auth_status", {
    status: 503,
    json: { error: "provider-discovery-offline" },
  });
  await manager(page)
    .getByRole("button", { name: "Refresh providers", exact: true })
    .click();
  await held.arrived;
  await expect(manager(page).getByRole("status")).toHaveText(
    "Reading providers…",
  );
  await expect(
    manager(page).getByRole("button", {
      name: "Refresh providers",
      exact: true,
    }),
  ).toBeDisabled();
  held.release();
  await held.finished;
  await expect(manager(page).getByRole("alert")).toHaveText(
    "Could not read credential metadata.",
  );
  await manager(page)
    .getByRole("button", { name: "Refresh providers", exact: true })
    .click();
  await expect(card(page, "auth-only")).toBeVisible();
  await expect(manager(page).getByRole("alert")).toHaveCount(0);
  expect(host.writes).toEqual([]);
});

test("same-name model selection sends the exact provider/id and then reads authoritative state", async ({
  page,
  host,
}) => {
  await start(page);
  await openManager(page);
  await expect(modelButton(page, "alpha")).toBeDisabled();
  host.permit("set_model");
  const write = host.hold("set_model", { json: envelope("set_model", {}) });
  await modelButton(page, "beta").click();
  await write.arrived;
  await expect(modelButton(page, "beta")).toBeDisabled();
  expect(host.writes.map((call) => call.body)).toEqual([
    { type: "set_model", provider: "beta", modelId: "shared" },
  ]);
  const before = host.count("get_state", A);
  host.states[A] = { ...host.states[A], provider: "beta", modelId: "shared" };
  write.release();
  await write.finished;
  await expect.poll(() => host.count("get_state", A)).toBeGreaterThan(before);
  await expect(modelButton(page, "beta")).toBeDisabled();
  await expect(modelButton(page, "alpha")).toBeEnabled();
  const order = host.calls
    .filter((call) => call.session === A)
    .map((call) => call.type);
  expect(order.lastIndexOf("get_state")).toBeGreaterThan(
    order.indexOf("set_model"),
  );
});

test("adding a provider collects protocol and model and submits the full configuration", async ({
  page,
  host,
}) => {
  await start(page);
  await openManager(page);
  await manager(page)
    .getByRole("button", { name: "Add connection", exact: true })
    .click();
  const modal = dialog(page);
  await modal.getByLabel("Provider ID", { exact: true }).fill("gateway");
  await modal.getByLabel("Protocol", { exact: true }).selectOption(
    "openai-responses",
  );
  await modal
    .getByLabel("Base URL", { exact: true })
    .fill("https://api.example.test/v1");
  await modal.getByLabel("Model ID", { exact: true }).fill("model-a");
  await modal.getByLabel("Model name (optional)", { exact: true }).fill("Model A");
  await modal.getByLabel("API Key", { exact: true }).fill("fixture-provider-key");
  await modal.getByRole("button", { name: "Fetch models", exact: true }).click();
  await expect(modal.getByLabel("Available models", { exact: true })).toHaveValue(
    "model-a",
  );
  await modal.getByLabel("Available models", { exact: true }).selectOption("model-b");
  await expect(modal.getByLabel("Model ID", { exact: true })).toHaveValue("model-b");
  await expect(
    modal.getByLabel("Context window (tokens, optional)", { exact: true }),
  ).toHaveValue("256000");
  await modal.getByRole("button", { name: "Test connection", exact: true }).click();
  await expect(modal).toContainText("23 ms");
  expect(host.count("get_provider_models")).toBe(1);
  expect(host.count("test_provider_config")).toBe(1);
  host.permit("set_provider_config");
  const write = host.hold("set_provider_config", {
    json: envelope("set_provider_config", {
      provider: "gateway",
      modelId: "model-a",
      api: "openai-responses",
      requiresRestart: true,
    }),
  });
  await modal
    .getByRole("button", { name: "Save provider", exact: true })
    .click();
  await write.arrived;
  expect(host.writes.map((call) => call.body)).toEqual([
    {
      type: "set_provider_config",
      provider: "gateway",
      api: "openai-responses",
      baseUrl: "https://api.example.test/v1",
      apiKey: "fixture-provider-key",
      modelId: "model-b",
      modelName: "Model A",
      contextWindow: 256000,
    },
  ]);
  write.release();
  await write.finished;
  await expect(modal).toContainText("models.json");
  await expect(modal.getByLabel("API Key", { exact: true })).toHaveValue("");
  await noSecretEcho(page, "fixture-provider-key");
});

test("API key is password-only, saved only on explicit click, cleared, and restart is explained without echo", async ({
  page,
  host,
}) => {
  await start(page);
  await openManager(page);
  await connection(page, "alpha");
  const modal = dialog(page);
  const key = modal.getByLabel("API Key", { exact: true });
  await expect(key).toHaveAttribute("type", "password");
  await key.fill("fixture-api-key-DO-NOT-RENDER");
  await key.blur();
  await rendered(page);
  expect(host.writes).toEqual([]);
  host.permit("set_api_key");
  const write = host.hold("set_api_key", {
    json: envelope("set_api_key", {
      provider: "alpha",
      ready: true,
      requiresRestart: true,
      apiKey: "fixture-api-key-DO-NOT-RENDER",
      secret: "server-secret-echo-DO-NOT-RENDER",
    }),
  });
  await modal
    .getByRole("button", { name: "Save API Key", exact: true })
    .click();
  await write.arrived;
  await expect(
    modal.getByRole("button", { name: "Save API Key", exact: true }),
  ).toBeDisabled();
  expect(host.writes.map((call) => call.body)).toEqual([
    {
      type: "set_api_key",
      provider: "alpha",
      apiKey: "fixture-api-key-DO-NOT-RENDER",
    },
  ]);
  write.release();
  await write.finished;
  await expect(key).toHaveValue("");
  await expect(modal).toContainText(/restart/i);
  await noSecretEcho(
    page,
    "fixture-api-key-DO-NOT-RENDER",
    "server-secret-echo-DO-NOT-RENDER",
  );
});

test("switching providers clears the secret and an older key response cannot clear a new provider draft", async ({
  page,
  host,
}) => {
  await start(page);
  await openManager(page);
  await connection(page, "alpha");
  const modal = dialog(page);
  const key = modal.getByLabel("API Key", { exact: true });
  await key.fill("old-alpha-key");
  host.permit("set_api_key");
  const write = host.hold("set_api_key", {
    json: envelope("set_api_key", {
      provider: "alpha",
      ready: true,
      requiresRestart: true,
    }),
  });
  await modal
    .getByRole("button", { name: "Save API Key", exact: true })
    .click();
  await write.arrived;
  await modal.getByLabel("Provider ID", { exact: true }).fill("beta");
  await expect(key).toHaveValue("");
  await key.fill("new-beta-unsaved-key");
  write.release();
  await write.finished;
  await rendered(page);
  await expect(modal.getByLabel("Provider ID", { exact: true })).toHaveValue(
    "beta",
  );
  await expect(key).toHaveValue("new-beta-unsaved-key");
  await expect(modal).not.toContainText(/restart/i);
  expect(host.count("set_api_key")).toBe(1);
  await noSecretEcho(page, "old-alpha-key", "new-beta-unsaved-key");
});

test("closing and reopening a dialog clears secrets and invalidates its older save owner", async ({
  page,
  host,
}) => {
  await start(page);
  await openManager(page);
  await connection(page, "alpha");
  await dialog(page)
    .getByLabel("API Key", { exact: true })
    .fill("closed-owner-key");
  host.permit("set_api_key");
  const write = host.hold("set_api_key", {
    json: envelope("set_api_key", {
      provider: "alpha",
      ready: true,
      requiresRestart: true,
    }),
  });
  await dialog(page)
    .getByRole("button", { name: "Save API Key", exact: true })
    .click();
  await write.arrived;
  await dialog(page)
    .getByRole("button", { name: "Close dialog", exact: true })
    .click();
  await connection(page, "alpha");
  await expect(dialog(page).getByLabel("API Key", { exact: true })).toHaveValue(
    "",
  );
  await dialog(page)
    .getByLabel("API Key", { exact: true })
    .fill("reopened-owner-draft");
  write.release();
  await write.finished;
  await rendered(page);
  await expect(dialog(page).getByLabel("API Key", { exact: true })).toHaveValue(
    "reopened-owner-draft",
  );
  await expect(dialog(page)).not.toContainText(/restart/i);
  await noSecretEcho(page, "closed-owner-key", "reopened-owner-draft");
});

test("unsaved provider changes and close discard secrets without any write", async ({
  page,
  host,
}) => {
  await start(page);
  await openManager(page);
  await connection(page, "alpha");
  const modal = dialog(page);
  const key = modal.getByLabel("API Key", { exact: true });
  await key.fill("unsaved-alpha-secret");
  await modal.getByLabel("Provider ID", { exact: true }).fill("beta");
  await expect(key).toHaveValue("");
  await key.fill("unsaved-beta-secret");
  await modal
    .getByRole("button", { name: "Close dialog", exact: true })
    .click();
  await connection(page, "beta");
  await expect(dialog(page).getByLabel("API Key", { exact: true })).toHaveValue(
    "",
  );
  await noSecretEcho(page, "unsaved-alpha-secret", "unsaved-beta-secret");
  expect(host.writes).toEqual([]);
});

test("a failed key response cannot echo a secret and does not retain or automatically retry it", async ({
  page,
  host,
}) => {
  await start(page);
  await openManager(page);
  await connection(page, "alpha");
  const modal = dialog(page);
  const key = modal.getByLabel("API Key", { exact: true });
  await key.fill("failed-key-secret-DO-NOT-RENDER");
  host.permit("set_api_key");
  const write = host.hold("set_api_key", {
    json: {
      type: "response",
      command: "set_api_key",
      success: false,
      error:
        "rejected failed-key-secret-DO-NOT-RENDER server-error-echo-DO-NOT-RENDER",
    },
  });
  await modal
    .getByRole("button", { name: "Save API Key", exact: true })
    .click();
  await write.arrived;
  await expect(key).toHaveValue("");
  write.release();
  await write.finished;
  await expect(modal.getByRole("alert")).toHaveText(
    "API Key submission failed. Check the node connection and permissions, then enter the key again.",
  );
  await expect(key).toHaveValue("");
  await expect(
    modal.getByRole("button", { name: "Save API Key", exact: true }),
  ).toBeDisabled();
  await noSecretEcho(
    page,
    "failed-key-secret-DO-NOT-RENDER",
    "server-error-echo-DO-NOT-RENDER",
  );
  await rendered(page);
  expect(host.count("set_api_key")).toBe(1);
});

for (const missing of [
  "action",
  "auth-read",
  "auth-write",
  "auth-commands",
  "model-write",
  "state-read",
] as const) {
  test(`missing ${missing} capability keeps the manager reachable without unsupported commands`, async ({
    page,
    host,
  }) => {
    if (missing === "action")
      host.actions = host.actions.filter(
        (action) => action !== "provider_auth",
      );
    if (missing === "auth-read")
      host.commands = host.commands.filter(
        (command) => command !== "get_auth_status",
      );
    if (missing === "auth-write")
      host.commands = host.commands.filter(
        (command) => command !== "set_api_key",
      );
    if (missing === "auth-commands")
      host.commands = host.commands.filter(
        (command) => !["get_auth_status", "set_api_key"].includes(command),
      );
    if (missing === "model-write")
      host.commands = host.commands.filter(
        (command) => command !== "set_model",
      );
    if (missing === "state-read")
      host.commands = host.commands.filter(
        (command) => command !== "get_state",
      );
    await start(page);
    // Native bootstrap reads predate this page; only manager-owned state reads
    // are capability-gated by the contract under test.
    const bootstrapStateReads = host.count("get_state");
    await openManager(page);
    await expect(card(page, "beta")).toBeVisible();
    if (missing === "model-write" || missing === "state-read") {
      await expect(modelButton(page, "beta")).toBeDisabled();
      await expect(manager(page)).toContainText(
        "Model selection requires both set_model and get_state support on this node.",
      );
    } else {
      await expect(manager(page)).toContainText(
        /does not support API Key configuration|Credential metadata is unavailable/,
      );
      await connection(page, "beta");
      await dialog(page)
        .getByLabel("API Key", { exact: true })
        .fill("unsupported-unsaved-key");
      const save = dialog(page).getByRole("button", {
        name: "Save API Key",
        exact: true,
      });
      if (missing === "auth-read") await expect(save).toBeEnabled();
      else await expect(save).toBeDisabled();
    }
    // Exercise the host's scheduled diagnostics as well, not just immediate render.
    await page.clock.runFor(500);
    if (["action", "auth-read", "auth-commands"].includes(missing))
      expect(host.count("get_auth_status")).toBe(0);
    if (missing === "state-read")
      expect(host.count("get_state")).toBe(bootstrapStateReads);
    expect(host.writes).toEqual([]);
  });
}

test("Base URL is an explicit local-only draft; clear and close send no mutation", async ({
  page,
  host,
}) => {
  await start(page);
  await openManager(page);
  await connection(page, "beta");
  const modal = dialog(page);
  const url = modal.getByLabel("Custom Base URL · local draft", {
    exact: true,
  });
  await url.fill("https://local-draft.invalid/v1");
  await url.blur();
  await expect(modal).toContainText(/local|draft/i);
  await modal
    .getByRole("button", { name: "Clear local draft", exact: true })
    .click();
  await expect(url).toHaveValue("");
  await url.fill("https://still-not-saved.invalid/v1");
  await modal
    .getByRole("button", { name: "Close dialog", exact: true })
    .click();
  await manager(page)
    .getByRole("button", { name: "Refresh providers", exact: true })
    .click();
  await expect(card(page, "beta")).toBeVisible();
  expect(host.writes).toEqual([]);
});

for (const boundary of ["close", "navigate"] as const) {
  for (const pending of ["read", "model", "key"] as const) {
    test(`${boundary} invalidates a pending ${pending} owner, including same-session reopen`, async ({
      page,
      host,
    }) => {
      await start(page);
      await openManager(page);
      await expect(card(page, "auth-only")).toBeVisible();
      let held: ReturnType<ProviderHost["hold"]>;
      if (pending === "read") {
        held = host.hold("get_auth_status", {
          json: envelope("get_auth_status", {
            providers: [auth("stale-owner-only")],
          }),
        });
        await manager(page)
          .getByRole("button", { name: "Refresh providers", exact: true })
          .click();
      } else if (pending === "model") {
        host.permit("set_model");
        held = host.hold("set_model", {
          json: envelope("set_model", { provider: "beta", modelId: "shared" }),
        });
        await modelButton(page, "beta").click();
      } else {
        await connection(page, "alpha");
        await dialog(page)
          .getByLabel("API Key", { exact: true })
          .fill("abandoned-key-draft");
        host.permit("set_api_key");
        held = host.hold("set_api_key", {
          json: envelope("set_api_key", {
            provider: "alpha",
            ready: true,
            requiresRestart: true,
            apiKey: "stale-response-secret",
          }),
        });
        await dialog(page)
          .getByRole("button", { name: "Save API Key", exact: true })
          .click();
      }
      await held.arrived;
      if (pending === "key")
        await dialog(page)
          .getByRole("button", { name: "Close dialog", exact: true })
          .click();
      if (boundary === "close") {
        await page
          .locator(".inspector")
          .getByRole("button", { name: "Close panel", exact: true })
          .click();
        await expect(manager(page)).toHaveCount(0);
        await openManager(page);
      } else {
        await selectSession(page, B);
        if (!(await manager(page).isVisible())) await openManager(page);
      }
      await expect(card(page, "auth-only")).toBeVisible();
      const active = boundary === "navigate" ? "beta" : "alpha";
      await expect(modelButton(page, active)).toBeDisabled();
      await connection(page, active);
      const key = dialog(page).getByLabel("API Key", { exact: true });
      await expect(key).toHaveValue("");
      await key.fill("current-owner-unsaved-draft");
      const stateReadsBeforeRelease = host.count("get_state");
      held.release();
      await held.finished;
      await rendered(page);
      expect(
        host.count("get_state"),
        "an invalidated owner must not initiate state refreshes",
      ).toBe(stateReadsBeforeRelease);
      await expect(key).toHaveValue("current-owner-unsaved-draft");
      await expect(dialog(page)).not.toContainText(/restart/i);
      await expect(card(page, "stale-owner-only")).toHaveCount(0);
      await expect(modelButton(page, active)).toHaveAttribute(
        "aria-pressed",
        "true",
      );
      await noSecretEcho(
        page,
        "abandoned-key-draft",
        "stale-response-secret",
        "current-owner-unsaved-draft",
      );
      expect(host.writes).toHaveLength(pending === "read" ? 0 : 1);
      expect(host.writes.every((call) => call.session === A)).toBe(true);
    });
  }
}

test("held discovery cannot roll back a newer composer model selection", async ({
  page,
  host,
}) => {
  await start(page);
  await openManager(page);
  await expect(card(page, "auth-only")).toBeVisible();
  const held = host.hold("get_auth_status", {
    json: envelope("get_auth_status", { providers: host.providers }),
  });
  const staleState = page.waitForResponse(
    (response) =>
      response.url().includes(`/sessions/${A}/commands`) &&
      response.request().postDataJSON().type === "get_state",
  );
  await manager(page)
    .getByRole("button", { name: "Refresh providers", exact: true })
    .click();
  await held.arrived;
  await (await staleState).finished();
  await rendered(page);
  host.permit("set_model");
  await page.locator(".composer-model").click();
  await page
    .locator(".dropdown-group")
    .filter({
      has: page.locator(".dropdown-group-label", { hasText: /^beta$/ }),
    })
    .locator(".dropdown-item")
    .click();
  await expect(modelButton(page, "beta")).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  held.release();
  await held.finished;
  await expect(
    manager(page).getByRole("button", {
      name: "Refresh providers",
      exact: true,
    }),
  ).toBeEnabled();
  await expect(modelButton(page, "beta")).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await expect(modelButton(page, "alpha")).toHaveAttribute(
    "aria-pressed",
    "false",
  );
  expect(host.writes).toEqual([
    {
      session: A,
      type: "set_model",
      body: { type: "set_model", provider: "beta", modelId: "shared" },
    },
  ]);
});

test("connection settings trap Tab, discard secrets on Escape and restore opener focus", async ({
  page,
  host,
}) => {
  await start(page);
  await openManager(page);
  await connection(page, "alpha");
  const modal = dialog(page);
  const key = modal.getByLabel("API Key", { exact: true });
  await key.fill("synthetic-keyboard-secret");
  const controls = modal.locator("button:enabled, input:enabled");
  await controls.last().focus();
  await page.keyboard.press("Tab");
  await expect(controls.first()).toBeFocused();
  await page.keyboard.press("Shift+Tab");
  await expect(controls.last()).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(modal).toHaveCount(0);
  await expect(
    card(page, "alpha").getByRole("button", {
      name: "Connection settings: alpha",
      exact: true,
    }),
  ).toBeFocused();
  await connection(page, "alpha");
  await expect(dialog(page).getByLabel("API Key", { exact: true })).toHaveValue(
    "",
  );
  await noSecretEcho(page, "synthetic-keyboard-secret");
  expect(host.writes).toEqual([]);
});

test("an older model read-back cannot roll back a newer composer model selection", async ({
  page,
  host,
}) => {
  host.models.push({
    provider: "gamma",
    id: "shared",
    name: "Shared model",
    contextWindow: 16000,
  });
  await start(page);
  await openManager(page);
  await expect(modelButton(page, "beta")).toBeEnabled();
  const held = host.hold("get_state", {
    json: envelope("get_state", { ...host.states[A], provider: "beta" }),
  });
  host.permit("set_model");
  await modelButton(page, "beta").click();
  await held.arrived;
  host.permit("set_model");
  await page.locator(".composer-model").click();
  await page
    .locator(".dropdown-group")
    .filter({
      has: page.locator(".dropdown-group-label", { hasText: /^gamma$/ }),
    })
    .locator(".dropdown-item")
    .click();
  await expect(modelButton(page, "gamma")).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  held.release();
  await held.finished;
  await expect(
    manager(page).getByRole("button", {
      name: "Refresh providers",
      exact: true,
    }),
  ).toBeEnabled();
  await expect(modelButton(page, "gamma")).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await expect(modelButton(page, "beta")).toHaveAttribute(
    "aria-pressed",
    "false",
  );
  expect(
    host.writes.map((call) => [call.body.provider, call.body.modelId]),
  ).toEqual([
    ["beta", "shared"],
    ["gamma", "shared"],
  ]);
});

for (const language of ["en", "zh"] as const) {
  test(`mobile provider editing never opens a native datalist and secondary notes start collapsed: ${language}`, async ({
    page,
    host,
  }) => {
    await page.setViewportSize({ width: 375, height: 844 });
    host.providers = [
      "alicoding",
      "anthropic",
      "hualong",
      "huoshan-copy",
      "llama-cpp",
      "openai",
    ].map(auth);
    await start(page, language);
    await openManager(page, language);
    await manager(page, language)
      .getByRole("button", { name: labels[language].add, exact: true })
      .click();
    const modal = dialog(page, language);
    const provider = modal.getByLabel(labels[language].provider, {
      exact: true,
    });
    await expect(provider).not.toHaveAttribute("list");
    await expect(modal.locator("datalist")).toHaveCount(0);
    await provider.fill("a");
    await expect(provider).toHaveValue("a");
    await expect(modal.getByLabel("API Key", { exact: true })).toBeVisible();
    const help = modal.locator("details.provider-manager-help");
    await expect(help).not.toHaveAttribute("open");
    await help.locator("summary").click();
    await expect(help).toHaveAttribute("open", "");
    await expect(help).toContainText(
      language === "zh"
        ? "仅提交你主动输入"
        : "Only the key you explicitly enter",
    );
    await help.locator("summary").click();
    await expect(help).not.toHaveAttribute("open");
    expect(
      await page.evaluate(() => document.documentElement.scrollWidth),
    ).toBeLessThanOrEqual(375);
    await provider.fill("openai");
    await expect(modal.getByLabel("API Key", { exact: true })).toHaveValue("");
    expect(host.writes).toEqual([]);
  });
}
