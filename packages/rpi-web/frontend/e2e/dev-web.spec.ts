import { chooseMenuOption } from "./controls";
import { test, expect, type Page, type Route } from "@playwright/test";

const sessionId = "session-1";
// Match the endpoint independently of api.snapshot's optional query parameters.
const snapshotRoute = (url: URL) =>
  url.pathname === `/api/v1/sessions/${sessionId}/snapshot`;

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
  tags: ["frontend"],
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
    { id: "project-a", name: "pi-rust", path: "/workspace/pi-rust", archived: false },
    { id: "project-b", name: "rpi-package", path: "/workspace/rpi-package", archived: false },
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
          path: body.path ?? `/workspace/projects/${body.name}`,
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
    if (path === "/api/v1/project-folders") {
      return route.fulfill({
        json: {
          root: "/workspace/projects",
          folders: [
            { name: "demo-app", path: "demo-app" },
            { name: "rpi-package", path: "rpi-package" },
            { name: "zimt", path: "zimt" },
          ],
        },
      });
    }
    if (/\/projects\/[^/]+\/rename$/.test(path)) {
      const body = request.postDataJSON() as { name: string };
      const id = path.split("/")[4];
      const project = projects.find((entry) => entry.id === id);
      if (!project)
        return route.fulfill({ status: 404, json: { error: "missing" } });
      project.name = body.name;
      return route.fulfill({ json: { project } });
    }
    if (/\/projects\/[^/]+\/metadata$/.test(path)) {
      const body = request.postDataJSON() as { archived: boolean };
      const id = path.split("/")[4];
      const project = projects.find((entry) => entry.id === id);
      if (!project)
        return route.fulfill({ status: 404, json: { error: "missing" } });
      project.archived = body.archived;
      return route.fulfill({ json: { project } });
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
    if (
      /\/projects\/[^/]+\/upload$/.test(path) &&
      request.method() === "POST"
    ) {
      return route.fulfill({
        json: { name: "notes.txt", path: ".rpi/uploads/notes.txt", size: 5 },
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
    if (/\/sessions\/[^/]+$/.test(path) && request.method() === "DELETE") {
      const id = path.split("/")[4];
      const index = sessions.findIndex((entry) => entry.sessionId === id);
      if (index >= 0) sessions.splice(index, 1);
      return route.fulfill({ status: 204, body: "" });
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
              {
                messageCount: 1,
                input: 100,
                output: 252,
                cacheRead: 20,
                cacheWrite: 0,
                totalTokens: 352,
                cachedTokens: 20,
                uncachedTokens: 80,
                cost: 0.01,
                costTotal: 0.01,
              },
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
  await expect(
    page
      .locator(".inspector .queue-item")
      .getByRole("button", { name: "Send now: pending", exact: true }),
  ).toBeVisible();
  const cancel = page.waitForRequest(
    (request) =>
      request.url().endsWith(`/sessions/${sessionId}/commands`) &&
      request.method() === "POST" &&
      request.postDataJSON()?.type === "cancel_queued",
  );
  await page
    .locator(".inspector .queue-item")
    .getByRole("button", { name: "Edit queued message" })
    .click();
  await expect((await cancel).postDataJSON()).toMatchObject({
    entryId: "queue-2",
  });
  await expect(page.locator(".composer textarea")).toHaveValue("pending");
  await expect(page.locator(".composer-queue-editing")).toHaveCount(0);
  await expect(page.locator(".inspector .queue-item")).toHaveCount(0);
});

test("a stopped session can send a selected queued message now", async ({
  page,
}) => {
  await mockHost(page);
  const commands: string[] = [];
  await page.route(
    `**/api/v1/sessions/${sessionId}/commands`,
    async (route) => {
      const body = route.request().postDataJSON();
      if (body.type === "cancel_queued" || body.type === "prompt") {
        commands.push(body.type);
        return route.fulfill({
          json: response(
            body.type === "cancel_queued"
              ? { entryId: "queue-2", outcome: "cancelled" }
              : { accepted: true },
            body.type,
          ),
        });
      }
      return route.fallback();
    },
  );
  await page.goto("/");
  await page.getByRole("button", { name: "Toggle inspector" }).click();
  await page
    .locator(".inspector .queue-item")
    .getByRole("button", { name: "Send now: pending", exact: true })
    .click();
  await expect.poll(() => commands).toEqual(["cancel_queued", "prompt"]);
});
test("failed queue cancellation restores the removed message", async ({
  page,
}) => {
  await mockHost(page);
  let release!: () => void;
  let cancelling = false;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.route(
    `**/api/v1/sessions/${sessionId}/commands`,
    async (route) => {
      const body = route.request().postDataJSON();
      if (body.type !== "cancel_queued") return route.fallback();
      cancelling = true;
      await held;
      return route.fulfill({
        json: {
          ...response({}, body.type),
          success: false,
          error: "Cancellation rejected",
        },
      });
    },
  );
  await page.goto("/");
  await page.getByRole("button", { name: "Toggle inspector" }).click();
  await page
    .locator(".inspector .queue-item")
    .getByRole("button", { name: "Cancel queued message" })
    .click();
  await expect.poll(() => cancelling).toBe(true);
  await expect(page.locator(".inspector .queue-item")).toHaveCount(0);
  release();
  await expect(page.locator(".inspector .queue-item")).toContainText("pending");
  await expect(page.locator(".error-banner")).toContainText(
    "Cancellation rejected",
  );
  expect(errors).toEqual([]);
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
    .getByRole("button", { name: "Expand project: rpi-package", exact: true })
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
    .pressSequentially("New workspace");
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
      name: "Expand project: New workspace",
      exact: true,
    }),
  ).toBeVisible();
});

test("clicking a project folder only expands it without opening a session", async ({
  page,
}) => {
  await mockHost(page);
  await page.goto("/");
  const project = page.getByRole("region", { name: "pi-rust", exact: true });
  const folder = project.locator(".project-name");
  await expect(folder).toHaveAttribute("aria-expanded", "true");
  await folder.click();
  await expect(folder).toHaveAttribute("aria-expanded", "false");
  await expect(project.locator(".project-session-link")).toHaveCount(0);
  await expect(page.getByRole("heading", { name: "Mock session" })).toBeVisible();
  expect(new URL(page.url()).searchParams.get("session")).toBe(sessionId);
  await folder.click();
  await expect(folder).toHaveAttribute("aria-expanded", "true");
});
test("projects can be renamed from the project actions menu", async ({
  page,
}) => {
  await mockHost(page);
  await page.goto("/");
  await page
    .getByRole("button", { name: "Project actions: pi-rust", exact: true })
    .click();
  await page.getByRole("menuitem", { name: "Rename", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "Rename project" });
  await dialog
    .getByLabel("Project name", { exact: true })
    .pressSequentially("renamed-project");
  await dialog.getByRole("button", { name: "Save", exact: true }).click();
  await expect(
    page.getByRole("button", {
      name: "Expand project: renamed-project",
      exact: true,
    }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", {
      name: "Expand project: pi-rust",
      exact: true,
    }),
  ).not.toBeVisible();
});
test("projects can be archived and restored from the project actions menu", async ({
  page,
}) => {
  await mockHost(page);
  await page.goto("/");
  await page
    .getByRole("button", { name: "Project actions: pi-rust", exact: true })
    .click();
  await page.getByRole("menuitem", { name: "Archive project", exact: true }).click();
  await expect(
    page.getByRole("region", { name: "pi-rust", exact: true }),
  ).toHaveCount(0);
  await page.getByRole("button", { name: "Show archived chats", exact: true }).click();
  await expect(
    page.getByRole("region", { name: "pi-rust", exact: true }),
  ).toBeVisible();
  await page
    .getByRole("button", { name: "Project actions: pi-rust", exact: true })
    .click();
  await page.getByRole("menuitem", { name: "Restore project", exact: true }).click();
  await page.getByRole("button", { name: "Show projects", exact: true }).click();
  await expect(
    page.getByRole("region", { name: "pi-rust", exact: true }),
  ).toBeVisible();
});

