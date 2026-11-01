import { test, expect, type Page } from "@playwright/test";
import {
  projectFileLink,
  inlineFileLink,
  browserLink,
} from "../src/messageLinks";

const project = {
  id: "links-project",
  name: "Links project",
  path: "/workspace/links",
};
const win = { ...project, path: "D:\\Work\\项目" };

test("file resolver contains paths, decodes once and validates line ranges", () => {
  expect(projectFileLink("./src/中文%20文件.ts#L12-L20", project)).toEqual({
    path: "src/中文 文件.ts",
    line: 12,
    endLine: 20,
  });
  expect(projectFileLink("/workspace/links/src/a.ts:12", project)?.path).toBe(
    "src/a.ts",
  );
  expect(projectFileLink("d:\\work\\项目\\源码\\文件.ts:12", win)).toEqual({
    path: "源码/文件.ts",
    line: 12,
    endLine: 12,
  });
  expect(projectFileLink("D:/WORK/项目/src/a.ts#L2-L4", win)?.line).toBe(2);
  expect(projectFileLink("d:/src/main.ts:12", { ...win, path: "D:/" })).toEqual(
    {
      path: "src/main.ts",
      line: 12,
      endLine: 12,
    },
  );
  expect(
    projectFileLink("/src/main.ts:12", { ...project, path: "/" })?.path,
  ).toBe("src/main.ts");
  for (const source of [
    "../secret",
    "a/../secret",
    "%2e%2e/secret",
    "%252e%252e/secret",
    "src/a%2520b.ts",
    "//host/a.ts",
    "\\\\host\\a.ts",
    "file:///workspace/links/a.ts",
    "javascript:alert(1)",
    "vscode:open",
    "data:text/plain,test",
    "/workspace/Links/a.ts",
    "/workspace/links-other/a.ts",
    "D:/Work/other/a.ts",
    "a%00.ts",
    "a\n.ts",
    "a.ts#L20-L12",
    "a.ts:99999999999999999",
    "a.ts?raw=1",
  ]) {
    expect(projectFileLink(source, project), source).toBeNull();
  }
  expect(projectFileLink("src/a.ts")).toBeNull();
  expect(inlineFileLink("identifier", project)).toBeNull();
  expect(inlineFileLink("README.md", project)?.path).toBe("README.md");
  expect(browserLink("https://example.com/a")).toBe("web");
  expect(browserLink("mailto:dev@example.com")).toBe("mail");
  expect(browserLink("javascript:alert(1)")).toBeNull();
});

const markdown = [
  "[Source](src/main.ts#L2-L3) [中文](src/%E4%B8%AD%E6%96%87%20%E6%96%87%E4%BB%B6.ts:2)",
  "`src/main.ts:2` and `identifier`",
  "https://example.com/docs [Mail](mailto:dev@example.com)",
  "[Unsafe](javascript:alert%281%29) [OS](file:///workspace/links/main.ts) [Escape](../secret.ts)",
  "```ts\nsrc/main.ts:2\n```",
  "```\nsrc/plain.ts\n```",
  "[![Linked photo](data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=)](https://example.com/photo)",
].join("\n\n");

