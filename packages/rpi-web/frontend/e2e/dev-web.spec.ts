import { test, expect, type Page, type Route } from "@playwright/test";

const sessionId = "session-1";

const capabilities = {
  protocol: "rpi",
  piCompatibilityTarget: "1.2.0",
  supportedCommands: [
    "get_state",
    "get_messages",
    "get_history",
    "get_stats",
    "get_queue",
    "queue_message",
    "cancel_queued",
    "clear_queue",
    "navigate",
    "prompt",
    "abort",
    "get_auth_status",
    "set_api_key",
  ],
  supportedActions: [
    "history",
    "queue",
    "queue_status",
    "fork",
    "clone",
    "stats",
    "history_mutation",
    "provider_auth",
    "session_metadata",
    "projects",
    "create_project",
    "session_organization",
    "files",
    "git_diff",
  ],
  parityComplete: false,
};

const session = {
  sessionId,
  projectId: "project-a",
  title: "Mock session",
  status: "ready",
  createdAt: 1_700_000_000_000,
  updatedAt: 1_700_000_000_100,
  lastSeq: 4,
  error: null,
  relation: null,
  parentSessionId: null,
  forkEntryId: null,
  latestSettlement: null,
};

function response(data: unknown, command = "mock") {
  return { type: "response", command, success: true, data };
}

async function mockHost(page: Page) {
  const projects = [
    { id: "project-a", name: "pi-rust", path: "/workspace/pi-rust" },
    { id: "project-b", name: "rpi-package", path: "/workspace/rpi-package" },
  ];
  const sessions: Array<
    typeof session & { pinned?: boolean; archived?: boolean }
  > = [{ ...session, pinned: false, archived: false }];
  await page.route("**/api/v1/**", async (route: Route) => {
    const request = route.request();
    const url = new URL(request.url());
    const path = url.pathname;

    if (path === "/api/v1/projects") {
      if (request.method() === "POST") {
        const body = request.postDataJSON();
        const project = {
          id: `project-${projects.length + 1}`,
          name: body.name,
          path: `/workspace/projects/${body.name}`,
        };
        projects.push(project);
        return route.fulfill({ status: 201, json: { project } });
      }
      return route.fulfill({
        json: {
          projects,
          defaultProjectId: "project-a",
          projectsRoot: "/workspace/projects",
        },
      });
    }
    if (/\/projects\/[^/]+\/files$/.test(path)) {
      return route.fulfill({
        json: {
          path: "",
          entries: [
            { name: "src", path: "src", kind: "directory", size: 0 },
            { name: "README.md", path: "README.md", kind: "file", size: 30 },
          ],
        },
      });
    }
    if (/\/projects\/[^/]+\/file$/.test(path)) {
      return route.fulfill({
        json: {
          path: url.searchParams.get("path"),
          content: path.includes("project-b")
            ? "# Package project\n"
            : "# Rust project\nHello workspace\n",
        },
      });
    }
    if (/\/projects\/[^/]+\/git\/status$/.test(path)) {
      return route.fulfill({
        json: {
          isRepository: true,
          branch: "main",
          files: [
            { path: "README.md", status: "MM", staged: true, unstaged: true },
          ],
        },
      });
    }
    if (/\/projects\/[^/]+\/git\/diff$/.test(path)) {
      return route.fulfill({
        json: {
          path: url.searchParams.get("path"),
          patch: `--- a/README.md\n+++ b/README.md\n@@ -1 +1 @@\n-before\n+${url.searchParams.get("staged") === "true" ? "staged change" : "working change"}\n`,
        },
      });
    }

    if (path === "/api/v1/auth" && request.method() === "GET") {
      return route.fulfill({ json: { required: false, authenticated: true } });
    }
    if (path === "/api/v1/capabilities") {
      return route.fulfill({ json: capabilities });
    }
    if (path === "/api/v1/sessions" && request.method() === "GET") {
      return route.fulfill({ json: { sessions } });
    }
    if (path === "/api/v1/sessions" && request.method() === "POST") {
      const body = request.postDataJSON();
      const created = {
        ...session,
        sessionId: `session-${sessions.length + 1}`,
        title: "New session",
        projectId: body.projectId ?? "project-a",
        createdAt: Date.now(),
        updatedAt: Date.now(),
        pinned: false,
        archived: false,
      };
      sessions.push(created);
      return route.fulfill({
        status: 201,
        json: {
          sessionId: created.sessionId,
          projectId: created.projectId,
          status: "ready",
        },
      });
    }
    if (/\/sessions\/[^/]+\/metadata$/.test(path)) {
      const entry = sessions.find((s) => s.sessionId === path.split("/")[4]);
      Object.assign(entry ?? {}, request.postDataJSON());
      return route.fulfill({ json: entry });
    }
    if (/\/sessions\/[^/]+\/rename$/.test(path)) {
      const entry = sessions.find((s) => s.sessionId === path.split("/")[4]);
      if (entry) entry.title = request.postDataJSON().title;
      return route.fulfill({ json: entry });
    }
    if (/\/sessions\/[^/]+\/snapshot$/.test(path)) {
      return route.fulfill({
        json: {
          sessionId,
          messages: [],
          history: {
            records: [
              {
                id: "record-1",
                type: "operation_finished",
                seq: 4,
                runId: "run-1",
                outcome: "completed",
                timestamp: 1_700_000_000_000,
              },
            ],
            total: 1,
            limit: 100,
            hasMore: false,
            firstSeq: 4,
            lastSeq: 4,
            openOperations: [],
          },
          events: [],
          lastSeq: 4,
          leafId: "leaf-1",
          pendingResume: false,
        },
      });
    }
    if (/\/sessions\/[^/]+\/events$/.test(path)) {
      return route.fulfill({
        status: 200,
        contentType: "text/event-stream",
        body: 'event: ready\ndata: {"type":"ready"}\n\n',
      });
    }
    if (/\/sessions\/[^/]+\/commands$/.test(path)) {
      const body = request.postDataJSON() as {
        type?: string;
        includeCompleted?: boolean;
      };
      switch (body.type) {
        case "get_state":
          return route.fulfill({
            json: response(
              {
                sessionId,
                leafId: "leaf-1",
                queue: { steeringCount: 0, followUpCount: 0 },
                stats: {
                  messageCount: 0,
                  cachedTokens: 0,
                  uncachedTokens: 0,
                  totalTokens: 0,
                  costTotal: 0,
                },
              },
              body.type,
            ),
          });
        case "get_messages":
          return route.fulfill({
            json: response(
              { messages: [], total: 0, hasMore: false },
              body.type,
            ),
          });
        case "get_history":
          return route.fulfill({
            json: response(
              {
                records: [
                  {
                    id: "record-1",
                    type: "operation_finished",
                    seq: 4,
                    runId: "run-1",
                    outcome: "completed",
                    timestamp: 1_700_000_000_000,
                  },
                ],
                total: 1,
                limit: 100,
                hasMore: false,
                firstSeq: 4,
                lastSeq: 4,
                openOperations: [],
              },
              body.type,
            ),
          });
        case "get_queue":
          return route.fulfill({
            json: response(
              {
                items: body.includeCompleted
                  ? [
                      {
                        entryId: "queue-1",
                        queue: "followUp",
                        status: "cancelled",
                        text: "old",
                      },
                    ]
                  : [
                      {
                        entryId: "queue-2",
                        queue: "followUp",
                        status: "queued",
                        text: "pending",
                      },
                    ],
                count: 1,
              },
              body.type,
            ),
          });
        case "get_stats":
          return route.fulfill({
            json: response(
              { messageCount: 0, totalTokens: 0, costTotal: 0 },
              body.type,
            ),
          });
        case "get_auth_status":
          return route.fulfill({
            json: response({ providers: [] }, body.type),
          });
        default:
          return route.fulfill({ json: response({}, body.type ?? "mock") });
      }
    }
    return route.fulfill({ json: response({}) });
  });
}

test("capabilities gate the history and queue inspector", async ({ page }) => {
  await mockHost(page);
  await page.goto("/");
  await expect(
    page.getByRole("heading", { name: "Mock session" }),
  ).toBeVisible();
  await page.getByRole("button", { name: "Toggle inspector" }).click();
  await expect(page.locator(".inspector")).toBeVisible();
  await expect(
    page
      .locator(".inspector h3.panel-heading > span:first-child")
      .filter({ hasText: "History" })
      .first(),
  ).toBeVisible();
  await expect(
    page
      .locator(".inspector h3.panel-heading > span:first-child")
      .filter({ hasText: "Queue" })
      .first(),
  ).toBeVisible();
});

test("history records expand and queue history toggles", async ({ page }) => {
  await mockHost(page);
  await page.goto("/");
  await page.getByRole("button", { name: "Toggle inspector" }).click();
  const historyItem = page.locator(".history-timeline-item");
  await expect(historyItem).toHaveCount(1);
  await historyItem.locator("summary").click();
  await expect(historyItem.locator("pre")).toContainText("operation_finished");
  await page.getByRole("button", { name: "History", exact: true }).click();
  await expect(
    page.locator(".queue-item").filter({ hasText: "cancelled" }),
  ).toBeVisible();
});

test("file previews, staged diffs and project switching use the selected root", async ({
  page,
}) => {
  await mockHost(page);
  await page.goto("/");
  const workbench = page.getByRole("complementary", {
    name: "Project files and changes",
  });
  await workbench
    .getByRole("button", { name: "README.md", exact: true })
    .click();
  await expect(workbench.getByLabel("File contents")).toContainText(
    "Hello workspace",
  );
  await workbench.getByRole("tab", { name: /Changes/ }).click();
  await workbench
    .locator(".change-group")
    .filter({ hasText: "Staged" })
    .getByRole("button")
    .click();
  await expect(workbench.getByLabel("File contents")).toContainText(
    "+staged change",
  );
  await workbench
    .locator(".change-group")
    .filter({ hasText: "Working changes" })
    .getByRole("button")
    .click();
  await expect(workbench.getByLabel("File contents")).toContainText(
    "+working change",
  );
  await page
    .getByRole("button", { name: "Open project: rpi-package", exact: true })
    .click();
  await workbench
    .getByRole("button", { name: "README.md", exact: true })
    .click();
  await expect(workbench.getByLabel("File contents")).toContainText(
    "Package project",
  );
  await expect(page.locator(".source-preview")).not.toContainText(
    "Rust project",
  );
});

test("projects contain chats and a new project starts its own conversation", async ({
  page,
}) => {
  await mockHost(page);
  await page.goto("/");
  const original = page.getByRole("region", { name: "pi-rust", exact: true });
  await expect(
    original.getByRole("button", { name: /^Mock session/ }),
  ).toBeVisible();
  await page.getByRole("button", { name: "New project", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "Add project" });
  await dialog
    .getByLabel("Project name", { exact: true })
    .fill("New workspace");
  await expect(dialog).toContainText("/workspace/projects/New workspace");
  await dialog
    .getByRole("button", { name: "Add project and start chat" })
    .click();
  await expect(dialog).not.toBeVisible();
  const created = page.getByRole("region", {
    name: "New workspace",
    exact: true,
  });
  await expect(created.locator(".project-session-link")).toHaveCount(1);
  await expect(
    page.getByRole("heading", { name: "New session", exact: true }),
  ).toBeVisible();
  await expect(
    original.getByRole("button", { name: /^Mock session/ }),
  ).toBeVisible();
  await page.reload();
  await expect(
    page.getByRole("button", {
      name: "Open project: New workspace",
      exact: true,
    }),
  ).toBeVisible();
});

test("rename, pin, archive and restore survive reloads", async ({ page }) => {
  await mockHost(page);
  await page.goto("/");
  await page
    .getByRole("button", { name: "Session actions: Mock session", exact: true })
    .click();
  await page.getByRole("menuitem", { name: "Rename", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "Rename session" });
  await dialog.getByLabel("Session name").fill("Design review");
  await dialog.getByRole("button", { name: "Save", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "Design review", exact: true }),
  ).toBeVisible();
  await page
    .getByRole("button", {
      name: "Session actions: Design review",
      exact: true,
    })
    .click();
  await page.getByRole("menuitem", { name: "Pin", exact: true }).click();
  await page.reload();
  await page
    .getByRole("button", {
      name: "Session actions: Design review",
      exact: true,
    })
    .click();
  await expect(
    page.getByRole("menuitem", { name: "Unpin", exact: true }),
  ).toBeVisible();
  await page.getByRole("menuitem", { name: "Archive", exact: true }).click();
  await expect(
    page.getByRole("button", {
      name: "Session actions: Design review",
      exact: true,
    }),
  ).toHaveCount(0);
  await page
    .getByRole("button", { name: "Show archived chats", exact: true })
    .click();
  await page
    .getByRole("button", {
      name: "Session actions: Design review",
      exact: true,
    })
    .click();
  await page
    .getByRole("menuitem", { name: "Restore chat", exact: true })
    .click();
  await page
    .getByRole("button", { name: "Show projects", exact: true })
    .click();
  await expect(
    page.getByRole("button", {
      name: "Session actions: Design review",
      exact: true,
    }),
  ).toBeVisible();
});