for (const width of [1280, 390]) {
  test(`project actions are visible without hover at ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 844 });
    await mockHost(page);
    await page.goto("/");
    if (width < 768) {
      await page.getByRole("button", { name: "Toggle sidebar", exact: true }).click();
    }
    await page.mouse.move(width - 1, 843);
    const actions = page.getByRole("button", {
      name: "Project actions: pi-rust",
      exact: true,
    });
    await expect(actions).toBeVisible();
    await expect(actions).toHaveCSS("opacity", "1");
    await actions.click();
    await expect(
      page.getByRole("menuitem", { name: "Archive project", exact: true }),
    ).toBeVisible();
  });
}

test("opening an existing folder is searchable and fills its project name", async ({
  page,
}) => {
  await mockHost(page);
  let createdBody: unknown;
  await page.route("**/api/v1/projects", async (route) => {
    if (route.request().method() === "POST") {
      createdBody = route.request().postDataJSON();
    }
    return route.fallback();
  });
  await page.goto("/");
  await page.getByRole("button", { name: "New project", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "Add project", exact: true });
  await dialog
    .getByRole("button", { name: "Open folder", exact: true })
    .click();
  const folderInput = dialog.getByLabel("Choose a folder", { exact: true });
  await folderInput.fill("zim");
  const zimt = dialog.getByRole("option", { name: /zimt/ });
  await expect(zimt).toBeVisible();
  await expect(dialog.getByRole("option", { name: /demo-app/ })).toHaveCount(0);
  await zimt.click();
  await expect(dialog.getByLabel("Project name", { exact: true })).toHaveValue(
    "zimt",
  );
  await dialog
    .getByLabel("Project name", { exact: true })
    .fill("Zimt workspace");
  await folderInput.fill("demo");
  await dialog.getByRole("option", { name: /demo-app/ }).click();
  await expect(dialog.getByLabel("Project name", { exact: true })).toHaveValue(
    "Zimt workspace",
  );
  await dialog
    .getByRole("button", { name: "Add project and start chat" })
    .click();
  await expect
    .poll(() => createdBody)
    .toEqual({
      name: "Zimt workspace",
      initGit: true,
      path: "demo-app",
    });
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

test("session tags can be edited without sidebar filters", async ({ page }) => {
  await mockHost(page);
  await page.goto("/");
  await page
    .getByRole("button", { name: "Session actions: Mock session", exact: true })
    .click();
  await page.getByRole("menuitem", { name: "Edit tags", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "Edit tags" });
  await dialog.getByLabel("Tags").fill("frontend, urgent, frontend");
  await dialog.getByRole("button", { name: "Save tags", exact: true }).click();
  await expect(
    page.locator(".session-tag", { hasText: "urgent" }),
  ).toBeVisible();
});
test("sessions can be permanently deleted from the session actions menu", async ({
  page,
}) => {
  await mockHost(page);
  await page.goto("/");
  await page
    .getByRole("button", { name: "Session actions: Mock session", exact: true })
    .click();
  await page.getByRole("menuitem", { name: "Delete permanently", exact: true }).click();
  await expect(
    page.getByRole("dialog", { name: "Delete session permanently" }),
  ).toContainText("This cannot be undone.");
  await page
    .getByRole("dialog", { name: "Delete session permanently" })
    .getByRole("button", { name: "Delete permanently", exact: true })
    .click();
  await expect(page.getByRole("heading", { name: "Mock session" })).toHaveCount(0);
});
test("project navigation collapses and remembers appearance", async ({
  page,
}) => {
  await mockHost(page);
  await page.goto("/");
  await page
    .getByRole("button", { name: "Collapse project: pi-rust", exact: true })
    .click();
  await expect(page.locator(".project-session")).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "Expand project: pi-rust", exact: true }).first(),
  ).toBeVisible();
  await page
    .getByRole("button", { name: "Expand project: pi-rust", exact: true })
    .first()
    .click();
  await expect(page.locator(".project-session")).toHaveCount(1);
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
  await expect(page.locator(".mobile-session-switcher small")).toHaveText(
    "pi-rust",
  );
  expect(
    await page
      .locator(".mobile-session-switcher svg")
      .evaluate((element) => getComputedStyle(element).width),
  ).toBe("16px");
  await page
    .getByRole("button", { name: "Switch session", exact: true })
    .click();
  await expect(page.locator(".mobile-session-sheet")).toBeVisible();
  await expect(page.locator(".mobile-session-search")).not.toBeFocused();
  await expect(
    page.locator(".mobile-session-option").filter({ hasText: "Mock session" }),
  ).toBeVisible();
  await page.getByRole("button", { name: "Close panel", exact: true }).click();
  expect(
    await page
      .locator(".composer")
      .first()
      .evaluate((element) => getComputedStyle(element).borderRadius),
  ).toBe("14px");
  expect(
    await page
      .locator(".toolbar")
      .evaluate((element) => element.scrollWidth <= element.clientWidth),
  ).toBe(true);

  await page
    .getByRole("button", { name: "Toggle sidebar", exact: true })
    .click();
  await expect(
    page.getByRole("button", { name: "Expand project: pi-rust", exact: true }),
  ).toBeVisible();
  expect(
    await page
      .locator(".project-tree")
      .evaluate((element) => element.scrollWidth <= element.clientWidth),
  ).toBe(true);
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
  await page.setViewportSize({ width: 1280, height: 900 });
  await expect(page.locator(".composer").first()).toHaveCSS(
    "border-radius",
    "17px",
  );
});

test("mobile session actions keep the ellipsis control compact", async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await mockHost(page);
  await page.goto("/");
  await page
    .getByRole("button", { name: "Toggle sidebar", exact: true })
    .click();
  const row = page.locator(".project-session").first();
  const trigger = row.getByRole("button", {
    name: "Session actions: Mock session",
    exact: true,
  });
  await expect(trigger).toBeVisible();
  const timeAndMenu = await row.evaluate((element) => {
    const time = element.querySelector("time")?.getBoundingClientRect();
    const menu = element
      .querySelector(".session-menu-trigger")
      ?.getBoundingClientRect();
    return {
      timeRight: time?.right ?? 0,
      menuLeft: menu?.left ?? 0,
    };
  });
  expect(timeAndMenu.timeRight).toBeLessThanOrEqual(timeAndMenu.menuLeft);
  expect(
    await row
      .locator(".session-menu")
      .evaluate((element) => element.getBoundingClientRect().width),
  ).toBeLessThanOrEqual(50);
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= window.innerWidth,
    ),
  ).toBe(true);
  await trigger.click();
  await expect(
    page.getByRole("menuitem", { name: "Rename", exact: true }),
  ).toBeVisible();
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
    snapshotRoute,
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
  await page.route(snapshotRoute, (route) =>
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
  await expect(prose.locator(".markdown-code-heading")).toContainText("Code");
  await expect(prose.locator(".markdown-copy").first()).toHaveAttribute(
    "aria-label",
    "Copy code",
  );
  await prose.locator(".markdown-copy").first().click();
  await expect(
    page.getByRole("button", { name: "Copy as Markdown", exact: true }).first(),
  ).toBeVisible();
  await page.getByRole("button", { name: "Copy as Markdown", exact: true }).first().click();
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
  await expect(
    page.locator(".message.assistant .message-copy").first(),
  ).toBeVisible();
  await expect(page.locator(".message.user .message-body")).toBeVisible();
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

test("uploads a file and includes its workspace path in the prompt", async ({
  page,
}) => {
  await mockHost(page);
  await page.goto("/");
  await expect(
    page.getByRole("heading", { name: "Mock session" }),
  ).toBeVisible();
  await page.locator('input[type="file"]').setInputFiles({
    name: "notes.txt",
    mimeType: "text/plain",
    buffer: Buffer.from("hello"),
  });
  await expect(page.locator(".composer-attachment")).toContainText("notes.txt");
  await page.locator(".composer textarea").fill("Please inspect this file");
  const request = page.waitForRequest(
    (candidate) =>
      candidate.url().includes(`/sessions/${sessionId}/commands`) &&
      candidate.method() === "POST" &&
      candidate.postDataJSON()?.type === "prompt",
  );
  await page.getByRole("button", { name: "Send message" }).click();
  await expect((await request).postDataJSON()).toMatchObject({
    message: expect.stringContaining(".rpi/uploads/notes.txt"),
  });
});

test("previews image attachments when the browser omits the MIME type", async ({
  page,
}) => {
  await mockHost(page);
  await page.goto("/");
  await expect(
    page.getByRole("heading", { name: "Mock session" }),
  ).toBeVisible();
  // setInputFiles infers MIME from the filename, even when mimeType is omitted.
  // Dispatch a real File with an explicitly empty type to exercise the fallback.
  const fileType = await page
    .locator('input[type="file"]')
    .evaluate((input: HTMLInputElement, base64) => {
      const bytes = Uint8Array.from(atob(base64), (char) => char.charCodeAt(0));
      const transfer = new DataTransfer();
      transfer.items.add(new File([bytes], "Screenshot.png", { type: "" }));
      input.files = transfer.files;
      const type = input.files[0].type;
      input.dispatchEvent(new Event("change", { bubbles: true }));
      return type;
    }, "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=");
  expect(fileType).toBe("");
  const preview = page.locator(".composer-attachment img");
  await expect(preview).toHaveCount(1);
  await expect
    .poll(() =>
      preview.evaluate((image: HTMLImageElement) => image.naturalWidth),
    )
    .toBeGreaterThan(0);
});

test("renders image blocks returned in the transcript", async ({ page }) => {
  await mockHost(page);
  await page.route(snapshotRoute, (route) =>
    route.fulfill({
      json: {
        sessionId,
        messages: [
          {
            role: "assistant",
            entryId: "image-assistant",
            content: [
              {
                type: "image",
                data: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=",
                mimeType: "image/png",
              },
              {
                type: "image_url",
                image_url: {
                  url: "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=",
                },
              },
            ],
          },
        ],
        events: [],
        lastSeq: 1,
        total: 1,
        hasMore: false,
      },
    }),
  );
  await page.goto("/");
  const images = page.locator(".transcript-image img");
  await expect(images).toHaveCount(2);
  for (const image of await images.all()) {
    await expect
      .poll(() =>
        image.evaluate((element: HTMLImageElement) => element.naturalWidth),
      )
      .toBeGreaterThan(0);
  }
});
const imagePng =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";
const nativeUserImage = {
  role: "user",
  entryId: "user-image",
  timestamp: 1_700_000_000_000,
  content: [
    { type: "text", text: "Inspect this image" },
    { type: "image", data: imagePng, mimeType: "image/png" },
  ],
};

test("Markdown data images decode without allowing unsafe links or HTML", async ({
  page,
}) => {
  await mockHost(page);
  await page.route(snapshotRoute, (route) =>
    route.fulfill({
      json: {
        sessionId,
        messages: [
          {
            role: "assistant",
            entryId: "markdown-image",
            content: [
              {
                type: "text",
                text: [
                  `![inline PNG](data:image/png;base64,${imagePng})`,
                  `[image link](data:image/png;base64,${imagePng})`,
                  "[unsafe link](data:text/html;base64,PHNjcmlwdD5hbGVydCgxKTwvc2NyaXB0Pg==)",
                  "[script link](javascript:alert%281%29)",
                  "![unsafe image](data:text/html;base64,PHNjcmlwdD5hbGVydCgxKTwvc2NyaXB0Pg==)",
                  '<img src="/raw-html-image.png" onerror="alert(1)">',
                ].join("\n\n"),
              },
            ],
          },
        ],
        events: [],
        lastSeq: 4,
        total: 1,
        hasMore: false,
      },
    }),
  );
  await page.goto("/");
  const image = page.getByAltText("inline PNG");
  await expect(image).toHaveAttribute(
    "src",
    `data:image/png;base64,${imagePng}`,
  );
  await expect
    .poll(() => image.evaluate((e: HTMLImageElement) => e.naturalWidth))
    .toBeGreaterThan(0);
  for (const label of ["unsafe link", "script link", "image link"]) {
    await expect(
      page.locator(".prose a").filter({ hasText: label }),
    ).not.toHaveAttribute("href", /^(data|javascript):/i);
  }
  await expect(page.getByAltText("unsafe image")).not.toHaveAttribute(
    "src",
    /^data:/i,
  );
  await expect(page.locator('img[src="/raw-html-image.png"]')).toHaveCount(0);
});

for (const imageOnly of [false, true]) {
  test(`user ${imageOnly ? "image-only" : "text and image"} messages survive snapshot and reload`, async ({
    page,
  }) => {
    await mockHost(page);
    await page.route(snapshotRoute, (route) =>
      route.fulfill({
        json: {
          sessionId,
          messages: [
            {
              ...nativeUserImage,
              content: imageOnly
                ? nativeUserImage.content.slice(1)
                : nativeUserImage.content,
            },
          ],
          events: [],
          lastSeq: 4,
          total: 1,
          hasMore: false,
        },
      }),
    );
    for (const reload of [false, true]) {
      if (reload) await page.reload();
      else await page.goto("/");
      const images = page.locator(".transcript-image img");
      await expect(images).toHaveCount(1);
      await expect
        .poll(() =>
          images.first().evaluate((e: HTMLImageElement) => e.naturalWidth),
        )
        .toBeGreaterThan(0);
      if (!imageOnly)
        await expect(
          page.getByText("Inspect this image", { exact: true }),
        ).toHaveCount(1);
    }
  });
}

test("live user images render at start and are replaced rather than duplicated at end", async ({
  page,
}) => {
  await mockHost(page);
  let finish!: () => void;
  const ended = new Promise<void>((resolve) => {
    finish = resolve;
  });
  await page.route(snapshotRoute, (route) =>
    route.fulfill({
      json: {
        sessionId,
        messages: [],
        events: [
          {
            seq: 5,
            event: { type: "message_start", message: nativeUserImage },
          },
        ],
        lastSeq: 5,
        total: 0,
        hasMore: false,
      },
    }),
  );
  await page.route(
    `**/api/v1/sessions/${sessionId}/events**`,
    async (route) => {
      await ended;
      await route.fulfill({
        contentType: "text/event-stream",
        body: `event: ready\ndata: {"type":"ready"}\n\nid: 6\nevent: message_end\ndata: ${JSON.stringify({ type: "message_end", message: { ...nativeUserImage, content: [{ type: "text", text: "Image received" }, nativeUserImage.content[1]] } })}\n\n`,
      });
    },
  );
  try {
    await page.goto("/");
    const images = page.locator(".transcript-image img");
    await expect(images).toHaveCount(1);
    finish();
    await expect(
      page.getByText("Image received", { exact: true }),
    ).toBeVisible();
    await expect(images).toHaveCount(1);
    await expect(
      page.getByText("Inspect this image", { exact: true }),
    ).toHaveCount(0);
    await expect
      .poll(() =>
        images.first().evaluate((e: HTMLImageElement) => e.naturalWidth),
      )
      .toBeGreaterThan(0);
  } finally {
    finish();
  }
});

test("snapshot overlap does not duplicate committed user images", async ({
  page,
}) => {
  await mockHost(page);
  await page.route(snapshotRoute, (route) =>
    route.fulfill({
      json: {
        sessionId,
        messages: [nativeUserImage],
        events: [
          {
            seq: 5,
            event: { type: "message_start", message: nativeUserImage },
          },
          { seq: 6, event: { type: "message_end", message: nativeUserImage } },
        ],
        lastSeq: 6,
        total: 1,
        hasMore: false,
      },
    }),
  );
  await page.goto("/");
  await expect(page.locator(".transcript-image img")).toHaveCount(1);
  await expect(
    page.getByText("Inspect this image", { exact: true }),
  ).toHaveCount(1);
});

for (const source of [
  "/artifacts/theme-design-mobile-390.png",
  "artifacts/theme-design-mobile-390.png",
  "./artifacts/theme-design-mobile-390.png",
  "http://127.0.0.1:4191/artifacts/theme-design-mobile-390.png",
]) {
  test(`project image URLs resolve and decode instead of receiving SPA HTML: ${source}`, async ({
    page,
    baseURL,
  }) => {
    const imageSource = source.replace("http://127.0.0.1:4191", baseURL!);
    await mockHost(page);
    await page.route(
      (url) => url.pathname === "/api/v1/projects/project-a/image",
      (route) => {
        expect(new URL(route.request().url()).searchParams.get("path")).toBe(
          "artifacts/theme-design-mobile-390.png",
        );
        return route.fulfill({
          contentType: "image/png",
          body: Buffer.from(imagePng, "base64"),
        });
      },
    );
    await page.route(snapshotRoute, (route) =>
      route.fulfill({
        json: {
          sessionId,
          messages: [
            {
              role: "assistant",
              entryId: "local-image",
              content: [
                { type: "text", text: `![Project image](${imageSource})` },
                { type: "image_url", image_url: { url: imageSource } },
              ],
            },
          ],
          events: [],
          lastSeq: 4,
          total: 1,
          hasMore: false,
        },
      }),
    );
    await page.goto("/");
    const markdown = page.getByAltText("Project image").first();
    await expect(markdown).toHaveAttribute(
      "src",
      "/api/v1/projects/project-a/image?path=artifacts%2Ftheme-design-mobile-390.png",
    );
    await expect
      .poll(() => markdown.evaluate((e: HTMLImageElement) => e.naturalWidth))
      .toBeGreaterThan(0);
    const native = page.locator(".transcript-image img");
    await expect
      .poll(() => native.evaluate((e: HTMLImageElement) => e.naturalWidth))
      .toBeGreaterThan(0);
    await markdown.focus();
    await markdown.press("Enter");
    const viewer = page.getByRole("dialog", {
      name: "Image viewer",
      exact: true,
    });
    await expect(viewer).toBeVisible();
    await expect
      .poll(() =>
        viewer.locator("img").evaluate((e: HTMLImageElement) => e.naturalWidth),
      )
      .toBeGreaterThan(0);
  });
}

test("project image resolution isolates identical paths across projects and reload", async ({
  page,
}) => {
  await mockHost(page);
  await page.route(
    (url) => url.pathname === "/api/v1/sessions",
    (route) =>
      route.fulfill({
        json: {
          sessions: [
            session,
            {
              ...session,
              sessionId: "session-2",
              projectId: "project-b",
              title: "Other image session",
            },
          ],
        },
      }),
  );
  await page.route(
    (url) => /^\/api\/v1\/projects\/project-[ab]\/image$/.test(url.pathname),
    (route) =>
      route.fulfill({
        contentType: "image/png",
        body: Buffer.from(imagePng, "base64"),
      }),
  );
  await page.route(
    (url) => /\/sessions\/[^/]+\/snapshot$/.test(url.pathname),
    (route) =>
      route.fulfill({
        json: {
          sessionId: route.request().url().includes("session-2")
            ? "session-2"
            : sessionId,
          messages: [
            {
              role: "assistant",
              entryId: "same-path",
              content: [
                { type: "text", text: "![Scoped image](/artifacts/same.png)" },
              ],
            },
          ],
          events: [],
          lastSeq: 4,
          total: 1,
          hasMore: false,
        },
      }),
  );
  await page.goto("/");
  await expect(page.getByAltText("Scoped image")).toHaveAttribute(
    "src",
    /\/projects\/project-a\/image\?/,
  );
  const expand = page
    .getByRole("region", { name: "rpi-package", exact: true })
    .locator(".project-collapse");
  if ((await expand.getAttribute("aria-expanded")) === "false")
    await expand.click();
  await page.getByRole("button", { name: /^Other image session/ }).click();
  await expect(page.getByAltText("Scoped image")).toHaveAttribute(
    "src",
    /\/projects\/project-b\/image\?/,
  );
  await page.reload();
  await expect(page.getByAltText("Scoped image")).toHaveAttribute(
    "src",
    /\/projects\/project-b\/image\?/,
  );
});

test("project image resolution rejects traversal and unsupported schemes without rewriting external images", async ({
  page,
}) => {
  await mockHost(page);
  let resourceRequests = 0;
  await page.route(
    (url) => url.pathname.endsWith("/image"),
    (route) => {
      resourceRequests++;
      return route.fulfill({
        status: 403,
        json: { error: "unexpected local image request" },
      });
    },
  );
  await page.route("https://example.com/external.png", (route) =>
    route.fulfill({
      contentType: "image/png",
      body: Buffer.from(imagePng, "base64"),
    }),
  );
  await page.route(snapshotRoute, (route) =>
    route.fulfill({
      json: {
        sessionId,
        messages: [
          {
            role: "assistant",
            entryId: "blocked-images",
            content: [
              {
                type: "text",
                text: [
                  "![Parent](../outside.png)",
                  "![Encoded parent](artifacts/%2e%2e/outside.png)",
                  "![Local file](file:///private/outside.png)",
                  "![Script](javascript:alert%281%29)",
                  "![External](https://example.com/external.png)",
                ].join("\n\n"),
              },
            ],
          },
        ],
        events: [],
        lastSeq: 4,
        total: 1,
        hasMore: false,
      },
    }),
  );
  await page.goto("/");
  for (const alt of ["Parent", "Encoded parent", "Local file", "Script"]) {
    await expect(page.getByAltText(alt, { exact: true })).not.toHaveAttribute(
      "src",
      /.+/,
    );
  }
  await expect(page.getByAltText("External")).toHaveAttribute(
    "src",
    "https://example.com/external.png",
  );
  await expect
    .poll(() =>
      page
        .getByAltText("External")
        .evaluate((e: HTMLImageElement) => e.naturalWidth),
    )
    .toBeGreaterThan(0);
  expect(resourceRequests).toBe(0);
});

async function mockImageViewer(page: Page) {
  await mockHost(page);
  const photo = (alt: string) => ({
    type: "image",
    data: imagePng,
    mimeType: "image/png",
    alt,
  });
  await page.route("**/viewer-large.svg", (route) =>
    route.fulfill({
      contentType: "image/svg+xml",
      body: '<svg xmlns="http://www.w3.org/2000/svg" width="3000" height="2000"><rect width="3000" height="2000" fill="#387aa5"/></svg>',
    }),
  );
  await page.route(snapshotRoute, (route) =>
    route.fulfill({
      json: {
        sessionId,
        messages: [
          {
            role: "user",
            entryId: "viewer-user",
            content: [photo("User photo")],
          },
          {
            role: "assistant",
            entryId: "viewer-assistant",
            content: [
              photo("Assistant photo"),
              {
                type: "text",
                text: "![Markdown photo](/viewer-large.svg)\n\n[![Linked photo](/viewer-large.svg)](/should-not-navigate)",
              },
            ],
          },
          {
            role: "toolResult",
            entryId: "viewer-tool",
            toolCallId: "photo-tool",
            toolName: "read",
            content: [photo("Tool photo")],
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

for (const label of [
  "User photo",
  "Assistant photo",
  "Tool photo",
  "Markdown photo",
  "Linked photo",
]) {
  test(`image viewer opens ${label} without navigation and restores focus`, async ({
    page,
  }) => {
    await mockImageViewer(page);
    let navigations = 0;
    await page.route("**/should-not-navigate", (route) => {
      navigations++;
      return route.fulfill({
        contentType: "text/html",
        body: "Unexpected navigation",
      });
    });
    await page.goto("/");
    if (label === "Tool photo")
      await page.locator(".tool-card > summary").click();
    const thumbnail = page.getByAltText(label).first();
    await thumbnail.scrollIntoViewIfNeeded();
    await expect
      .poll(() => thumbnail.evaluate((e: HTMLImageElement) => e.naturalWidth))
      .toBeGreaterThan(0);
    const source = await thumbnail.evaluate(
      (e: HTMLImageElement) => e.currentSrc,
    );
    const overflow = await page.evaluate(() => document.body.style.overflow);
    await thumbnail.click();
    const viewer = page.getByRole("dialog", {
      name: "Image viewer",
      exact: true,
    });
    await expect(viewer).toBeVisible();
    await expect(viewer.locator("img")).toHaveAttribute("src", source);
    await expect
      .poll(() =>
        viewer.locator("img").evaluate((e: HTMLImageElement) => e.naturalWidth),
      )
      .toBeGreaterThan(0);
    expect(await page.evaluate(() => document.body.style.overflow)).toBe(
      "hidden",
    );
    await viewer.locator("img").click();
    await expect(viewer).toBeVisible();
    await viewer.getByRole("button", { name: "Close dialog" }).click();
    await expect(viewer).toHaveCount(0);
    await expect(thumbnail).toBeFocused();
    expect(await page.evaluate(() => document.body.style.overflow)).toBe(
      overflow,
    );
    expect(new URL(page.url()).pathname).toBe("/");
    expect(navigations).toBe(0);
  });
}

test("image viewer supports keyboard open, trapped focus, Escape and backdrop close", async ({
  page,
}) => {
  await mockImageViewer(page);
  await page.goto("/");
  const thumbnail = page.getByAltText("User photo").first();
  await thumbnail.focus();
  await page.keyboard.press("Enter");
  const viewer = page.getByRole("dialog", {
    name: "Image viewer",
    exact: true,
  });
  await expect(viewer).toBeVisible();
  await expect(
    viewer.getByRole("button", { name: "Close dialog" }),
  ).toBeFocused();
  await page.keyboard.press("Tab");
  await expect(
    viewer.getByRole("button", { name: "Original size" }),
  ).toBeFocused();
  await page.keyboard.press("Tab");
  await expect(
    viewer.getByRole("button", { name: "Close dialog" }),
  ).toBeFocused();
  await page.keyboard.press("Shift+Tab");
  await expect(
    viewer.getByRole("button", { name: "Original size" }),
  ).toBeFocused();
  await page.keyboard.press("Shift+Tab");
  await expect(
    viewer.getByRole("button", { name: "Close dialog" }),
  ).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(viewer).toHaveCount(0);
  await expect(thumbnail).toBeFocused();
  await page.keyboard.press("Space");
  await expect(viewer).toBeVisible();
  await page.mouse.click(2, 2);
  await expect(viewer).toHaveCount(0);
  await expect(thumbnail).toBeFocused();
});

test("image viewer fits a mobile viewport and toggles original size in Chinese", async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.addInitScript(() =>
    localStorage.setItem("rpi-web-language", "zh"),
  );
  await mockImageViewer(page);
  await page.goto("/");
  await page.getByAltText("Markdown photo").click();
  const viewer = page.getByRole("dialog", { name: "图片查看器", exact: true });
  await expect(viewer).toBeVisible();
  const image = viewer.locator("img");
  await expect
    .poll(() => image.evaluate((e: HTMLImageElement) => e.naturalWidth))
    .toBe(3000);
  const fitted = await image.boundingBox();
  expect(fitted!.width).toBeLessThan(390);
  expect(fitted!.height).toBeLessThan(844);
  await viewer.getByRole("button", { name: "原始尺寸" }).click();
  await expect
    .poll(() =>
      image.evaluate((e: HTMLImageElement) => e.getBoundingClientRect().width),
    )
    .toBe(3000);
  const viewport = viewer.locator(".image-viewer-viewport");
  expect(await viewport.evaluate((e) => e.scrollWidth > e.clientWidth)).toBe(
    true,
  );
  await viewer.getByRole("button", { name: "适应窗口" }).click();
  await expect
    .poll(() =>
      image.evaluate((e: HTMLImageElement) => e.getBoundingClientRect().width),
    )
    .toBeLessThan(390);
  await viewer.getByRole("button", { name: "关闭对话框" }).click();
  await expect(viewer).toHaveCount(0);
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
  await chooseMenuOption(page, "语言", "en");
  await expect(
    inspector.getByRole("heading", { name: "Provider auth" }),
  ).toBeVisible();
  await expect(inspector).toContainText(
    "Restart this session to reload provider credentials.",
  );
  await expect(
    page.getByRole("button", { name: "Sign out", exact: true }),
  ).toHaveText("");
  await chooseMenuOption(page, "Language", "zh");
  await page.getByRole("button", { name: "关闭面板", exact: true }).click();
  await page.getByRole("button", { name: "运行时设置", exact: true }).click();
  await expect(inspector.getByText("暂无可用工具列表。")).toBeVisible();
  await chooseMenuOption(page, "思考深度", { label: "低" });
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
  await chooseMenuOption(page, "Language", "zh");
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

test("late chat creation preserves the selected project without opening its session", async ({
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
    .getByRole("button", { name: "Expand project: rpi-package", exact: true })
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
  await page.route(snapshotRoute, (route) =>
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
  await chooseMenuOption(page, "Language", "zh");
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
  await chooseMenuOption(page, "Thinking level", "high");
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
  await chooseMenuOption(page, "Language", "zh");
  await expect(page.locator(".composer-thinking")).toHaveCount(1);
  await expect(page.locator(".composer-thinking")).toContainText("思考深度");
  await expect(
    page.getByRole("button", { name: "思考深度", exact: true }),
  ).toBeEnabled();
});

test("mobile composer keeps thinking level picker visible", async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 });
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
  await page.route("**/api/v1/sessions/*/commands", async (route) => {
    const body = route.request().postDataJSON();
    if (body.type === "get_available_thinking_levels")
      return route.fulfill({
        json: response({ levels: ["low", "high"] }, body.type),
      });
    return route.fallback();
  });
  await page.goto("/");
  const thinking = page.locator(".composer-thinking");
  await expect(thinking).toBeVisible();
  await expect(thinking).toContainText("Thinking level");
  await thinking.click();
  await expect(
    page.getByRole("listbox", { name: "Choose thinking level", exact: true }),
  ).toBeVisible();
  await page.locator(".dropdown-popover .dropdown-item").first().click();
  await expect(
    page.getByRole("listbox", { name: "Choose thinking level", exact: true }),
  ).toBeHidden();
});

test("snapshot replay keeps a closed dialog and completed recovery state closed", async ({
  page,
}) => {
  await mockHost(page);
  await page.route(snapshotRoute, (route) =>
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
  await page.route(snapshotRoute, (route) => {
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
  await page.route(snapshotRoute, (route) =>
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
    await page.goto("/");
    await page
      .getByRole("button", {
        name:
          action === "fork" ? "Fork current session" : "Clone current session",
        exact: true,
      })
      .click();
    await page.getByRole("dialog").getByRole("textbox").fill("Derived chat");
    await page
      .getByRole("dialog")
      .getByRole("button", { name: "Submit", exact: true })
      .click();
    await expect.poll(() => pending).toBe(true);
    await page
      .getByRole("button", { name: "Expand project: rpi-package", exact: true })
      .click();
    const finished = page.waitForResponse((res) =>
      res.url().endsWith(`/${action}`),
    );
    release();
    await (await finished).finished();
    await chooseMenuOption(page, "Language", "zh");
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
    .getByRole("button", { name: "Expand project: rpi-package", exact: true })
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
  await chooseMenuOption(page, "Language", "zh");
  await expect(page.getByLabel("文件内容")).not.toContainText(
    "Stale project A contents",
  );
});

test("compaction notes survive pagination and update when language changes", async ({
  page,
}) => {
  await mockHost(page);
  await page.route(snapshotRoute, (route) =>
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
  await chooseMenuOption(page, "Language", "zh");
  await expect(
    page.locator(".transcript-note").filter({ hasText: "会话已压缩" }),
  ).toHaveCount(2);
});

test("markdown keeps executable URLs and raw HTML inert", async ({ page }) => {
  await mockHost(page);
  await page.route(snapshotRoute, (route) =>
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
  await chooseMenuOption(page, "Language", "zh");
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
  await page.route(snapshotRoute, (route) => {
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
  await page.goto("/");
  await expect(page.locator(".message.user")).toContainText(
    "Original transcript",
  );
  await page.getByRole("button", { name: "Toggle inspector" }).click();
  await page.locator(".history-timeline-item summary").click();
  await page.getByRole("button", { name: "Rewind", exact: true }).click();
  await page
    .getByRole("dialog")
    .getByRole("button", { name: "Submit", exact: true })
    .click();
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

for (const kind of ["single", "input", "multiple", "cancel"] as const) {
  test(`ask_user ${kind} returns a native structured answer`, async ({
    page,
  }) => {
    await mockHost(page);
    let submitted: Record<string, unknown> | undefined;
    await page.route(snapshotRoute, (route) =>
      route.fulfill({
        json: {
          sessionId,
          status: "awaiting_input",
          messages: [],
          lastSeq: 4,
          events: [],
          pendingDialogs: [
            {
              requestId: "ask-native",
              toolCallId: "ask-1",
              ui: {
                kind: kind === "input" ? "input" : "selector",
                question: "Choose a target",
                context: "Native ask_user test",
                options:
                  kind === "input"
                    ? []
                    : [
                        { title: "Linux", description: "Server build" },
                        { title: "Windows" },
                      ],
                allowMultiple: kind === "multiple",
                allowComment: kind === "multiple",
                allowFreeform: kind === "input",
              },
            },
          ],
        },
      }),
    );
    await page.route(`**/api/v1/sessions/${sessionId}/commands`, (route) => {
      const body = route.request().postDataJSON();
      if (body.type !== "ui_dialog_response") return route.fallback();
      submitted = body;
      return route.fulfill({ json: response({ accepted: true }, body.type) });
    });
    await page.goto("/");
    const dialog = page.getByRole("dialog");
    await expect(
      dialog.getByRole("heading", { name: "Choose a target" }),
    ).toBeVisible();
    if (kind === "cancel") {
      await dialog.getByRole("button", { name: "Cancel", exact: true }).click();
      await expect
        .poll(() => submitted)
        .toMatchObject({ requestId: "ask-native", cancelled: true });
      expect(submitted).not.toHaveProperty("answer");
      return;
    }
    if (kind !== "input")
      await dialog.getByRole("button", { name: "Linux Server build" }).click();
    if (kind === "multiple") {
      await dialog
        .getByRole("button", { name: "Windows", exact: true })
        .click();
      await expect(
        dialog.getByRole("button", { name: "Windows", exact: true }),
      ).toHaveAttribute("aria-pressed", "true");
    }
    if (kind !== "single") {
      await dialog.getByRole("textbox").fill("Deploy to staging");
      await dialog.getByRole("button", { name: "Submit", exact: true }).click();
    }
    const answer =
      kind === "single"
        ? { values: ["Linux"] }
        : kind === "input"
          ? { values: ["Deploy to staging"], text: "Deploy to staging" }
          : { values: ["Linux", "Windows"], text: "Deploy to staging" };
    await expect.poll(() => submitted?.answer).toEqual(answer);
  });
}

test("history renders before the model catalog finishes loading", async ({
  page,
}) => {
  await mockHost(page);
  await page.route("**/api/v1/capabilities", (route) =>
    route.fulfill({
      json: {
        ...capabilities,
        supportedCommands: [
          ...capabilities.supportedCommands,
          "get_available_models",
        ],
      },
    }),
  );
  let release!: () => void;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.route(
    `**/api/v1/sessions/${sessionId}/commands`,
    async (route) => {
      if (route.request().postDataJSON().type !== "get_available_models")
        return route.fallback();
      await held;
      return route.fulfill({ json: response({ models: [] }) });
    },
  );
  await page.goto("/");
  try {
  await expect(page.locator(".transcript-note")).toHaveCount(0);
    await expect(
      page.getByRole("status", { name: "Loading conversation" }),
    ).toBeHidden();
  } finally {
    release();
  }
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
  await page.route(snapshotRoute, (route) =>
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
  // Keep interacting with the active permission modal; its backdrop correctly
  // prevents changing the workspace language while a response is pending.
  await page.setViewportSize({ width: 1100, height: 800 });
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
      {
        type: "toolCall",
        id: "call-1",
        name: "read",
        arguments: { path: "src/main.ts" },
      },
    ],
  };
  await page.route(snapshotRoute, (route) =>
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
            content: [
              {
                type: "text",
                text: JSON.stringify({
                  content: [{ type: "text", text: "const value = 1;" }],
                  details: null,
                  usage: null,
                  terminate: false,
                }),
              },
            ],
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
  await expect(page.locator(".tool-card")).toContainText("const value = 1;");
  await expect(page.locator(".tool-card")).not.toContainText('"content"');
  await expect(page.locator(".read-code-path")).toHaveText("src/main.ts");
  await expect(page.locator(".read-code pre code")).toHaveClass(
    /language-typescript/,
  );
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
  await page.route(snapshotRoute, (route) =>
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
    .getByRole("button", { name: "Expand project: rpi-package", exact: true })
    .click();
  const finished = page.waitForResponse(
    (res) =>
      res.url().endsWith("/sessions") && res.request().method() === "POST",
  );
  release();
  await (await finished).finished();
  await chooseMenuOption(page, "Language", "zh");
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
    snapshotRoute,
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
  await page.route(snapshotRoute, (route) => {
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
  await expect(
    page.getByRole("button", { name: "Abort", exact: true }),
  ).toHaveCount(0);
});

test("snapshot boundary status supersedes an earlier idle runtime read", async ({
  page,
}) => {
  await mockHost(page);
  await page.route(snapshotRoute, (route) =>
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
  await expect(
    page.getByRole("button", { name: "Abort", exact: true }),
  ).toBeEnabled();
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
        .getByRole("button", { name: "Expand project: pi-rust", exact: true })
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
    await page.route(snapshotRoute, (route) => {
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
  await expect(
    page.getByRole("button", { name: "Abort", exact: true }),
  ).toBeEnabled();
  release();
  await expect(page.locator(".error-banner")).toContainText(
    "Cannot compact yet",
  );
  await expect(
    page.getByRole("button", { name: "Abort", exact: true }),
  ).toHaveCount(0);
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
  await page.route(snapshotRoute, (route) =>
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
  await expect(
    page.getByRole("button", { name: "Abort", exact: true }),
  ).toBeEnabled();
  release();
  await expect(page.locator(".error-banner")).toContainText(
    "Resume unavailable",
  );
  await expect(
    page.getByRole("button", { name: "Continue", exact: true }),
  ).toBeEnabled();
  await expect(
    page.getByRole("button", { name: "Abort", exact: true }),
  ).toHaveCount(0);
});

test("drafts survive reload and mark the owning session", async ({ page }) => {
  await mockHost(page);
  await page.goto("/");
  const input = page.locator(".composer textarea");
  await input.fill("Keep this draft");
  await expect(
    page.locator(".project-session").filter({ hasText: "Draft" }),
  ).toBeVisible();
  await page
    .getByRole("button", { name: "New session", exact: false })
    .first()
    .click();
  await expect(input).toHaveValue("");
  await page
    .locator(".project-session-link")
    .filter({ hasText: "Mock session" })
    .click();
  await expect(input).toHaveValue("Keep this draft");
  await page.reload();
  await expect(input).toHaveValue("Keep this draft");
  await expect(
    page.locator(".project-session").filter({ hasText: "Draft" }),
  ).toBeVisible();
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
  await page.route(snapshotRoute, (route) =>
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
  await page.goto("/");
  await page.getByRole("button", { name: "Load earlier messages" }).click();
  await expect.poll(() => pending).toBe(true);
  await page.getByRole("button", { name: "Toggle inspector" }).click();
  await page.locator(".history-timeline-item summary").click();
  await page.getByRole("button", { name: "Rewind", exact: true }).click();
  await page
    .getByRole("dialog")
    .getByRole("button", { name: "Submit", exact: true })
    .click();
  await expect(page.locator(".message.user")).toContainText("Restored branch");
  const finished = page.waitForResponse(
    (res) =>
      res.url().endsWith("/commands") &&
      res.request().postDataJSON().type === "get_messages",
  );
  release();
  await (await finished).finished();
  await chooseMenuOption(page, "Language", "zh");
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
  await expect(
    page.getByRole("button", { name: "Abort", exact: true }),
  ).toBeEnabled();
});

test("a live settlement replaces the matching restored settlement", async ({
  page,
}) => {
  await mockHost(page);
  await page.route(snapshotRoute, (route) =>
    route.fulfill({
      json: {
        sessionId,
        status: "ready",
        messages: [],
        events: [],
        lastSeq: 4,
        history: {
          records: [
            {
              type: "operation_finished",
              seq: 4,
              outcome: "aborted",
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
      },
    }),
  );
  await page.route(`**/api/v1/sessions/${sessionId}/events?*`, (route) =>
    route.fulfill({
      contentType: "text/event-stream",
      body: 'id: 5\nevent: agent_settled\ndata: {"type":"agent_settled","runId":"run-2","outcome":"aborted"}\n\n',
    }),
  );
  await page.goto("/");
  await expect(
    page.locator(".transcript-note").filter({ hasText: "Run aborted" }),
  ).toHaveCount(1);
});

test("live settlement labels follow the selected language", async ({
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
  await chooseMenuOption(page, "Language", "zh");
  await expect(
    page
      .locator(".transcript-note")
      .filter({ hasText: "运行状态：失败 · 已中止" }),
  ).toHaveCount(1);
  await chooseMenuOption(page, "语言", "en");
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
  const model = page.getByRole("button", { name: "Model", exact: true });
  await chooseMenuOption(page, "Model", { label: "Model Beta (beta)" });
  await expect
    .poll(() => changes)
    .toEqual([
      expect.objectContaining({
        type: "set_model",
        modelId: "shared-id",
        provider: "beta",
      }),
    ]);
  await expect(model).toContainText("Model Beta (beta)");
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
  await chooseMenuOption(page, "Language", "zh");
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
  const thinking = page.getByRole("button", {
    name: "Thinking level",
    exact: true,
  });
  await chooseMenuOption(page, "Thinking level", "high");
  await expect(thinking).toContainText("low");
  await chooseMenuOption(page, "Model", { label: "Second model (test)" });
  await expect.poll(() => loadingSecondLevels).toBe(true);
  await expect(thinking).toBeDisabled();
  releaseLevels();
  await expect(thinking).toContainText("off");
  await thinking.click();
  const options = page
    .getByRole("listbox", { name: "Thinking level", exact: true })
    .getByRole("option");
  await expect(options).toHaveCount(2);
  await expect(options.nth(0)).toHaveAccessibleName("—");
  await expect(options.nth(1)).toHaveAccessibleName("off");
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
    await page.route(snapshotRoute, (route) =>
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
    await expect(page.locator(".shell-command")).toHaveText("sleep 1");
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
    if (outcome !== "closed") {
      if (outcome !== "error")
        await page.locator(".shell-card summary").click();
      await expect(page.locator(".shell-output")).toHaveText("command output");
    }
    const frozen = await timer.innerText();
    await page.waitForTimeout(350);
    await expect(timer).toHaveText(frozen);
    await page.reload();
    await expect(timer).toHaveText("1.2s");
    await expect(page.locator(".shell-command")).toHaveText("sleep 1");
    await chooseMenuOption(page, "Language", "zh");
    await expect(timer).toHaveAccessibleName("耗时 1.2s");
    await page.setViewportSize({ width: 390, height: 844 });
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
    ).toBe(true);
  });
}

test("bash card folds a multiline command into one line and strips ANSI output", async ({
  page,
}) => {
  await mockHost(page);
  const command =
    "set -e\nnpm test -- --runInBand --maxWorkers=2 --detectOpenHandles\nnpm run lint";
  await page.route(snapshotRoute, (route) =>
    route.fulfill({
      json: {
        sessionId,
        status: "ready",
        messages: [
          {
            role: "assistant",
            content: [
              {
                type: "toolCall",
                id: "bash-card",
                name: "bash",
                arguments: { command, description: "Run tests" },
              },
            ],
          },
          {
            role: "toolResult",
            toolCallId: "bash-card",
            toolName: "bash",
            isError: false,
            content: [
              { type: "text", text: "\u001b[32mok\u001b[0m\n2 passing" },
            ],
          },
        ],
        lastSeq: 5,
        events: [],
      },
    }),
  );
  await page.goto("/");
  await expect(page.locator(".shell-command")).toHaveText(
    "set -e npm test -- --runInBand --maxWorkers=2 --detectOpenHandles npm run lint",
  );
  await expect(page.locator(".shell-command")).toHaveCSS(
    "text-overflow",
    "ellipsis",
  );
  await expect(page.locator(".shell-meta")).toContainText(
    "description=Run tests",
  );
  await page.locator(".shell-card summary").click();
  await expect(page.locator(".shell-input")).toContainText("detectOpenHandles");
  await expect(page.locator(".shell-output")).toHaveText("ok 2 passing");
  await expect(page.locator(".tool-card")).not.toContainText("\u001b");
  await page.setViewportSize({ width: 390, height: 844 });
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
  ).toBe(true);
});

test("composer uses Shift+Enter for a newline without sending", async ({
  page,
}) => {
  await mockHost(page);
  await page.goto("/");
  const input = page.locator(".composer textarea");
  await input.fill("first line");
  await input.press("Shift+Enter");
  await input.type("second line");
  await expect(input).toHaveValue("first line\nsecond line");
  await expect(page.locator(".composer-shortcut-hint")).toContainText(
    "new line",
  );
});

test("initial snapshot preserves slash feedback entered while history is loading", async ({
  page,
}) => {
  await mockHost(page);
  let release!: () => void;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  let requested = false;
  await page.route(
    snapshotRoute,
    async (route) => {
      requested = true;
      await held;
      await route.fallback();
    },
  );
  await page.goto("/");
  await expect.poll(() => requested).toBe(true);
  const input = page.locator(".composer textarea");
  await input.fill("/help");
  await input.press("Enter");
  await expect(page.locator(".transcript-note")).toContainText(["/help"]);
  release();
  await expect(page.locator(".transcript-note")).toContainText(["/help"]);
  await expect(
    page.locator(".transcript-note").filter({ hasText: "/help" }),
  ).toBeVisible();
});

test("plan tool renders a structured outline and markdown body", async ({
  page,
}) => {
  await mockHost(page);
  await page.route(snapshotRoute, (route) =>
    route.fulfill({
      json: {
        sessionId,
        status: "ready",
        lastSeq: 9,
        events: [],
        messages: [
          {
            role: "assistant",
            content: [
              {
                type: "toolCall",
                id: "plan-1",
                name: "plan",
                arguments: {
                  plan: "# Plan\n\n- Step one\n- Step two\n\nDone.",
                },
              },
            ],
          },
          {
            role: "toolResult",
            toolCallId: "plan-1",
            toolName: "plan",
            content: [
              {
                type: "text",
                text: "# Plan\n\n- Step one\n- Step two\n\nDone.",
              },
            ],
          },
        ],
      },
    }),
  );
  await page.goto("/");
  await expect(page.locator(".plan-card")).toBeVisible();
  await page.locator(".plan-card summary").click();
  await expect(page.locator(".plan-outline li")).toHaveCount(2);
  await expect(page.locator(".plan-outline")).toContainText("Step one");
  await expect(page.locator(".plan-detail")).toContainText("Done.");
});

test("todo tool renders a read-only checklist with completion count", async ({
  page,
}) => {
  await mockHost(page);
  await page.route(snapshotRoute, (route) =>
    route.fulfill({
      json: {
        sessionId,
        status: "ready",
        lastSeq: 9,
        events: [],
        messages: [
          {
            role: "assistant",
            content: [
              { type: "toolCall", id: "todo-1", name: "todo", arguments: {} },
            ],
          },
          {
            role: "toolResult",
            toolCallId: "todo-1",
            toolName: "todo",
            content: [
              {
                type: "text",
                text: "1/2 completed\n[✓] #0: First task\n[ ] #1: Second task",
              },
            ],
          },
        ],
      },
    }),
  );
  await page.goto("/");
  await expect(page.locator(".todo-card")).toBeVisible();
  await expect(page.locator(".todo-list li")).toHaveCount(2);
  await expect(page.locator(".todo-list li.done")).toHaveCount(1);
  await expect(page.locator(".todo-card summary")).toContainText("1/2");
});

test("composer model picker groups by provider and applies the selection", async ({
  page,
}) => {
  await mockHost(page);
  const catalog = [
    { id: "alpha-id", name: "Model Alpha", provider: "alpha" },
    { id: "beta-id", name: "Model Beta", provider: "beta" },
  ];
  let selected = { modelId: "alpha-id", provider: "alpha" };
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
      selected = { modelId: body.modelId, provider: body.provider };
      return route.fulfill({ json: response({}, body.type) });
    }
    return route.fallback();
  });
  await page.goto("/");
  await page.locator(".composer-model").click();
  await expect(page.locator(".dropdown-popover")).toBeVisible();
  await expect(page.locator(".dropdown-group-label")).toHaveText([
    "alpha",
    "beta",
  ]);
  await page
    .locator(".dropdown-item")
    .filter({ hasText: "Model Beta" })
    .click();
  await expect(page.locator(".dropdown-popover")).toBeHidden();
  await expect
    .poll(() => changes)
    .toEqual([
      expect.objectContaining({ type: "set_model", provider: "beta" }),
    ]);
  await expect(page.locator(".composer-model")).toContainText("Model Beta");
});

test("edit tool renders a file diff and powershell renders as a terminal", async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await mockHost(page);
  await page.route(snapshotRoute, (route) =>
    route.fulfill({
      json: {
        sessionId,
        status: "ready",
        lastSeq: 9,
        events: [],
        messages: [
          {
            role: "assistant",
            content: [
              {
                type: "toolCall",
                id: "edit-1",
                name: "edit",
                arguments: {
                  path: "src/app.ts",
                  edits: [{ oldText: "const a = 1;", newText: "const a = 2;" }],
                },
              },
            ],
          },
          {
            role: "toolResult",
            toolCallId: "edit-1",
            toolName: "edit",
            content: [{ type: "text", text: "Edited src/app.ts" }],
          },
          {
            role: "assistant",
            content: [
              {
                type: "toolCall",
                id: "ps-1",
                name: "powershell",
                arguments: {
                  command:
                    "rg -n -A120 -B30 \"tool-row|tool-card|message-list|virtual-row|message-body\" packages/rpi-web/frontend/src/styles.css packages/rpi-web/frontend/src/desktop.css | sed -n '1,300p'",
                },
              },
            ],
          },
          {
            role: "toolResult",
            toolCallId: "ps-1",
            toolName: "powershell",
            content: [{ type: "text", text: "Mode  LastWriteTime  Name" }],
          },
        ],
      },
    }),
  );
  await page.goto("/");
  await expect(page.locator(".edit-card")).toBeVisible();
  await expect(page.locator(".edit-card")).toHaveCSS("border-radius", "0px");
  await page.locator(".edit-card > summary").click();
  await expect(page.locator(".edit-path")).toContainText("src/app.ts");
  await expect(page.locator(".edit-line.remove")).toBeVisible();
  await expect(page.locator(".edit-line.remove")).toContainText("const a = 1;");
  await expect(page.locator(".edit-line.add")).toContainText("const a = 2;");
  await expect(page.locator(".shell-card")).toBeVisible();
  const transcriptBox = await page.locator(".transcript").boundingBox();
  const shellBox = await page.locator(".shell-card").boundingBox();
  expect(transcriptBox).not.toBeNull();
  expect(shellBox).not.toBeNull();
  expect(shellBox!.x).toBeGreaterThanOrEqual(transcriptBox!.x);
  expect(shellBox!.x + shellBox!.width).toBeLessThanOrEqual(
    transcriptBox!.x + transcriptBox!.width,
  );
  const status = page.locator(".shell-card .tool-status");
  const statusBox = await status.boundingBox();
  expect(statusBox).not.toBeNull();
  expect(statusBox!.x + statusBox!.width).toBeLessThanOrEqual(
    shellBox!.x + shellBox!.width,
  );
  await expect(page.locator(".shell-card .shell-command")).toContainText(
    "rg -n",
  );
  await page.locator(".shell-card summary").click();
  await expect(page.locator(".shell-card .shell-branch")).toHaveText("└");
});

for (const mode of [
  "legacy messages",
  "live messages",
  "native totals",
] as const) {
  test(`token metrics survive diagnostics and reload with ${mode}`, async ({
    page,
  }) => {
    await mockHost(page);
    const legacy = mode !== "native totals";
    const live = mode === "live messages";
    const stats = {
      messageCount: 4,
      cachedTokens: legacy ? 0 : 800,
      uncachedTokens: legacy ? 0 : 200,
      totalTokens: legacy ? 0 : 1050,
      costTotal: 0,
      cacheHitRatio: null,
    };
    await page.route(`**/api/v1/sessions/${sessionId}/commands`, (route) => {
      const { type } = route.request().postDataJSON();
      if (type === "get_stats")
        return route.fulfill({ json: response(stats, type) });
      if (type === "get_state")
        return route.fulfill({ json: response({ sessionId, stats }, type) });
      return route.fallback();
    });
    await page.route(snapshotRoute, (route) =>
      route.fulfill({
        json: {
          sessionId,
          status: "ready",
          lastSeq: 4,
          events: [],
          hasMore: true,
          messages: [
            {
              role: "assistant",
              content: [{ type: "text", text: "Earlier reply" }],
              usage: { input: 10, output: 20, cacheRead: 0, totalTokens: 30 },
            },
            {
              role: "assistant",
              content: [{ type: "text", text: "Latest reply" }],
              usage: {
                input: 1530,
                output: 1182,
                cacheRead: 158208,
                cacheWrite: 0,
                totalTokens: 160920,
              },
            },
          ],
        },
      }),
    );
    if (live)
      await page.route(`**/api/v1/sessions/${sessionId}/events**`, (route) => {
        const events = [
          { input: 100, output: 50, cacheRead: 100, totalTokens: 250 },
          { input: 400, output: 120, cacheRead: 600, totalTokens: 1120 },
        ]
          .map(
            (usage, index) =>
              `id: ${5 + index}\nevent: message_end\ndata: ${JSON.stringify({
                type: "message_end",
                message: {
                  role: "assistant",
                  content: [{ type: "text", text: "Live reply" }],
                  usage,
                },
              })}\n\n`,
          )
          .join("");
        return route.fulfill({
          contentType: "text/event-stream",
          body: events,
        });
      });
    for (const reload of [false, true]) {
      const diagnostics = page.waitForResponse(
        (res) =>
          res.url().endsWith("/commands") &&
          res.request().postDataJSON()?.type === "get_stats",
      );
      if (reload) await page.reload();
      else await page.goto("/");
      await diagnostics;
      const metrics = page.locator(".composer-metrics");
      await expect(
        metrics.getByLabel(
          `Cache hit: ${live ? "60%" : legacy ? "99%" : "80%"}`,
          { exact: true },
        ),
      ).toBeVisible();
      await expect(
        metrics.getByLabel(
          `Input tokens: ${live ? "1.0k" : legacy ? "160k" : "1.0k"}`,
          { exact: true },
        ),
      ).toBeVisible();
      await expect(
        metrics.getByLabel(
          `Output tokens: ${live ? "120" : legacy ? "1.2k" : "50"}`,
          { exact: true },
        ),
      ).toBeVisible();
      await metrics.locator(":scope > summary").click();
      await expect(
        metrics.getByLabel(
          `Cached tokens: ${live ? "600" : legacy ? "158k" : "800"}`,
          { exact: true },
        ),
      ).toBeVisible();
      await expect(metrics).not.toContainText("Usage from the latest response");
      if (reload)
        await metrics.screenshot({
          path: test.info().outputPath("token-metrics.png"),
        });
    }
  });
}

test("composer shows the active project path, branch and linked worktree on narrow screens", async ({
  page,
}) => {
  await page.setViewportSize({ width: 375, height: 844 });
  await mockHost(page);
  await page.route("**/api/v1/projects/project-a/git/status", (route) =>
    route.fulfill({
      json: {
        isRepository: true,
        branch: "codex/a-long-worktree-branch",
        files: [],
        isWorktree: true,
        worktreePath: "/workspace/pi-rust",
      },
    }),
  );
  await page.goto("/");
  const context = page.locator(".composer-workspace");
  await expect(
    context.getByRole("button", { name: "/workspace/pi-rust", exact: true }),
  ).toBeVisible();
  await expect(
    context.getByLabel("Branch: codex/a-long-worktree-branch", { exact: true }),
  ).toBeVisible();
  await expect(context.getByText("worktree", { exact: true })).toBeVisible();
  for (const width of [320, 375, 1280]) {
    await page.setViewportSize({ width, height: 844 });
    expect(
      await context.evaluate(
        (element) => element.scrollWidth <= element.clientWidth,
      ),
    ).toBe(true);
  }
  await page.screenshot({
    path: test.info().outputPath("project-context.png"),
  });
  await page.setViewportSize({ width: 375, height: 844 });
  await page
    .getByRole("button", { name: "Toggle sidebar", exact: true })
    .click();
  await page
    .getByRole("button", { name: "Expand project: rpi-package", exact: true })
    .click();
  await expect(
    context.getByRole("button", {
      name: "/workspace/rpi-package",
      exact: true,
    }),
  ).toBeVisible();
  await expect(
    context.getByLabel("Branch: main", { exact: true }),
  ).toBeVisible();
  await expect(context.getByText("worktree", { exact: true })).toBeHidden();
  await expect(context).not.toContainText("codex/a-long-worktree-branch");
});

test("mobile composer footer keeps model picker usable beside session metrics", async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await mockHost(page);
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
  await page.route("**/api/v1/sessions/*/commands", async (route) => {
    const body = route.request().postDataJSON();
    if (body.type === "get_available_models") {
      return route.fulfill({
        json: response(
          {
            models: [
              {
                id: "mobile-model",
                name: "Mobile model",
                provider: "test-provider",
              },
            ],
          },
          body.type,
        ),
      });
    }
    if (body.type === "get_state") {
      return route.fulfill({
        json: response(
          {
            sessionId,
            leafId: "leaf-1",
            modelId: "mobile-model",
            provider: "test-provider",
            context: { tokens: 129000, window: 227000, percent: 57 },
          },
          body.type,
        ),
      });
    }
    return route.fallback();
  });
  await page.goto("/");

  const model = page.getByRole("button", {
    name: "Choose model",
    exact: true,
  });
  await expect(model).toBeVisible();
  const modelBox = await model.boundingBox();
  expect(modelBox).not.toBeNull();
  expect(modelBox!.x + modelBox!.width).toBeLessThanOrEqual(390);

  const metrics = page.locator(".composer-metrics");
  const footer = page.locator(".composer-footer");
  await expect(metrics.getByLabel(/^Context:/)).toBeVisible();
  await expect(
    metrics.getByLabel("Context: 129k / 227k", { exact: true }),
  ).toContainText("129k / 227k");
  await expect(metrics.getByLabel(/^Cache hit:/)).toBeVisible();
  await expect(metrics.getByLabel(/^Input tokens: 100$/)).toBeVisible();
  await expect(metrics.getByLabel(/^Output tokens: 252$/)).toBeVisible();
  await expect(metrics.getByLabel(/^Cache hit: 20%$/)).toBeVisible();
  await expect(
    metrics.getByLabel("Total tokens: 352", { exact: true }),
  ).toBeHidden();
  await expect(metrics.getByLabel(/^Cached tokens:/)).toBeHidden();
  await expect(metrics.getByLabel(/^Cost:/)).toBeHidden();
  expect(
    await metrics.evaluate(
      (element) => element.scrollWidth <= element.clientWidth,
    ),
  ).toBe(true);
  await metrics.locator(":scope > summary").click();
  await expect(metrics.getByLabel(/^Cached tokens:/)).toBeVisible();
  const metricsBox = await metrics.boundingBox();
  const footerBox = await footer.boundingBox();
  expect(metricsBox).not.toBeNull();
  expect(footerBox).not.toBeNull();
  expect(metricsBox!.y).toBeGreaterThan(footerBox!.y + footerBox!.height - 1);

  await metrics.locator(":scope > summary").click();
  for (const width of [320, 375, 390]) {
    await page.setViewportSize({ width, height: 844 });
    const summary = metrics.locator(":scope > summary");
    await expect(summary.locator(".composer-metric")).toHaveCount(4);
    const layout = await summary.evaluate((element) => {
      const tops = Array.from(
        element.querySelectorAll(".composer-metric"),
        (item) => item.getBoundingClientRect().top,
      );
      return {
        singleLine: Math.max(...tops) - Math.min(...tops) < 1,
        fits: element.scrollWidth <= element.clientWidth,
      };
    });
    expect(layout).toEqual({ singleLine: true, fits: true });
    if (width === 375)
      await metrics.screenshot({
        path: test.info().outputPath("compact-footer-mobile.png"),
      });
  }

  await model.click();
  const popover = page.getByRole("listbox", {
    name: "Choose model",
    exact: true,
  });
  await expect(popover).toBeVisible();
  const popoverBox = await popover.boundingBox();
  expect(popoverBox).not.toBeNull();
  expect(popoverBox!.y).toBeGreaterThanOrEqual(0);
  expect(popoverBox!.y + popoverBox!.height).toBeLessThanOrEqual(844);
  await page.getByRole("option", { name: "Mobile model" }).click();
  await expect(popover).toBeHidden();
});

// Keep the initial transcript tall enough that top paging cannot be confused
// with bottom-following, including at mobile widths.
const upwardPagingMessages = Array.from({ length: 20 }, (_, index) => ({
  role: "user",
  entryId: `scroll-current-${index}`,
  content: Array.from(
    { length: 8 },
    (_, line) =>
      `Current message ${index}, line ${line}: enough history to scroll.`,
  ).join("\n"),
}));

async function mockUpwardPaging(page: Page) {
  await mockHost(page);
  await page.route(snapshotRoute, (route) =>
    route.fulfill({
      json: {
        sessionId,
        messages: upwardPagingMessages,
        events: [],
        lastSeq: 4,
        firstId: "scroll-current-0",
        hasMore: true,
      },
    }),
  );
}

async function scrollPagingTo(page: Page, top: number) {
  await page.locator(".transcript").evaluate(async (element, value) => {
    element.scrollTop = value;
    // Also exercise repeated events at the same position (trackpad/held scroll).
    element.dispatchEvent(new Event("scroll", { bubbles: true }));
    await new Promise<void>((resolve) =>
      requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
    );
  }, top);
}

async function visiblePagingAnchor(page: Page) {
  return page.locator(".transcript").evaluate((element) => {
    const top = element.getBoundingClientRect().top;
    const row = Array.from(
      element.querySelectorAll("[data-transcript-row]"),
    ).find((item) => item.getBoundingClientRect().bottom > top + 20);
    if (!row) throw new Error("Expected a visible transcript row");
    return {
      id: row.getAttribute("data-transcript-row")!,
      offset: row.getBoundingClientRect().top - top,
    };
  });
}

async function expectPagingAnchor(
  page: Page,
  anchor: { id: string; offset: number },
) {
  await expect
    .poll(() =>
      page.locator(".transcript").evaluate((element, saved) => {
        const row = Array.from(
          element.querySelectorAll("[data-transcript-row]"),
        ).find((item) => item.getAttribute("data-transcript-row") === saved.id);
        if (!row) return Number.MAX_SAFE_INTEGER;
        return Math.abs(
          row.getBoundingClientRect().top -
            element.getBoundingClientRect().top -
            saved.offset,
        );
      }, anchor),
    )
    .toBeLessThanOrEqual(3);
}

for (const viewport of [
  { name: "desktop", width: 1280, height: 900 },
  { name: "mobile", width: 390, height: 844 },
]) {
  test(`upward paging: ${viewport.name} auto-loads 48 without a button click and stops at exhaustion`, async ({
    page,
  }) => {
    await page.setViewportSize({
      width: viewport.width,
      height: viewport.height,
    });
    await mockUpwardPaging(page);
    const requests: Array<{ beforeId: string; limit: number }> = [];
    await page.route(`**/api/v1/sessions/${sessionId}/commands`, (route) => {
      const body = route.request().postDataJSON();
      if (body.type !== "get_messages" || !body.beforeId)
        return route.fallback();
      requests.push(body);
      return route.fulfill({
        json: response(
          {
            messages: [
              {
                role: "user",
                entryId: "scroll-oldest",
                content: "Oldest auto-loaded message",
              },
            ],
            firstId: "scroll-oldest",
            hasMore: false,
          },
          "get_messages",
        ),
      });
    });
    await page.goto("/");
    await expect(page.locator(".message.user")).toHaveCount(20);
    await expect(
      page.getByRole("button", { name: "Load earlier messages" }),
    ).toBeVisible();
    await expect
      .poll(() =>
        page.locator(".transcript").evaluate((element) => element.scrollTop),
      )
      .toBeGreaterThan(160);
    await page.waitForTimeout(250);
    expect(requests).toHaveLength(0); // No initial automatic fetch.
    await scrollPagingTo(page, 161);
    expect(requests).toHaveLength(0);
    await scrollPagingTo(page, 120);
    await expect(
      page.getByText("Oldest auto-loaded message", { exact: true }),
    ).toBeVisible();
    expect(requests).toMatchObject([
      { beforeId: "scroll-current-0", limit: 48 },
    ]);
    await expect(
      page.getByRole("button", { name: "Load earlier messages" }),
    ).toHaveCount(0);
    for (const top of [400, 100, 0, 400, 0]) await scrollPagingTo(page, top);
    await page.waitForTimeout(250);
    expect(requests).toHaveLength(1);
  });
}

test("upward paging: held scrolling deduplicates requests and prepending preserves the visible row", async ({
  page,
}) => {
  await mockUpwardPaging(page);
  let release!: () => void;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  let requests = 0;
  await page.route(
    `**/api/v1/sessions/${sessionId}/commands`,
    async (route) => {
      const body = route.request().postDataJSON();
      if (body.type !== "get_messages" || !body.beforeId)
        return route.fallback();
      requests++;
      await held;
      return route.fulfill({
        json: response(
          {
            messages: Array.from({ length: 8 }, (_, index) => ({
              role: "user",
              entryId: `scroll-older-${index}`,
              content: `Prepended history ${index}\nSecond line\nThird line\nFourth line`,
            })),
            firstId: "scroll-older-0",
            hasMore: true,
          },
          "get_messages",
        ),
      });
    },
  );
  await page.goto("/");
  await expect(page.locator(".message.user")).toHaveCount(20);
  await scrollPagingTo(page, 120);
  await expect.poll(() => requests).toBe(1);
  try {
    for (const top of [110, 90, 130, 80, 100]) await scrollPagingTo(page, top);
    await expect(
      page.getByRole("button", { name: "Load earlier messages" }),
    ).toBeDisabled();
    expect(requests).toBe(1);
    // Move while the request is held: capture must happen at response time.
    await scrollPagingTo(page, 450);
    const anchor = await visiblePagingAnchor(page);
    release();
    await expect(page.locator(".message.user")).toHaveCount(28);
    await expectPagingAnchor(page, anchor);
    await page.waitForTimeout(250);
    await expectPagingAnchor(page, anchor);
    expect(requests).toBe(1);
  } finally {
    release();
  }
});

test("upward paging: a failed automatic cursor stays quiet until leaving and reentering the top", async ({
  page,
}) => {
  await mockUpwardPaging(page);
  const cursors: string[] = [];
  await page.route(`**/api/v1/sessions/${sessionId}/commands`, (route) => {
    const body = route.request().postDataJSON();
    if (body.type !== "get_messages" || !body.beforeId) return route.fallback();
    cursors.push(body.beforeId);
    if (cursors.length === 1)
      return route.fulfill({
        status: 503,
        json: { error: "Temporary earlier history failure" },
      });
    return route.fulfill({
      json: response(
        {
          messages: [
            {
              role: "user",
              entryId: "scroll-retried",
              content: "Retried earlier history",
            },
          ],
          firstId: "scroll-retried",
          hasMore: false,
        },
        "get_messages",
      ),
    });
  });
  await page.goto("/");
  await expect(page.locator(".message.user")).toHaveCount(20);
  await scrollPagingTo(page, 120);
  await expect.poll(() => cursors.length).toBe(1);
  const fallback = page.getByRole("button", { name: "Load earlier messages" });
  await expect(fallback).toBeEnabled();
  for (const top of [100, 60, 0, 120, 40]) await scrollPagingTo(page, top);
  await page.waitForTimeout(350);
  expect(cursors).toEqual(["scroll-current-0"]);
  await scrollPagingTo(page, 400);
  await scrollPagingTo(page, 120);
  await expect(
    page.getByText("Retried earlier history", { exact: true }),
  ).toBeVisible();
  expect(cursors).toEqual(["scroll-current-0", "scroll-current-0"]);
});

test("upward paging: a no-progress cursor ends paging even when the host reports hasMore", async ({
  page,
}) => {
  await mockUpwardPaging(page);
  let requests = 0;
  await page.route(`**/api/v1/sessions/${sessionId}/commands`, (route) => {
    const body = route.request().postDataJSON();
    if (body.type !== "get_messages" || !body.beforeId) return route.fallback();
    requests++;
    return route.fulfill({
      json: response(
        {
          messages: [upwardPagingMessages[0]],
          firstId: body.beforeId,
          hasMore: true,
        },
        "get_messages",
      ),
    });
  });
  await page.goto("/");
  await expect(page.locator(".message.user")).toHaveCount(20);
  await scrollPagingTo(page, 120);
  await expect.poll(() => requests).toBe(1);
  await expect(
    page.getByRole("button", { name: "Load earlier messages" }),
  ).toHaveCount(0);
  await expect(page.locator(".message.user")).toHaveCount(20);
  for (const top of [400, 0, 400, 100, 0]) await scrollPagingTo(page, top);
  await page.waitForTimeout(350);
  expect(requests).toBe(1);
});

test("upward paging: advances the cursor for the next page without duplicate rows", async ({
  page,
}) => {
  await mockUpwardPaging(page);
  const cursors: string[] = [];
  await page.route(`**/api/v1/sessions/${sessionId}/commands`, (route) => {
    const body = route.request().postDataJSON();
    if (body.type !== "get_messages" || !body.beforeId) return route.fallback();
    cursors.push(body.beforeId);
    return route.fulfill({
      json: response(
        {
          messages: [
            {
              role: "user",
              entryId: `page-${cursors.length}`,
              content: `Earlier page ${cursors.length}`,
            },
          ],
          firstId: `page-${cursors.length}`,
          hasMore: cursors.length < 2,
        },
        "get_messages",
      ),
    });
  });
  await page.goto("/");
  await expect(page.locator(".message.user")).toHaveCount(20);
  await scrollPagingTo(page, 120);
  await expect(page.locator(".message.user")).toHaveCount(21);
  await scrollPagingTo(page, 400);
  await scrollPagingTo(page, 120);
  await expect(page.locator(".message.user")).toHaveCount(22);
  expect(cursors).toEqual(["scroll-current-0", "page-1"]);
  await expect(
    page.getByRole("button", { name: "Load earlier messages" }),
  ).toHaveCount(0);
});

test("upward paging: a late older image does not move the reader's row", async ({
  page,
}) => {
  await mockUpwardPaging(page);
  let release!: () => void;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  let imageRequested = false;
  await page.route("**/late-history.svg", async (route) => {
    imageRequested = true;
    await held;
    return route.fulfill({
      contentType: "image/svg+xml",
      body: '<svg xmlns="http://www.w3.org/2000/svg" width="640" height="960"><rect width="640" height="960" fill="blue"/></svg>',
    });
  });
  await page.route(`**/api/v1/sessions/${sessionId}/commands`, (route) => {
    const body = route.request().postDataJSON();
    if (body.type !== "get_messages" || !body.beforeId) return route.fallback();
    return route.fulfill({
      json: response(
        {
          messages: [
            {
              role: "assistant",
              entryId: "late-image",
              content: [
                {
                  type: "text",
                  text: "![Late earlier image](/late-history.svg)",
                },
              ],
            },
            ...Array.from({ length: 5 }, (_, i) => ({
              role: "user",
              entryId: `late-old-${i}`,
              content: `Older row ${i}\nMore text\nMore text`,
            })),
          ],
          firstId: "late-image",
          hasMore: false,
        },
        "get_messages",
      ),
    });
  });
  await page.goto("/");
  await expect(page.locator(".message.user")).toHaveCount(20);
  await scrollPagingTo(page, 120);
  const anchor = await visiblePagingAnchor(page);
  try {
    await expect(page.locator(".message.user")).toHaveCount(25);
    await expectPagingAnchor(page, anchor);
    // Simulate an image request started earlier but completing after it scrolls
    // above the reader. Chromium may otherwise defer an offscreen lazy image.
    await page
      .getByAltText("Late earlier image")
      .evaluate((i: HTMLImageElement) => {
        i.loading = "eager";
      });
    await expect.poll(() => imageRequested).toBe(true);
    release();
    await expect
      .poll(() =>
        page
          .getByAltText("Late earlier image")
          .evaluate((i: HTMLImageElement) => i.naturalHeight),
      )
      .toBe(960);
    await expectPagingAnchor(page, anchor);
  } finally {
    release();
  }
});

test("upward paging: jumping to latest during a pending page keeps bottom follow", async ({
  page,
}) => {
  await mockUpwardPaging(page);
  let release!: () => void;
  const held = new Promise<void>((resolve) => {
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
      await held;
      return route.fulfill({
        json: response(
          {
            messages: [
              {
                role: "user",
                entryId: "old-follow",
                content: "Old follow row",
              },
            ],
            firstId: "old-follow",
            hasMore: false,
          },
          "get_messages",
        ),
      });
    },
  );
  await page.goto("/");
  await expect(page.locator(".message.user")).toHaveCount(20);
  await scrollPagingTo(page, 120);
  await expect.poll(() => pending).toBe(true);
  await scrollPagingTo(page, 1000000);
  release();
  await expect(page.locator(".message.user")).toHaveCount(21);
  await expect
    .poll(() =>
      page
        .locator(".transcript")
        .evaluate((n) => n.scrollHeight - n.clientHeight - n.scrollTop),
    )
    .toBeLessThan(3);
});

test("upward paging: an invisible page advances automatically without another scroll", async ({
  page,
}) => {
  await mockUpwardPaging(page);
  const cursors: string[] = [];
  await page.route(`**/api/v1/sessions/${sessionId}/commands`, (route) => {
    const body = route.request().postDataJSON();
    if (body.type !== "get_messages" || !body.beforeId) return route.fallback();
    cursors.push(body.beforeId);
    return route.fulfill({
      json: response(
        {
          messages:
            cursors.length === 1
              ? []
              : [
                  {
                    role: "user",
                    entryId: "visible-old",
                    content: "Visible earlier history",
                  },
                ],
          firstId: cursors.length === 1 ? "invisible-old" : "visible-old",
          hasMore: cursors.length === 1,
        },
        "get_messages",
      ),
    });
  });
  await page.goto("/");
  await expect(page.locator(".message.user")).toHaveCount(20);
  await scrollPagingTo(page, 0);
  await expect(
    page.getByText("Visible earlier history", { exact: true }),
  ).toBeVisible();
  expect(cursors).toEqual(["scroll-current-0", "invisible-old"]);
});

// Codex-style disclosure regressions: these fixtures deliberately retain both
// arguments and results, so a compact summary must not replace either payload.
test("tool disclosures keep read paths and search queries visible while closed on desktop and mobile", async ({
  page,
}) => {
  await mockHost(page);
  await page.route(snapshotRoute, (route) =>
    route.fulfill({
      json: {
        sessionId,
        status: "ready",
        lastSeq: 9,
        events: [],
        messages: [
          {
            role: "assistant",
            content: [
              {
                type: "toolCall",
                id: "disclosure-read",
                name: "read",
                arguments: { path: "src/disclosure-example.ts" },
              },
              {
                type: "toolCall",
                id: "disclosure-search",
                name: "websearch",
                arguments: { query: "Codex disclosure keyboard accessibility" },
              },
            ],
          },
          {
            role: "toolResult",
            toolCallId: "disclosure-read",
            toolName: "read",
            content: [
              { type: "text", text: "export const disclosureExample = true;" },
            ],
          },
          {
            role: "toolResult",
            toolCallId: "disclosure-search",
            toolName: "websearch",
            content: [
              {
                type: "text",
                text: "Search result: accessible disclosure patterns",
              },
            ],
          },
        ],
      },
    }),
  );
  for (const width of [1280, 390]) {
    await page.setViewportSize({ width, height: 844 });
    await page.goto("/");
    const cards = page.locator(".tool-card.tool-done");
    await expect(cards).toHaveCount(2);
    const read = cards.filter({
      has: page.locator("code.tool-target", {
        hasText: "src/disclosure-example.ts",
      }),
    });
    const search = cards.filter({
      has: page.locator("code.tool-target", {
        hasText: "Codex disclosure keyboard accessibility",
      }),
    });
    await expect(read).toHaveJSProperty("open", false);
    await expect(search).toHaveJSProperty("open", false);
    await expect(read.locator(":scope > summary .tool-action")).toHaveText(
      "Read",
    );
    await expect(search.locator(":scope > summary .tool-action")).toHaveText(
      "Searched",
    );
    await expect(
      read.locator(":scope > summary code.tool-target"),
    ).toBeVisible();
    await expect(
      search.locator(":scope > summary code.tool-target"),
    ).toBeVisible();
    await read.locator(":scope > summary").click();
    await expect(read).toHaveJSProperty("open", true);
    await expect(
      read.getByText("export const disclosureExample = true;", {
        exact: false,
      }),
    ).toBeVisible();
    await search.locator(":scope > summary").click();
    await expect(
      search.getByText("Search result: accessible disclosure patterns", {
        exact: false,
      }),
    ).toBeVisible();
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
    ).toBe(true);
  }
});

test("unknown tool preserves its real name and exposes full arguments and result with Enter", async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await mockHost(page);
  await page.route(snapshotRoute, (route) =>
    route.fulfill({
      json: {
        sessionId,
        status: "ready",
        lastSeq: 9,
        events: [],
        messages: [
          {
            role: "assistant",
            content: [
              {
                type: "toolCall",
                id: "disclosure-unknown",
                name: "custom_inventory_probe",
                arguments: {
                  warehouse: "north-depot",
                  filters: { includeArchived: true },
                  ids: [17, 42],
                },
              },
            ],
          },
          {
            role: "toolResult",
            toolCallId: "disclosure-unknown",
            toolName: "custom_inventory_probe",
            content: [
              {
                type: "text",
                text: "Inventory probe result: 42 widgets available",
              },
            ],
          },
        ],
      },
    }),
  );
  await page.goto("/");
  const card = page.locator(".tool-card.tool-done");
  const summary = card.locator(":scope > summary");
  await expect(card).toHaveJSProperty("open", false);
  await expect(summary).toContainText("custom_inventory_probe");
  await summary.focus();
  await page.keyboard.press("Enter");
  await expect(card).toHaveJSProperty("open", true);
  const args = card.locator("pre.tool-arguments");
  await expect(args).toBeVisible();
  expect(JSON.parse((await args.textContent()) ?? "null")).toEqual({
    warehouse: "north-depot",
    filters: { includeArchived: true },
    ids: [17, 42],
  });
  await expect(
    card.getByText("Inventory probe result: 42 widgets available", {
      exact: false,
    }),
  ).toBeVisible();
  await summary.focus();
  await page.keyboard.press("Enter");
  await expect(card).toHaveJSProperty("open", false);
  await expect(args).toBeHidden();
});

test("multiline edit opens in one click with real diff lines and addition/removal counts on mobile", async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await mockHost(page);
  await page.route(snapshotRoute, (route) =>
    route.fulfill({
      json: {
        sessionId,
        status: "ready",
        lastSeq: 9,
        events: [],
        messages: [
          {
            role: "assistant",
            content: [
              {
                type: "toolCall",
                id: "disclosure-edit",
                name: "edit",
                arguments: {
                  path: "src/multiline.ts",
                  edits: [
                    {
                      oldText: "const before = 1;\nconst removed = true;",
                      newText:
                        "const after = 2;\nconst added = true;\nexport { after };",
                    },
                  ],
                },
              },
            ],
          },
          {
            role: "toolResult",
            toolCallId: "disclosure-edit",
            toolName: "edit",
            content: [{ type: "text", text: "Edited src/multiline.ts" }],
          },
        ],
      },
    }),
  );
  await page.goto("/");
  const card = page.locator(".edit-card.tool-done");
  const summary = card.locator(":scope > summary");
  await expect(card).toHaveJSProperty("open", false);
  await expect(summary.locator("code.tool-target")).toHaveText(
    "src/multiline.ts",
  );
  await expect(summary.locator(".tool-action")).toHaveText("Edited");
  await expect(summary.locator(".edit-count")).toContainText("+3");
  await expect(summary.locator(".edit-count")).toContainText("−2");
  await expect(card.locator("summary")).toHaveCount(1);
  await summary.click();
  await expect(card).toHaveJSProperty("open", true);
  const removed = card.locator(".edit-line.remove");
  const added = card.locator(".edit-line.add");
  await expect(removed).toHaveCount(2);
  await expect(added).toHaveCount(3);
  await expect(removed).toContainText([
    "const before = 1;",
    "const removed = true;",
  ]);
  await expect(added).toContainText([
    "const after = 2;",
    "const added = true;",
    "export { after };",
  ]);
  for (const line of [...(await removed.all()), ...(await added.all())])
    await expect(line).toBeVisible();
});

test("plan and todo disclosures start closed and reveal structured bodies in one click", async ({
  page,
}) => {
  await mockHost(page);
  await page.route(snapshotRoute, (route) =>
    route.fulfill({
      json: {
        sessionId,
        status: "ready",
        lastSeq: 9,
        events: [],
        messages: [
          {
            role: "assistant",
            content: [
              {
                type: "toolCall",
                id: "disclosure-plan",
                name: "plan",
                arguments: {
                  plan: "# Disclosure plan\n\n- Inspect controls\n- Verify keyboard\n\nKeep details accessible.",
                },
              },
              {
                type: "toolCall",
                id: "disclosure-todo",
                name: "todo",
                arguments: {},
              },
            ],
          },
          {
            role: "toolResult",
            toolCallId: "disclosure-plan",
            toolName: "plan",
            content: [
              {
                type: "text",
                text: "# Disclosure plan\n\n- Inspect controls\n- Verify keyboard\n\nKeep details accessible.",
              },
            ],
          },
          {
            role: "toolResult",
            toolCallId: "disclosure-todo",
            toolName: "todo",
            content: [
              {
                type: "text",
                text: "1/2 completed\n[✓] #0: Inspect controls\n[ ] #1: Verify keyboard",
              },
            ],
          },
        ],
      },
    }),
  );
  await page.goto("/");
  const plan = page.locator(".plan-card");
  const todo = page.locator(".todo-card");
  await expect(plan).toHaveJSProperty("open", false);
  await expect(todo).toHaveJSProperty("open", false);
  await expect(plan.locator(".plan-outline")).toBeHidden();
  await expect(todo.locator(".todo-list")).toBeHidden();
  await plan.locator(":scope > summary").click();
  await expect(plan.locator(".plan-outline li")).toHaveCount(2);
  await expect(plan.locator(".plan-detail")).toContainText(
    "Keep details accessible.",
  );
  await expect(plan.locator(".plan-detail")).toBeVisible();
  await todo.locator(":scope > summary").click();
  await expect(todo.locator(".todo-list li")).toHaveCount(2);
  await expect(todo.locator(".todo-list li.done")).toHaveCount(1);
  await expect(todo.locator(".todo-list")).toBeVisible();
  await expect(todo.locator(":scope > summary")).toContainText("1/2");
});

test("a new tool error autoopens but manual collapse survives later SSE output and timing updates", async ({
  page,
}) => {
  await mockHost(page);
  let releaseError!: () => void;
  let releaseUpdate!: () => void;
  const errorGate = new Promise<void>((resolve) => {
    releaseError = resolve;
  });
  const updateGate = new Promise<void>((resolve) => {
    releaseUpdate = resolve;
  });
  await page.route(snapshotRoute, (route) =>
    route.fulfill({
      json: {
        sessionId,
        status: "running",
        lastSeq: 4,
        events: [],
        messages: [
          {
            role: "assistant",
            content: [
              {
                type: "toolCall",
                id: "disclosure-error",
                name: "custom_failure_probe",
                arguments: { path: "missing.txt" },
                webTiming: { elapsedMs: 100, running: true },
              },
            ],
          },
        ],
      },
    }),
  );
  let requests = 0;
  await page.route(
    `**/api/v1/sessions/${sessionId}/events?*`,
    async (route) => {
      const attempt = requests++;
      await (attempt === 0 ? errorGate : updateGate);
      const event = {
        type: "tool_execution_end",
        toolCallId: "disclosure-error",
        toolName: "custom_failure_probe",
        isError: true,
        result:
          attempt === 0
            ? "Missing file: initial failure"
            : "Missing file: refreshed diagnostics",
        webTiming: { elapsedMs: attempt === 0 ? 1200 : 2400, running: false },
      };
      await route.fulfill({
        contentType: "text/event-stream",
        body: `retry: 100\nid: ${5 + attempt}\nevent: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`,
      });
    },
  );
  try {
    await page.goto("/");
    const card = page.locator(".tool-card");
    await expect(card).toHaveClass(/tool-running/);
    await expect(card).toHaveJSProperty("open", false);
    releaseError();
    await expect(card).toHaveClass(/tool-error/);
    await expect(card).toHaveJSProperty("open", true);
    await expect(
      card.getByText("Missing file: initial failure", { exact: false }),
    ).toBeVisible();
    await card.locator(":scope > summary").click();
    await expect(card).toHaveJSProperty("open", false);
    releaseUpdate();
    await expect(card.locator(".tool-elapsed")).toHaveText("2.4s");
    await expect(card).toHaveJSProperty("open", false);
    await card.locator(":scope > summary").click();
    await expect(
      card.getByText("Missing file: refreshed diagnostics", { exact: false }),
    ).toBeVisible();
  } finally {
    releaseError();
    releaseUpdate();
  }
});

test("opening a running shell tool survives elapsed ticks and its completed SSE result", async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await mockHost(page);
  let release!: () => void;
  const gate = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.route(snapshotRoute, (route) =>
    route.fulfill({
      json: {
        sessionId,
        status: "running",
        lastSeq: 4,
        events: [],
        messages: [
          {
            role: "assistant",
            content: [
              {
                type: "toolCall",
                id: "disclosure-live",
                name: "bash",
                arguments: { command: "printf 'disclosure complete'" },
                webTiming: { elapsedMs: 100, running: true },
              },
            ],
          },
        ],
      },
    }),
  );
  await page.route(
    `**/api/v1/sessions/${sessionId}/events?*`,
    async (route) => {
      await gate;
      const event = {
        type: "tool_execution_end",
        toolCallId: "disclosure-live",
        toolName: "bash",
        isError: false,
        result: "disclosure complete",
        webTiming: { elapsedMs: 1600, running: false },
      };
      await route.fulfill({
        contentType: "text/event-stream",
        body: `retry: 60000\nid: 5\nevent: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`,
      });
    },
  );
  try {
    await page.goto("/");
    const card = page.locator(".tool-card.tool-running");
    await expect(card).toHaveJSProperty("open", false);
    await expect(card.locator(".shell-command")).toHaveText(
      "printf 'disclosure complete'",
    );
    await card.locator(":scope > summary").click();
    await expect(card).toHaveJSProperty("open", true);
    const timer = card.locator(".tool-elapsed");
    const initial = await timer.innerText();
    await expect.poll(() => timer.innerText()).not.toBe(initial);
    await expect(card).toHaveJSProperty("open", true);
    release();
    const done = page.locator(".tool-card.tool-done");
    await expect(done).toHaveJSProperty("open", true);
    await expect(done.locator(".tool-elapsed")).toHaveText("1.6s");
    await expect(done.locator(".shell-output")).toHaveText(
      "disclosure complete",
    );
    await expect(done.locator(".shell-output")).toBeVisible();
    await done.locator(":scope > summary").click();
    await expect(done).toHaveJSProperty("open", false);
  } finally {
    release();
  }
});


for (const name of ["todo", "plan"]) {
  test(`a manually opened ${name} stays open when its result selects a structured renderer`, async ({
    page,
  }) => {
    await mockHost(page);
    let release!: () => void;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    await page.route(snapshotRoute, (route) =>
      route.fulfill({
        json: {
          sessionId,
          status: "running",
          lastSeq: 4,
          events: [],
          messages: [
            {
              role: "assistant",
              content: [
                {
                  type: "toolCall",
                  id: "structured-transition",
                  name,
                  arguments: {},
                },
              ],
            },
          ],
        },
      }),
    );
    await page.route(
      `**/api/v1/sessions/${sessionId}/events?*`,
      async (route) => {
        await gate;
        const event = {
          type: "tool_execution_end",
          toolCallId: "structured-transition",
          toolName: name,
          isError: false,
          result:
            name === "todo"
              ? "1/2 completed\n[✓] #0: Inspect controls\n[ ] #1: Verify keyboard"
              : "# Structured result\n\n- Inspect controls\n- Verify keyboard",
        };
        await route.fulfill({
          contentType: "text/event-stream",
          body: `retry: 60000
id: 5
event: ${event.type}
data: ${JSON.stringify(event)}

`,
        });
      },
    );
    try {
      await page.goto("/");
      const card = page.locator(".tool-card");
      await expect(card).toHaveClass(/tool-(pending|running)/);
      await expect(card).toHaveJSProperty("open", false);
      await card.locator(":scope > summary").click();
      await expect(card).toHaveJSProperty("open", true);
      release();
      await expect(card).toHaveClass(new RegExp(`${name}-card`));
      await expect(card).toHaveJSProperty("open", true);
      await expect(
        card.locator(name === "todo" ? ".todo-list" : ".plan-outline"),
      ).toBeVisible();
    } finally {
      release();
    }
  });
}

test("expanding a tool at latest keeps its header in place instead of following output to the bottom", async ({
  page,
}) => {
  await mockHost(page);
  await page.route(snapshotRoute, (route) =>
    route.fulfill({
      json: {
        sessionId,
        status: "ready",
        lastSeq: 4,
        events: [],
        messages: [
          ...upwardPagingMessages,
          {
            role: "assistant",
            content: [
              {
                type: "toolCall",
                id: "long-tool",
                name: "bash",
                arguments: { command: "npm test" },
              },
            ],
          },
          {
            role: "toolResult",
            toolCallId: "long-tool",
            toolName: "bash",
            content: [
              {
                type: "text",
                text: Array.from(
                  { length: 100 },
                  (_, i) => `Passing test ${i}`,
                ).join("\n"),
              },
            ],
          },
        ],
      },
    }),
  );
  await page.goto("/");
  const summary = page.locator(".shell-card > summary");
  await expect(summary).toBeVisible();
  await expect
    .poll(() =>
      page
        .locator(".transcript")
        .evaluate((n) => n.scrollHeight - n.clientHeight - n.scrollTop),
    )
    .toBeLessThan(3);
  const before = await summary.boundingBox();
  await summary.focus();
  await page.keyboard.press("Enter");
  await expect(page.locator(".shell-card")).toHaveJSProperty("open", true);
  await page.evaluate(
    () =>
      new Promise((resolve) =>
        requestAnimationFrame(() => requestAnimationFrame(resolve)),
      ),
  );
  const after = await summary.boundingBox();
  expect(Math.abs(after!.y - before!.y)).toBeLessThan(3);
  await expect
    .poll(() =>
      page
        .locator(".transcript")
        .evaluate((n) => n.scrollHeight - n.clientHeight - n.scrollTop),
    )
    .toBeGreaterThan(100);
});

// Attachment projection must remain display-only: native text and upload paths stay intact.
test.describe("attachment cards", () => {
  const docxPath =
    ".rpi/uploads/c11ee75a-625a-4aec-a575-401843f7af66-电话卡分销小程序.docx";
  const actualUserText =
    "已附加文件:\n- .rpi/uploads/c11ee75a-625a-4aec-a575-401843f7af66-电话卡分销小程序.docx  这个附件显示需要优化";

  async function attachmentSnapshot(page: Page, messages: unknown[]) {
    await page.route(snapshotRoute, (route) =>
      route.fulfill({
        json: { sessionId, status: "ready", lastSeq: 4, events: [], messages },
      }),
    );
  }

  test("projects the real Chinese DOCX and trailing prose on desktop and 390px", async ({
    page,
  }) => {
    await mockHost(page);
    await attachmentSnapshot(page, [
      { role: "user", content: [{ type: "text", text: actualUserText }] },
    ]);
    for (const width of [1280, 390]) {
      await page.setViewportSize({ width, height: 844 });
      if (width === 1280) await page.goto("/");
      else await page.reload();
      const card = page.locator(".message.user details.message-attachment");
      await expect(card).toHaveCount(1);
      await expect(card).toHaveJSProperty("open", false);
      const summary = card.locator(":scope > summary");
      await expect(summary.locator(".attachment-name")).toHaveText(
        "电话卡分销小程序.docx",
      );
      await expect(summary.locator(".attachment-meta")).toHaveText("DOCX");
      await expect(summary).not.toContainText("c11ee75a");
      await expect(page.locator(".message.user .plain-message")).toContainText(
        "这个附件显示需要优化",
      );
      await expect(card.locator(".attachment-path")).toBeHidden();
      await summary.click();
      await expect(card).toHaveJSProperty("open", true);
      await expect(card.locator("div.attachment-path code")).toHaveText(
        docxPath,
      );
      await expect(card.locator(".attachment-path")).not.toContainText(
        "这个附件显示需要优化",
      );
      expect(
        await page.evaluate(
          () => document.documentElement.scrollWidth <= innerWidth,
        ),
      ).toBe(true);
      expect(
        await page
          .locator(".transcript")
          .evaluate((node) => node.scrollWidth <= node.clientWidth),
      ).toBe(true);
    }
  });

  test("supports keyboard expansion and copying the original path without fetching DOCX", async ({
    page,
  }) => {
    await page.addInitScript(() => {
      Object.defineProperty(navigator, "clipboard", {
        configurable: true,
        value: {
          writeText: async (text: string) => {
            (
              window as Window & { attachmentCopiedPath?: string }
            ).attachmentCopiedPath = text;
          },
        },
      });
    });
    const fileRequests: string[] = [];
    page.on("request", (request) => {
      const url = new URL(request.url());
      if (
        request.resourceType() === "image" ||
        /\.docx$/i.test(url.pathname) ||
        url.pathname.includes("/uploads/") ||
        /\/projects\/[^/]+\/file$/.test(url.pathname)
      ) {
        fileRequests.push(request.url());
      }
    });
    await mockHost(page);
    await attachmentSnapshot(page, [
      { role: "user", content: [{ type: "text", text: actualUserText }] },
    ]);
    await page.goto("/");
    const card = page.locator("details.message-attachment");
    await expect(card).toHaveJSProperty("open", false);
    await expect(card.locator("img, iframe, object")).toHaveCount(0);
    const summary = card.locator(":scope > summary");
    await summary.focus();
    await page.keyboard.press("Enter");
    await expect(card).toHaveJSProperty("open", true);
    await expect(card.locator(".attachment-path code")).toHaveText(docxPath);
    const copy = card.getByRole("button", {
      name: "Copy file path",
      exact: true,
    });
    await copy.focus();
    await page.keyboard.press("Enter");
    await expect
      .poll(() =>
        page.evaluate(
          () =>
            (window as Window & { attachmentCopiedPath?: string })
              .attachmentCopiedPath,
        ),
      )
      .toBe(docxPath);
    await summary.focus();
    await page.keyboard.press("Enter");
    await expect(card).toHaveJSProperty("open", false);
    expect(fileRequests).toEqual([]);
  });

  test("keeps multiple English attachment filenames identical in live events and reload snapshots", async ({
    page,
  }) => {
    await mockHost(page);
    const paths = [
      ".rpi/uploads/11111111-1111-4111-8111-111111111111-quarterly report.docx",
      ".rpi/uploads/22222222-2222-4222-8222-222222222222-review  final.docx",
    ];
    const message = {
      role: "user",
      content: [
        {
          type: "text",
          text: `Please review both.\n\nAttached files:\n${paths.map((path) => `- ${path}`).join("\n")}`,
        },
      ],
    };
    let persisted = false;
    await page.route(snapshotRoute, (route) =>
      route.fulfill({
        json: {
          sessionId,
          status: "ready",
          lastSeq: persisted ? 5 : 4,
          events: [],
          messages: persisted ? [message] : [],
        },
      }),
    );
    let release!: () => void;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    let delivered = false;
    await page.route(
      `**/api/v1/sessions/${sessionId}/events**`,
      async (route) => {
        await gate;
        const body = delivered
          ? 'event: ready\ndata: {"type":"ready"}\n\n'
          : `retry: 60000\nid: 5\nevent: message_end\ndata: ${JSON.stringify({ type: "message_end", message })}\n\n`;
        delivered = true;
        await route.fulfill({ contentType: "text/event-stream", body });
      },
    );
    try {
      await page.goto("/");
      await expect(
        page.getByRole("heading", { name: "Mock session" }),
      ).toBeVisible();
      await expect(page.locator("details.message-attachment")).toHaveCount(0);
      release();
      for (const reload of [false, true]) {
        if (reload) {
          persisted = true;
          await page.reload();
        }
        const cards = page.locator(".message.user details.message-attachment");
        await expect(cards).toHaveCount(2);
        await expect(cards.locator(".attachment-name")).toHaveText([
          "quarterly report.docx",
          "review  final.docx",
        ]);
        await expect(cards.locator(".attachment-meta")).toHaveText([
          "DOCX",
          "DOCX",
        ]);
        await expect(
          page.locator(".message.user .plain-message"),
        ).toContainText("Please review both.");
        for (let index = 0; index < paths.length; index++) {
          await expect(cards.nth(index)).toHaveJSProperty("open", false);
          await cards.nth(index).locator(":scope > summary").click();
          await expect(
            cards.nth(index).locator(".attachment-path code"),
          ).toHaveText(paths[index]);
        }
      }
    } finally {
      release();
    }
  });

  test("leaves code, quotes, ordinary paths, mixed invalid lists and assistant text raw", async ({
    page,
  }) => {
    await mockHost(page);
    const rawTexts = [
      `\`\`\`text\nAttached files:\n- ${docxPath}\n\`\`\``,
      `    Attached files:\n    - ${docxPath}`,
      `> Attached files:\n> - ${docxPath}`,
      `Ordinary workspace path: ${docxPath}`,
      `Attached files:\n- ${docxPath}\n- not-an-upload.docx`,
    ];
    await attachmentSnapshot(page, [
      ...rawTexts.map((text) => ({
        role: "user",
        content: [{ type: "text", text }],
      })),
      {
        role: "assistant",
        content: [{ type: "text", text: `Attached files:\n- ${docxPath}` }],
      },
    ]);
    await page.goto("/");
    const bodies = page.locator(".message.user .message-body");
    await expect(bodies).toHaveCount(rawTexts.length);
    for (let index = 0; index < rawTexts.length; index++) {
      // textContent preserves literal indentation and fences, unlike normalized text assertions.
      expect(await bodies.nth(index).textContent()).toContain(rawTexts[index]);
    }
    await expect(
      page.locator(".message.assistant .message-body"),
    ).toContainText(docxPath);
    await expect(page.locator("details.message-attachment")).toHaveCount(0);
  });

  test("preserves composer filename, size, removal and the uploaded native prompt path", async ({
    page,
  }) => {
    await mockHost(page);
    await page.goto("/");
    await expect(
      page.getByRole("heading", { name: "Mock session" }),
    ).toBeVisible();
    const upload = () =>
      page.locator('input[type="file"]').setInputFiles({
        name: "notes.txt",
        mimeType: "text/plain",
        buffer: Buffer.from("hello"),
      });
    await upload();
    const card = page.locator(".composer-attachment");
    await expect(card).toHaveCount(1);
    await expect(card.locator(".attachment-name")).toHaveText("notes.txt");
    await expect(card.locator(".attachment-meta")).toHaveText(
      /TXT\s*·\s*5\s*B/,
    );
    await card
      .getByRole("button", {
        name: "Remove attachment: notes.txt",
        exact: true,
      })
      .click();
    await expect(card).toHaveCount(0);
    await upload();
    await expect(card.locator(".attachment-name")).toHaveText("notes.txt");
    await page.locator(".composer textarea").fill("Please inspect this file");
    const prompt = page.waitForRequest(
      (request) =>
        request.url().includes(`/sessions/${sessionId}/commands`) &&
        request.method() === "POST" &&
        request.postDataJSON()?.type === "prompt",
    );
    await page.getByRole("button", { name: "Send message" }).click();
    const body = (await prompt).postDataJSON();
    expect(body.message).toContain("Please inspect this file");
    expect(body.message).toMatch(
      /(?:Attached files:|已附加文件:)\n- \.rpi\/uploads\/notes\.txt$/,
    );
    expect(body.message.match(/\.rpi\/uploads\/notes\.txt/g)).toHaveLength(1);
    await expect(card).toHaveCount(0);
  });
});