async function host(page: Page, text = markdown, root = project.path) {
  const reads: Array<{ project: string; path: string }> = [];
  const unexpected: string[] = [];
  let fail = false;
  let held: (() => void) | null = null;
  let hold = false;
  const summary = (id: string) => ({
    sessionId: id,
    projectId: id === "links-session" ? project.id : "other-project",
    title: id === "links-session" ? "Links session" : "Other links session",
    tags: [],
    status: "ready",
    createdAt: 1700000000000,
    updatedAt: id === "links-session" ? 1700000000100 : 1700000000000,
    lastSeq: 4,
    error: null,
    relation: null,
    parentSessionId: null,
    forkEntryId: null,
    latestSettlement: null,
  });
  await page.addInitScript(() =>
    localStorage.setItem("rpi-web-language", "en"),
  );
  await page.route("**/api/**", async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    const path = url.pathname;
    const json = (value: unknown) => route.fulfill({ json: value });
    if (path === "/api/v1/auth")
      return json({ required: false, authenticated: true });
    if (path === "/api/v1/capabilities")
      return json({
        protocol: "rpi",
        supportedCommands: [
          "get_state",
          "get_queue",
          "get_stats",
          "get_history",
          "get_messages",
        ],
        supportedActions: ["projects"],
        parityComplete: false,
      });
    if (path === "/api/v1/projects")
      return json({
        projects: [
          { ...project, path: root },
          {
            id: "other-project",
            name: "Other project",
            path: "/workspace/other",
          },
        ],
        defaultProjectId: project.id,
        projectsRoot: "/workspace",
      });
    if (/\/projects\/[^/]+\/file$/.test(path)) {
      reads.push({
        project: path.split("/")[4],
        path: url.searchParams.get("path")!,
      });
      if (hold)
        await new Promise<void>((resolve) => {
          held = resolve;
        });
      if (fail)
        return route.fulfill({
          status: 500,
          json: { error: "Mock read failure" },
        });
      return json({
        path: url.searchParams.get("path"),
        content: "// preview\nconst answer = 42;\nconsole.log(answer);\n// end",
      });
    }
    if (path === "/api/v1/sessions")
      return json({
        sessions: [summary("links-session"), summary("other-session")],
      });
    const match = path.match(/^\/api\/v1\/sessions\/([^/]+)\/(.+)$/);
    if (match) {
      const [, id, action] = match;
      if (action === "open") return json(summary(id));
      if (action === "snapshot")
        return json({
          sessionId: id,
          status: "ready",
          messages: [
            {
              role: "assistant",
              entryId: id + "-message",
              content: [
                {
                  type: "text",
                  text: id === "links-session" ? text : "Other project message",
                },
              ],
            },
          ],
          firstId: id + "-message",
          hasMore: false,
          events: [],
          lastSeq: 4,
          leafId: "leaf",
          pendingResume: false,
        });
      if (action === "events")
        return route.fulfill({
          contentType: "text/event-stream",
          body: 'retry: 60000\nevent: ready\ndata: {"type":"ready"}\n\n',
        });
      if (action === "run-timings/query")
        return json({
          timings: [],
          complete: true,
          cursor: null,
          scannedBytes: 0,
          totalBytes: 0,
        });
      if (action === "commands") {
        const type = request.postDataJSON().type;
        const data =
          type === "get_state"
            ? {
                sessionId: id,
                leafId: "leaf",
                activeOperation: null,
                queue: { steeringCount: 0, followUpCount: 0 },
                stats: {},
              }
            : type === "get_history"
              ? { records: [], total: 0, hasMore: false, openOperations: [] }
              : type === "get_queue"
                ? { items: [], count: 0 }
                : {};
        if (
          !["get_state", "get_history", "get_queue", "get_stats"].includes(type)
        )
          unexpected.push(type);
        return json({ type: "response", command: type, success: true, data });
      }
    }
    unexpected.push(request.method() + " " + path);
    return route.fulfill({
      status: 500,
      json: { error: "Unexpected mock API" },
    });
  });
  return {
    reads,
    unexpected,
    fail: () => {
      fail = true;
    },
    recover: () => {
      fail = false;
    },
    hold: () => {
      hold = true;
    },
    release: () => {
      held?.();
      hold = false;
    },
  };
}
const preview = (page: Page) =>
  page.getByRole("dialog", { name: "File preview", exact: true });

for (const width of [1280, 390])
  test(`message file links and browser links at ${width}px`, async ({
    page,
  }) => {
    await page.setViewportSize({ width, height: 844 });
    const mock = await host(page);
    await page.addInitScript(() => {
      Object.defineProperty(navigator.clipboard, "writeText", {
        value: async (text: string) => {
          (window as unknown as { copiedText: string }).copiedText = text;
        },
      });
    });
    await page.goto("/", { waitUntil: "domcontentloaded" });
    const source = page.getByRole("button", { name: "Source", exact: true });
    await expect(source).toBeVisible();
    await page.waitForTimeout(150);
    expect(mock.reads).toEqual([]);
    const web = page.getByRole("link", {
      name: "https://example.com/docs",
      exact: true,
    });
    await expect(web).toHaveAttribute("target", "_blank");
    await expect(web).toHaveAttribute("rel", "noopener noreferrer");
    await expect(
      page.getByRole("link", { name: "Mail", exact: true }),
    ).toHaveAttribute("href", "mailto:dev@example.com");
    for (const name of ["Unsafe", "OS", "Escape"])
      await expect(page.getByRole("link", { name, exact: true })).toHaveCount(
        0,
      );
    await expect(
      page.locator(".markdown-code-block .message-file-link"),
    ).toHaveCount(0);
    await expect(page.locator(".markdown-code-block pre").first()).toHaveText(
      "src/main.ts:2\n",
    );
    await source.click();
    await expect(preview(page)).toBeVisible();
    await expect(
      preview(page).locator('[data-highlighted="true"]'),
    ).toHaveCount(2);
    await expect(
      preview(page).locator('[data-line="2"] .hljs-keyword'),
    ).toHaveText("const");
    await page.screenshot({ path: test.info().outputPath(`file-preview-${width}.png`) });
    expect(mock.reads).toEqual([{ project: project.id, path: "src/main.ts" }]);
    await preview(page)
      .getByRole("button", { name: "Copy path", exact: true })
      .click();
    await expect
      .poll(() =>
        page.evaluate(
          () => (window as unknown as { copiedText: string }).copiedText,
        ),
      )
      .toBe("src/main.ts");
    await preview(page)
      .getByRole("button", { name: "Copy code", exact: true })
      .click();
    await expect
      .poll(() =>
        page.evaluate(
          () => (window as unknown as { copiedText: string }).copiedText,
        ),
      )
      .toContain("const answer = 42;");
    const box = (await preview(page).boundingBox())!;
    expect(box.width).toBeLessThan(width);
    await page.keyboard.press("Escape");
    await expect(preview(page)).toHaveCount(0);
    await expect(source).toBeFocused();
    await page.getByRole("button", { name: "中文", exact: true }).click();
    await expect(preview(page).locator(".file-preview-code")).toBeVisible();
    expect(mock.reads.at(-1)?.path).toBe("src/中文 文件.ts");
    await preview(page)
      .getByRole("button", { name: "Close", exact: true })
      .click();
    await page
      .getByRole("button", { name: "src/main.ts:2", exact: true })
      .click();
    await expect(
      preview(page).locator('[data-highlighted="true"]'),
    ).toHaveCount(1);
    await page.keyboard.press("Escape");
    await page
      .getByRole("button", { name: "Enlarge image: Linked photo" })
      .focus();
    await page.keyboard.press("Enter");
    await expect(page.locator(".image-viewer")).toBeVisible();
    await page.keyboard.press("Escape");
    expect(mock.unexpected).toEqual([]);
  });