test("project navigation collapses, searches and remembers appearance", async ({
  page,
}) => {
  await mockHost(page);
  await page.goto("/");
  await page
    .getByRole("button", { name: "Collapse project: pi-rust", exact: true })
    .click();
  await expect(page.locator(".project-session")).toHaveCount(0);
  await page.reload();
  await expect(
    page.getByRole("button", { name: "Expand project: pi-rust", exact: true }),
  ).toBeVisible();
  await page
    .getByRole("textbox", { name: "Search projects or chats" })
    .fill("Mock");
  await expect(page.locator(".project-session")).toHaveCount(1);
  await expect(
    page.getByRole("button", {
      name: "Open project: rpi-package",
      exact: true,
    }),
  ).toHaveCount(0);
  await page
    .getByRole("button", { name: "Toggle appearance", exact: true })
    .click();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await page.reload();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
});

test("split widths persist and separators support the keyboard", async ({
  page,
}) => {
  await mockHost(page);
  await page.goto("/");
  const split = page.getByRole("separator", { name: "Resize project panel" });
  await expect(split).toHaveAttribute("aria-valuenow", "440");
  await split.focus();
  await split.press("ArrowLeft");
  await expect(split).toHaveAttribute("aria-valuenow", "460");
  await page.reload();
  await expect(
    page.getByRole("separator", { name: "Resize project panel" }),
  ).toHaveAttribute("aria-valuenow", "460");
});

test("rejected prompts restore the draft and remove the optimistic message", async ({
  page,
}) => {
  await mockHost(page);
  await page.route(
    `**/api/v1/sessions/${sessionId}/commands`,
    async (route) => {
      if (route.request().postDataJSON().type !== "prompt")
        return route.fallback();
      return route.fulfill({
        json: {
          type: "response",
          success: false,
          error: "a run is already active",
        },
      });
    },
  );
  await page.goto("/");
  const input = page.getByRole("textbox", {
    name: "Send a prompt, or type / for commands…",
  });
  await input.fill("Keep this draft");
  await input.press("Enter");
  await expect(page.locator(".error-banner")).toContainText(
    "a run is already active",
  );
  await expect(input).toHaveValue("Keep this draft");
  await expect(page.locator(".message.user")).toHaveCount(0);
});

test("IME confirmation does not submit a prompt", async ({ page }) => {
  await mockHost(page);
  let prompts = 0;
  page.on("request", (request) => {
    if (
      request.method() === "POST" &&
      request.url().endsWith("/commands") &&
      request.postDataJSON().type === "prompt"
    )
      prompts++;
  });
  await page.goto("/");
  const input = page.getByRole("textbox", {
    name: "Send a prompt, or type / for commands…",
  });
  await input.fill("中文输入");
  await input.dispatchEvent("keydown", {
    key: "Enter",
    code: "Enter",
    isComposing: true,
    bubbles: true,
  });
  await expect(input).toHaveValue("中文输入");
  expect(prompts).toBe(0);
});

test("mobile navigation opens without horizontal overflow", async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await mockHost(page);
  await page.goto("/");
  await expect(
    page.getByRole("heading", { name: "What shall we build?" }),
  ).toBeVisible();
  await page
    .getByRole("button", { name: "Toggle sidebar", exact: true })
    .click();
  await expect(
    page.getByRole("button", { name: "Open project: pi-rust", exact: true }),
  ).toBeVisible();
  await page.getByRole("button", { name: "Close sidebar" }).click();
  await page.getByRole("button", { name: "Toggle project files" }).click();
  await expect(
    page.getByRole("tab", { name: "Files", exact: true }),
  ).toBeVisible();
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= window.innerWidth,
    ),
  ).toBe(true);
});

test("messages sent during a run enter the follow-up queue", async ({
  page,
}) => {
  await mockHost(page);
  const queued: string[] = [];
  await page.route(`**/api/v1/sessions/${sessionId}/events**`, async (route) =>
    route.fulfill({
      contentType: "text/event-stream",
      body: 'id: 5\nevent: agent_start\ndata: {"type":"agent_start"}\n\n',
    }),
  );
  await page.route(
    `**/api/v1/sessions/${sessionId}/commands`,
    async (route) => {
      const body = route.request().postDataJSON();
      if (body.type !== "queue_message") return route.fallback();
      queued.push(body.message);
      expect(body.deliverAs).toBe("followUp");
      return route.fulfill({
        json: response({ accepted: true, entryId: "queued-test" }, body.type),
      });
    },
  );
  await page.goto("/");
  await expect(
    page.getByRole("button", { name: "Abort", exact: true }),
  ).toBeEnabled();
  const input = page.getByRole("textbox", {
    name: "Send a prompt, or type / for commands…",
  });
  await input.fill("Next task");
  await input.press("Enter");
  await expect.poll(() => queued).toEqual(["Next task"]);
  await expect(page.locator(".message.user")).toHaveCount(0);
  await expect(input).toHaveValue("");
});

test("an event replay gap reloads the native snapshot", async ({ page }) => {
  await mockHost(page);
  let snapshots = 0;
  let streams = 0;
  await page.route(
    `**/api/v1/sessions/${sessionId}/snapshot`,
    async (route) => {
      snapshots++;
      return route.fulfill({
        json: {
          sessionId,
          messages:
            snapshots > 1
              ? [
                  {
                    role: "user",
                    entryId: "restored",
                    content: "Recovered transcript",
                  },
                ]
              : [],
          events: [],
          lastSeq: 4,
          total: 1,
          hasMore: false,
        },
      });
    },
  );
  await page.route(
    `**/api/v1/sessions/${sessionId}/events**`,
    async (route) => {
      streams++;
      return route.fulfill({
        contentType: "text/event-stream",
        body:
          streams === 1
            ? 'event: resync_required\ndata: {"type":"resync_required"}\n\n'
            : 'event: ready\ndata: {"type":"ready"}\n\n',
      });
    },
  );
  await page.goto("/");
  await expect(page.locator(".message.user")).toContainText(
    "Recovered transcript",
  );
  expect(snapshots).toBeGreaterThan(1);
});

const markdownExample = [
  "## Markdown 排版检查",
  "",
  "第一行保留换行。",
  "第二行包含 **重点**、*强调* 和 `inline_code`。",
  "",
  "这是独立段落，验证正文间距。",
  "",
  "### 实现步骤",
  "",
  "1. 检查布局",
  "",
  "   这段说明属于第一项。",
  "",
  "   - 子项目 A",
  "   - 子项目 B",
  "",
  "2. 验证代码和表格",
  "",
  "- [x] 已完成的检查",
  "- [ ] 待处理的检查",
  "",
  "> 引用中的第一行。",
  "> 第二行仍然属于引用。",
  "",
  "```typescript",
  'const label = "保留代码缩进";',
  "function render() {",
  '  return "' + "long_code_".repeat(30) + '";',
  "}",
  "```",
  "",
  "| 功能 | 说明 | 状态 |",
  "| :--- | :--- | ---: |",
  "| 列表 | 保留嵌套关系与段落 | 正常 |",
  "| 长字段 | `" + "long_field_".repeat(28) + "` | 正常 |",
  "",
  "![布局示意图](/markdown-test-image.svg)",
  "",
  "[链接](https://example.com/" + "long-path-".repeat(25) + ")",
  "",
  "结束段落。",
].join("\n");

async function mockMarkdownTranscript(page: Page) {
  await mockHost(page);
  await page.route("**/markdown-test-image.svg", (route) =>
    route.fulfill({
      contentType: "image/svg+xml",
      body: '<svg xmlns="http://www.w3.org/2000/svg" width="1800" height="90"><rect width="1800" height="90" fill="#e7e7e3"/><text x="30" y="55" font-size="26" fill="#292927">Responsive image</text></svg>',
    }),
  );
  await page.route(`**/api/v1/sessions/${sessionId}/snapshot`, (route) =>
    route.fulfill({
      json: {
        sessionId,
        messages: [
          {
            role: "user",
            entryId: "md-user",
            content: "请保留这三行：\n第二行\n第三行",
          },
          {
            role: "assistant",
            entryId: "md-assistant",
            content: [
              {
                type: "thinking",
                thinking: "**检查思路**\n\n- 确认段落\n- 确认列表",
              },
              { type: "text", text: markdownExample },
            ],
          },
          {
            role: "assistant",
            entryId: "md-next",
            content: [{ type: "text", text: "下一条回复。" }],
          },
        ],
        events: [],
        lastSeq: 4,
        total: 3,
        hasMore: false,
      },
    }),
  );
}

test("markdown preserves chat line breaks, readable code and contained layouts", async ({
  page,
}, testInfo) => {
  await mockMarkdownTranscript(page);
  await page.setViewportSize({ width: 1440, height: 1200 });
  await page.goto("/");
  const prose = page.locator(".message.assistant .prose").first();
  await expect(
    prose.getByRole("heading", { name: "Markdown 排版检查" }),
  ).toBeVisible();
  await page.locator(".transcript").evaluate((element) => {
    element.scrollTop = 0;
  });
  expect
    .soft(
      await page
        .locator(".message.user p")
        .evaluate((element) => getComputedStyle(element).whiteSpace),
    )
    .toBe("pre-wrap");
  await expect
    .soft(
      prose.locator("p").filter({ hasText: "第一行保留换行" }).locator("br"),
    )
    .toHaveCount(1);
  expect
    .soft(
      await prose
        .locator("pre code")
        .evaluate((element) => parseFloat(getComputedStyle(element).fontSize)),
    )
    .toBeGreaterThanOrEqual(12);
  expect
    .soft(
      await prose
        .locator("li > p")
        .first()
        .evaluate((element) =>
          parseFloat(getComputedStyle(element).marginBottom),
        ),
    )
    .toBeLessThanOrEqual(4);
  const overflows = await page
    .locator(".transcript")
    .evaluate((element) => element.scrollWidth > element.clientWidth);
  expect.soft(overflows).toBe(false);
  const gap = await page
    .locator(".message.assistant")
    .evaluateAll(
      (elements) =>
        elements[1].getBoundingClientRect().top -
        elements[0].querySelector(".prose")!.getBoundingClientRect().bottom,
    );
  expect.soft(gap).toBeLessThanOrEqual(32);
  await expect(prose.locator("ol > li")).toHaveCount(2);
  await expect(prose.locator("ol ul > li")).toHaveCount(2);
  await expect(prose.locator('input[type="checkbox"]:checked')).toHaveCount(1);
  await expect(prose.locator("pre code")).toContainText('  return "long_code_');
  const table = prose.getByRole("region", {
    name: "Table, scroll horizontally",
  });
  await table.focus();
  await table.press("ArrowRight");
  await expect
    .poll(() => table.evaluate((element) => element.scrollLeft))
    .toBeGreaterThan(0);
  await table.evaluate((element) => {
    element.scrollLeft = 0;
  });
  await page.locator(".thinking-row summary").click();
  await expect(page.locator(".thinking-row strong")).toHaveText("检查思路");
  await expect(page.locator(".thinking-row li")).toHaveCount(2);
  await page.locator(".thinking-row summary").click();
  await page.locator(".transcript").evaluate((element) => {
    element.scrollTop = 0;
  });
  await page.screenshot({ path: testInfo.outputPath("markdown-desktop.png") });

  await page.setViewportSize({ width: 375, height: 812 });
  await page.reload();
  await expect(prose.locator("img")).toBeVisible();
  expect(
    await page
      .locator(".transcript")
      .evaluate((element) => element.scrollWidth <= element.clientWidth),
  ).toBe(true);
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
  ).toBe(true);
  await prose.locator("pre").scrollIntoViewIfNeeded();
  expect(
    await prose
      .locator("pre")
      .evaluate((element) => element.scrollWidth > element.clientWidth),
  ).toBe(true);
  await page.screenshot({ path: testInfo.outputPath("markdown-mobile.png") });
});