for (const width of [1280, 390]) {
  test(`user message has one localized sender label and equal corners at ${width}px`, async ({
    page,
  }) => {
    await page.setViewportSize({ width, height: 844 });
    await mockHost(page);
    await page.route(snapshotRoute, (route) =>
      route.fulfill({
        json: {
          sessionId,
          status: "ready",
          lastSeq: 4,
          events: [],
          messages: [
            { role: "user", content: "Please inspect the attachment" },
            {
              role: "assistant",
              content: [{ type: "text", text: "I will inspect it." }],
            },
          ],
        },
      }),
    );
    await page.goto("/");
    const message = page.locator(".message.user");
    await expect(message.locator(".message-heading")).toHaveText("You");
    await expect(message.locator(".user-avatar")).toHaveCount(0);
    const corners = () =>
      message.locator(".message-body").evaluate((node) => {
        const style = getComputedStyle(node);
        return [
          style.borderTopLeftRadius,
          style.borderTopRightRadius,
          style.borderBottomRightRadius,
          style.borderBottomLeftRadius,
        ];
      });
    expect(await corners()).toEqual(["16px", "16px", "16px", "16px"]);
    if (width === 390)
      await page
        .getByRole("button", { name: "Toggle sidebar", exact: true })
        .click();
    await chooseMenuOption(page, "Language", "zh");
    if (width === 390)
      await page.locator(".sidebar .sidebar-brand button").click();
    await expect(message.locator(".message-heading")).toHaveText("你");
    await expect(message.locator(".message-heading strong")).toHaveCount(1);
    await expect(
      page.locator(".message.assistant .message-heading strong"),
    ).toHaveText("rpi");
    expect(await corners()).toEqual(["16px", "16px", "16px", "16px"]);
    expect(
      await page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth,
      ),
    ).toBe(true);
  });
}