test("Windows authored URLs survive Markdown URL safety and preview without OS navigation", async ({
  page,
}) => {
  const mock = await host(
    page,
    "[Windows](D:/Work/项目/src/a.ts:2) [Outside](D:/Work/else/a.ts)",
    win.path,
  );
  await page.goto("/", { waitUntil: "domcontentloaded" });
  await page.getByRole("button", { name: "Windows", exact: true }).click();
  await expect(preview(page).locator('[data-highlighted="true"]')).toHaveCount(
    1,
  );
  expect(mock.reads).toEqual([{ project: project.id, path: "src/a.ts" }]);
  await expect(
    page.getByRole("link", { name: "Outside", exact: true }),
  ).toHaveCount(0);
  expect(mock.unexpected).toEqual([]);
});

test("read errors retry; close aborts; navigation cannot show a stale project file", async ({
  page,
}) => {
  const mock = await host(page);
  let aborted = 0;
  await page.addInitScript(() => {
    const original = window.fetch;
    (window as unknown as { fileAborts: number }).fileAborts = 0;
    window.fetch = (input, init) => {
      if (String(input).includes("/file?"))
        init?.signal?.addEventListener("abort", () => {
          (window as unknown as { fileAborts: number }).fileAborts++;
        });
      return original(input, init);
    };
  });
  await page.goto("/", { waitUntil: "domcontentloaded" });
  mock.fail();
  await page.getByRole("button", { name: "Source", exact: true }).click();
  await expect(preview(page).getByRole("alert")).toContainText(
    "Mock read failure",
  );
  mock.recover();
  await preview(page)
    .getByRole("button", { name: "Retry", exact: true })
    .click();
  await expect(preview(page).locator(".file-preview-code")).toBeVisible();
  expect(mock.reads).toHaveLength(2);
  await page.keyboard.press("Escape");
  aborted = await page.evaluate(
    () => (window as unknown as { fileAborts: number }).fileAborts,
  );
  mock.hold();
  await page.getByRole("button", { name: "Source", exact: true }).click();
  await expect.poll(() => mock.reads.length).toBe(3);
  await page.keyboard.press("Escape");
  await expect
    .poll(() =>
      page.evaluate(
        () => (window as unknown as { fileAborts: number }).fileAborts,
      ),
    )
    .toBe(aborted + 1);
  mock.release();
  mock.hold();
  await page.getByRole("button", { name: "Source", exact: true }).click();
  await expect.poll(() => mock.reads.length).toBe(4);
  // Simulate app-driven navigation while the native modal makes the sidebar inert.
  await page
    .locator(".project-session-link")
    .filter({ hasText: "Other links session" })
    .evaluate((element: HTMLButtonElement) => element.click());
  await expect
    .poll(() =>
      page.evaluate(
        () => (window as unknown as { fileAborts: number }).fileAborts,
      ),
    )
    .toBe(aborted + 2);
  await expect(page.locator(".toolbar h1")).toHaveText("Other links session");
  mock.release();
  await page.waitForTimeout(150);
  await expect(preview(page)).toHaveCount(0);
  await expect(page.locator(".markdown-body")).not.toContainText(
    "const answer",
  );
  expect(mock.unexpected).toEqual([]);
});