test("streamed markdown handles incomplete fences and stays formatted in dark mode", async ({
  page,
}, testInfo) => {
  await mockMarkdownTranscript(page);
  await page.addInitScript(() => localStorage.setItem("rpi-theme", "dark"));
  let streams = 0;
  let releaseComplete!: () => void;
  const readyToComplete = new Promise<void>((resolve) => {
    releaseComplete = resolve;
  });
  const partial = "### 流式代码\n\n```typescript\nconst partial";
  const complete =
    '### 流式代码\n\n```typescript\nconst partial = "完成";\n```\n\n第一行。\n第二行。';
  const event = (seq: number, type: string, text: string) =>
    `id: ${seq}\nevent: ${type}\ndata: ${JSON.stringify({ type, message: { role: "assistant", content: [{ type: "text", text }] } })}\n\n`;
  await page.route(
    `**/api/v1/sessions/${sessionId}/events**`,
    async (route) => {
      streams++;
      if (streams > 1) await readyToComplete;
      return route.fulfill({
        contentType: "text/event-stream",
        body:
          streams === 1
            ? event(5, "message_start", "") +
              event(6, "message_update", partial)
            : event(7, "message_update", complete) +
              event(8, "message_end", complete),
      });
    },
  );
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.goto("/");
  const streamed = page.locator(".message.assistant").last();
  await expect(streamed.locator("pre code")).toHaveText("const partial\n");
  releaseComplete();
  await expect(streamed.locator("pre code")).toHaveText(
    'const partial = "完成";\n',
  );
  await expect(streamed.locator("p br")).toHaveCount(1);
  await expect(page.getByRole("heading", { name: "流式代码" })).toHaveCount(1);
  await expect(streamed.locator(".hljs-keyword")).toHaveText("const");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  expect(errors).toEqual([]);
  await streamed.scrollIntoViewIfNeeded();
  await page.screenshot({
    path: testInfo.outputPath("markdown-dark-stream.png"),
  });
});

test("narrow authenticated sidebar uses icons and all settings follow the chosen language", async ({
  page,
}, testInfo) => {
  await mockMarkdownTranscript(page);
  await page.setViewportSize({ width: 1280, height: 1000 });
  await page.addInitScript(() => {
    localStorage.setItem("rpi-web-language", "zh");
    localStorage.setItem("rpi-sidebar-width", "200");
    localStorage.setItem("rpi-workbench-width", "300");
  });
  let signedOut = false;
  let submittedLevel = "";
  await page.route("**/api/v1/auth", (route) => {
    if (route.request().method() === "DELETE") signedOut = true;
    return route.fulfill({
      json: { required: true, authenticated: !signedOut },
    });
  });
  await page.route("**/api/v1/capabilities", (route) =>
    route.fulfill({
      json: {
        ...capabilities,
        supportedCommands: [
          ...capabilities.supportedCommands,
          "get_available_thinking_levels",
          "set_thinking_level",
        ],
      },
    }),
  );
  await page.route(`**/api/v1/sessions/${sessionId}/commands`, (route) => {
    const body = route.request().postDataJSON();
    if (body.type === "get_auth_status")
      return route.fulfill({
        json: response(
          {
            providers: [
              {
                provider: "test-provider",
                ready: false,
                source: "none",
                oauth: false,
                oauthAvailable: false,
              },
              {
                provider: "oauth-provider",
                ready: true,
                source: "oauth",
                oauth: true,
                oauthAvailable: true,
              },
            ],
          },
          body.type,
        ),
      });
    if (body.type === "get_available_thinking_levels")
      return route.fulfill({
        json: response({ levels: ["low", "high"] }, body.type),
      });
    if (body.type === "set_thinking_level") {
      submittedLevel = body.level;
      return route.fulfill({ json: response({}, body.type) });
    }
    if (body.type === "set_api_key")
      return route.fulfill({
        json: response(
          { provider: body.provider, ready: true, requiresRestart: true },
          body.type,
        ),
      });
    return route.fallback();
  });
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.goto("/");
  const logout = page.getByRole("button", { name: "退出登录", exact: true });
  await expect(logout).toBeVisible();
  await expect(logout).toHaveText("");
  await expect(logout.locator("svg")).toHaveCount(1);
  const account = page.locator(".account-panel");
  expect(
    await account.evaluate((element) => element.getBoundingClientRect().height),
  ).toBeLessThanOrEqual(44);
  expect(
    await account.evaluate(
      (element) => element.scrollWidth <= element.clientWidth,
    ),
  ).toBe(true);
  await page.getByRole("button", { name: "切换诊断面板" }).click();
  const inspector = page.locator(".inspector");
  await expect(
    inspector.getByRole("heading", { name: "服务商授权" }),
  ).toBeVisible();
  await expect(inspector).toContainText("未配置");
  await expect(inspector.getByRole("heading", { name: /^队列/ })).toBeVisible();
  await expect(
    inspector.getByRole("heading", { name: "可用能力" }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "刷新 oauth-provider OAuth" }),
  ).toHaveText("");
  await inspector.getByRole("button", { name: "test-provider 未配置" }).click();
  const key = inspector.getByLabel("test-provider 的 API 密钥", {
    exact: true,
  });
  await key.fill("test-only-key");
  await inspector.getByRole("button", { name: "保存密钥" }).click();
  await expect(inspector).toContainText("重启会话后加载新的服务商凭据");
  const overflow = await inspector.evaluate((element) => {
    const right = element.getBoundingClientRect().right;
    return [...element.querySelectorAll("*")]
      .filter((child) => child.getBoundingClientRect().right > right + 1)
      .map(
        (child) =>
          `${child.tagName}.${child.className}: ${child.textContent?.slice(0, 40)}`,
      );
  });
  expect(overflow).toEqual([]);
  await page.locator(".transcript").evaluate((element) => {
    element.scrollTop = 0;
  });
  await page.screenshot({ path: testInfo.outputPath("sidebar-chinese.png") });
  await page
    .getByRole("combobox", { name: "语言", exact: true })
    .selectOption("en");
  await expect(
    inspector.getByRole("heading", { name: "Provider auth" }),
  ).toBeVisible();
  await expect(inspector).toContainText(
    "Restart this session to reload provider credentials.",
  );
  await expect(
    page.getByRole("button", { name: "Sign out", exact: true }),
  ).toHaveText("");
  await page
    .getByRole("combobox", { name: "Language", exact: true })
    .selectOption("zh");
  await page.getByRole("button", { name: "关闭面板", exact: true }).click();
  await page.getByRole("button", { name: "运行时设置", exact: true }).click();
  await expect(inspector.getByText("暂无可用工具列表。")).toBeVisible();
  await page
    .getByRole("combobox", { name: "思考深度", exact: true })
    .selectOption({ label: "低" });
  await expect.poll(() => submittedLevel).toBe("low");
  expect(errors).toEqual([]);
  await logout.click();
  await expect(page.getByRole("heading", { name: "登录 rpi" })).toBeVisible();
});

test("delayed diagnostics from another chat cannot replace the active chat queue", async ({
  page,
}) => {
  await mockHost(page);
  let release!: () => void;
  const ready = new Promise<void>((resolve) => {
    release = resolve;
  });
  let pending = false;
  await page.route("**/api/v1/sessions/*/commands", async (route) => {
    const body = route.request().postDataJSON();
    if (body.type !== "get_queue") return route.fallback();
    const previous = route.request().url().includes(`/${sessionId}/`);
    if (previous) {
      pending = true;
      await ready;
    }
    return route.fulfill({
      json: response(
        {
          items: [
            {
              entryId: previous ? "old-queue" : "current-queue",
              queue: "followUp",
              status: "queued",
              text: previous ? "Old chat queue" : "Current chat queue",
            },
          ],
        },
        body.type,
      ),
    });
  });
  await page.goto("/");
  await expect.poll(() => pending).toBe(true);
  await page
    .getByRole("button", { name: "New session", exact: false })
    .first()
    .click();
  await expect(
    page.getByRole("heading", { name: "New session" }),
  ).toBeVisible();
  await page.getByRole("button", { name: "Toggle inspector" }).click();
  await expect(page.locator(".queue-item")).toContainText("Current chat queue");
  const oldResponse = page.waitForResponse(
    (res) =>
      res.url().includes(`/${sessionId}/commands`) &&
      res.request().postDataJSON().type === "get_queue",
  );
  release();
  await (await oldResponse).finished();
  await page
    .getByRole("combobox", { name: "Language", exact: true })
    .selectOption("zh");
  await expect(page.locator(".inspector h2")).toHaveText("会话诊断");
  await expect(page.locator(".queue-item")).toContainText("Current chat queue");
  await expect(page.locator(".queue-item")).not.toContainText("Old chat queue");
});

test("archiving a previous chat does not navigate away from a newly selected chat", async ({
  page,
}) => {
  await mockHost(page);
  let release!: () => void;
  const ready = new Promise<void>((resolve) => {
    release = resolve;
  });
  let pending = false;
  await page.route(
    `**/api/v1/sessions/${sessionId}/metadata`,
    async (route) => {
      if (!route.request().postDataJSON().archived) return route.fallback();
      pending = true;
      await ready;
      return route.fallback();
    },
  );
  await page.goto("/");
  await page
    .getByRole("button", { name: "New session", exact: false })
    .first()
    .click();
  await expect(
    page.getByRole("heading", { name: "New session" }),
  ).toBeVisible();
  await page
    .locator(".project-session-link")
    .filter({ hasText: "Mock session" })
    .click();
  await page
    .getByRole("button", { name: "Session actions: Mock session" })
    .click();
  await page.getByRole("menuitem", { name: "Archive", exact: true }).click();
  await expect.poll(() => pending).toBe(true);
  await page
    .locator(".project-session-link")
    .filter({ hasText: "New session" })
    .click();
  const activeUrl = page.url();
  const oldResponse = page.waitForResponse((res) =>
    res.url().includes(`/${sessionId}/metadata`),
  );
  release();
  await (await oldResponse).finished();
  await expect(
    page.locator(".project-session-link").filter({ hasText: "Mock session" }),
  ).toHaveCount(0);
  await expect(
    page.getByRole("heading", { name: "New session" }),
  ).toBeVisible();
  expect(page.url()).toBe(activeUrl);
});

test("a failed session list does not create a replacement chat", async ({
  page,
}) => {
  await mockHost(page);
  let created = 0;
  await page.route("**/api/v1/sessions", (route) => {
    if (route.request().method() === "GET")
      return route.fulfill({
        status: 503,
        json: { error: "Session list unavailable" },
      });
    created++;
    return route.fallback();
  });
  await page.goto("/");
  await expect(page.locator(".error-banner")).toContainText(
    "Session list unavailable",
  );
  await page.getByRole("button", { name: "Settings Ctrl ," }).click();
  await expect(
    page.getByRole("dialog", { name: "Settings", exact: true }),
  ).toBeVisible();
  expect(created).toBe(0);
});

test("expired access returns to login and can reopen the same chat", async ({
  page,
}) => {
  await page.clock.install();
  await mockHost(page);
  let expired = false;
  await page.route("**/api/v1/auth", (route) => {
    if (route.request().method() === "POST") expired = false;
    return route.fulfill({ json: { required: true, authenticated: !expired } });
  });
  await page.route("**/api/v1/sessions", (route) => {
    if (expired)
      return route.fulfill({ status: 401, json: { error: "unauthorized" } });
    return route.fallback();
  });
  await page.goto("/");
  await expect(
    page.getByRole("heading", { name: "Mock session" }),
  ).toBeVisible();
  await page
    .getByRole("textbox", { name: "Send a prompt, or type / for commands…" })
    .fill("Private draft");
  expired = true;
  await page.clock.fastForward(5001);
  await expect(
    page.getByRole("heading", { name: "Sign in to rpi" }),
  ).toBeVisible();
  await expect(page.locator(".workspace")).toHaveCount(0);
  await page
    .getByLabel("Workspace password", { exact: true })
    .fill("test-only-password");
  await page.getByRole("button", { name: "Sign in", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "Mock session" }),
  ).toBeVisible();
  await expect(
    page.getByRole("textbox", {
      name: "Send a prompt, or type / for commands…",
    }),
  ).toHaveValue("");
});