// Native timing metadata is supplemental: never derive elapsed time from tool
// messages, browser clocks, transcript positions, or an earlier session's cache.
test.describe("run timing regressions", () => {
  const timingRoute = /\/api\/v1\/sessions\/[^/]+\/run-timings\/query$/;
  const finalMessage = (
    entryId = "elapsed-final",
    text = "Final native answer",
  ) => ({
    role: "assistant",
    entryId,
    content: [{ type: "text", text }],
  });
  const nativeTiming = (
    finalEntryId = "elapsed-final",
    runId = "elapsed-run",
    outcome = "completed",
    elapsedMs = 138_000,
  ) => ({
    runId,
    lane: "main",
    finalEntryId,
    startedAt: 1000,
    finishedAt: 1000 + elapsedMs,
    elapsedMs,
    outcome,
  });
  const timingPage = (timings: unknown[] = [], extra = {}) => ({
    timings,
    complete: true,
    cursor: "g",
    scannedBytes: 100,
    totalBytes: 100,
    ...extra,
  });
  const gate = () => {
    let release!: () => void;
    const promise = new Promise<void>((resolve) => {
      release = resolve;
    });
    return { promise, release };
  };
  async function history(page: Page, messages: unknown[], extra = {}) {
    await page.route(snapshotRoute, (route) =>
      route.fulfill({
        json: { sessionId, messages, events: [], lastSeq: 4, ...extra },
      }),
    );
  }

  test("run elapsed history attaches once after the final projected row and survives reload", async ({
    page,
  }) => {
    await mockHost(page);
    const png =
      "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";
    await history(page, [
      {
        role: "assistant",
        entryId: "elapsed-tool-call",
        content: [
          { type: "text", text: "Inspecting the file" },
          {
            type: "toolCall",
            id: "elapsed-read",
            name: "read",
            arguments: { path: "README.md" },
          },
        ],
      },
      {
        role: "toolResult",
        entryId: "elapsed-tool-result",
        toolCallId: "elapsed-read",
        content: [{ type: "text", text: "File inspected" }],
      },
      {
        ...finalMessage(),
        content: [
          { type: "text", text: "First final block" },
          { type: "image", mimeType: "image/png", data: png },
          { type: "text", text: "Last final block" },
        ],
      },
    ]);
    const requests: Array<{ entryIds: string[]; cursor?: string }> = [];
    await page.route(timingRoute, (route) => {
      expect(route.request().method()).toBe("POST");
      requests.push(route.request().postDataJSON());
      return route.fulfill({ json: timingPage([nativeTiming()]) });
    });
    const footer = page.locator('.run-elapsed[data-run-id="elapsed-run"]');
    for (let load = 0; load < 2; load++) {
      if (load) await page.reload();
      else await page.goto("/");
      await expect(footer).toHaveText("Worked for 2m 18s");
      await expect(page.locator(".run-elapsed")).toHaveCount(1);
      await expect(footer).toHaveAttribute("data-entry-id", "elapsed-final");
      const row = page.locator(".transcript-row").filter({ has: footer });
      await expect(row).toContainText("Last final block");
      await expect(row.locator(":scope > .run-elapsed:last-child")).toHaveCount(
        1,
      );
      await expect(
        row.locator("xpath=following-sibling::*[@data-transcript-row]"),
      ).toHaveCount(0);
      await expect(page.locator(".tool-card .run-elapsed")).toHaveCount(0);
    }
    expect(requests.length).toBeGreaterThanOrEqual(2);
    for (const request of requests)
      expect(request.entryIds).toContain("elapsed-final");
    expect(
      requests.filter((request) => !request.cursor).length,
    ).toBeGreaterThanOrEqual(2);
  });

  test("run elapsed live completion refreshes native snapshot before attaching the footer", async ({
    page,
  }) => {
    await mockHost(page);
    const settled = gate();
    let snapshots = 0;
    let queriedFinal = false;
    await page.route(snapshotRoute, (route) =>
      route.fulfill({
        json: {
          sessionId,
          lastSeq: snapshots++ ? 5 : 4,
          events: [],
          messages:
            snapshots > 1
              ? [finalMessage()]
              : [
                  {
                    role: "user",
                    entryId: "elapsed-prompt",
                    content: "Please work",
                  },
                ],
        },
      }),
    );
    await page.route(timingRoute, (route) => {
      const { entryIds } = route.request().postDataJSON();
      queriedFinal ||= entryIds.includes("elapsed-final");
      return route.fulfill({
        json: timingPage(queriedFinal ? [nativeTiming()] : []),
      });
    });
    await page.route(
      `**/api/v1/sessions/${sessionId}/events?*`,
      async (route) => {
        await settled.promise;
        return route.fulfill({
          contentType: "text/event-stream",
          body: 'id: 5\nevent: agent_settled\ndata: {"type":"agent_settled","runId":"elapsed-run","outcome":"completed"}\n\n',
        });
      },
    );
    await page.goto("/");
    await expect(page.locator(".message.user")).toContainText("Please work");
    await expect(page.locator(".run-elapsed")).toHaveCount(0);
    settled.release();
    await expect(
      page.locator('.run-elapsed[data-run-id="elapsed-run"]'),
    ).toHaveText("Worked for 2m 18s");
    expect(snapshots).toBeGreaterThanOrEqual(2);
    expect(queriedFinal).toBe(true);
    await expect(page.locator(".run-elapsed")).toHaveCount(1);
  });

  test("run timing older pagination attaches multiple footers without replacing current timing", async ({
    page,
  }) => {
    await mockHost(page);
    await history(page, [finalMessage()], {
      firstId: "elapsed-final",
      hasMore: true,
    });
    await page.route(`**/api/v1/sessions/${sessionId}/commands`, (route) => {
      const body = route.request().postDataJSON();
      if (body.type !== "get_messages") return route.fallback();
      expect(body.beforeId).toBe("elapsed-final");
      return route.fulfill({
        json: response(
          {
            messages: [
              finalMessage("older-a", "Older answer A"),
              finalMessage("older-b", "Older answer B"),
            ],
            firstId: "older-a",
            hasMore: false,
          },
          "get_messages",
        ),
      });
    });
    const queries: string[][] = [];
    await page.route(timingRoute, (route) => {
      const { entryIds } = route.request().postDataJSON();
      queries.push(entryIds);
      return route.fulfill({
        json: timingPage(
          [
            nativeTiming(),
            nativeTiming("older-a", "run-older-a"),
            nativeTiming("older-b", "run-older-b"),
          ].filter((timing) => entryIds.includes(timing.finalEntryId)),
        ),
      });
    });
    await page.goto("/");
    await expect(page.locator(".run-elapsed")).toHaveCount(1);
    await page.getByRole("button", { name: "Load earlier messages" }).click();
    await expect(page.locator(".run-elapsed")).toHaveCount(3);
    for (const [id, text] of [
      ["run-older-a", "Older answer A"],
      ["run-older-b", "Older answer B"],
      ["elapsed-run", "Final native answer"],
    ]) {
      const footer = page.locator(`.run-elapsed[data-run-id="${id}"]`);
      await expect(footer).toHaveCount(1);
      await expect(
        page.locator(".transcript-row").filter({ has: footer }),
      ).toContainText(text);
    }
    expect(
      queries.some((ids) => ids.includes("older-a") && ids.includes("older-b")),
    ).toBe(true);
  });

  test("run timing session switch discards a stale response even with the same final entry ID", async ({
    page,
  }) => {
    await mockHost(page);
    await page.route(
      (url) => /\/sessions\/[^/]+\/snapshot$/.test(url.pathname),
      (route) =>
        route.fulfill({
          json: {
            sessionId: new URL(route.request().url()).pathname.split("/")[4],
            messages: [finalMessage()],
            events: [],
            lastSeq: 4,
          },
        }),
    );
    const old = gate();
    let pending = false;
    let delivered = false;
    await page.route(timingRoute, async (route) => {
      if (route.request().url().includes(`/${sessionId}/`)) {
        pending = true;
        await old.promise;
        await route.fulfill({
          json: timingPage([nativeTiming("elapsed-final", "stale-run")]),
        });
        delivered = true;
      } else {
        await route.fulfill({
          json: timingPage([nativeTiming("elapsed-final", "new-run")]),
        });
      }
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
    await expect(
      page.locator('.run-elapsed[data-run-id="new-run"]'),
    ).toHaveText("Worked for 2m 18s");
    old.release();
    await expect.poll(() => delivered).toBe(true);
    await chooseMenuOption(page, "Language", "zh");
    await expect(
      page.locator('.run-elapsed[data-run-id="new-run"]'),
    ).toHaveText("已用时 2 分 18 秒");
    await expect(
      page.locator('.run-elapsed[data-run-id="stale-run"]'),
    ).toHaveCount(0);
    await expect(page.locator(".run-elapsed")).toHaveCount(1);
  });

  test("run timing 409 reset clears published timing and restarts without the stale cursor", async ({
    page,
  }) => {
    await mockHost(page);
    await history(page, [finalMessage()]);
    const conflict = gate();
    const restart = gate();
    const queries: Array<{ entryIds: string[]; cursor?: string }> = [];
    await page.route(timingRoute, async (route) => {
      queries.push(route.request().postDataJSON());
      if (queries.length === 1)
        return route.fulfill({
          json: timingPage([nativeTiming("elapsed-final", "before-reset")], {
            complete: false,
            cursor: "obsolete",
          }),
        });
      if (queries.length === 2) {
        await conflict.promise;
        return route.fulfill({ status: 409, json: { error: "cursor reset" } });
      }
      await restart.promise;
      return route.fulfill({
        json: timingPage([nativeTiming("elapsed-final", "after-reset")]),
      });
    });
    await page.goto("/");
    await expect(
      page.locator('.run-elapsed[data-run-id="before-reset"]'),
    ).toHaveCount(1);
    await expect.poll(() => queries.length).toBe(2);
    expect(queries[1].cursor).toBe("obsolete");
    conflict.release();
    await expect.poll(() => queries.length).toBe(3);
    await expect(page.locator(".run-elapsed")).toHaveCount(0);
    expect(queries[2].cursor).toBeUndefined();
    expect(queries[2].entryIds).toEqual(queries[0].entryIds);
    restart.release();
    await expect(
      page.locator('.run-elapsed[data-run-id="after-reset"]'),
    ).toHaveText("Worked for 2m 18s");
    await expect(
      page.locator('.run-elapsed[data-run-id="before-reset"]'),
    ).toHaveCount(0);
  });

  test("run timing continuation follows cursor until complete and does not duplicate a repeated timing", async ({
    page,
  }) => {
    await mockHost(page);
    await history(page, [finalMessage()]);
    const queries: Array<{ cursor?: string }> = [];
    await page.route(timingRoute, (route) => {
      queries.push(route.request().postDataJSON());
      const n = queries.length;
      return route.fulfill({
        json: timingPage(n === 1 ? [] : [nativeTiming()], {
          complete: n >= 3,
          cursor: `slice-${n}`,
          scannedBytes: n * 30,
        }),
      });
    });
    await page.goto("/");
    await expect.poll(() => queries.length).toBe(3);
    expect(queries.map((query) => query.cursor)).toEqual([
      undefined,
      "slice-1",
      "slice-2",
    ]);
    await expect(page.locator(".run-elapsed")).toHaveCount(1);
    await expect(page.locator(".run-elapsed")).toHaveText("Worked for 2m 18s");
  });

  test("run timing stalled continuation is bounded and never invents a footer", async ({
    page,
  }) => {
    await mockHost(page);
    await history(page, [finalMessage()]);
    let requests = 0;
    await page.route(timingRoute, (route) => {
      requests++;
      return route.fulfill({
        json: timingPage([], {
          complete: false,
          cursor: "stalled",
          scannedBytes: 10,
        }),
      });
    });
    await page.goto("/");
    await expect.poll(() => requests, { timeout: 8000 }).toBe(5);
    // Exceed the backoff to distinguish stopped work from a slow retry loop.
    await page.waitForTimeout(1300);
    expect(requests).toBe(5);
    await expect(page.locator(".run-elapsed")).toHaveCount(0);
    await expect(page.locator(".error-banner")).toHaveCount(0);
  });

  test("run timing old backend 404 stays quiet and disables further lookups on pagination", async ({
    page,
  }) => {
    await mockHost(page);
    await history(page, [finalMessage()], {
      firstId: "elapsed-final",
      hasMore: true,
    });
    await page.route(`**/api/v1/sessions/${sessionId}/commands`, (route) => {
      if (route.request().postDataJSON().type !== "get_messages")
        return route.fallback();
      return route.fulfill({
        json: response(
          {
            messages: [finalMessage("older", "Older native answer")],
            firstId: "older",
            hasMore: false,
          },
          "get_messages",
        ),
      });
    });
    let requests = 0;
    const errors: string[] = [];
    page.on("pageerror", (error) => errors.push(error.message));
    await page.route(timingRoute, (route) => {
      requests++;
      return route.fulfill({
        status: 404,
        json: { error: "Unknown endpoint" },
      });
    });
    await page.goto("/");
    await expect.poll(() => requests).toBe(1);
    await page.getByRole("button", { name: "Load earlier messages" }).click();
    await expect(page.locator(".message.assistant").first()).toContainText(
      "Older native answer",
    );
    await chooseMenuOption(page, "Language", "zh");
    await expect(page.locator(".run-elapsed")).toHaveCount(0);
    await expect(page.locator(".error-banner")).toHaveCount(0);
    expect(requests).toBe(1);
    expect(errors).toEqual([]);
  });

  test("run timing invalid or missing metadata never produces a local fallback footer", async ({
    page,
  }) => {
    await mockHost(page);
    await history(page, [
      finalMessage(),
      finalMessage("missing", "No native timing"),
    ]);
    let queried = false;
    await page.route(timingRoute, (route) => {
      queried = true;
      return route.fulfill({
        json: timingPage([
          { ...nativeTiming(), elapsedMs: -1 },
          { ...nativeTiming("elapsed-final", "mismatch"), finishedAt: 2000 },
          {
            ...nativeTiming("elapsed-final", "fractional"),
            elapsedMs: 1.5,
            finishedAt: 1001.5,
          },
          {
            ...nativeTiming("elapsed-final", "unknown-outcome"),
            outcome: "running",
          },
          {
            ...nativeTiming("elapsed-final", "missing-duration"),
            elapsedMs: undefined,
          },
          nativeTiming("not-in-transcript", "unrelated"),
        ]),
      });
    });
    await page.goto("/");
    await expect.poll(() => queried).toBe(true);
    await expect(page.locator(".message.assistant")).toHaveCount(2);
    await chooseMenuOption(page, "Language", "zh");
    await expect(page.locator(".run-elapsed")).toHaveCount(0);
    await expect(page.locator(".error-banner")).toHaveCount(0);
  });

  test("run timing repeated 409 clears each pass including the exhausted third conflict", async ({
    page,
  }) => {
    await mockHost(page);
    await history(page, [finalMessage()]);
    const conflicts = [gate(), gate(), gate()];
    const queries: Array<{ entryIds: string[]; cursor?: string }> = [];
    await page.route(timingRoute, async (route) => {
      queries.push(route.request().postDataJSON());
      const index = queries.length - 1;
      const pass = Math.floor(index / 2);
      if (pass >= conflicts.length)
        return route.fulfill({ status: 500, json: { error: "retry budget exceeded" } });
      if (index % 2 === 0)
        return route.fulfill({
          json: timingPage([nativeTiming("elapsed-final", `pass-${pass}`)], {
            complete: false,
            cursor: `pass-${pass}-cursor`,
          }),
        });
      await conflicts[pass].promise;
      return route.fulfill({ status: 409, json: { error: "cursor reset" } });
    });
    await page.goto("/");
    for (let pass = 0; pass < conflicts.length; pass++) {
      await expect.poll(() => queries.length).toBe((pass + 1) * 2);
      await expect(page.locator(".run-elapsed")).toHaveCount(1);
      await expect(
        page.locator(`.run-elapsed[data-run-id="pass-${pass}"]`),
      ).toBeVisible();
      expect(queries[pass * 2].cursor).toBeUndefined();
      expect(queries[pass * 2].entryIds).toEqual(["elapsed-final"]);
      expect(queries[pass * 2 + 1].cursor).toBe(`pass-${pass}-cursor`);
      conflicts[pass].release();
    }
    await expect(page.locator(".run-elapsed")).toHaveCount(0);
    // Longer than the stalled-scan backoff: exhausted conflicts must stop work.
    await page.waitForTimeout(1300);
    expect(queries).toHaveLength(6);
    await expect(page.locator(".error-banner")).toHaveCount(0);
  });

  for (const olderPage of [false, true]) {
    test(`run elapsed terminal tool result follows trailing assistant text ${olderPage ? "after loading earlier messages" : "and survives reload"}`, async ({
      page,
    }) => {
      await mockHost(page);
      const call = {
        role: "assistant",
        entryId: "terminal-call",
        content: [
          {
            type: "toolCall",
            id: "terminal",
            name: "read",
            arguments: { path: "terminal.txt" },
          },
          { type: "text", text: "Trailing visible assistant text" },
        ],
      };
      const result = {
        role: "toolResult",
        entryId: "terminal-result",
        toolCallId: "terminal",
        content: [{ type: "text", text: "Terminal tool output" }],
      };
      await history(page, olderPage ? [result] : [call, result], {
        firstId: olderPage ? "terminal-result" : "terminal-call",
        hasMore: olderPage,
      });
      if (olderPage)
        await page.route(`**/api/v1/sessions/${sessionId}/commands`, (route) => {
          const body = route.request().postDataJSON();
          if (body.type !== "get_messages") return route.fallback();
          expect(body.beforeId).toBe("terminal-result");
          return route.fulfill({
            json: response(
              { messages: [call], firstId: "terminal-call", hasMore: false },
              "get_messages",
            ),
          });
        });
      await page.route(timingRoute, (route) => {
        expect(route.request().postDataJSON().entryIds).toContain("terminal-result");
        return route.fulfill({
          json: timingPage([
            nativeTiming("terminal-result", "terminal-run", "aborted"),
          ]),
        });
      });
      const footer = page.locator('.run-elapsed[data-run-id="terminal-run"]');
      for (let load = 0; load < (olderPage ? 1 : 2); load++) {
        if (load) await page.reload();
        else await page.goto("/");
        await expect(footer).toBeVisible();
        if (olderPage)
          await page.getByRole("button", { name: "Load earlier messages" }).click();
        await expect(page.locator(".message.assistant")).toContainText(
          "Trailing visible assistant text",
        );
        await expect(page.locator(".tool-card")).toContainText("Terminal tool output");
        await expect(page.locator(".run-elapsed")).toHaveCount(1);
        await expect(footer).toHaveAttribute("data-entry-id", "terminal-result");
        await expect(footer).toHaveText("Worked for 2m 18s · Stopped");
        await expect(page.locator(".tool-card .run-elapsed")).toHaveCount(0);
        await expect.poll(async () => {
          const bounds = await footer.boundingBox();
          const outputs = await page.locator(".tool-card, .message.assistant").all();
          if (!bounds || outputs.length < 2) return false;
          for (const output of outputs) {
            const box = await output.boundingBox();
            if (!box || bounds.y < box.y + box.height - 1) return false;
          }
          return true;
        }).toBe(true);
        await expect(
          page.locator(".transcript-row").filter({ has: footer })
            .locator("xpath=following-sibling::*[@data-transcript-row]"),
        ).toHaveCount(0);
      }
    });
  }

  test("run elapsed mobile 390px localizes completed hours and aborted failed markers without overflow", async ({
    page,
  }) => {
    await page.setViewportSize({ width: 390, height: 844 });
    await mockHost(page);
    await history(page, [
      finalMessage(),
      finalMessage("hour-final", "Long completed run"),
      finalMessage("aborted-final", "Interrupted answer"),
      finalMessage("failed-final", "Failed answer"),
    ]);
    await page.route(timingRoute, (route) =>
      route.fulfill({
        json: timingPage([
          nativeTiming(),
          nativeTiming("hour-final", "hour-run", "completed", 3_738_000),
          nativeTiming("aborted-final", "aborted-run", "aborted"),
          nativeTiming("failed-final", "failed-run", "failed"),
        ]),
      }),
    );
    await page.goto("/");
    await expect(page.locator(".run-elapsed")).toHaveCount(4);
    const assertFits = async () => {
      expect(
        await page.evaluate(
          () => document.documentElement.scrollWidth <= innerWidth,
        ),
      ).toBe(true);
      for (const footer of await page.locator(".run-elapsed").all()) {
        expect(
          await footer.evaluate((node) => {
            const rect = node.getBoundingClientRect();
            return (
              rect.left >= 0 &&
              rect.right <= innerWidth &&
              node.scrollWidth <= node.clientWidth + 1
            );
          }),
        ).toBe(true);
      }
    };
    await expect(
      page.locator('.run-elapsed[data-run-id="elapsed-run"]'),
    ).toHaveText("Worked for 2m 18s");
    await expect(
      page.locator('.run-elapsed[data-run-id="hour-run"]'),
    ).toHaveText("Worked for 1h 2m 18s");
    await expect(page.locator(".run-aborted .run-elapsed-outcome")).toHaveText(
      " · Stopped",
    );
    await expect(page.locator(".run-failed .run-elapsed-outcome")).toHaveText(
      " · Failed",
    );
    await assertFits();
    await page
      .getByRole("button", { name: "Toggle sidebar", exact: true })
      .click();
    await chooseMenuOption(page, "Language", "zh");
    await page.locator(".sidebar .sidebar-brand button").click();
    await expect(
      page.locator('.run-elapsed[data-run-id="elapsed-run"]'),
    ).toHaveText("已用时 2 分 18 秒");
    await expect(
      page.locator('.run-elapsed[data-run-id="hour-run"]'),
    ).toHaveText("已用时 1 小时 2 分 18 秒");
    await expect(page.locator(".run-aborted .run-elapsed-outcome")).toHaveText(
      " · 已停止",
    );
    await expect(page.locator(".run-failed .run-elapsed-outcome")).toHaveText(
      " · 失败",
    );
    await assertFits();
  });

  test("run timing batches over 400 entry IDs and restarts earlier batches on a later conflict", async ({ page }) => {
    await mockHost(page);
    await history(page, Array.from({ length: 401 }, (_, i) => finalMessage(`batch-${i}`, `Answer ${i}`)));
    const conflict = gate();
    const refreshed = gate();
    const queries: Array<{ entryIds: string[]; cursor?: string }> = [];
    await page.route(timingRoute, async route => {
      const query = route.request().postDataJSON();
      queries.push(query);
      if (queries.length === 1) return route.fulfill({ json: timingPage([nativeTiming("batch-0", "old-first")], { cursor: "g1" }) });
      if (queries.length === 2) {
        await conflict.promise;
        return route.fulfill({ status: 409, json: { error: "generation replaced" } });
      }
      if (queries.length === 3) {
        await refreshed.promise;
        return route.fulfill({ json: timingPage([nativeTiming("batch-0", "fresh-first")], { cursor: "g2" }) });
      }
      return route.fulfill({ json: timingPage([nativeTiming("batch-400", "fresh-last")], { cursor: "g2" }) });
    });
    await page.goto("/");
    await expect(page.locator('.run-elapsed[data-run-id="old-first"]')).toHaveCount(1);
    await expect.poll(() => queries.length).toBe(2);
    expect(queries.map(q => q.entryIds.length)).toEqual([400, 1]);
    expect(queries[1].cursor).toBe("g1");
    conflict.release();
    await expect.poll(() => queries.length).toBe(3);
    await expect(page.locator(".run-elapsed")).toHaveCount(0);
    expect(queries[2].cursor).toBeUndefined();
    expect(queries[2].entryIds).toEqual(queries[0].entryIds);
    refreshed.release();
    await expect(page.locator('.run-elapsed[data-run-id="fresh-first"]')).toHaveCount(1);
    await expect(page.locator('.run-elapsed[data-run-id="fresh-last"]')).toHaveCount(1);
    await expect(page.locator('.run-elapsed[data-run-id="old-first"]')).toHaveCount(0);
    expect(queries[3].cursor).toBe("g2");
    expect(queries[3].entryIds).toEqual(["batch-400"]);
    expect(queries).toHaveLength(4);
  });
});

// Command previews must never truncate the native command used for display/copy.
test.describe("shell command previews", () => {
  const wrapped = `printf '%s\n' ${Array.from({ length: 90 }, (_, i) => `'argument ${i}'`).join(" ")}`;
  const heredoc = [
    "python - <<'PY'",
    'message = "quoted value"',
    "values = ['one', 'two']",
    "for value in values:",
    "    print(message, value)",
    "print('last visible only when expanded')",
    "PY",
  ].join("\n");

  async function commandSnapshot(page: Page, commands: string[]) {
    await mockHost(page);
    await page.route(snapshotRoute, (route) =>
      route.fulfill({
        json: {
          sessionId,
          status: "ready",
          lastSeq: 4,
          events: [],
          messages: commands.flatMap((command, index) => [
            {
              role: "assistant",
              content: [{
                type: "toolCall",
                id: `preview-${index}`,
                name: "bash",
                arguments: { command },
              }],
            },
            {
              role: "toolResult",
              toolCallId: `preview-${index}`,
              toolName: "bash",
              isError: false,
              content: [{ type: "text", text: "done" }],
            },
          ]),
        },
      }),
    );
  }

  async function assertFits(page: Page) {
    await expect.poll(() => page.evaluate(() =>
      document.documentElement.scrollWidth <= innerWidth,
    )).toBe(true);
    await expect.poll(() => page.locator(".transcript").evaluate((node) =>
      node.scrollWidth <= node.clientWidth + 1,
    )).toBe(true);
  }

  test("clips wrapped and heredoc commands to four rendered lines on desktop and mobile until explicitly expanded", async ({ page }) => {
    await commandSnapshot(page, [wrapped, heredoc, "pwd"]);
    for (const width of [1280, 390]) {
      await page.setViewportSize({ width, height: 844 });
      await page.goto("/");
      const cards = page.locator(".shell-card");
      await expect(cards).toHaveCount(3);
      for (const [index, command] of [wrapped, heredoc, "pwd"].entries()) {
        const card = cards.nth(index);
        await expect(card).toHaveJSProperty("open", false);
        await card.locator(":scope > summary").click();
        const preview = card.locator("pre.shell-input");
        const code = preview.locator("code");
        await expect(code).toBeVisible();
        expect(await code.textContent()).toBe(command);
        const show = card.getByRole("button", { name: "Show full command", exact: true });
        if (index === 2) {
          await expect(show).toHaveCount(0);
          continue;
        }
        await expect(show).toHaveAttribute("aria-expanded", "false");
        const codeId = await code.getAttribute("id");
        expect(codeId).toBeTruthy();
        await expect(show).toHaveAttribute("aria-controls", codeId!);
        expect(await show.evaluate((node) => node.closest("pre") === null)).toBe(true);
        const assertClipped = async () => {
          await expect.poll(() => preview.evaluate((node) => {
            const style = getComputedStyle(node);
            const padding = parseFloat(style.paddingTop) + parseFloat(style.paddingBottom);
            return node.scrollHeight > node.clientHeight + 1 &&
              Math.abs(node.clientHeight - padding - 4 * parseFloat(style.lineHeight)) <= 2;
          })).toBe(true);
        };
        await assertClipped();
        await show.click();
        const collapse = card.getByRole("button", { name: "Collapse command", exact: true });
        await expect(collapse).toHaveAttribute("aria-expanded", "true");
        await expect(collapse).toHaveAttribute("aria-controls", codeId!);
        await expect.poll(() => preview.evaluate((node) =>
          node.scrollHeight <= node.clientHeight + 1,
        )).toBe(true);
        expect(await code.textContent()).toBe(command);
        await assertFits(page);
        await collapse.click();
        await expect(show).toHaveAttribute("aria-expanded", "false");
        await assertClipped();
        await expect(card).toHaveJSProperty("open", true);
      }
      await assertFits(page);
    }
  });

  test("copies the exact full collapsed command and supports Enter and Space expansion", async ({ page }) => {
    await page.addInitScript(() => {
      Object.defineProperty(navigator, "clipboard", {
        configurable: true,
        value: {
          writeText: async (text: string) => {
            (window as Window & { shellCopiedCommand?: string }).shellCopiedCommand = text;
          },
        },
      });
    });
    await commandSnapshot(page, [heredoc]);
    await page.goto("/");
    const card = page.locator(".shell-card");
    await expect(card).toHaveJSProperty("open", false);
    await card.locator(":scope > summary").click();
    const show = card.getByRole("button", { name: "Show full command", exact: true });
    await expect(show).toHaveAttribute("aria-expanded", "false");
    await card.getByRole("button", { name: "Copy command", exact: true }).click();
    await expect.poll(() => page.evaluate(() =>
      (window as Window & { shellCopiedCommand?: string }).shellCopiedCommand,
    )).toBe(heredoc);
    await expect(show).toHaveAttribute("aria-expanded", "false");
    for (const key of ["Enter", "Space"]) {
      await show.focus();
      await page.keyboard.press(key);
      const collapse = card.getByRole("button", { name: "Collapse command", exact: true });
      await expect(collapse).toHaveAttribute("aria-expanded", "true");
      await expect.poll(() => card.locator("pre.shell-input").evaluate((node) =>
        node.scrollHeight <= node.clientHeight + 1,
      )).toBe(true);
      await collapse.focus();
      await page.keyboard.press(key);
      await expect(show).toHaveAttribute("aria-expanded", "false");
      await expect(card).toHaveJSProperty("open", true);
    }
    expect(await card.locator(".shell-input code").textContent()).toBe(heredoc);
  });

  test("localizes command controls in Chinese and detects overflow after a mobile resize without changing native text", async ({ page }) => {
    await page.addInitScript(() => localStorage.setItem("rpi-web-language", "zh"));
    // Under four rendered lines at 1280px, but over four at 390px.
    const command = `printf '%s\n' ${Array.from({ length: 24 }, () => "token").join(" ")}`;
    await page.setViewportSize({ width: 1280, height: 844 });
    await commandSnapshot(page, [command]);
    await page.goto("/");
    await page.getByRole("button", { name: "关闭项目面板", exact: true }).click();
    const card = page.locator(".shell-card");
    await expect(card).toHaveJSProperty("open", false);
    await card.locator(":scope > summary").click();
    const preview = card.locator("pre.shell-input");
    const code = preview.locator("code");
    await expect(code).toBeVisible();
    expect(await code.textContent()).toBe(command);
    await expect.poll(() => preview.evaluate((node) =>
      node.scrollHeight <= node.clientHeight + 1,
    )).toBe(true);
    const show = card.getByRole("button", { name: "展开完整命令", exact: true });
    await expect(show).toHaveCount(0);
    // No navigation or new snapshot: ResizeObserver must remeasure the same command.
    await page.setViewportSize({ width: 390, height: 844 });
    await page.locator(".sidebar .sidebar-brand button").click();
    await expect(show).toHaveAttribute("aria-expanded", "false");
    await expect.poll(() => preview.evaluate((node) =>
      node.scrollHeight > node.clientHeight + 1,
    )).toBe(true);
    expect(await code.textContent()).toBe(command);
    await show.click();
    const collapse = card.getByRole("button", { name: "收起命令", exact: true });
    await expect(collapse).toHaveAttribute("aria-expanded", "true");
    await expect.poll(() => preview.evaluate((node) =>
      node.scrollHeight <= node.clientHeight + 1,
    )).toBe(true);
    await assertFits(page);
    await collapse.click();
    await expect(show).toHaveAttribute("aria-expanded", "false");
    await page.setViewportSize({ width: 1280, height: 844 });
    await expect(show).toHaveCount(0);
    await expect(card).toHaveJSProperty("open", true);
    expect(await code.textContent()).toBe(command);
  });
});

test.describe("Mermaid diagrams", () => {
  const flow = 'flowchart TD\n  start[开始] --> process[处理数据]\n  process --> done[完成]\n';
  const sequence = 'sequenceDiagram\n  participant A as 用户\n  participant B as 服务\n  A->>B: 你好\n  B-->>A: 收到\n';
  const failure = "Unable to render this diagram. The syntax may be incomplete or unsupported; source is shown below.";
  const fence = (source: string) => `\`\`\`mermaid\n${source}\`\`\``;

  async function diagramSnapshot(page: Page, text: string) {
    await mockHost(page);
    await page.route(snapshotRoute, (route) => route.fulfill({
      json: {
        sessionId,
        messages: [{ role: "assistant", entryId: "mermaid-message", content: [{ type: "text", text }] }],
        events: [], lastSeq: 4, total: 1, hasMore: false,
      },
    }));
  }

  async function decodedDiagram(image: ReturnType<Page["locator"]>) {
    await expect(image).toHaveAttribute("src", /^data:image\/svg\+xml/);
    await expect.poll(() => image.evaluate(async (node: HTMLImageElement) => {
      try { await node.decode(); return node.naturalWidth; } catch { return 0; }
    })).toBeGreaterThan(0);
    return image.evaluate((node: HTMLImageElement) => {
      const svg = decodeURIComponent(node.src.slice(node.src.indexOf(",") + 1));
      const document = new DOMParser().parseFromString(svg, "image/svg+xml");
      return {
        svg,
        text: document.documentElement.textContent ?? "",
        viewBox: document.documentElement.getAttribute("viewBox"),
        parseErrors: document.querySelectorAll("parsererror").length,
      };
    });
  }

  async function fits(page: Page) {
    await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1)).toBe(true);
    await expect.poll(() => page.locator(".transcript").evaluate((node) => node.scrollWidth <= node.clientWidth + 1)).toBe(true);
  }

  // Reconnection gates mirror the existing streamed Markdown tests. No native host
  // or Mermaid stub is involved; each stage is a real protocol message patch.
  async function streamStages(page: Page, stages: string[]) {
    const releases: Array<() => void> = [];
    const gates = stages.map(() => new Promise<void>((resolve) => releases.push(resolve)));
    let connection = 0;
    await page.route(`**/api/v1/sessions/${sessionId}/events**`, async (route) => {
      const index = connection++;
      if (index >= stages.length) {
        return route.fulfill({ contentType: "text/event-stream", body: 'retry: 60000\nevent: ready\ndata: {"type":"ready"}\n\n' });
      }
      await gates[index];
      const message = { role: "assistant", content: [{ type: "text", text: stages[index] }] };
      const event = (seq: number, type: string) =>
        `id: ${seq}\nevent: ${type}\ndata: ${JSON.stringify({ type, message })}\n\n`;
      return route.fulfill({
        contentType: "text/event-stream",
        body: "retry: 20\n" + event(5 + index * 2, index === 0 ? "message_start" : "message_update") +
          (index === stages.length - 1 ? event(6 + index * 2, "message_end") : ""),
      });
    });
    return releases;
  }

  test("renders Chinese flowcharts and sequences as decoded SVG images beside ordinary code, with exact source and copy", async ({ page }) => {
    await page.addInitScript(() => {
      Object.defineProperty(navigator, "clipboard", {
        configurable: true,
        value: { writeText: async (text: string) => { (window as Window & { mermaidCopied?: string }).mermaidCopied = text; } },
      });
    });
    await diagramSnapshot(page, `${fence(flow)}\n\n${fence(sequence)}\n\n\`\`\`js\nconst answer = 42;\n\`\`\``);
    await page.goto("/");
    const blocks = page.locator(".mermaid-block");
    await expect(blocks).toHaveCount(2);
    for (const [index, label] of ["开始", "用户"].entries()) {
      const block = blocks.nth(index);
      await expect(block).toHaveClass(/markdown-code-block/);
      const decoded = await decodedDiagram(block.locator(".mermaid-diagram img"));
      expect(decoded.parseErrors).toBe(0);
      expect(decoded.text).toContain(label);
      expect(decoded.viewBox).toMatch(/\S+\s+\S+\s+\S+\s+\S+/);
      expect(decoded.svg).not.toContain("<foreignObject");
      await expect(block.locator(".mermaid-diagram svg, object, iframe")).toHaveCount(0);
    }
    await expect(page.locator(".markdown-code-block:not(.mermaid-block) pre code")).toHaveText("const answer = 42;\n");
    const first = blocks.first();
    await expect(first.locator(".mermaid-source")).toHaveCount(0);
    await first.getByRole("button", { name: "View source", exact: true }).click();
    await expect(first.locator(".mermaid-source")).toBeVisible();
    expect(await first.locator(".mermaid-source code").textContent()).toBe(flow);
    await first.getByRole("button", { name: "Copy code", exact: true }).click();
    await expect.poll(() => page.evaluate(() => (window as Window & { mermaidCopied?: string }).mermaidCopied)).toBe(flow);
    await first.getByRole("button", { name: "View diagram", exact: true }).click();
    await decodedDiagram(first.locator(".mermaid-diagram img"));
    await expect(first.locator(".mermaid-source")).toHaveCount(0);
  });

  test("follows root theme mutations on desktop and 390px, fits the keyboard image viewer and restores focus", async ({ page }) => {
    await page.setViewportSize({ width: 1280, height: 844 });
    await page.addInitScript(() => localStorage.setItem("rpi-theme", "light"));
    await diagramSnapshot(page, fence(flow));
    await page.goto("/");
    const block = page.locator(".mermaid-block");
    const image = block.locator(".mermaid-diagram img");
    const light = await decodedDiagram(image);
    await expect(block).toHaveAttribute("data-theme", "light");
    await fits(page);
    const lightSource = await image.getAttribute("src");
    await page.evaluate(() => { document.documentElement.dataset.theme = "dark"; });
    await expect(block).toHaveAttribute("data-theme", "dark");
    await expect(image).not.toHaveAttribute("src", lightSource!);
    const dark = await decodedDiagram(image);
    for (const label of ["开始", "处理数据", "完成"]) {
      expect(light.text).toContain(label);
      expect(dark.text).toContain(label);
    }
    await page.setViewportSize({ width: 390, height: 844 });
    await fits(page);
    const enlarge = block.getByRole("button", { name: "Enlarge image: Mermaid diagram", exact: true });
    await enlarge.scrollIntoViewIfNeeded();
    await enlarge.focus();
    await page.keyboard.press("Enter");
    const viewer = page.getByRole("dialog", { name: "Image viewer", exact: true });
    await expect(viewer).toBeVisible();
    await decodedDiagram(viewer.locator("img"));
    const box = await viewer.locator("img").boundingBox();
    expect(box!.width).toBeLessThan(390);
    expect(box!.height).toBeLessThan(844);
    await viewer.getByRole("button", { name: "Original size", exact: true }).click();
    await viewer.getByRole("button", { name: "Fit to window", exact: true }).click();
    await page.keyboard.press("Escape");
    await expect(viewer).toHaveCount(0);
    await expect(enlarge).toBeFocused();
    await fits(page);
  });

  test("localizes source, diagram and image controls in Chinese", async ({ page }) => {
    await page.addInitScript(() => localStorage.setItem("rpi-web-language", "zh"));
    await diagramSnapshot(page, fence(flow));
    await page.goto("/");
    const block = page.locator(".mermaid-block");
    const image = block.locator(".mermaid-diagram img");
    await decodedDiagram(image);
    await expect(image).toHaveAttribute("alt", "Mermaid 流程图");
    await block.getByRole("button", { name: "查看源码", exact: true }).click();
    expect(await block.locator(".mermaid-source code").textContent()).toBe(flow);
    await expect(block.getByRole("button", { name: "复制代码", exact: true })).toBeVisible();
    await block.getByRole("button", { name: "查看图表", exact: true }).click();
    await image.click();
    const viewer = page.getByRole("dialog", { name: "图片查看器", exact: true });
    await expect(viewer).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(viewer).toHaveCount(0);
  });

  test("shows exact invalid source and recovers when SSE repairs the diagram without render orphans", async ({ page }) => {
    const errors: string[] = [];
    page.on("pageerror", (error) => errors.push(error.message));
    const invalid = "flowchart TD\n  start[unfinished\n";
    await diagramSnapshot(page, "");
    const release = await streamStages(page, [fence(invalid), fence(flow)]);
    try {
      await page.goto("/");
      release[0]();
      const block = page.locator(".mermaid-block");
      await expect(block).toHaveCount(1);
      await expect(block.locator(".mermaid-status")).toHaveText(failure);
      expect(await block.locator(".mermaid-source code").textContent()).toBe(invalid);
      await expect(block.locator(".mermaid-diagram img")).toHaveCount(0);
      release[1]();
      const decoded = await decodedDiagram(block.locator(".mermaid-diagram img"));
      expect(decoded.text).toContain("开始");
      await expect(block.locator(".mermaid-status")).toHaveCount(0);
      await block.getByRole("button", { name: "View source", exact: true }).click();
      expect(await block.locator(".mermaid-source code").textContent()).toBe(flow);
      await expect(page.locator(".mermaid-render-scratch")).toHaveCount(0);
      expect(errors).toEqual([]);
    } finally {
      // Release route handlers even if an assertion fails before the repair.
      release.forEach((resolve) => resolve());
    }
  });

  test("rejects source-owned config and external resources while rendering strict click links only as an image", async ({ page, baseURL }) => {
    expect(baseURL).toBeTruthy();
    const origin = new URL(baseURL!).origin;
    const externalRequests: string[] = [];
    const errors: string[] = [];
    page.on("pageerror", (error) => errors.push(error.message));
    await page.addInitScript(() => {
      (window as Window & { mermaidAttack?: string }).mermaidAttack = "untouched";
    });
    await page.route("**/*", async (route) => {
      const url = new URL(route.request().url());
      if (/^https?:$/.test(url.protocol) && (url.origin !== origin || url.pathname.startsWith("/mermaid-attack/"))) {
        externalRequests.push(url.href);
        return route.abort();
      }
      return route.fallback();
    });
    const attacks = [
      '%%{init: {"securityLevel": "loose", "flowchart": {"htmlLabels": true}}}%%\nflowchart TD\n  A --> B\n',
      'flowchart TD\n  A["<img src=\'https://mermaid-attack.invalid/remote.png\' onerror=\'window.mermaidAttack=1\'>"]\n',
      'flowchart TD\n  A@{ img: "https://mermaid-attack.invalid/shape.png", label: "Remote" }\n  style A fill:url(https://mermaid-attack.invalid/fill.svg)\n',
      'flowchart TD\n  A@{ "img": "/mermaid-attack/pending.png" }\n',
      'flowchart TD\n  A@{ "\\u0069mg": "https://mermaid-attack.invalid/escaped.png" }\n',
      'flowchart TD\n  A --> B\n  classDef default fill:u\\72l(/mermaid-attack/fill.svg)\n',
      'flowchart TD\n  A --> B\n  style A background-image:image-set("/mermaid-attack/background.png")\n',
      'flowchart TD\n  中[Node]\n  style 中 filter:u\\72l(/mermaid-attack/filter.svg#f)\n',
    ];
    const strict = 'flowchart TD\n  A[Safe] --> B[Done]\n  click A "javascript:window.mermaidAttack=1" "Unsafe link"\n';
    await diagramSnapshot(page, [...attacks, strict].map(fence).join("\n\n"));
    await page.goto("/");
    const blocks = page.locator(".mermaid-block");
    await expect(blocks).toHaveCount(attacks.length + 1);
    for (const [index, source] of attacks.entries()) {
      const block = blocks.nth(index);
      await expect(block.locator(".mermaid-status")).toHaveText(failure);
      expect(await block.locator(".mermaid-source code").textContent()).toBe(source);
      await expect(block.locator(".mermaid-diagram img")).toHaveCount(0);
    }
    const normal = blocks.nth(attacks.length);
    const decoded = await decodedDiagram(normal.locator(".mermaid-diagram img"));
    expect(decoded.parseErrors).toBe(0);
    expect(decoded.text).toContain("Safe");
    // Never activate the untrusted link: the SVG must stay in inert image mode.
    await expect(blocks.locator(".mermaid-diagram svg, script, iframe, object, embed, a, [onclick], [onerror], [onload]")).toHaveCount(0);
    // The image's React click handler opens our viewer, never a diagram callback.
    await expect(normal.locator(".mermaid-diagram img")).not.toHaveAttribute("onclick");
    await expect(page.locator(".mermaid-render-scratch")).toHaveCount(0);
    expect(await page.evaluate(() => (window as Window & { mermaidAttack?: string }).mermaidAttack)).toBe("untouched");
    expect(externalRequests).toEqual([]);
    expect(errors).toEqual([]);
  });

  test("preserves all oversized source and still renders the following bounded diagram without orphans", async ({ page }) => {
    const errors: string[] = [];
    page.on("pageerror", (error) => errors.push(error.message));
    // A small line count isolates the character bound from the line-count bound.
    const oversized = `flowchart TD\n  A[${"x".repeat(20_001)}] --> B[End]\n`;
    expect(oversized.length).toBeGreaterThan(20_000);
    await diagramSnapshot(page, `${fence(oversized)}\n\n${fence(flow)}`);
    await page.goto("/");
    const blocks = page.locator(".mermaid-block");
    await expect(blocks).toHaveCount(2);
    const rejected = blocks.first();
    await expect(rejected.locator(".mermaid-status")).toHaveText(failure);
    expect(await rejected.locator(".mermaid-source code").textContent()).toBe(oversized);
    await expect(rejected.locator(".mermaid-diagram img")).toHaveCount(0);
    const decoded = await decodedDiagram(blocks.nth(1).locator(".mermaid-diagram img"));
    expect(decoded.parseErrors).toBe(0);
    expect(decoded.text).toContain("开始");
    await expect(page.locator(".mermaid-render-scratch")).toHaveCount(0);
    expect(errors).toEqual([]);
  });
});

