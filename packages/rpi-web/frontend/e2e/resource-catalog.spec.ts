import { test, expect, type Page, type Route } from "@playwright/test";

// The resource catalog is a read-only native `rpi resource list` view. Both
// scopes are requested explicitly; no install/remove command is ever sent.
const project = {
  id: "resources-project",
  name: "Resources project",
  path: "/workspace/resources",
  archived: false,
};
const sessionId = "resources-session";

const catalog = (scope: "global" | "project") => ({
  version: 1,
  success: true,
  operation: "list",
  scope,
  kind: null,
  name: null,
  data: {
    outcome: "observed",
    resources:
      scope === "global"
        ? [
            {
              kind: "skill",
              scope: "global",
              identity: "global:skills:playwright-agentd",
              name: "playwright-agentd",
              ownership: "unmanaged",
              installed: false,
              inUse: true,
              path: "/home/user/.rpi/agent/skills/playwright-agentd",
              diagnostics: ["unverified candidate; no code executed"],
            },
            {
              kind: "extension",
              scope: "global",
              name: "librpi_web_extension",
              version: "0.1.53",
              inUse: true,
            },
          ]
        : [
            {
              kind: "skill",
              scope: "project",
              name: "project-skill",
              inUse: false,
              path: "/workspace/resources/.rpi/skills/project-skill",
            },
          ],
    diagnostics: [],
  },
});

async function host(page: Page, failProjectScope = false) {
  const requests: Array<{ scope: string | null; project: string }> = [];
  await page.addInitScript(() =>
    localStorage.setItem("rpi-web-language", "en"),
  );
  await page.route("**/api/**", async (route: Route) => {
    const request = route.request();
    const url = new URL(request.url());
    const path = url.pathname;
    const json = (value: unknown) => route.fulfill({ json: value });
    if (path === "/api/v1/auth")
      return json({ required: false, authenticated: true });
    if (path === "/api/v1/capabilities")
      return json({
        protocol: "rpi",
        supportedCommands: ["get_state", "get_queue", "get_stats"],
        supportedActions: ["projects"],
        parityComplete: false,
      });
    if (path === "/api/v1/projects")
      return json({
        projects: [project],
        defaultProjectId: project.id,
        projectsRoot: "/workspace",
      });
    if (path === "/api/v1/sessions")
      return json({
        sessions: [
          {
            sessionId,
            projectId: project.id,
            title: "Resources session",
            tags: [],
            status: "ready",
            createdAt: 1_700_000_000_000,
            updatedAt: 1_700_000_000_100,
            lastSeq: 4,
            error: null,
            relation: null,
            parentSessionId: null,
            forkEntryId: null,
            latestSettlement: null,
          },
        ],
      });
    if (path.endsWith("/open"))
      return json({
        sessionId,
        projectId: project.id,
        title: "Resources session",
        status: "ready",
        lastSeq: 4,
      });
    if (path.endsWith("/snapshot"))
      return json({
        sessionId,
        messages: [{ role: "user", entryId: "u1", content: "hello" }],
        events: [],
        lastSeq: 4,
        total: 1,
        hasMore: false,
      });
    if (path.endsWith("/events"))
      return route.fulfill({
        contentType: "text/event-stream",
        body: "retry: 60000\n\n",
      });
    if (path.endsWith("/resources")) {
      const scope = url.searchParams.get("scope");
      requests.push({ scope, project: path.split("/")[4] });
      if (failProjectScope && scope === "project")
        return route.fulfill({
          status: 400,
          json: { error: "scope must be project or global" },
        });
      if (scope !== "global" && scope !== "project")
        return route.fulfill({
          status: 400,
          json: { error: "scope must be project or global" },
        });
      return json(catalog(scope));
    }
    if (path.endsWith("/commands"))
      return json({
        type: "response",
        command: "mock",
        success: true,
        data: {},
      });
    return json({});
  });
  return requests;
}

test("the inspector lists both global and project resources", async ({
  page,
}) => {
  const requests = await host(page);
  await page.goto("/");
  await expect(
    page.getByRole("button", { name: "Toggle inspector" }),
  ).toBeEnabled();
  await page.getByRole("button", { name: "Toggle inspector" }).click();
  const panel = page.locator(".resource-panel");
  await expect(panel).toBeVisible();
  await expect(panel.getByRole("heading", { name: "Global" })).toBeVisible();
  await expect(panel.getByRole("heading", { name: "Project" })).toBeVisible();
  await expect(panel.locator(".resource-item")).toHaveCount(3);
  await expect(panel).toContainText("playwright-agentd");
  await expect(panel).toContainText("librpi_web_extension");
  await expect(panel).toContainText("project-skill");
  await expect(panel).toContainText("requires restarting the native host");
  // React StrictMode mounts effects twice in development, so each scope can be
  // requested twice. The contract is that both scopes are requested for this
  // project, with no implicit scope and no non-GET resource mutation.
  expect(requests.length).toBeGreaterThanOrEqual(2);
  expect(new Set(requests.map((entry) => entry.scope))).toEqual(
    new Set(["global", "project"]),
  );
  expect(new Set(requests.map((entry) => entry.project))).toEqual(
    new Set([project.id]),
  );
});

test("a failing scope is reported without hiding the other scope", async ({
  page,
}) => {
  const requests = await host(page, true);
  await page.goto("/");
  await page.getByRole("button", { name: "Toggle inspector" }).click();
  const panel = page.locator(".resource-panel");
  await expect(panel).toBeVisible();
  await expect(
    panel.getByText("playwright-agentd", { exact: true }),
  ).toBeVisible();
  await expect(panel.locator(".resource-error")).toHaveCount(1);
  await expect(panel.locator(".resource-error")).toContainText(
    "scope must be project or global",
  );
  expect(requests.length).toBeGreaterThanOrEqual(2);
  expect(new Set(requests.map((entry) => entry.scope))).toEqual(
    new Set(["global", "project"]),
  );
});