test("late chat creation preserves a subsequent project selection", async ({
  page,
}) => {
  await mockHost(page);
  let release!: () => void;
  const ready = new Promise<void>((resolve) => {
    release = resolve;
  });
  let pending = false;
  await page.route("**/api/v1/sessions", async (route) => {
    if (route.request().method() !== "POST") return route.fallback();
    pending = true;
    await ready;
    return route.fallback();
  });
  await page.goto("/");
  await expect(
    page.getByRole("heading", { name: "Mock session" }),
  ).toBeVisible();
  await page
    .getByRole("button", { name: "New session", exact: false })
    .first()
    .click();
  await expect.poll(() => pending).toBe(true);
  await page
    .getByRole("button", { name: "Open project: rpi-package", exact: true })
    .click();
  const completed = page.waitForResponse(
    (res) =>
      res.url().endsWith("/sessions") && res.request().method() === "POST",
  );
  release();
  await (await completed).finished();
  await expect(
    page.locator(".project-session-link").filter({ hasText: "New session" }),
  ).toHaveCount(1);
  await expect(page.locator(".toolbar .breadcrumb")).toContainText(
    "rpi-package",
  );
  expect(new URL(page.url()).searchParams.has("session")).toBe(false);
});

test("pagination from a previous chat does not prepend into the current chat", async ({
  page,
}) => {
  await mockHost(page);
  await page.route(`**/api/v1/sessions/${sessionId}/snapshot`, (route) =>
    route.fulfill({
      json: {
        sessionId,
        messages: [
          { role: "user", entryId: "first", content: "Current old chat" },
        ],
        events: [],
        lastSeq: 4,
        firstId: "first",
        hasMore: true,
      },
    }),
  );
  let release!: () => void;
  const ready = new Promise<void>((resolve) => {
    release = resolve;
  });
  let pending = false;
  await page.route(
    `**/api/v1/sessions/${sessionId}/commands`,
    async (route) => {
      const body = route.request().postDataJSON();
      if (body.type !== "get_messages" || !body.beforeId)
        return route.fallback();
      pending = true;
      await ready;
      return route.fulfill({
        json: response(
          {
            messages: [
              {
                role: "user",
                entryId: "older",
                content: "Older message from another chat",
              },
            ],
            firstId: "older",
            hasMore: false,
          },
          body.type,
        ),
      });
    },
  );
  await page.goto("/");
  await page.getByRole("button", { name: "Load earlier messages" }).click();
  await expect.poll(() => pending).toBe(true);
  await page
    .getByRole("button", { name: "New session", exact: false })
    .first()
    .click();
  await expect(
    page.getByRole("heading", { name: "New session" }),
  ).toBeVisible();
  const completed = page.waitForResponse(
    (res) =>
      res.url().includes(`/${sessionId}/commands`) &&
      res.request().postDataJSON().type === "get_messages",
  );
  release();
  await (await completed).finished();
  await page
    .getByRole("combobox", { name: "Language", exact: true })
    .selectOption("zh");
  await expect(page.locator(".message.user")).toHaveCount(0);
});

test("a pending runtime change cannot overwrite the next chat runtime", async ({
  page,
}) => {
  await mockHost(page);
  await page.route("**/api/v1/capabilities", (route) =>
    route.fulfill({
      json: {
        ...capabilities,
        supportedCommands: [
          ...capabilities.supportedCommands,
          "get_available_thinking_levels",
          "set_thinking_level",
        ],
      },
    }),
  );
  let release!: () => void;
  const ready = new Promise<void>((resolve) => {
    release = resolve;
  });
  let pending = false;
  await page.route("**/api/v1/sessions/*/commands", async (route) => {
    const body = route.request().postDataJSON();
    if (body.type === "get_available_thinking_levels")
      return route.fulfill({
        json: response({ levels: ["low", "high"] }, body.type),
      });
    if (body.type === "set_thinking_level") {
      pending = true;
      await ready;
      return route.fulfill({ json: response({}, body.type) });
    }
    return route.fallback();
  });
  await page.goto("/");
  await page
    .getByRole("button", { name: "Runtime settings", exact: true })
    .click();
  await page
    .getByRole("combobox", { name: "Thinking level", exact: true })
    .selectOption("high");
  await expect.poll(() => pending).toBe(true);
  await page
    .getByRole("button", { name: "New session", exact: false })
    .first()
    .click();
  await expect(
    page.getByRole("heading", { name: "New session" }),
  ).toBeVisible();
  const completed = page.waitForResponse(
    (res) =>
      res.url().includes(`/${sessionId}/commands`) &&
      res.request().postDataJSON().type === "set_thinking_level",
  );
  release();
  await (await completed).finished();
  await page
    .getByRole("combobox", { name: "Language", exact: true })
    .selectOption("zh");
  await expect(page.locator(".composer-thinking")).toHaveCount(0);
  await expect(
    page.getByRole("combobox", { name: "思考深度", exact: true }),
  ).toBeEnabled();
});

test("snapshot replay keeps a closed dialog and completed recovery state closed", async ({
  page,
}) => {
  await mockHost(page);
  await page.route(`**/api/v1/sessions/${sessionId}/snapshot`, (route) =>
    route.fulfill({
      json: {
        sessionId,
        messages: [],
        lastSeq: 6,
        pendingResume: true,
        pendingDialog: {
          type: "ui_dialog_request",
          requestId: "permission-old",
          ui: { purpose: "permission", question: "Old permission request" },
        },
        events: [
          {
            seq: 5,
            event: { type: "ui_dialog_closed", requestId: "permission-old" },
          },
          {
            seq: 6,
            event: {
              type: "agent_settled",
              outcome: "completed",
              summary: "Recovered completion",
              pendingResume: false,
            },
          },
        ],
      },
    }),
  );
  await page.goto("/");
  await expect(page.locator(".transcript-note")).toContainText(
    "Recovered completion",
  );
  await expect(page.locator(".dialog-backdrop")).toHaveCount(0);
  await expect(page.locator(".notice-banner")).toHaveCount(0);
});

test("an empty snapshot event list still seeds the live event deduplication cursor", async ({
  page,
}) => {
  await mockHost(page);
  const cursors: Array<string | null> = [];
  await page.route(`**/api/v1/sessions/${sessionId}/events?*`, (route) => {
    cursors.push(new URL(route.request().url()).searchParams.get("after"));
    return route.fulfill({
      contentType: "text/event-stream",
      body:
        'id: 4\nevent: agent_settled\ndata: {"type":"agent_settled","summary":"Stale settlement"}\n\n' +
        'id: 5\nevent: agent_settled\ndata: {"type":"agent_settled","summary":"Current settlement"}\n\n',
    });
  });
  await page.goto("/");
  await expect(
    page.locator(".transcript-note").filter({ hasText: "Current settlement" }),
  ).toHaveCount(1);
  await expect(
    page.locator(".transcript-note").filter({ hasText: "Stale settlement" }),
  ).toHaveCount(0);
  expect(cursors[0]).toBe("4");
});

test("a failed snapshot is visible and retry restores history before connecting", async ({
  page,
}) => {
  await mockHost(page);
  let failed = true;
  let connections = 0;
  await page.route(`**/api/v1/sessions/${sessionId}/events?*`, (route) => {
    connections++;
    return route.fallback();
  });
  await page.route(`**/api/v1/sessions/${sessionId}/snapshot`, (route) => {
    if (failed)
      return route.fulfill({
        status: 503,
        json: { error: "Snapshot unavailable" },
      });
    return route.fallback();
  });
  await page.goto("/");
  await expect(page.locator(".error-banner")).toContainText(
    "Snapshot unavailable",
  );
  expect(connections).toBe(0);
  failed = false;
  await page.getByRole("button", { name: "Retry connection" }).click();
  await expect.poll(() => connections).toBeGreaterThan(0);
  await expect(page.locator(".error-banner")).toHaveCount(0);
});