// Keep this fixture local: summary layout must not depend on a live native host.
test.describe("Codex tool summary layout", () => {
  type SummaryTool = {
    name: string;
    args: Record<string, unknown>;
    result: string;
    state?: "done" | "running" | "error";
  };

  async function summarySnapshot(page: Page, tools: SummaryTool[]) {
    await mockHost(page);
    await page.route(snapshotRoute, (route) =>
      route.fulfill({
        json: {
          sessionId,
          status: tools.some((tool) => tool.state === "running") ? "running" : "ready",
          lastSeq: 9,
          events: [],
          messages: [
            {
              role: "assistant",
              content: tools.map((tool, index) => ({
                type: "toolCall",
                id: `codex-summary-${index}`,
                name: tool.name,
                arguments: tool.args,
                webTiming: {
                  elapsedMs: 1234,
                  running: tool.state === "running",
                },
              })),
            },
            ...tools.flatMap((tool, index) =>
              tool.state === "running" ? [] : [{
                role: "toolResult",
                toolCallId: `codex-summary-${index}`,
                toolName: tool.name,
                isError: tool.state === "error",
                content: [{ type: "text", text: tool.result }],
                webTiming: { elapsedMs: 1234, running: false },
              }],
            ),
          ],
        },
      }),
    );
  }

  async function expectSummaryGeometry(summary: ReturnType<Page["locator"]>) {
    await summary.scrollIntoViewIfNeeded();
    await expect(summary.locator(":scope > :first-child")).toHaveClass("tool-status");
    await expect(summary.locator(":scope > :last-child")).toHaveClass(/tool-chevron/);
    const geometry = await summary.evaluate((node) => {
      const box = (element: Element) => {
        const rect = element.getBoundingClientRect();
        return { x: rect.x, right: rect.right, width: rect.width };
      };
      return {
        bounds: box(node),
        viewport: window.innerWidth,
        parts: [
          // Measure the stable icon slot, not a spinner's rotating SVG bounds.
          ".tool-status", ".tool-action", ".tool-target",
          ".tool-summary-meta", ".tool-elapsed", ".tool-chevron",
        ].flatMap((selector) => {
          const element = node.querySelector(selector);
          return element ? [{ selector, ...box(element) }] : [];
        }),
      };
    });
    expect(geometry.bounds.x).toBeGreaterThanOrEqual(0);
    expect(geometry.bounds.right).toBeLessThanOrEqual(geometry.viewport + 1);
    for (const [index, part] of geometry.parts.entries()) {
      expect(part.width, part.selector).toBeGreaterThan(0);
      expect(part.x, part.selector).toBeGreaterThanOrEqual(geometry.bounds.x - 1);
      expect(part.right, part.selector).toBeLessThanOrEqual(geometry.bounds.right + 1);
      if (index > 0) {
        const previous = geometry.parts[index - 1];
        expect(part.x, `${previous.selector} precedes ${part.selector}`).toBeGreaterThan(previous.x);
        expect(part.x, "summary parts do not overlap").toBeGreaterThanOrEqual(previous.right - 1);
      }
    }
    const status = summary.locator(".tool-status");
    const label = await status.getAttribute("title");
    expect(label).toBeTruthy();
    await expect(status.locator(".sr-only")).toHaveText(label!);
    await expect(summary).toHaveAccessibleName(new RegExp(label!));
    await expect(status.locator("svg")).toHaveAttribute("aria-hidden", "true");
    await expect(summary.locator(".tool-chevron")).toHaveAttribute("aria-hidden", "true");
  }

  test("completed tool icons lead and native chevrons trail at desktop and mobile widths", async ({ page }) => {
    const tools: Array<SummaryTool & { icon: string }> = [
      { name: "read", args: { path: "src/example.ts" }, result: "export const answer = 42;", icon: "file-text" },
      { name: "bash", args: { command: "printf summary" }, result: "summary", icon: "terminal" },
      { name: "edit", args: { path: "src/edit.ts", edits: [{ oldText: "old", newText: "new" }] }, result: "Edited src/edit.ts", icon: "pencil" },
      { name: "write", args: { path: "src/write.ts", content: "new" }, result: "Wrote src/write.ts", icon: "pencil" },
      { name: "websearch", args: { query: "summary layout" }, result: "Search result", icon: "search" },
      { name: "plan", args: { plan: "# Summary plan\n\n- Inspect icons\n- Verify layout" }, result: "# Summary plan\n\n- Inspect icons\n- Verify layout", icon: "list-checks" },
      { name: "todo", args: {}, result: "1/2 completed\n[✓] #0: Inspect icons\n[ ] #1: Verify layout", icon: "list-checks" },
      { name: "webfetch", args: { url: "https://example.com" }, result: "Fetched page", icon: "globe" },
      { name: "custom_summary_probe", args: { path: "probe.txt" }, result: "Unknown tool payload", icon: "wrench" },
    ];
    await summarySnapshot(page, tools);
    for (const width of [1280, 390]) {
      await page.setViewportSize({ width, height: 844 });
      await page.goto("/");
      const cards = page.locator(".tool-card");
      await expect(cards).toHaveCount(tools.length);
      for (const [index, tool] of tools.entries()) {
        const card = cards.nth(index);
        const summary = card.locator(":scope > summary");
        await expect(card).toHaveClass(/tool-done/);
        await expect(card).toHaveJSProperty("open", false);
        await expect(summary.locator(`.tool-status > svg.lucide-${tool.icon}`)).toHaveCount(1);
        await expect(summary.locator(".tool-status .spin, .tool-status .lucide-circle-alert")).toHaveCount(0);
        await expect(summary.locator(".tool-elapsed")).toHaveText("1.2s");
        await expectSummaryGeometry(summary);
      }
      // Metadata is still in the middle, not replaced by either icon.
      await expect(cards.nth(2).locator("summary .edit-count")).toContainText("+1");
      await expect(cards.nth(2).locator("summary .edit-count")).toContainText("−1");
      await expect(cards.nth(5).locator("summary .tool-summary-meta")).not.toBeEmpty();
      await expect(cards.nth(6).locator("summary .tool-summary-meta")).toContainText("1/2");
      const read = cards.first();
      const summary = read.locator(":scope > summary");
      await summary.locator(":scope > .tool-chevron:last-child").click();
      await expect(read).toHaveJSProperty("open", true);
      await expect(read.getByText("export const answer = 42;", { exact: false })).toBeVisible();
      await summary.focus();
      await page.keyboard.press("Enter");
      await expect(read).toHaveJSProperty("open", false);
      await summary.locator(":scope > .tool-status:first-child").click();
      await expect(read).toHaveJSProperty("open", true);
      expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    }
  });

  test("long targets ellipsize without losing edit metadata or elapsed time on desktop and mobile", async ({ page }) => {
    const path = `src/${"very-long-directory-segment/".repeat(30)}summary.ts`;
    await summarySnapshot(page, [{
      name: "edit",
      args: { path, edits: [{ oldText: "const before = 1;", newText: "const after = 2;\nexport { after };" }] },
      result: `Edited ${path}`,
    }]);
    for (const width of [1280, 390]) {
      await page.setViewportSize({ width, height: 844 });
      await page.goto("/");
      const card = page.locator(".edit-card.tool-done");
      const summary = card.locator(":scope > summary");
      const target = summary.locator(".tool-target");
      await expect(card).toHaveJSProperty("open", false);
      await expect(target).toHaveText(path);
      await expect(target).toHaveAttribute("title", path);
      await expect(target).toHaveCSS("text-overflow", "ellipsis");
      await expect(target).toHaveCSS("overflow-x", "hidden");
      await expect(target).toHaveCSS("white-space", "nowrap");
      expect(await target.evaluate((node) => node.scrollWidth > node.clientWidth)).toBe(true);
      await expect(summary.locator(".edit-count")).toContainText("+2");
      await expect(summary.locator(".edit-count")).toContainText("−1");
      await expect(summary.locator(".tool-elapsed")).toHaveText("1.2s");
      await expectSummaryGeometry(summary);
      expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
      await summary.locator(".tool-chevron").click();
      await expect(card.locator(".edit-line.add")).toHaveCount(2);
      await expect(card.locator(".edit-line.remove")).toHaveCount(1);
    }
  });

  test("running spinner and error alert preserve status labels, auto-open, manual collapse and Enter", async ({ page }) => {
    await summarySnapshot(page, [
      { name: "bash", args: { command: "sleep 10" }, result: "", state: "running" },
      { name: "read", args: { path: "missing.txt" }, result: "Missing file: summary regression", state: "error" },
    ]);
    for (const width of [1280, 390]) {
      await page.setViewportSize({ width, height: 844 });
      await page.goto("/");
      const running = page.locator(".tool-card.tool-running");
      const error = page.locator(".tool-card.tool-error");
      await expect(running).toHaveJSProperty("open", false);
      await expect(error).toHaveJSProperty("open", true);
      const runningSummary = running.locator(":scope > summary");
      const errorSummary = error.locator(":scope > summary");
      await expect(runningSummary.locator(".tool-status > svg.spin")).toHaveCount(1);
      await expect(runningSummary.locator(".tool-status .lucide-terminal")).toHaveCount(0);
      await expect(errorSummary.locator(".tool-status > svg.lucide-circle-alert")).toHaveCount(1);
      await expect(errorSummary.locator(".tool-status .lucide-file-text, .tool-status .spin")).toHaveCount(0);
      await expectSummaryGeometry(runningSummary);
      await expectSummaryGeometry(errorSummary);
      await expect(error.getByText("Missing file: summary regression", { exact: false })).toBeVisible();
      await errorSummary.locator(":scope > .tool-chevron:last-child").click();
      await expect(error).toHaveJSProperty("open", false);
      await expect(error.getByText("Missing file: summary regression", { exact: false })).toBeHidden();
      // A live elapsed tick in the sibling must not undo the reader's collapse.
      const timer = runningSummary.locator(".tool-elapsed");
      const initial = await timer.innerText();
      await expect.poll(() => timer.innerText()).not.toBe(initial);
      await expect(error).toHaveJSProperty("open", false);
      await errorSummary.focus();
      await page.keyboard.press("Enter");
      await expect(error).toHaveJSProperty("open", true);
      await expect(error.getByText("Missing file: summary regression", { exact: false })).toBeVisible();
      await runningSummary.locator(".tool-chevron").click();
      await expect(running).toHaveJSProperty("open", true);
      expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    }
  });
});