test("a rejected permission answer keeps the dialog available for retry", async ({
  page,
}) => {
  await mockHost(page);
  await page.route(`**/api/v1/sessions/${sessionId}/snapshot`, (route) =>
    route.fulfill({
      json: {
        sessionId,
        messages: [],
        lastSeq: 4,
        events: [],
        pendingDialog: {
          requestId: "permission-1",
          ui: {
            purpose: "permission",
            question: "Allow test operation?",
            options: [{ title: "Allow once", value: "allow_once" }],
          },
        },
      },
    }),
  );
  await page.route(`**/api/v1/sessions/${sessionId}/commands`, (route) => {
    const body = route.request().postDataJSON();
    if (body.type === "ui_dialog_response")
      return route.fulfill({
        json: {
          type: "response",
          command: body.type,
          success: false,
          error: "Permission response rejected",
        },
      });
    return route.fallback();
  });
  await page.goto("/");
  await page.getByRole("button", { name: "Allow once", exact: true }).click();
  await expect(page.locator(".error-banner")).toContainText(
    "Permission response rejected",
  );
  await expect(
    page.getByRole("heading", { name: "Allow test operation?" }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Allow once", exact: true }),
  ).toBeEnabled();
});

for (const action of ["fork", "clone"] as const) {
  test(`a late ${action} response preserves subsequent project navigation`, async ({
    page,
  }) => {
    await mockHost(page);
    let release!: () => void;
    const held = new Promise<void>((resolve) => {
      release = resolve;
    });
    let pending = false;
    await page.route(
      `**/api/v1/sessions/${sessionId}/${action}`,
      async (route) => {
        pending = true;
        await held;
        return route.fulfill({
          status: 201,
          json: { sessionId: `${action}-new` },
        });
      },
    );
    page.on("dialog", (dialog) => dialog.accept("Derived chat"));
    await page.goto("/");
    await page
      .getByRole("button", {
        name:
          action === "fork" ? "Fork current session" : "Clone current session",
        exact: true,
      })
      .click();
    await expect.poll(() => pending).toBe(true);
    await page
      .getByRole("button", { name: "Open project: rpi-package", exact: true })
      .click();
    const finished = page.waitForResponse((res) =>
      res.url().endsWith(`/${action}`),
    );
    release();
    await (await finished).finished();
    await page
      .getByRole("combobox", { name: "Language", exact: true })
      .selectOption("zh");
    expect(new URL(page.url()).searchParams.has("session")).toBe(false);
    await expect(page.locator(".toolbar .breadcrumb")).toContainText(
      "rpi-package",
    );
  });
}

test("late file contents cannot replace the newly selected project preview", async ({
  page,
}) => {
  await mockHost(page);
  let release!: () => void;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  let pending = false;
  await page.route("**/api/v1/projects/project-a/file?*", async (route) => {
    pending = true;
    await held;
    return route.fulfill({
      json: { path: "README.md", content: "Stale project A contents" },
    });
  });
  await page.goto("/");
  const workbench = page.getByRole("complementary", {
    name: "Project files and changes",
  });
  await workbench
    .getByRole("button", { name: "README.md", exact: true })
    .click();
  await expect.poll(() => pending).toBe(true);
  await page
    .getByRole("button", { name: "Open project: rpi-package", exact: true })
    .click();
  await workbench
    .getByRole("button", { name: "README.md", exact: true })
    .click();
  await expect(workbench.getByLabel("File contents")).toContainText(
    "Package project",
  );
  const finished = page.waitForResponse((res) =>
    res.url().includes("project-a/file?"),
  );
  release();
  await (await finished).finished();
  await page
    .getByRole("combobox", { name: "Language", exact: true })
    .selectOption("zh");
  await expect(page.getByLabel("文件内容")).not.toContainText(
    "Stale project A contents",
  );
});

test("compaction notes survive pagination and update when language changes", async ({
  page,
}) => {
  await mockHost(page);
  await page.route(`**/api/v1/sessions/${sessionId}/snapshot`, (route) =>
    route.fulfill({
      json: {
        sessionId,
        messages: [
          {
            role: "compaction",
            entryId: "current-compaction",
            tokensBefore: 100,
            retainedMessages: 2,
          },
        ],
        events: [],
        lastSeq: 4,
        firstId: "current-compaction",
        hasMore: true,
      },
    }),
  );
  await page.route(`**/api/v1/sessions/${sessionId}/commands`, (route) => {
    const body = route.request().postDataJSON();
    if (body.type === "get_messages")
      return route.fulfill({
        json: response(
          {
            messages: [
              {
                role: "compaction",
                entryId: "older-compaction",
                tokensBefore: 50,
                retainedMessages: 1,
              },
            ],
            hasMore: false,
          },
          body.type,
        ),
      });
    return route.fallback();
  });
  await page.goto("/");
  await page.getByRole("button", { name: "Load earlier messages" }).click();
  await expect(
    page
      .locator(".transcript-note")
      .filter({ hasText: "Conversation compacted" }),
  ).toHaveCount(2);
  await page
    .getByRole("combobox", { name: "Language", exact: true })
    .selectOption("zh");
  await expect(
    page.locator(".transcript-note").filter({ hasText: "会话已压缩" }),
  ).toHaveCount(2);
});

test("markdown keeps executable URLs and raw HTML inert", async ({ page }) => {
  await mockHost(page);
  await page.route(`**/api/v1/sessions/${sessionId}/snapshot`, (route) =>
    route.fulfill({
      json: {
        sessionId,
        events: [],
        lastSeq: 4,
        messages: [
          {
            role: "assistant",
            entryId: "unsafe-markdown",
            content: [
              {
                type: "text",
                text: '[Unsafe link](javascript:alert(1))\n\n<script>window.markdownExecuted=true</script>\n\n<img src=x onerror="window.markdownExecuted=true">\n\n```html\n<script>literal code</script>\n```',
              },
            ],
          },
        ],
      },
    }),
  );
  await page.goto("/");
  await expect(page.locator(".markdown-body pre code")).toContainText(
    "<script>literal code</script>",
  );
  expect(
    await page.getByRole("link", { name: "Unsafe link" }).getAttribute("href"),
  ).not.toMatch(/^javascript:/i);
  await expect(
    page.locator(".markdown-body script, .markdown-body img"),
  ).toHaveCount(0);
  expect(
    await page.evaluate(
      () =>
        (window as unknown as { markdownExecuted?: boolean }).markdownExecuted,
    ),
  ).toBeUndefined();
});

test("provider changes clear secret drafts and an old save cannot clear a new draft", async ({
  page,
}) => {
  await mockHost(page);
  let release!: () => void;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  let pending = false;
  await page.route(
    `**/api/v1/sessions/${sessionId}/commands`,
    async (route) => {
      const body = route.request().postDataJSON();
      if (body.type === "get_auth_status")
        return route.fulfill({
          json: response(
            {
              providers: ["alpha", "beta"].map((provider) => ({
                provider,
                ready: false,
                source: "none",
                oauth: false,
                oauthAvailable: false,
              })),
            },
            body.type,
          ),
        });
      if (body.type === "set_api_key") {
        pending = true;
        await held;
        return route.fulfill({
          json: response({ provider: body.provider, ready: true }, body.type),
        });
      }
      return route.fallback();
    },
  );
  await page.goto("/");
  await page.getByRole("button", { name: "Toggle inspector" }).click();
  await page.locator(".auth-provider").filter({ hasText: "alpha" }).click();
  await page
    .getByLabel("API key for alpha", { exact: true })
    .fill("test-only-old-key");
  await page.getByRole("button", { name: "Save key", exact: true }).click();
  await expect.poll(() => pending).toBe(true);
  await page
    .getByLabel("API key for alpha", { exact: true })
    .fill("test-only-new-draft");
  const finished = page.waitForResponse(
    (res) =>
      res.url().endsWith("/commands") &&
      res.request().postDataJSON().type === "set_api_key",
  );
  release();
  await (await finished).finished();
  await page
    .getByRole("combobox", { name: "Language", exact: true })
    .selectOption("zh");
  await expect(page.locator("input[type=password]")).toHaveValue(
    "test-only-new-draft",
  );
  await page.locator(".auth-provider").filter({ hasText: "beta" }).click();
  await expect(page.locator("input[type=password]")).toHaveValue("");
});

test("rewinding the current chat reloads its native transcript", async ({
  page,
}) => {
  await mockHost(page);
  let rewound = false;
  let snapshots = 0;
  await page.route(`**/api/v1/sessions/${sessionId}/snapshot`, (route) => {
    snapshots++;
    return route.fulfill({
      json: {
        sessionId,
        lastSeq: 4,
        events: [],
        messages: [
          {
            role: "user",
            entryId: "visible-entry",
            content: rewound ? "Rewound transcript" : "Original transcript",
          },
        ],
      },
    });
  });
  await page.route(`**/api/v1/sessions/${sessionId}/commands`, (route) => {
    const body = route.request().postDataJSON();
    if (body.type === "get_history")
      return route.fulfill({
        json: response(
          {
            records: [
              {
                id: "op-1",
                seq: 4,
                type: "operation_finished",
                sourceLeafId: "older-leaf",
                outcome: "completed",
              },
            ],
            total: 1,
            hasMore: false,
          },
          body.type,
        ),
      });
    if (body.type === "navigate") {
      rewound = true;
      return route.fulfill({
        json: response({ outcome: "completed" }, body.type),
      });
    }
    return route.fallback();
  });
  page.on("dialog", (dialog) => dialog.accept());
  await page.goto("/");
  await expect(page.locator(".message.user")).toContainText(
    "Original transcript",
  );
  await page.getByRole("button", { name: "Toggle inspector" }).click();
  await page.locator(".history-timeline-item summary").click();
  await page.getByRole("button", { name: "Rewind", exact: true }).click();
  await expect(page.locator(".message.user")).toContainText(
    "Rewound transcript",
  );
  expect(snapshots).toBeGreaterThan(1);
});

test("an old prompt acknowledgement does not block a new chat or unlock its pending send", async ({
  page,
}) => {
  await mockHost(page);
  const releases: Array<() => void> = [];
  const pending: string[] = [];
  await page.route("**/api/v1/sessions/*/commands", async (route) => {
    const body = route.request().postDataJSON();
    if (body.type !== "prompt") return route.fallback();
    const id = new URL(route.request().url()).pathname.split("/")[4];
    const held = new Promise<void>((resolve) => {
      releases.push(resolve);
    });
    pending.push(id);
    await held;
    return route.fulfill({ json: response({ accepted: true }, body.type) });
  });
  await page.goto("/");
  const composer = page.getByRole("textbox", {
    name: "Send a prompt, or type / for commands…",
  });
  await composer.fill("First chat request");
  await composer.press("Enter");
  await expect.poll(() => pending.length).toBe(1);
  await page
    .getByRole("button", { name: "New session", exact: false })
    .first()
    .click();
  await expect(
    page.getByRole("heading", { name: "New session" }),
  ).toBeVisible();
  await composer.fill("Second chat request");
  await composer.press("Enter");
  await expect.poll(() => pending).toEqual([sessionId, "session-2"]);
  const oldFinished = page.waitForResponse(
    (res) =>
      res.url().includes(`/${sessionId}/commands`) &&
      res.request().postDataJSON().type === "prompt",
  );
  releases[0]();
  await (await oldFinished).finished();
  await composer.fill("Do not submit twice");
  await composer.press("Enter");
  const newFinished = page.waitForResponse(
    (res) =>
      res.url().includes("/session-2/commands") &&
      res.request().postDataJSON().type === "prompt",
  );
  releases[1]();
  await (await newFinished).finished();
  expect(pending).toEqual([sessionId, "session-2"]);
});

test("consecutive permission dialogs remain independent of delayed acknowledgements", async ({
  page,
}) => {
  await mockHost(page);
  let emitNext!: () => void;
  const nextDialog = new Promise<void>((resolve) => {
    emitNext = resolve;
  });
  const releases: Array<() => void> = [];
  await page.route(`**/api/v1/sessions/${sessionId}/snapshot`, (route) =>
    route.fulfill({
      json: {
        sessionId,
        messages: [],
        lastSeq: 4,
        events: [],
        pendingDialog: {
          requestId: "first",
          ui: {
            purpose: "permission",
            question: "First question",
            options: [{ title: "Answer first", value: true }],
          },
        },
      },
    }),
  );
  await page.route(
    `**/api/v1/sessions/${sessionId}/events?*`,
    async (route) => {
      await nextDialog;
      return route.fulfill({
        contentType: "text/event-stream",
        body:
          'id: 5\nevent: ui_dialog_closed\ndata: {"type":"ui_dialog_closed","requestId":"first"}\n\n' +
          'id: 6\nevent: ui_dialog_request\ndata: {"type":"ui_dialog_request","requestId":"second","ui":{"purpose":"permission","question":"Second question","options":[{"title":"Answer second","value":true}]}}\n\n',
      });
    },
  );
  await page.route(
    `**/api/v1/sessions/${sessionId}/commands`,
    async (route) => {
      const body = route.request().postDataJSON();
      if (body.type !== "ui_dialog_response") return route.fallback();
      const held = new Promise<void>((resolve) => {
        releases.push(resolve);
      });
      if (body.requestId === "first") emitNext();
      await held;
      return route.fulfill({ json: response({}, body.type) });
    },
  );
  await page.goto("/");
  await page.getByRole("button", { name: "Answer first" }).click();
  const second = page.getByRole("button", { name: "Answer second" });
  await expect(second).toBeEnabled();
  await second.click();
  await expect.poll(() => releases.length).toBe(2);
  const firstFinished = page.waitForResponse(
    (res) =>
      res.url().endsWith("/commands") &&
      res.request().postDataJSON().requestId === "first",
  );
  releases[0]();
  await (await firstFinished).finished();
  await page
    .getByRole("combobox", { name: "Language", exact: true })
    .selectOption("zh");
  await expect(
    page.getByRole("heading", { name: "Second question" }),
  ).toBeVisible();
  await expect(second).toBeDisabled();
  releases[1]();
  await expect(second).toBeEnabled();
});

test("pending queue submission preserves the next draft and prevents duplicate delivery", async ({
  page,
}) => {
  await mockHost(page);
  let release!: () => void;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  const messages: string[] = [];
  await page.route(
    `**/api/v1/sessions/${sessionId}/commands`,
    async (route) => {
      const body = route.request().postDataJSON();
      if (body.type !== "queue_message") return route.fallback();
      messages.push(body.message);
      if (messages.length === 1) await held;
      return route.fulfill({ json: response({ accepted: true }, body.type) });
    },
  );
  await page.goto("/");
  await page.getByRole("button", { name: "Toggle inspector" }).click();
  const input = page.getByRole("textbox", { name: "Queue a follow-up…" });
  await input.fill("First queued item");
  await input.press("Enter");
  await expect.poll(() => messages.length).toBe(1);
  await input.press("Enter");
  await input.fill("Next unsent draft");
  const finished = page.waitForResponse(
    (res) =>
      res.url().endsWith("/commands") &&
      res.request().postDataJSON().type === "queue_message",
  );
  release();
  await (await finished).finished();
  await expect(input).toHaveValue("Next unsent draft");
  await input.press("Enter");
  await expect
    .poll(() => messages)
    .toEqual(["First queued item", "Next unsent draft"]);
});

test("an archive response respects restoration from another tab", async ({
  page,
}) => {
  await page.clock.install();
  await mockHost(page);
  let archived = false;
  let release!: () => void;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  const calls: boolean[] = [];
  await page.route("**/api/v1/sessions", (route) => {
    if (route.request().method() !== "GET") return route.fallback();
    return route.fulfill({ json: { sessions: [{ ...session, archived }] } });
  });
  await page.route(
    `**/api/v1/sessions/${sessionId}/metadata`,
    async (route) => {
      archived = route.request().postDataJSON().archived;
      calls.push(archived);
      if (archived) await held;
      return route.fulfill({ json: { sessionId, archived } });
    },
  );
  await page.goto("/");
  const actions = page.getByRole("button", {
    name: "Session actions: Mock session",
    exact: true,
  });
  await actions.click();
  await page.getByRole("menuitem", { name: "Archive", exact: true }).click();
  await expect.poll(() => calls.length).toBe(1);
  await page.clock.fastForward(5001);
  await page
    .getByRole("button", { name: "Runtime settings", exact: true })
    .click();
  expect(calls).toEqual([true]);
  // Another tab restores the catalog before this tab receives its acknowledgement.
  archived = false;
  const refreshed = page.waitForResponse(
    (res) =>
      res.url().endsWith("/sessions") && res.request().method() === "GET",
  );
  release();
  await (await refreshed).finished();
  await expect(page.locator(".inspector h2")).toHaveText("Runtime settings");
  await expect(
    page.getByRole("heading", { name: "Mock session", exact: true }),
  ).toBeVisible();
  expect(new URL(page.url()).searchParams.get("session")).toBe(sessionId);
});

for (const endpoint of ["auth", "capabilities"]) {
  test(`failed initial ${endpoint} is retryable without opening a partial workspace`, async ({
    page,
  }) => {
    await mockHost(page);
    let failed = true;
    await page.route(`**/api/v1/${endpoint}`, (route) =>
      failed
        ? route.fulfill({
            status: 503,
            json: { error: "temporarily unavailable" },
          })
        : route.fallback(),
    );
    await page.goto("/");
    await expect(
      page.getByRole("button", { name: "Retry access check" }),
    ).toBeVisible();
    await expect(page.locator(".workspace")).toHaveCount(0);
    failed = false;
    await page.getByRole("button", { name: "Retry access check" }).click();
    await expect(
      page.getByRole("heading", { name: "Mock session", exact: true }),
    ).toBeVisible();
  });
}

test("user and tool result events preserve assistant replies and synchronize another tab's message", async ({
  page,
}) => {
  await mockHost(page);
  const events = [
    {
      type: "message_start",
      message: {
        role: "assistant",
        content: [{ type: "text", text: "Keep assistant reply" }],
      },
    },
    {
      type: "message_end",
      message: {
        role: "assistant",
        content: [{ type: "text", text: "Keep assistant reply" }],
      },
    },
    {
      type: "message_start",
      message: { role: "user", content: "Other tab user" },
    },
    {
      type: "message_end",
      message: { role: "user", content: "Other tab user" },
    },
    {
      type: "message_end",
      message: {
        role: "toolResult",
        toolCallId: "tool-1",
        content: [{ type: "text", text: "Tool-only result" }],
      },
    },
  ];
  await page.route(`**/api/v1/sessions/${sessionId}/events?*`, (route) =>
    route.fulfill({
      contentType: "text/event-stream",
      body: events
        .map(
          (event, index) =>
            `id: ${index + 5}\nevent: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`,
        )
        .join(""),
    }),
  );
  await page.goto("/");
  await expect(page.locator(".message.assistant")).toHaveText(
    /Keep assistant reply/,
  );
  await expect(page.locator(".message.user")).toHaveCount(1);
  await expect(page.locator(".message.user")).toContainText("Other tab user");
  await expect(page.locator(".message.assistant")).not.toContainText(
    "Tool-only result",
  );
});

test("snapshot overlap neither duplicates committed messages nor downgrades completed tools", async ({
  page,
}) => {
  await mockHost(page);
  const assistant = {
    role: "assistant",
    timestamp: 42,
    content: [
      { type: "text", text: "Already committed" },
      { type: "toolCall", id: "call-1", name: "read", arguments: {} },
    ],
  };
  await page.route(`**/api/v1/sessions/${sessionId}/snapshot`, (route) =>
    route.fulfill({
      json: {
        sessionId,
        lastSeq: 9,
        messages: [
          { ...assistant, entryId: "durable-1" },
          {
            role: "toolResult",
            timestamp: 43,
            entryId: "durable-2",
            toolCallId: "call-1",
            content: [{ type: "text", text: "Completed result" }],
          },
        ],
        history: {
          records: [
            {
              type: "operation_finished",
              id: "history-1",
              runId: "run-overlap",
              outcome: "completed",
            },
          ],
          hasMore: false,
        },
        events: [
          {
            seq: 5,
            event: {
              type: "message_start",
              message: { ...assistant, content: [] },
            },
          },
          { seq: 6, event: { type: "message_end", message: assistant } },
          {
            seq: 7,
            event: {
              type: "tool_execution_start",
              toolCallId: "call-1",
              toolName: "read",
            },
          },
          {
            seq: 8,
            event: {
              type: "tool_execution_update",
              toolCallId: "call-1",
              toolName: "read",
              partialResult: "Incomplete result",
            },
          },
          {
            seq: 9,
            event: {
              type: "agent_settled",
              runId: "run-overlap",
              summary: "Completed once",
            },
          },
        ],
      },
    }),
  );
  await page.goto("/");
  await expect(page.locator(".message.assistant")).toHaveCount(1);
  await expect(page.locator(".tool-card.tool-done")).toHaveCount(1);
  await page.locator(".tool-card summary").click();
  await expect(page.locator(".tool-card")).toContainText("Completed result");
  await expect(page.locator(".transcript-note")).toHaveText(["Completed once"]);
});

test("native permission dialog blocks global new-chat shortcuts", async ({
  page,
}) => {
  await mockHost(page);
  let created = 0;
  await page.route("**/api/v1/sessions", (route) => {
    if (route.request().method() === "POST") created++;
    return route.fallback();
  });
  await page.route(`**/api/v1/sessions/${sessionId}/snapshot`, (route) =>
    route.fulfill({
      json: {
        sessionId,
        messages: [],
        lastSeq: 4,
        events: [],
        pendingDialog: {
          requestId: "pending",
          ui: { purpose: "permission", question: "Pending authorization" },
        },
      },
    }),
  );
  await page.goto("/");
  await expect(
    page.getByRole("dialog", { name: "Pending authorization" }),
  ).toBeVisible();
  await page.keyboard.press("Control+n");
  await expect(
    page.getByRole("dialog", { name: "Pending authorization" }),
  ).toBeVisible();
  expect(created).toBe(0);
});

test("project creation retries finish opening the existing project without creating it twice", async ({
  page,
}) => {
  await mockHost(page);
  let created = 0;
  let failed = true;
  await page.route("**/api/v1/projects", (route) => {
    if (route.request().method() === "POST") {
      created++;
      return route.fallback();
    }
    if (created && failed)
      return route.fulfill({
        status: 503,
        json: { error: "Project refresh unavailable" },
      });
    return route.fallback();
  });
  await page.goto("/");
  await page
    .getByRole("button", { name: "Add project", exact: true })
    .first()
    .click();
  const dialog = page.getByRole("dialog", { name: "Add project", exact: true });
  await dialog.getByLabel("Project name").fill("recoverable-project");
  await dialog
    .getByRole("button", { name: "Add project and start chat" })
    .click();
  await expect(dialog.getByRole("alert")).toContainText(
    "Project refresh unavailable",
  );
  failed = false;
  await dialog
    .getByRole("button", { name: "Continue opening project" })
    .click();
  await expect(dialog).toHaveCount(0);
  await expect(page.locator(".toolbar .breadcrumb")).toContainText(
    "recoverable-project",
  );
  expect(created).toBe(1);
});

test("a late failed chat creation does not mark the subsequently selected project as failed", async ({
  page,
}) => {
  await mockHost(page);
  let release!: () => void;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  let pending = false;
  await page.route("**/api/v1/sessions", async (route) => {
    if (route.request().method() !== "POST") return route.fallback();
    pending = true;
    await held;
    return route.fulfill({
      status: 503,
      json: { error: "Old creation failed" },
    });
  });
  await page.goto("/");
  await page
    .getByRole("button", { name: "New session", exact: false })
    .first()
    .click();
  await expect.poll(() => pending).toBe(true);
  await page
    .getByRole("button", { name: "Open project: rpi-package", exact: true })
    .click();
  const finished = page.waitForResponse(
    (res) =>
      res.url().endsWith("/sessions") && res.request().method() === "POST",
  );
  release();
  await (await finished).finished();
  await page
    .getByRole("combobox", { name: "Language", exact: true })
    .selectOption("zh");
  await expect(page.locator(".toolbar .breadcrumb")).toContainText(
    "rpi-package",
  );
  await expect(page.locator(".error-banner")).toHaveCount(0);
});

for (const failure of [false, true]) {
  test(`creating in another project keeps the active context until ${failure ? "failure" : "success"}`, async ({
    page,
  }) => {
    await mockHost(page);
    let release!: () => void;
    const held = new Promise<void>((resolve) => {
      release = resolve;
    });
    let pending = false;
    await page.route("**/api/v1/sessions", async (route) => {
      if (route.request().method() !== "POST") return route.fallback();
      pending = true;
      expect(route.request().postDataJSON().projectId).toBe("project-b");
      await held;
      if (failure)
        return route.fulfill({
          status: 503,
          json: { error: "Creation unavailable" },
        });
      return route.fallback();
    });
    await page.goto("/");
    await expect(page.locator(".toolbar h1")).toHaveText("Mock session");
    await page
      .getByRole("button", { name: "New session in: rpi-package", exact: true })
      .click();
    await expect.poll(() => pending).toBe(true);
    await expect(page.locator(".toolbar .breadcrumb")).toContainText("pi-rust");
    await expect(page.locator(".toolbar h1")).toHaveText("Mock session");
    release();
    if (failure) {
      await expect(page.locator(".error-banner")).toContainText(
        "Creation unavailable",
      );
      await expect(page.locator(".toolbar .breadcrumb")).toContainText(
        "pi-rust",
      );
      await expect(page.locator(".toolbar h1")).toHaveText("Mock session");
    } else {
      await expect(page.locator(".toolbar .breadcrumb")).toContainText(
        "rpi-package",
      );
      await expect(page.locator(".toolbar h1")).toHaveText("New session");
    }
  });
}

test("an older initial session list cannot select a chat missing from the refreshed list", async ({
  page,
}) => {
  await mockHost(page);
  let release!: () => void;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  let calls = 0;
  await page.route("**/api/v1/sessions", async (route) => {
    if (route.request().method() !== "GET") return route.fallback();
    if (++calls === 1) {
      await held;
      return route.fulfill({ json: { sessions: [session] } });
    }
    return route.fulfill({
      json: {
        sessions: [
          { ...session, sessionId: "session-2", title: "Current chat" },
        ],
      },
    });
  });
  await page.goto("/");
  await expect.poll(() => calls).toBe(1);
  await page
    .getByRole("button", { name: "Refresh chats", exact: true })
    .click();
  await expect.poll(() => calls).toBe(2);
  release();
  await expect(page.locator(".toolbar h1")).toHaveText("Current chat");
  await expect(page).toHaveURL(/session=session-2/);
});

test("a boundary snapshot buffers live assistant events while restoring a native-only user prompt", async ({
  page,
}) => {
  await mockHost(page);
  let release!: () => void;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  let snapshots = 0;
  await page.route(
    `**/api/v1/sessions/${sessionId}/snapshot`,
    async (route) => {
      if (++snapshots === 1) return route.fallback();
      await held;
      return route.fulfill({
        json: {
          sessionId,
          messages: [
            {
              role: "user",
              content: "Native-only prompt",
              entryId: "native-user",
              timestamp: 123,
            },
          ],
          lastSeq: 5,
          events: [],
        },
      });
    },
  );
  const events = [
    { type: "agent_start" },
    {
      type: "message_start",
      message: {
        role: "assistant",
        content: [{ type: "text", text: "Buffered response" }],
      },
    },
    {
      type: "message_end",
      message: {
        role: "assistant",
        content: [{ type: "text", text: "Buffered response" }],
      },
    },
  ];
  await page.route(`**/api/v1/sessions/${sessionId}/events?*`, (route) =>
    route.fulfill({
      contentType: "text/event-stream",
      body: events
        .map(
          (event, index) =>
            `id: ${index + 5}\nevent: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`,
        )
        .join(""),
    }),
  );
  await page.goto("/");
  await expect.poll(() => snapshots).toBe(2);
  release();
  await expect(page.locator(".message.user")).toContainText(
    "Native-only prompt",
  );
  await expect(page.locator(".message.assistant")).toHaveCount(1);
  await expect(page.locator(".message.assistant")).toContainText(
    "Buffered response",
  );
});

test("run boundary refresh preserves loaded earlier messages and their pagination cursor", async ({
  page,
}) => {
  await mockHost(page);
  let release!: () => void;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  let snapshots = 0;
  await page.route(`**/api/v1/sessions/${sessionId}/snapshot`, (route) => {
    snapshots++;
    return route.fulfill({
      json: {
        sessionId,
        messages: [
          { role: "user", entryId: "current", content: "Current prompt" },
        ],
        firstId: "current",
        hasMore: true,
        lastSeq: 4,
        events: [],
      },
    });
  });
  await page.route(`**/api/v1/sessions/${sessionId}/commands`, (route) => {
    if (route.request().postDataJSON().type !== "get_messages")
      return route.fallback();
    return route.fulfill({
      json: response(
        {
          messages: [
            { role: "user", entryId: "earliest", content: "Earlier prompt" },
          ],
          firstId: "earliest",
          hasMore: false,
        },
        "get_messages",
      ),
    });
  });
  await page.route(
    `**/api/v1/sessions/${sessionId}/events?*`,
    async (route) => {
      await held;
      return route.fulfill({
        contentType: "text/event-stream",
        body: 'id: 5\nevent: agent_settled\ndata: {"type":"agent_settled","runId":"run-2","outcome":"completed"}\n\n',
      });
    },
  );
  await page.goto("/");
  await page.getByRole("button", { name: "Load earlier messages" }).click();
  await expect(page.locator(".message.user")).toContainText([
    "Earlier prompt",
    "Current prompt",
  ]);
  release();
  await expect.poll(() => snapshots).toBeGreaterThanOrEqual(2);
  await expect(page.locator(".message.user")).toContainText([
    "Earlier prompt",
    "Current prompt",
  ]);
  await expect(
    page.getByRole("button", { name: "Load earlier messages" }),
  ).toHaveCount(0);
});

test("a failed chat creation does not hide rejection of the still-active prompt", async ({
  page,
}) => {
  await mockHost(page);
  let rejectPrompt!: () => void;
  const held = new Promise<void>((resolve) => {
    rejectPrompt = resolve;
  });
  let pending = false;
  await page.route(
    `**/api/v1/sessions/${sessionId}/commands`,
    async (route) => {
      if (route.request().postDataJSON().type !== "prompt")
        return route.fallback();
      pending = true;
      await held;
      return route.fulfill({
        json: {
          type: "response",
          command: "prompt",
          success: false,
          error: "Prompt was rejected",
        },
      });
    },
  );
  await page.route("**/api/v1/sessions", (route) =>
    route.request().method() === "POST"
      ? route.fulfill({ status: 503, json: { error: "Creation unavailable" } })
      : route.fallback(),
  );
  await page.goto("/");
  const composer = page.locator(".composer textarea");
  await composer.fill("Keep my rejected prompt");
  await composer.press("Enter");
  await expect.poll(() => pending).toBe(true);
  await page
    .getByRole("button", { name: "New session in: rpi-package", exact: true })
    .click();
  await expect(page.locator(".error-banner")).toContainText(
    "Creation unavailable",
  );
  rejectPrompt();
  await expect(composer).toHaveValue("Keep my rejected prompt");
  await expect(page.locator(".error-banner")).toContainText(
    "Prompt was rejected",
  );
  await expect(page.locator(".message.user")).toHaveCount(0);
  await expect(page.locator(".composer button.danger")).toBeDisabled();
});

test("snapshot boundary status supersedes an earlier idle runtime read", async ({
  page,
}) => {
  await mockHost(page);
  await page.route(`**/api/v1/sessions/${sessionId}/snapshot`, (route) =>
    route.fulfill({
      json: {
        sessionId,
        messages: [],
        events: [],
        lastSeq: 5,
        status: "running",
        pendingDialog: null,
      },
    }),
  );
  await page.goto("/");
  await expect(page.locator(".composer button.danger")).toBeEnabled();
  await expect(page.locator(".composer-queue-hint")).toBeVisible();
});

for (const target of ["chat", "project"]) {
  test(`reselecting the active ${target} preserves pending prompt failure recovery`, async ({
    page,
  }) => {
    await mockHost(page);
    let reject!: () => void;
    const held = new Promise<void>((resolve) => {
      reject = resolve;
    });
    let pending = false;
    await page.route(
      `**/api/v1/sessions/${sessionId}/commands`,
      async (route) => {
        if (route.request().postDataJSON().type !== "prompt")
          return route.fallback();
        pending = true;
        await held;
        return route.fulfill({
          json: {
            type: "response",
            command: "prompt",
            success: false,
            error: "Rejected active prompt",
          },
        });
      },
    );
    await page.goto("/");
    const composer = page.locator(".composer textarea");
    await composer.fill("Recover this draft");
    await composer.press("Enter");
    await expect.poll(() => pending).toBe(true);
    if (target === "chat")
      await page
        .locator(".project-session-link")
        .filter({ hasText: "Mock session" })
        .click();
    else
      await page
        .getByRole("button", { name: "Open project: pi-rust", exact: true })
        .click();
    reject();
    await expect(composer).toHaveValue("Recover this draft");
    await expect(page.locator(".error-banner")).toContainText(
      "Rejected active prompt",
    );
    await expect(page.locator(".message.user")).toHaveCount(0);
  });
}

for (const source of ["snapshot", "live"]) {
  test(`parallel permission requests survive ${source} delivery and reload in order`, async ({
    page,
  }) => {
    await mockHost(page);
    const dialogs = ["first", "second"].map((requestId) => ({
      type: "ui_dialog_request",
      requestId,
      ui: {
        purpose: "permission",
        question: `Question ${requestId}`,
        options: [{ title: `Answer ${requestId}`, value: true }],
      },
    }));
    const answered = new Set<string>();
    let requestsSent = false;
    let firstCloseReserved = false;
    let releaseFirst!: () => void;
    let releaseSecond!: () => void;
    const first = new Promise<void>((resolve) => {
      releaseFirst = resolve;
    });
    const second = new Promise<void>((resolve) => {
      releaseSecond = resolve;
    });
    await page.route(`**/api/v1/sessions/${sessionId}/snapshot`, (route) => {
      const pending =
        source === "snapshot" || answered.size
          ? dialogs.filter((dialog) => !answered.has(dialog.requestId))
          : [];
      return route.fulfill({
        json: {
          sessionId,
          messages: [],
          events: [],
          lastSeq: answered.size
            ? 6 + answered.size
            : source === "live"
              ? 4
              : 6,
          status: pending.length ? "awaiting_input" : "running",
          pendingDialogs: pending,
          pendingDialog: pending[0] ?? null,
        },
      });
    });
    await page.route(
      `**/api/v1/sessions/${sessionId}/events?*`,
      async (route) => {
        const after = Math.max(
          Number(new URL(route.request().url()).searchParams.get("after")),
          Number(route.request().headers()["last-event-id"] ?? 0),
        );
        if (source === "live" && after < 6 && !requestsSent) {
          requestsSent = true;
          return route.fulfill({
            contentType: "text/event-stream",
            body:
              "retry: 10\n" +
              dialogs
                .map(
                  (dialog, index) =>
                    `id: ${index + 5}\nevent: ui_dialog_request\ndata: ${JSON.stringify(dialog)}\n\n`,
                )
                .join(""),
          });
        }
        const closeFirst = after < 7 && !firstCloseReserved;
        if (closeFirst) firstCloseReserved = true;
        await (closeFirst ? first : second);
        const requestId = closeFirst ? "first" : "second";
        return route.fulfill({
          contentType: "text/event-stream",
          body: `retry: 10\nid: ${closeFirst ? 7 : 8}\nevent: ui_dialog_closed\ndata: ${JSON.stringify({ type: "ui_dialog_closed", requestId })}\n\n`,
        });
      },
    );
    await page.route(`**/api/v1/sessions/${sessionId}/commands`, (route) => {
      const body = route.request().postDataJSON();
      if (body.type !== "ui_dialog_response") return route.fallback();
      answered.add(body.requestId);
      if (body.requestId === "first") releaseFirst();
      else releaseSecond();
      return route.fulfill({ json: response({}, body.type) });
    });
    await page.goto("/");
    await expect(
      page.getByRole("heading", { name: "Question first" }),
    ).toBeVisible();
    await expect(
      page.getByRole("button", { name: "Answer second" }),
    ).toHaveCount(0);
    await page.getByRole("button", { name: "Answer first" }).click();
    await expect(
      page.getByRole("heading", { name: "Question second" }),
    ).toBeVisible();
    await page.reload();
    await expect(
      page.getByRole("heading", { name: "Question second" }),
    ).toBeVisible();
    await page.getByRole("button", { name: "Answer second" }).click();
    await expect(page.locator(".dialog-backdrop")).toHaveCount(0);
    expect([...answered]).toEqual(["first", "second"]);
  });
}

test("a rejected compact command releases the composer and restores its draft", async ({
  page,
}) => {
  await mockHost(page);
  let release!: () => void;
  let pending = false;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.route("**/api/v1/capabilities", (route) =>
    route.fulfill({
      json: {
        ...capabilities,
        supportedCommands: [...capabilities.supportedCommands, "compact"],
      },
    }),
  );
  await page.route(
    `**/api/v1/sessions/${sessionId}/commands`,
    async (route) => {
      if (route.request().postDataJSON().type !== "compact")
        return route.fallback();
      pending = true;
      await held;
      return route.fulfill({
        json: {
          type: "response",
          command: "compact",
          success: false,
          error: "Cannot compact yet",
        },
      });
    },
  );
  await page.goto("/");
  const input = page.locator(".composer textarea");
  await input.fill("/compact keep recent context");
  await input.press("Enter");
  await expect.poll(() => pending).toBe(true);
  await expect(page.locator(".connection")).toContainText("reconnecting");
  await expect(page.locator(".composer button.danger")).toBeEnabled();
  release();
  await expect(page.locator(".error-banner")).toContainText(
    "Cannot compact yet",
  );
  await expect(page.locator(".composer button.danger")).toBeDisabled();
  await expect(input).toHaveValue("/compact keep recent context");
  await expect(page.locator(".connection")).toContainText("reconnecting");
});

test("a rejected resume keeps its recovery action and releases the busy composer", async ({
  page,
}) => {
  await mockHost(page);
  let release!: () => void;
  let pending = false;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.route("**/api/v1/capabilities", (route) =>
    route.fulfill({
      json: {
        ...capabilities,
        supportedCommands: [
          ...capabilities.supportedCommands,
          "resume_pending",
        ],
      },
    }),
  );
  await page.route(`**/api/v1/sessions/${sessionId}/snapshot`, (route) =>
    route.fulfill({
      json: {
        sessionId,
        messages: [],
        lastSeq: 4,
        events: [],
        pendingResume: true,
      },
    }),
  );
  await page.route(
    `**/api/v1/sessions/${sessionId}/commands`,
    async (route) => {
      if (route.request().postDataJSON().type !== "resume_pending")
        return route.fallback();
      pending = true;
      await held;
      return route.fulfill({
        json: {
          type: "response",
          command: "resume_pending",
          success: false,
          error: "Resume unavailable",
        },
      });
    },
  );
  await page.goto("/");
  await page.getByRole("button", { name: "Continue", exact: true }).click();
  await expect.poll(() => pending).toBe(true);
  await expect(page.locator(".connection")).toContainText("reconnecting");
  await expect(page.locator(".composer button.danger")).toBeEnabled();
  release();
  await expect(page.locator(".error-banner")).toContainText(
    "Resume unavailable",
  );
  await expect(
    page.getByRole("button", { name: "Continue", exact: true }),
  ).toBeEnabled();
  await expect(page.locator(".composer button.danger")).toBeDisabled();
});

test("an executed slash command does not return as a draft after switching chats", async ({
  page,
}) => {
  await mockHost(page);
  await page.goto("/");
  const input = page.locator(".composer textarea");
  await input.fill("/help");
  await input.press("Enter");
  await expect(input).toHaveValue("");
  await page
    .getByRole("button", { name: "New session", exact: false })
    .first()
    .click();
  await expect(page.locator(".toolbar h1")).toHaveText("New session");
  await page
    .locator(".project-session-link")
    .filter({ hasText: "Mock session" })
    .click();
  await expect(page.locator(".toolbar h1")).toHaveText("Mock session");
  await expect(input).toHaveValue("");
});

test("late message pagination from before rewind cannot enter the restored branch", async ({
  page,
}) => {
  await mockHost(page);
  let rewound = false;
  let release!: () => void;
  let pending = false;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.route(`**/api/v1/sessions/${sessionId}/snapshot`, (route) =>
    route.fulfill({
      json: {
        sessionId,
        events: [],
        lastSeq: 4,
        hasMore: !rewound,
        firstId: "current",
        messages: [
          {
            role: "user",
            entryId: "current",
            content: rewound ? "Restored branch" : "Before rewind",
          },
        ],
      },
    }),
  );
  await page.route(
    `**/api/v1/sessions/${sessionId}/commands`,
    async (route) => {
      const body = route.request().postDataJSON();
      if (body.type === "get_history")
        return route.fulfill({
          json: response(
            {
              records: [
                {
                  id: "finished",
                  seq: 4,
                  type: "operation_finished",
                  sourceLeafId: "target",
                  outcome: "completed",
                },
              ],
              total: 1,
              hasMore: false,
            },
            body.type,
          ),
        });
      if (body.type === "navigate") {
        rewound = true;
        return route.fulfill({
          json: response({ outcome: "completed" }, body.type),
        });
      }
      if (body.type === "get_messages" && body.beforeId) {
        pending = true;
        await held;
        return route.fulfill({
          json: response(
            {
              messages: [
                {
                  role: "user",
                  entryId: "wrong-branch",
                  content: "Stale branch message",
                },
              ],
              firstId: "wrong-branch",
              hasMore: false,
            },
            body.type,
          ),
        });
      }
      return route.fallback();
    },
  );
  page.on("dialog", (dialog) => dialog.accept());
  await page.goto("/");
  await page.getByRole("button", { name: "Load earlier messages" }).click();
  await expect.poll(() => pending).toBe(true);
  await page.getByRole("button", { name: "Toggle inspector" }).click();
  await page.locator(".history-timeline-item summary").click();
  await page.getByRole("button", { name: "Rewind", exact: true }).click();
  await expect(page.locator(".message.user")).toContainText("Restored branch");
  const finished = page.waitForResponse(
    (res) =>
      res.url().endsWith("/commands") &&
      res.request().postDataJSON().type === "get_messages",
  );
  release();
  await (await finished).finished();
  await page
    .getByRole("combobox", { name: "Language", exact: true })
    .selectOption("zh");
  await expect(page.locator(".message.user")).toHaveCount(1);
  await expect(page.locator(".message.user")).not.toContainText(
    "Stale branch message",
  );
});

test("an operation accepted in another tab stays busy without an agent_start event", async ({
  page,
}) => {
  await mockHost(page);
  await page.route(`**/api/v1/sessions/${sessionId}/events?*`, (route) =>
    route.fulfill({
      contentType: "text/event-stream",
      body: 'id: 5\nevent: operation_accepted\ndata: {"type":"operation_accepted","command":"compact"}\n\n',
    }),
  );
  await page.goto("/");
  await expect(page.locator(".connection")).toContainText("reconnecting");
  await expect(page.locator(".composer button.danger")).toBeEnabled();
  await expect(page.locator(".composer-queue-hint")).toBeVisible();
});

test("live and restored settlement labels follow the selected language", async ({
  page,
}) => {
  await mockHost(page);
  await page.route(`**/api/v1/sessions/${sessionId}/events?*`, (route) =>
    route.fulfill({
      contentType: "text/event-stream",
      body: 'id: 5\nevent: agent_settled\ndata: {"type":"agent_settled","runId":"run-2","outcome":"failed","aborted":true}\n\n',
    }),
  );
  await page.goto("/");
  await expect(
    page
      .locator(".transcript-note")
      .filter({ hasText: "Run failed · aborted" }),
  ).toHaveCount(1);
  await page
    .getByRole("combobox", { name: "Language", exact: true })
    .selectOption("zh");
  await expect(
    page
      .locator(".transcript-note")
      .filter({ hasText: "运行状态：失败 · 已中止" }),
  ).toHaveCount(1);
  await expect(
    page.locator(".transcript-note").filter({ hasText: "运行状态：已完成" }),
  ).toHaveCount(1);
  await page
    .getByRole("combobox", { name: "语言", exact: true })
    .selectOption("en");
  await expect(
    page
      .locator(".transcript-note")
      .filter({ hasText: "Run failed · aborted" }),
  ).toHaveCount(1);
});

test("same model IDs from different providers keep their identity in runtime selection", async ({
  page,
}) => {
  await mockHost(page);
  const catalog = [
    {
      id: "shared-id",
      name: "Model Alpha",
      provider: "alpha",
      contextWindow: 1000,
    },
    {
      id: "shared-id",
      name: "Model Beta",
      provider: "beta",
      contextWindow: 8000,
    },
  ];
  let selected = {
    modelId: "shared-id",
    provider: "alpha",
    thinkingLevel: "off",
  };
  const changes: unknown[] = [];
  await page.route("**/api/v1/capabilities", (route) =>
    route.fulfill({
      json: {
        ...capabilities,
        supportedCommands: [
          ...capabilities.supportedCommands,
          "get_available_models",
          "set_model",
        ],
      },
    }),
  );
  await page.route("**/api/v1/sessions/*/commands", (route) => {
    const body = route.request().postDataJSON();
    if (body.type === "get_available_models")
      return route.fulfill({ json: response({ models: catalog }, body.type) });
    if (body.type === "get_state")
      return route.fulfill({ json: response(selected, body.type) });
    if (body.type === "set_model") {
      changes.push(body);
      selected = {
        ...selected,
        modelId: body.modelId,
        provider: body.provider,
      };
      return route.fulfill({ json: response({}, body.type) });
    }
    return route.fallback();
  });
  await page.goto("/");
  await page
    .getByRole("button", { name: "Runtime settings", exact: true })
    .click();
  const model = page.getByRole("combobox", { name: "Model", exact: true });
  await model.selectOption({ label: "Model Beta (beta)" });
  await expect
    .poll(() => changes)
    .toEqual([
      expect.objectContaining({
        type: "set_model",
        modelId: "shared-id",
        provider: "beta",
      }),
    ]);
  await expect(model.locator("option:checked")).toHaveText("Model Beta (beta)");
  await expect(page.locator(".inspector")).toContainText("8k");
});

test("a late tool change cannot replace the next chat tool catalog", async ({
  page,
}) => {
  await mockHost(page);
  let release!: () => void;
  let pending = false;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.route("**/api/v1/capabilities", (route) =>
    route.fulfill({
      json: {
        ...capabilities,
        supportedCommands: [
          ...capabilities.supportedCommands,
          "get_available_tools",
          "set_active_tools",
        ],
      },
    }),
  );
  await page.route("**/api/v1/sessions/*/commands", async (route) => {
    const body = route.request().postDataJSON();
    const old = route.request().url().includes("/session-1/");
    if (body.type === "get_available_tools")
      return route.fulfill({
        json: response(
          {
            tools: old ? ["old-tool"] : ["new-tool"],
            activeTools: old ? ["old-tool"] : [],
          },
          body.type,
        ),
      });
    if (body.type === "set_active_tools") {
      pending = true;
      await held;
      return route.fulfill({ json: response({}, body.type) });
    }
    return route.fallback();
  });
  await page.goto("/");
  await page
    .getByRole("button", { name: "Runtime settings", exact: true })
    .click();
  await page.getByRole("checkbox", { name: "old-tool", exact: true }).click();
  await expect.poll(() => pending).toBe(true);
  await page
    .getByRole("button", { name: "New session", exact: false })
    .first()
    .click();
  await expect(page.locator(".toolbar h1")).toHaveText("New session");
  await expect(
    page.getByRole("checkbox", { name: "new-tool", exact: true }),
  ).toBeVisible();
  const finished = page.waitForResponse(
    (res) =>
      res.url().includes("/session-1/commands") &&
      res.request().postDataJSON().type === "set_active_tools",
  );
  release();
  await (await finished).finished();
  await page
    .getByRole("combobox", { name: "Language", exact: true })
    .selectOption("zh");
  await expect(
    page.getByRole("checkbox", { name: "new-tool", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("checkbox", { name: "old-tool", exact: true }),
  ).toHaveCount(0);
});

test("model changes reload supported thinking levels and keep authoritative runtime values", async ({
  page,
}) => {
  await mockHost(page);
  const models = [
    { id: "first", name: "First model", provider: "test" },
    { id: "second", name: "Second model", provider: "test" },
  ];
  let state = { modelId: "first", provider: "test", thinkingLevel: "off" };
  let releaseLevels!: () => void;
  let loadingSecondLevels = false;
  const heldLevels = new Promise<void>((resolve) => {
    releaseLevels = resolve;
  });
  await page.route("**/api/v1/capabilities", (route) =>
    route.fulfill({
      json: {
        ...capabilities,
        supportedCommands: [
          ...capabilities.supportedCommands,
          "get_available_models",
          "set_model",
          "get_available_thinking_levels",
          "set_thinking_level",
        ],
      },
    }),
  );
  await page.route("**/api/v1/sessions/*/commands", async (route) => {
    const body = route.request().postDataJSON();
    if (body.type === "get_available_models")
      return route.fulfill({ json: response({ models }, body.type) });
    if (body.type === "get_available_thinking_levels") {
      if (state.modelId === "second") {
        loadingSecondLevels = true;
        await heldLevels;
      }
      return route.fulfill({
        json: response(
          {
            levels:
              state.modelId === "first" ? ["off", "low", "high"] : ["off"],
          },
          body.type,
        ),
      });
    }
    if (body.type === "get_state")
      return route.fulfill({ json: response(state, body.type) });
    if (body.type === "set_model")
      state = { ...state, modelId: body.modelId, thinkingLevel: "off" };
    if (body.type === "set_thinking_level")
      state = { ...state, thinkingLevel: "low" };
    return route.fallback();
  });
  await page.goto("/");
  await page
    .getByRole("button", { name: "Runtime settings", exact: true })
    .click();
  const thinking = page.getByRole("combobox", {
    name: "Thinking level",
    exact: true,
  });
  await thinking.selectOption("high");
  await expect(thinking).toHaveValue("low");
  await page
    .getByRole("combobox", { name: "Model", exact: true })
    .selectOption({ label: "Second model (test)" });
  await expect.poll(() => loadingSecondLevels).toBe(true);
  await expect(thinking).toBeDisabled();
  releaseLevels();
  await expect(thinking).toHaveValue("off");
  await expect(thinking.locator("option")).toHaveText(["—", "off"]);
});

test("slash model selection rejects ambiguous names and accepts provider qualified IDs", async ({
  page,
}) => {
  await mockHost(page);
  const models = [
    { id: "shared", name: "Shared model", provider: "alpha" },
    { id: "shared", name: "Shared model", provider: "beta" },
  ];
  let selected = { modelId: "shared", provider: "alpha" };
  const changes: unknown[] = [];
  await page.route("**/api/v1/capabilities", (route) =>
    route.fulfill({
      json: {
        ...capabilities,
        supportedCommands: [
          ...capabilities.supportedCommands,
          "get_available_models",
          "set_model",
        ],
      },
    }),
  );
  await page.route("**/api/v1/sessions/*/commands", (route) => {
    const body = route.request().postDataJSON();
    if (body.type === "get_available_models")
      return route.fulfill({ json: response({ models }, body.type) });
    if (body.type === "get_state")
      return route.fulfill({ json: response(selected, body.type) });
    if (body.type === "set_model") {
      changes.push(body);
      selected = { modelId: body.modelId, provider: body.provider };
    }
    return route.fallback();
  });
  await page.goto("/");
  const input = page.locator(".composer textarea");
  await input.fill("/model shared");
  await input.press("Enter");
  await expect(page.locator(".transcript-note")).toContainText([
    "No unique model matches",
  ]);
  expect(changes).toEqual([]);
  await input.fill("/model beta/shared");
  await input.press("Enter");
  await expect
    .poll(() => changes)
    .toEqual([
      expect.objectContaining({ provider: "beta", modelId: "shared" }),
    ]);
});

for (const outcome of ["done", "error", "closed"] as const) {
  test(`bash elapsed time advances, freezes after ${outcome}, and survives reload`, async ({
    page,
  }) => {
    await mockHost(page);
    let finished = false;
    let releaseEnd!: () => void;
    const end = new Promise<void>((resolve) => {
      releaseEnd = resolve;
    });
    const messages = () => [
      {
        role: "assistant",
        timestamp: 123,
        content: [
          {
            type: "toolCall",
            id: "bash-timed",
            name: "bash",
            arguments: { command: "sleep 1" },
            webTiming: { elapsedMs: finished ? 1234 : 200, running: !finished },
          },
        ],
      },
      ...(finished
        ? [
            {
              role: "toolResult",
              toolCallId: "bash-timed",
              toolName: "bash",
              isError: outcome !== "done",
              content: [{ type: "text", text: "command output" }],
              webTiming: { elapsedMs: 1234, running: false },
            },
          ]
        : []),
    ];
    await page.route(`**/api/v1/sessions/${sessionId}/snapshot`, (route) =>
      route.fulfill({
        json: {
          sessionId,
          status: finished ? "ready" : "running",
          messages: messages(),
          lastSeq: finished ? 5 : 4,
          events: [],
        },
      }),
    );
    await page.route(
      `**/api/v1/sessions/${sessionId}/events?*`,
      async (route) => {
        await end;
        const event =
          outcome === "closed"
            ? { type: "host_closed" }
            : {
                type: "tool_execution_end",
                toolCallId: "bash-timed",
                toolName: "bash",
                isError: outcome === "error",
                result: "command output",
                webTiming: { elapsedMs: 1234, running: false },
              };
        await route.fulfill({
          contentType: "text/event-stream",
          body: `retry: 60000\nid: 5\nevent: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`,
        });
      },
    );
    await page.goto("/");
    const timer = page.locator(".tool-elapsed");
    await expect(timer).toBeVisible();
    const first = await timer.innerText();
    await expect.poll(() => timer.innerText()).not.toBe(first);
    finished = true;
    releaseEnd();
    await expect(
      page.locator(`.tool-card.tool-${outcome === "done" ? "done" : "error"}`),
    ).toBeVisible();
    if (outcome !== "closed") await expect(timer).toHaveText("1.2s");
    const frozen = await timer.innerText();
    await page.waitForTimeout(350);
    await expect(timer).toHaveText(frozen);
    await page.reload();
    await expect(timer).toHaveText("1.2s");
    await page
      .getByRole("combobox", { name: "Language", exact: true })
      .selectOption("zh");
    await expect(timer).toHaveAccessibleName("耗时 1.2s");
    await page.setViewportSize({ width: 390, height: 844 });
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
    ).toBe(true);
  });
}