for (const language of ["en", "zh"] as const) {
  test(`composer queue omits its visible title but keeps controls and accessible names in ${language}`, async ({ page }) => {
    await page.addInitScript(lang => localStorage.setItem("rpi-web-language", lang), language);
    await mockHost(page);
    const close = language === "zh" ? "关闭队列" : "Close queue";
    const view = language === "zh" ? "查看排队消息" : "View queued messages";
    for (const width of [1280, 390]) {
      await page.setViewportSize({ width, height: 844 });
      await page.goto("/");
      const panel = page.locator(".composer-queue-panel");
      const heading = panel.locator(".composer-queue-heading");
      await expect(panel).toBeVisible();
      await expect(heading).toHaveAccessibleName(close);
      await expect(heading).toHaveText("1");
      await expect(heading.locator("strong")).toHaveCount(0);
      await expect(panel.locator(".composer-queue-item")).toContainText("pending");
      const edit = panel.getByRole("button", { name: language === "zh" ? "编辑排队消息" : "Edit queued message", exact: true });
      await expect(edit).toBeVisible();
      await heading.click();
      await expect(heading).toHaveAccessibleName(view);
      await expect(heading).toHaveAttribute("aria-expanded", "false");
      await expect(heading.locator(".composer-queue-preview")).toHaveText("pending");
      expect(await heading.innerText()).not.toContain(language === "zh" ? "队列" : "Queue");
      await heading.focus();
      await page.keyboard.press("Enter");
      await expect(heading).toHaveAccessibleName(close);
      await expect(heading).toHaveAttribute("aria-expanded", "true");
      await expect(heading).toHaveText("1");
      await expect(edit).toBeVisible();
      expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
    }
  });
}

test.describe("session issue badges", () => {
  type Summary = import("../src/api").SessionSummary;
  const rawError = "PRIVATE_HOST_ERROR_do_not_render";
  function seed(): Summary[] {
    const other = (id: string, extra: Partial<Summary>): Summary => ({
      ...session, sessionId: id, title: id, tags: [], ...extra,
    });
    return [
      { ...session },
      other("known-failed", { latestSettlement: { outcome: "failed" } }),
      other("known-interrupted", { latestSettlement: { outcome: "interrupted" } }),
      other("host-error", { error: rawError }),
      other("error-status", { status: "error" }),
      other("recoverable", { status: "error", error: rawError, latestSettlement: { outcome: "interrupted", pendingResume: true } }),
      other("normal-completed", { latestSettlement: { outcome: "completed" } }),
      other("normal-aborted", { error: rawError, latestSettlement: { outcome: "aborted" } }),
      other("normal-declined", { error: rawError, latestSettlement: { outcome: "declined" } }),
      other("offline-unknown", { status: "offline" }),
      other("closed-normal", { status: "closed" }),
      ...["running", "starting", "awaiting_input"].map(status => other(`${status}-with-old-failed`, {
        status, error: rawError, latestSettlement: { outcome: "failed", pendingResume: true },
      })),
    ];
  }
  async function setup(page: Page) {
    await mockHost(page);
    const state = { sessions: seed(), listReads: 0, otherRequests: [] as string[] };
    const listRoute = (url: URL) => url.pathname === "/api/v1/sessions";
    await page.route(listRoute, route => {
      if (route.request().method() !== "GET") return route.fallback();
      state.listReads++;
      return route.fulfill({ json: { sessions: state.sessions } });
    });
    page.on("request", request => {
      const path = new URL(request.url()).pathname;
      const match = path.match(/^\/api\/v1\/sessions\/([^/]+)(?:\/|$)/);
      if (match && match[1] !== sessionId) state.otherRequests.push(path);
    });
    return state;
  }
  const row = (page: Page, title: string) => page.locator(".project-session")
    .filter({ has: page.locator(".session-title", { hasText: title }) });

  test("desktop shows summary evidence before opening sessions without leaking errors", async ({ page }) => {
    await page.setViewportSize({ width: 1280, height: 900 });
    const state = await setup(page);
    await page.goto("/");
    const sidebar = page.locator(".project-tree");
    await expect(sidebar.locator(".session-issue-failed")).toHaveCount(4);
    await expect(sidebar.locator(".session-issue-resume")).toHaveCount(1);
    for (const title of ["known-failed", "known-interrupted", "host-error", "error-status"]) {
      const badge = row(page, title).locator(".session-issue-failed");
      await expect(badge).toBeVisible();
      await expect(badge).toHaveText("Interrupted");
      await expect(badge).toHaveAccessibleName("Interrupted. This session reported an error or failed run. Open it to review and retry.");
      await expect(badge).toHaveAttribute("title", "This session reported an error or failed run. Open it to review and retry.");
      await expect(row(page, title).locator(".project-session-link")).toHaveAttribute("title", title);
    }
    await expect(row(page, "recoverable").locator(".session-issue-resume")).toHaveText("Needs resume");
    for (const title of ["Mock session", "normal-completed", "normal-aborted", "normal-declined", "offline-unknown", "closed-normal", "running-with-old-failed", "starting-with-old-failed", "awaiting_input-with-old-failed"])
      await expect(row(page, title).locator(".session-issue")).toHaveCount(0);
    await expect(page.locator("body")).not.toContainText(rawError);
    expect(await sidebar.locator("[title]").evaluateAll(elements => elements.map(el => el.getAttribute("title")).join("\n"))).not.toContain(rawError);
    expect(state.otherRequests).toEqual([]);
    await row(page, "known-failed").locator(".project-session-link").click();
    await expect(row(page, "known-failed")).toHaveClass(/active/);
  });

  test("English and Chinese labels agree in desktop rows and the 390px mobile picker", async ({ page }) => {
    const state = await setup(page);
    await page.goto("/");
    for (const language of ["en", "zh"] as const) {
      await page.evaluate(lang => localStorage.setItem("rpi-web-language", lang), language);
      await page.setViewportSize({ width: 1280, height: 844 });
      await page.reload();
      const failed = language === "zh" ? "异常中断" : "Interrupted";
      const resume = language === "zh" ? "待恢复" : "Needs resume";
      await expect(row(page, "known-failed").locator(".session-issue-failed")).toHaveText(failed);
      await expect(row(page, "recoverable").locator(".session-issue-resume")).toHaveText(resume);
      await page.setViewportSize({ width: 390, height: 844 });
      // Start with the mobile defaults instead of desktop overlays left open.
      await page.reload();
      await page.getByRole("button", { name: language === "zh" ? "切换会话" : "Switch session", exact: true }).click();
      const picker = page.locator(".mobile-session-sheet");
      await expect(picker).toBeVisible();
      await expect(picker.locator(".session-issue-failed")).toHaveText([failed, failed, failed, failed]);
      await expect(picker.locator(".session-issue-resume")).toHaveText(resume);
      for (const badge of await picker.locator(".session-issue").all()) {
        await expect(badge).toBeVisible();
        await expect(badge).toHaveAccessibleName(/.+/);
      }
      expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
      await expect(picker.locator(".mobile-session-option").filter({ hasText: "known-failed" })).toContainText(failed);
      await expect(picker).not.toContainText(rawError);
      expect(await picker.locator(".session-issue").evaluateAll(elements => elements.map(el => el.getAttribute("title")).join("\n"))).not.toContain(rawError);
      await page.getByRole("button", { name: language === "zh" ? "关闭面板" : "Close panel", exact: true }).click();
    }
    expect(state.otherRequests).toEqual([]);
  });

  test("list refresh clears a noncurrent failure for running and completed summaries", async ({ page }) => {
    await page.setViewportSize({ width: 1280, height: 900 });
    const state = await setup(page);
    state.sessions = [state.sessions[0], { ...state.sessions[1], error: rawError }];
    await page.goto("/");
    const failedRow = row(page, "known-failed");
    await expect(failedRow.locator(".session-issue-failed")).toHaveText("Interrupted");
    await expect(row(page, "Mock session")).toHaveClass(/active/);
    for (const update of [
      { status: "running" },
      { status: "ready", error: null, latestSettlement: { outcome: "completed" } },
    ]) {
      state.sessions = [state.sessions[0], { ...state.sessions[1], ...update }];
      const reads = state.listReads;
      const refreshed = page.waitForResponse(response => new URL(response.url()).pathname === "/api/v1/sessions" && response.request().method() === "GET");
      await page.getByRole("button", { name: "Refresh chats", exact: true }).click();
      await refreshed;
      expect(state.listReads).toBeGreaterThan(reads);
      await expect(failedRow.locator(".session-issue")).toHaveCount(0);
      await expect(row(page, "Mock session")).toHaveClass(/active/);
    }
    expect(state.otherRequests).toEqual([]);
  });
});
