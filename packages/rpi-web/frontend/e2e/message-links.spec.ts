import { test, expect, type Page } from "@playwright/test";
import {
  projectFileLink,
  inlineFileLink,
  browserLink,
} from "../src/messageLinks";
import { projectImageSource } from "../src/projectImages";
import { readWorkbook } from "../src/spreadsheet";
import * as XLSX from "xlsx";
import { absoluteProjectPath } from "../src/projectPaths";

test.use({
  baseURL: process.env.RPI_MESSAGE_LINKS_BASE_URL || "http://127.0.0.1:4191",
});

// Real 1x1 PNG, served as bytes so Chromium exercises image decoding.
const png = Buffer.from(
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=",
  "base64",
);

const project = {
  id: "links-project",
  name: "Links project",
  path: "/workspace/links",
};
const win = { ...project, path: "D:\\Work\\项目" };
// Minimal bytes with the signatures the document route validates.
const pdfBytes = Buffer.from("%PDF-1.7\n% mock pdf\n");
const docxBytes = Buffer.from([0x50, 0x4b, 0x03, 0x04, 0x00, 0x00]);
/** Two-sheet workbook with a number, a date and a formula result. */
function workbookBytes() {
  const book = XLSX.utils.book_new();
  const budget = XLSX.utils.aoa_to_sheet([
    ["项目", "金额", "日期"],
    ["差旅", 1250, new Date(Date.UTC(2026, 0, 15))],
    ["合计", 1250, ""],
  ]);
  XLSX.utils.book_append_sheet(book, budget, "预算");
  const notes = XLSX.utils.aoa_to_sheet([["备注"], ["第二张表"]]);
  XLSX.utils.book_append_sheet(book, notes, "备注");
  return Buffer.from(
    XLSX.write(book, { type: "array", bookType: "xlsx" }) as ArrayBuffer,
  );
}
const sheetBytes = workbookBytes();

test("absolute paths follow the server root's platform, not the browser", () => {
  // Unix: a backslash is a legal filename character and must be preserved.
  expect(absoluteProjectPath(project.path, "src/main.ts")).toBe(
    "/workspace/links/src/main.ts",
  );
  expect(absoluteProjectPath(project.path, "README.md")).toBe(
    "/workspace/links/README.md",
  );
  expect(absoluteProjectPath(project.path, "src/a\\b.ts")).toBe(
    "/workspace/links/src/a\\b.ts",
  );
  expect(absoluteProjectPath("/", "src/main.ts")).toBe("/src/main.ts");
  // Windows: drive roots, forward-slash roots, UNC roots, and a mixed input.
  expect(absoluteProjectPath(win.path, "src/a.ts")).toBe(
    "D:\\Work\\项目\\src\\a.ts",
  );
  expect(absoluteProjectPath("D:/", "src/a.ts")).toBe("D:/src/a.ts");
  expect(absoluteProjectPath("D:\\", "src/a.ts")).toBe("D:\\src\\a.ts");
  expect(absoluteProjectPath("\\\\server\\share\\proj", "src/a.ts")).toBe(
    "\\\\server\\share\\proj\\src\\a.ts",
  );
  // A root written with `/` keeps that separator; do not emit a mixed path.
  expect(absoluteProjectPath("C:/Work/proj", "src/a.ts")).toBe(
    "C:/Work/proj/src/a.ts",
  );
  expect(absoluteProjectPath("", "src/a.ts")).toBe("");
  expect(absoluteProjectPath(project.path, "")).toBe("");
  // A stripped verbatim UNC root is ambiguous; do not guess at an absolute path.
  expect(absoluteProjectPath("UNC\\server\\share", "src/a.ts")).toBe("");
  expect(absoluteProjectPath("relative/root", "src/a.ts")).toBe("");
});

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
  // The project file endpoint rejects these components with 403, so the
  // resolver must not advertise a preview that can only fail.
  for (const source of [
    ".rpi/uploads/a.md",
    "./.rpi/uploads/a.md",
    "/workspace/links/.rpi/uploads/a.md",
    "src/.git/config",
    "node_modules/pkg/index.js",
    "target/debug/build.rs",
    ".deploy/release.json",
    ".env.local",
    "src/.env",
    ".ENV",
    ".RPI/uploads/a.md",
  ])
    expect(projectFileLink(source, project), source).toBeNull();
  expect(inlineFileLink(".rpi/uploads/a.md", project)).toBeNull();
  // The server's `hidden()` also rejects any `.env*` prefix, not just `.env`.
  expect(projectFileLink("src/.envoy/config.ts", project)).toBeNull();
  expect(projectFileLink("src/environment.ts", project)?.path).toBe(
    "src/environment.ts",
  );
  expect(inlineFileLink("identifier", project)).toBeNull();
  // A bare filename or a git ref has no directory of its own; joining it onto
  // the project root would open a path the agent never wrote.
  expect(inlineFileLink("README.md", project)).toBeNull();
  expect(inlineFileLink("message-links.spec.ts", project)).toBeNull();
  expect(inlineFileLink("origin/main", project)).toBeNull();
  expect(inlineFileLink("src/main.ts:2", project)?.line).toBe(2);
  expect(inlineFileLink("./src/main.ts", project)?.path).toBe("src/main.ts");
  expect(inlineFileLink("src/Makefile", project)).toBeNull();
  // The image endpoint shares the file endpoint's hidden-component rule.
  expect(projectImageSource(".rpi/uploads/shot.png", project)).toBe("");
  expect(projectImageSource("node_modules/pkg/logo.png", project)).toBe("");
  expect(projectImageSource("assets/shot.png", project)).toBe(
    `/api/v1/projects/${project.id}/image?path=assets%2Fshot.png`,
  );
  expect(browserLink("https://example.com/a")).toBe("web");
  expect(browserLink("mailto:dev@example.com")).toBe("mail");
  expect(browserLink("javascript:alert(1)")).toBeNull();
});

test("linked worktree paths preview from their own checkout", () => {
  // Agents create sibling worktrees (`git worktree add ../name`) and write
  // there; the absolute path names the checkout, so the link must carry it.
  const worktrees = { ...project, worktrees: ["/workspace/links-wt"] };
  expect(projectFileLink("/workspace/links-wt/src/a.ts:7", worktrees)).toEqual({
    root: "/workspace/links-wt",
    path: "src/a.ts",
    line: 7,
    endLine: 7,
  });
  // A relative reference has no checkout signal: it stays project-relative.
  expect(projectFileLink("src/a.ts", worktrees)?.root).toBeUndefined();
  // A sibling that only shares a prefix is a different directory.
  expect(
    projectFileLink("/workspace/links-wt-other/src/a.ts", worktrees),
  ).toBeNull();
  // The project root keeps winning for its own files.
  expect(projectFileLink("/workspace/links/src/a.ts", worktrees)?.root).toBe(
    undefined,
  );
  // A worktree nested inside the checkout is attributed to the worktree.
  const nested = projectFileLink("/workspace/links/wt/src/a.ts", {
    ...project,
    worktrees: ["/workspace/links/wt"],
  });
  expect({ root: nested?.root, path: nested?.path }).toEqual({
    root: "/workspace/links/wt",
    path: "src/a.ts",
  });
  // The browsable boundary applies inside a worktree too.
  for (const source of [
    "/workspace/links-wt/.rpi/sessions/a.jsonl",
    "/workspace/links-wt/../secret.ts",
    "/workspace/links-wt/target/debug/build.rs",
  ])
    expect(projectFileLink(source, worktrees), source).toBeNull();
  // Windows roots carry worktrees the same way, case-folded for the drive.
  const windows = { ...win, worktrees: ["D:\\Work\\wt"] };
  expect(projectFileLink("d:/work/wt/src/a.ts:4", windows)?.root).toBe(
    "D:\\Work\\wt",
  );

  expect(
    projectImageSource("/workspace/links-wt/assets/shot.png", worktrees),
  ).toBe(
    `/api/v1/projects/${project.id}/image?path=assets%2Fshot.png&root=%2Fworkspace%2Flinks-wt`,
  );
  expect(
    projectImageSource("/workspace/links-wt-other/assets/shot.png", worktrees),
  ).toBe("");
  expect(projectImageSource("/workspace/other/shot.png", worktrees)).toBe("");
});

const markdown = [
  "[Source](src/main.ts#L2-L3) [中文](src/%E4%B8%AD%E6%96%87%20%E6%96%87%E4%BB%B6.ts:2)",
  "`src/main.ts:2` and `identifier`",
  "`message-links.spec.ts` and `origin/main` stay plain code",
  "`.rpi/uploads/notes.md` [Hidden](.rpi/uploads/notes.md)",
  "https://example.com/docs [Mail](mailto:dev@example.com)",
  "[Unsafe](javascript:alert%281%29) [OS](file:///workspace/links/main.ts) [Escape](../secret.ts)",
  "[Worktree](/workspace/links-wt/src/a.ts:3)",
  "```ts\nsrc/main.ts:2\n```",
  "```\nsrc/plain.ts\n```",
  "[![Linked photo](data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=)](https://example.com/photo)",
].join("\n\n");

async function host(
  page: Page,
  text = markdown,
  root = project.path,
  language = "en",
  worktrees: string[] = [],
) {
  const reads: Array<{
    project: string;
    path: string;
    root: string | null;
  }> = [];
  const imageReads: Array<{
    project: string;
    path: string;
    method: string;
    header: string | undefined;
  }> = [];
  let imageMode: "ok" | "error" | "mime" | "decode" = "ok";
  // Per-path text so a Markdown document can be previewed as written.
  const files: Record<string, string> = {};
  // Directory index used by the nested-path resolver.
  const listings: Record<
    string,
    Array<{ name: string; path: string; kind: "directory" | "file" }>
  > = {};
  const missing = new Set<string>();
  const worktreeListings: Record<
    string,
    Array<{ name: string; path: string; kind: "directory" | "file" }>
  > = {};
  const documents: Array<{
    project: string;
    path: string;
    root: string | null;
  }> = [];
  let documentsUnsupported = false;
  const downloads: Array<{ path: string; root: string | null }> = [];
  let downloadsUnsupported = false;
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
  await page.addInitScript(
    (value) => localStorage.setItem("rpi-web-language", value),
    language,
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
          { ...project, path: root, worktrees },
          {
            id: "other-project",
            name: "Other project",
            path: "/workspace/other",
          },
        ],
        defaultProjectId: project.id,
        projectsRoot: "/workspace",
      });
    if (/\/projects\/[^/]+\/image$/.test(path)) {
      imageReads.push({
        project: path.split("/")[4],
        path: url.searchParams.get("path")!,
        method: request.method(),
        header: request.headers()["x-rpi-web"],
      });
      if (hold)
        await new Promise<void>((resolve) => {
          held = resolve;
        });
      if (imageMode === "error")
        return route.fulfill({
          status: 413,
          json: { error: "Mock image exceeds 20 MiB" },
        });
      return route.fulfill({
        contentType: imageMode === "mime" ? "image/svg+xml" : "image/png",
        body:
          imageMode === "decode" ? Buffer.from("not a decodable image") : png,
      });
    }
    if (
      /\/projects\/[^/]\w+\/document$/.test(path) ||
      /\/projects\/[^/]+\/document$/.test(path)
    ) {
      const target = url.searchParams.get("path") ?? "";
      documents.push({
        project: path.split("/")[4],
        path: target,
        root: url.searchParams.get("root"),
      });
      if (documentsUnsupported)
        // A host without the route answers with an empty, non-JSON body.
        return route.fulfill({ status: 404, body: "" });
      if (missing.has(target))
        return route.fulfill({ status: 404, json: { error: "not found" } });
      if (target.toLowerCase().endsWith(".pdf"))
        return route.fulfill({
          status: 200,
          contentType: "application/pdf",
          body: pdfBytes,
        });
      if (/\.xlsx$/i.test(target))
        return route.fulfill({
          status: 200,
          contentType:
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
          body: sheetBytes,
        });
      if (/\.docx$/i.test(target))
        return route.fulfill({
          status: 200,
          contentType:
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
          body: docxBytes,
        });
      return route.fulfill({
        status: 415,
        json: { error: "unsupported document format" },
      });
    }
    if (/\/projects\/[^/]+\/download$/.test(path)) {
      downloads.push({
        path: url.searchParams.get("path") ?? "",
        root: url.searchParams.get("root"),
      });
      if (downloadsUnsupported) return route.fulfill({ status: 404, body: "" });
      return route.fulfill({
        status: 200,
        contentType: "application/octet-stream",
        headers: {
          "content-disposition": 'attachment; filename="manual.bin"',
        },
        body: Buffer.from([0x00, 0x01, 0x02]),
      });
    }
    if (/\/projects\/[^/]+\/files$/.test(path)) {
      const read = url.searchParams.get("path") ?? "";
      const root = url.searchParams.get("root") ?? "";
      return json({
        path: read,
        entries: root
          ? (worktreeListings[`${root}:${read}`] ?? [])
          : (listings[read] ?? []),
      });
    }
    if (/\/projects\/[^/]+\/file$/.test(path)) {
      reads.push({
        project: path.split("/")[4],
        path: url.searchParams.get("path")!,
        root: url.searchParams.get("root"),
      });
      if (hold)
        await new Promise<void>((resolve) => {
          held = resolve;
        });
      if (/\.(?:pdf|bin)$/i.test(url.searchParams.get("path") ?? ""))
        return route.fulfill({
          status: 400,
          json: { error: "Binary file cannot be previewed as text" },
        });
      if (
        missing.has(url.searchParams.get("path") ?? "") &&
        !url.searchParams.get("root")
      )
        return route.fulfill({
          status: 404,
          json: { error: "file not found" },
        });
      if (fail)
        return route.fulfill({
          status: 500,
          json: { error: "Mock read failure" },
        });
      return json({
        path: url.searchParams.get("path"),
        content:
          files[url.searchParams.get("path") ?? ""] ??
          "// preview\nconst answer = 42;\nconsole.log(answer);\n// end",
      });
    }
    if (path === "/api/v1/sessions")
      return json({
        sessions: [summary("links-session"), summary("other-session")],
      });
    // The client probes a single session before treating a closed event stream
    // as a retired host. These sessions stay live, so no reopen is attempted.
    const single = path.match(/^\/api\/v1\/sessions\/([^/]+)$/);
    if (single) return json(summary(single[1]));
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
    files,
    listings,
    worktreeListings,
    missing,
    documents,
    downloads,
    unsupportedDocuments: () => {
      documentsUnsupported = true;
    },
    unsupportedDownloads: () => {
      downloadsUnsupported = true;
    },
    imageReads,
    imageMode: (mode: typeof imageMode) => {
      imageMode = mode;
    },
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
    const mock = await host(page, markdown, project.path, "en", [
      "/workspace/links-wt",
    ]);
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
    await page
      .locator(".message.assistant")
      .first()
      .screenshot({
        path: test.info().outputPath(`message-links-${width}.png`),
      });
    const openable = page
      .locator("button.message-file-link")
      .filter({ hasText: "src/main.ts:2" })
      .first();
    const style = await openable.evaluate((node) => {
      const button = getComputedStyle(node);
      const code = getComputedStyle(node.querySelector("code")!);
      return {
        decoration: button.textDecorationLine,
        buttonBackground: button.backgroundColor,
        codeBackground: code.backgroundColor,
      };
    });
    // An openable path is underlined; its code chip stays transparent so it
    // reads as a path, not as a second background inside the button.
    expect(style.decoration).toContain("underline");
    expect(style.codeBackground).toBe("rgba(0, 0, 0, 0)");
    expect(style.buttonBackground).toBe("rgba(0, 0, 0, 0)");
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
    // `.rpi` is rejected by the file endpoint, so it must stay plain text.
    await expect(
      page.locator(".message-file-link").filter({ hasText: ".rpi/" }),
    ).toHaveCount(0);
    await expect(
      page.locator("code").filter({ hasText: ".rpi/uploads/notes.md" }),
    ).toHaveCount(1);
    // A bare filename and a git ref have no project directory: no preview that
    // could only concatenate a wrong path, and no hyperlink styling.
    for (const bare of ["message-links.spec.ts", "origin/main"]) {
      await expect(
        page.locator(".message-file-link").filter({ hasText: bare }),
      ).toHaveCount(0);
      const code = page.locator("code").filter({ hasText: bare });
      await expect(code).toHaveCount(1);
      expect(
        await code.evaluate(
          (node) => getComputedStyle(node.parentElement!).textDecorationLine,
        ),
      ).not.toContain("underline");
    }
    await expect(page.getByText("Hidden", { exact: true })).toHaveCount(1);
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
    await page.screenshot({
      path: test.info().outputPath(`file-preview-${width}.png`),
    });
    expect(mock.reads).toEqual([
      { project: project.id, path: "src/main.ts", root: null },
    ]);
    await expect(preview(page).locator(".file-preview-path")).toContainText(
      "/workspace/links/src/main.ts",
    );
    await preview(page)
      .getByRole("button", { name: "Copy path", exact: true })
      .click();
    await expect
      .poll(() =>
        page.evaluate(
          () => (window as unknown as { copiedText: string }).copiedText,
        ),
      )
      .toBe("/workspace/links/src/main.ts");
    await preview(page)
      .getByRole("button", { name: "Copy relative path", exact: true })
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
    // A file an agent wrote in a sibling worktree previews from that checkout,
    // not from the like-named path in the project root.
    await page.getByRole("button", { name: "Worktree", exact: true }).click();
    await expect(preview(page).locator(".file-preview-code")).toBeVisible();
    expect(mock.reads.at(-1)).toEqual({
      project: project.id,
      path: "src/a.ts",
      root: "/workspace/links-wt",
    });
    await expect(preview(page).locator(".file-preview-path")).toContainText(
      "/workspace/links-wt/src/a.ts",
    );
    await expect(preview(page).locator('[data-line="3"]')).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(preview(page)).toHaveCount(0);
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
  expect(mock.reads).toEqual([
    { project: project.id, path: "src/a.ts", root: null },
  ]);
  // The mock host runs on Linux, but the server root is a Windows path: the
  // copied value must still use native separators.
  await page.addInitScript(() => {
    Object.defineProperty(navigator.clipboard, "writeText", {
      value: async (text: string) => {
        (window as unknown as { copiedText: string }).copiedText = text;
      },
    });
  });
  await page.reload({ waitUntil: "domcontentloaded" });
  await page.getByRole("button", { name: "Windows", exact: true }).click();
  await expect(preview(page).locator(".file-preview-path")).toContainText(
    "D:\\Work\\项目\\src\\a.ts",
  );
  await preview(page)
    .getByRole("button", { name: "Copy path", exact: true })
    .click();
  await expect
    .poll(() =>
      page.evaluate(
        () => (window as unknown as { copiedText: string }).copiedText,
      ),
    )
    .toBe("D:\\Work\\项目\\src\\a.ts");
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

const imagePath = "assets/中文 photo.PNG";
const imageMarkdown = `[Photo](assets/%E4%B8%AD%E6%96%87%20photo.PNG) [Binary](assets/manual.bin)`;
type ImageAudit = {
  created: string[];
  revoked: string[];
  aborts: number;
  signals: boolean[];
  copied: string;
};

async function auditImages(page: Page) {
  await page.addInitScript(() => {
    const audit: ImageAudit = {
      created: [],
      revoked: [],
      aborts: 0,
      signals: [],
      copied: "",
    };
    (window as unknown as { imageAudit: ImageAudit }).imageAudit = audit;
    const create = URL.createObjectURL.bind(URL);
    const revoke = URL.revokeObjectURL.bind(URL);
    URL.createObjectURL = (blob) => {
      const url = create(blob);
      audit.created.push(url);
      return url;
    };
    URL.revokeObjectURL = (url) => {
      audit.revoked.push(url);
      revoke(url);
    };
    const fetch = window.fetch.bind(window);
    window.fetch = (input, init) => {
      if (String(input).includes("/image?")) {
        audit.signals.push(init?.signal instanceof AbortSignal);
        init?.signal?.addEventListener("abort", () => {
          audit.aborts++;
        });
      }
      return fetch(input, init);
    };
    Object.defineProperty(navigator.clipboard, "writeText", {
      value: async (value: string) => {
        audit.copied = value;
      },
    });
  });
}
const imageAudit = (page: Page) =>
  page.evaluate(
    () => (window as unknown as { imageAudit: ImageAudit }).imageAudit,
  );

for (const width of [1280, 390])
  test(`message image preview decodes, copies and zooms at ${width}px`, async ({
    page,
  }) => {
    await page.setViewportSize({ width, height: 844 });
    const mock = await host(page, imageMarkdown);
    await auditImages(page);
    await page.goto("/", { waitUntil: "domcontentloaded" });
    const link = page.getByRole("button", { name: "Photo", exact: true });
    await expect(link).toBeVisible();
    expect(mock.imageReads).toEqual([]);
    await link.click();
    const image = preview(page).locator("img.zoomable-image");
    await expect(image).toBeVisible();
    await expect
      .poll(() =>
        image.evaluate(
          (img: HTMLImageElement) => img.complete && img.naturalWidth,
        ),
      )
      .toBe(1);
    const src = await image.getAttribute("src");
    expect(src).toMatch(/^blob:/);
    expect(mock.imageReads).toEqual([
      { project: project.id, path: imagePath, method: "GET", header: "1" },
    ]);
    expect(mock.reads).toEqual([]);
    expect((await imageAudit(page)).signals).toEqual([true]);
    await expect(
      preview(page).getByRole("button", { name: "Copy code", exact: true }),
    ).toHaveCount(0);
    await expect(preview(page).locator(".file-preview-path")).toContainText(
      `/workspace/links/${imagePath}`,
    );
    await preview(page)
      .getByRole("button", { name: "Copy path", exact: true })
      .click();
    await expect
      .poll(async () => (await imageAudit(page)).copied)
      .toBe(`/workspace/links/${imagePath}`);
    await preview(page)
      .getByRole("button", { name: "Copy relative path", exact: true })
      .click();
    await expect
      .poll(async () => (await imageAudit(page)).copied)
      .toBe(imagePath);
    expect((await preview(page).boundingBox())!.width).toBeLessThan(width);
    await image.click();
    await expect(page.locator(".image-viewer")).toBeVisible();
    await expect(page.locator(".image-viewer img")).toHaveAttribute(
      "src",
      src!,
    );
    await page.keyboard.press("Escape");
    await expect(page.locator(".image-viewer")).toHaveCount(0);
    await expect(preview(page)).toBeVisible();
    await expect(image).toBeFocused();
    await page.keyboard.press("Escape");
    await expect(preview(page)).toHaveCount(0);
    await expect(link).toBeFocused();
    await expect
      .poll(async () => (await imageAudit(page)).revoked)
      .toContain(src);
    expect(mock.unexpected).toEqual([]);
  });

for (const language of ["en", "zh"])
  test(`image HTTP, MIME and decode failures retry (${language})`, async ({
    page,
  }) => {
    const zh = language === "zh";
    const mock = await host(page, imageMarkdown, project.path, language);
    await auditImages(page);
    mock.imageMode("error");
    await page.goto("/", { waitUntil: "domcontentloaded" });
    await page.getByRole("button", { name: "Photo", exact: true }).click();
    const dialog = page.getByRole("dialog", {
      name: zh ? "文件预览" : "File preview",
      exact: true,
    });
    await expect(dialog.getByRole("alert")).toContainText(
      "Mock image exceeds 20 MiB",
    );
    const retry = dialog.getByRole("button", {
      name: zh ? "重试" : "Retry",
      exact: true,
    });
    mock.imageMode("mime");
    await retry.click();
    await expect.poll(() => mock.imageReads.length).toBe(2);
    await expect(dialog.getByRole("alert")).toBeVisible();
    await expect(dialog.locator("img")).toHaveCount(0);
    expect((await imageAudit(page)).created).toEqual([]);
    mock.imageMode("decode");
    await retry.click();
    await expect.poll(() => mock.imageReads.length).toBe(3);
    await expect(dialog.getByRole("alert")).toContainText(
      zh ? /图片|图像/ : /image/i,
    );
    await expect
      .poll(async () => (await imageAudit(page)).created.length)
      .toBe(1);
    const invalid = (await imageAudit(page)).created[0];
    mock.imageMode("ok");
    await retry.click();
    await expect
      .poll(async () => (await imageAudit(page)).revoked)
      .toContain(invalid);
    await expect(dialog.locator("img.zoomable-image")).toBeVisible();
    await expect
      .poll(() =>
        dialog
          .locator("img.zoomable-image")
          .evaluate((img: HTMLImageElement) => img.naturalWidth),
      )
      .toBe(1);
    expect(mock.imageReads).toHaveLength(4);
    expect(mock.reads).toEqual([]);
    await page.keyboard.press("Escape");
    await expect
      .poll(async () => {
        const audit = await imageAudit(page);
        return audit.created.every((url) => audit.revoked.includes(url));
      })
      .toBe(true);
    expect(mock.unexpected).toEqual([]);
  });

test("image close aborts, navigation revokes blobs and ignores pending stale responses", async ({
  page,
}) => {
  const mock = await host(page, imageMarkdown);
  await auditImages(page);
  await page.goto("/", { waitUntil: "domcontentloaded" });
  const photo = page.getByRole("button", { name: "Photo", exact: true });
  mock.hold();
  await photo.click();
  await expect.poll(() => mock.imageReads.length).toBe(1);
  await page.keyboard.press("Escape");
  await expect.poll(async () => (await imageAudit(page)).aborts).toBe(1);
  mock.release();
  await photo.click();
  await expect(preview(page).locator("img.zoomable-image")).toBeVisible();
  const src = await preview(page).locator("img").getAttribute("src");
  const navigate = () =>
    page
      .locator(".project-session-link")
      .filter({ hasText: "Other links session" })
      .evaluate((element: HTMLButtonElement) => element.click());
  await navigate();
  await expect(page.locator(".toolbar h1")).toHaveText("Other links session");
  await expect(preview(page)).toHaveCount(0);
  await expect
    .poll(async () => (await imageAudit(page)).revoked)
    .toContain(src);
  await page
    .locator(".project-session-link")
    .filter({ hasText: "Links session" })
    .first()
    .click();
  await expect(photo).toBeVisible();
  mock.hold();
  await photo.click();
  await expect.poll(() => mock.imageReads.length).toBe(3);
  const before = await imageAudit(page);
  await navigate();
  await expect
    .poll(async () => (await imageAudit(page)).aborts)
    .toBe(before.aborts + 1);
  mock.release();
  await page.waitForTimeout(150);
  await expect(preview(page)).toHaveCount(0);
  await expect(page.locator(".image-viewer")).toHaveCount(0);
  expect((await imageAudit(page)).created).toEqual(before.created);
  expect(mock.reads).toEqual([]);
  expect(mock.unexpected).toEqual([]);
});

test("unsupported binary links retain the text error route, never an image", async ({
  page,
}) => {
  const mock = await host(page, imageMarkdown);
  await page.goto("/", { waitUntil: "domcontentloaded" });
  await page.getByRole("button", { name: "Binary", exact: true }).click();
  await expect(preview(page).getByRole("alert")).toContainText(
    "Binary file cannot be previewed as text",
  );
  await expect(preview(page).locator("img")).toHaveCount(0);
  await expect(preview(page).locator(".file-preview-pdf")).toHaveCount(0);
  expect(mock.imageReads).toEqual([]);
  expect(mock.documents).toEqual([]);
  expect(mock.reads).toEqual([
    { project: project.id, path: "assets/manual.bin", root: null },
  ]);
  expect(mock.unexpected).toEqual([]);
});

test("PDF links stream bytes into the browser viewer", async ({ page }) => {
  const mock = await host(
    page,
    "[Report](docs/report.pdf)",
    project.path,
    "zh",
  );
  await page.goto("/", { waitUntil: "domcontentloaded" });
  await page.getByRole("button", { name: "Report", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "文件预览", exact: true });
  const pdf = dialog.locator("embed.file-preview-pdf");
  await expect(pdf).toBeVisible();
  await expect(pdf).toHaveAttribute("type", "application/pdf");
  await expect(dialog.locator(".file-preview-code")).toHaveCount(0);
  expect(mock.documents).toEqual([
    { project: project.id, path: "docs/report.pdf", root: null },
  ]);
  // No text read is attempted for a document the browser renders itself.
  expect(mock.reads).toEqual([]);
  await expect(
    dialog.getByRole("button", { name: "关闭", exact: true }),
  ).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(dialog).toHaveCount(0);
  expect(mock.unexpected).toEqual([]);
});

test("an older host offers a local copy instead of a dead end", async ({
  page,
}) => {
  const mock = await host(page, "[Report](docs/report.pdf)", project.path);
  mock.unsupportedDocuments();
  await page.goto("/", { waitUntil: "domcontentloaded" });
  await page.getByRole("button", { name: "Report", exact: true }).click();
  const dialog = preview(page);
  await expect(dialog.getByRole("alert")).toContainText(
    "This service cannot stream documents",
  );
  // The picker reads the file in the page, so no further host read is needed.
  await expect(
    dialog.getByRole("button", { name: "Preview a local copy", exact: true }),
  ).toBeVisible();
  await dialog.locator('input[type="file"]').setInputFiles({
    name: "report.pdf",
    mimeType: "application/pdf",
    buffer: pdfBytes,
  });
  await expect(dialog.locator(".file-preview-pdf")).toBeVisible();
  expect(mock.reads).toEqual([]);
  expect(mock.unexpected).toEqual([]);
});

test("Office documents offer a download instead of pretending to render", async ({
  page,
}) => {
  const mock = await host(page, "[Spec](docs/spec.docx)", project.path);
  await page.goto("/", { waitUntil: "domcontentloaded" });
  await page.getByRole("button", { name: "Spec", exact: true }).click();
  const dialog = preview(page);
  await expect(dialog.locator(".file-preview-office")).toContainText(
    "spec.docx",
  );
  // The card never pretends to render; both actions stay available.
  await expect(
    dialog.getByRole("button", { name: "Download", exact: true }),
  ).toBeVisible();
  await expect(
    dialog.getByRole("button", { name: "Preview a local copy", exact: true }),
  ).toBeVisible();
  await expect(dialog.locator(".file-preview-code")).toHaveCount(0);
  expect(mock.documents).toEqual([
    { project: project.id, path: "docs/spec.docx", root: null },
  ]);
  expect(mock.unexpected).toEqual([]);
});

test("all raster extensions select image reads case-insensitively", async ({
  page,
}) => {
  const extensions = ["png", "JPG", "jpeg", "GIF", "WebP", "BMP"];
  const mock = await host(
    page,
    extensions.map((ext) => `[${ext}](assets/photo.${ext})`).join(" "),
  );
  await page.goto("/", { waitUntil: "domcontentloaded" });
  for (const ext of extensions) {
    await page.getByRole("button", { name: ext, exact: true }).click();
    await expect(preview(page).locator("img.zoomable-image")).toBeVisible();
    expect(mock.imageReads.at(-1)?.path).toBe(`assets/photo.${ext}`);
    await page.keyboard.press("Escape");
  }
  expect(mock.imageReads).toHaveLength(extensions.length);
  expect(mock.reads).toEqual([]);
  expect(mock.unexpected).toEqual([]);
});

const notesDoc = [
  "# 需求文档",
  "",
  "| 项 | 内容 |",
  "| --- | --- |",
  "| 版本 | V1.0 |",
  "",
  "- 第一项",
  "- 第二项",
].join("\n");

for (const width of [1280, 390])
  test(`markdown preview reads rendered text, source and icon tools at ${width}px`, async ({
    page,
  }) => {
    await page.setViewportSize({ width, height: 844 });
    const mock = await host(page, "[Docs](docs/notes.md)", project.path, "zh");
    mock.files["docs/notes.md"] = notesDoc;
    await page.addInitScript(() => {
      Object.defineProperty(navigator.clipboard, "writeText", {
        value: async (text: string) => {
          (window as unknown as { copiedText: string }).copiedText = text;
        },
      });
    });
    await page.goto("/", { waitUntil: "domcontentloaded" });
    const dialog = page.getByRole("dialog", { name: "文件预览", exact: true });
    await page.getByRole("button", { name: "Docs", exact: true }).click();
    await expect(dialog.locator(".file-preview-markdown")).toBeVisible();
    await expect(dialog.locator(".file-preview-markdown h1")).toHaveText(
      "需求文档",
    );
    await expect(dialog.locator(".file-preview-markdown table")).toBeVisible();
    await expect(dialog.locator(".file-preview-code")).toHaveCount(0);
    await page.screenshot({
      path: test.info().outputPath(`markdown-preview-${width}.png`),
    });
    // Copy stays a compact icon strip instead of three labelled buttons.
    const tools = dialog.locator(".file-preview-tools button");
    for (const label of [
      "复制路径",
      "复制相对路径",
      "复制代码",
      "全屏查看",
      "关闭",
    ]) {
      const button = dialog.getByRole("button", { name: label, exact: true });
      await expect(button).toBeVisible();
      const box = (await button.boundingBox())!;
      expect(box.width).toBeLessThanOrEqual(34);
      expect(box.height).toBeLessThanOrEqual(34);
    }
    expect(await tools.count()).toBeGreaterThanOrEqual(6);
    await dialog.getByRole("button", { name: "复制路径", exact: true }).click();
    await expect
      .poll(() =>
        page.evaluate(
          () => (window as unknown as { copiedText: string }).copiedText,
        ),
      )
      .toBe("/workspace/links/docs/notes.md");
    await dialog.getByRole("button", { name: "查看源码", exact: true }).click();
    await expect(dialog.locator(".file-preview-code")).toBeVisible();
    await expect(dialog.locator('[data-line="1"]')).toContainText("# 需求文档");
    await dialog.getByRole("button", { name: "预览排版", exact: true }).click();
    await expect(dialog.locator(".file-preview-markdown")).toBeVisible();
    const normal = (await dialog.boundingBox())!;
    await dialog.getByRole("button", { name: "全屏查看", exact: true }).click();
    const full = (await dialog.boundingBox())!;
    expect(full.width).toBeGreaterThanOrEqual(width - 1);
    expect(full.height).toBeGreaterThanOrEqual(844 - 1);
    expect(full.width).toBeGreaterThan(normal.width);
    await dialog.getByRole("button", { name: "退出全屏", exact: true }).click();
    const restored = (await dialog.boundingBox())!;
    expect(restored.width).toBeLessThanOrEqual(normal.width + 1);
    await dialog.getByRole("button", { name: "关闭", exact: true }).click();
    await expect(dialog).toHaveCount(0);
    expect(mock.reads).toEqual([
      { project: project.id, path: "docs/notes.md", root: null },
    ]);
    expect(mock.unexpected).toEqual([]);
  });

test("source-first preview keeps a line deep link and offers rendered Markdown", async ({
  page,
}) => {
  const mock = await host(page, "[Docs](docs/notes.md:3)", project.path);
  mock.files["docs/notes.md"] = notesDoc;
  await page.goto("/", { waitUntil: "domcontentloaded" });
  await page.getByRole("button", { name: "Docs", exact: true }).click();
  const dialog = preview(page);
  await expect(dialog.locator(".file-preview-code")).toBeVisible();
  await expect(dialog.locator('[data-highlighted="true"]')).toHaveCount(1);
  await dialog
    .getByRole("button", { name: "Rendered view", exact: true })
    .click();
  await expect(dialog.locator(".file-preview-markdown")).toBeVisible();
  await expect(dialog.locator(".file-preview-code")).toHaveCount(0);
  expect(mock.unexpected).toEqual([]);
});

test("a path written from a nested package resolves to its unique project file", async ({
  page,
}) => {
  const mock = await host(page, "[Notes](src/notes.md)", project.path);
  // The message says `src/notes.md`; the file lives in a nested package.
  mock.missing.add("src/notes.md");
  mock.files["packages/web/src/notes.md"] = "# 恢复\n\n嵌套包内的文件";
  mock.listings[""] = [
    { name: "packages", path: "packages", kind: "directory" },
  ];
  mock.listings["packages"] = [
    { name: "web", path: "packages/web", kind: "directory" },
  ];
  mock.listings["packages/web"] = [
    { name: "src", path: "packages/web/src", kind: "directory" },
  ];
  mock.listings["packages/web/src"] = [
    { name: "notes.md", path: "packages/web/src/notes.md", kind: "file" },
  ];
  await page.goto("/", { waitUntil: "domcontentloaded" });
  await page.getByRole("button", { name: "Notes", exact: true }).click();
  const dialog = preview(page);
  await expect(dialog.locator(".file-preview-markdown h1")).toHaveText("恢复");
  await expect(dialog.locator(".file-preview-path")).toContainText(
    "/workspace/links/packages/web/src/notes.md",
  );
  await expect(dialog.locator(".file-preview-resolved")).toBeVisible();
  // The failed literal read is retried once, with the resolved path.
  expect(mock.reads.map((read) => read.path)).toEqual([
    "src/notes.md",
    "packages/web/src/notes.md",
  ]);
  expect(mock.unexpected).toEqual([]);
});

test("an ambiguous nested path offers the matching files", async ({ page }) => {
  const mock = await host(page, "[Notes](src/notes.md)", project.path);
  mock.missing.add("src/notes.md");
  mock.listings[""] = [
    { name: "a", path: "a", kind: "directory" },
    { name: "b", path: "b", kind: "directory" },
  ];
  mock.listings["a"] = [{ name: "src", path: "a/src", kind: "directory" }];
  mock.listings["a/src"] = [
    { name: "notes.md", path: "a/src/notes.md", kind: "file" },
  ];
  mock.listings["b"] = [{ name: "src", path: "b/src", kind: "directory" }];
  mock.listings["b/src"] = [
    { name: "notes.md", path: "b/src/notes.md", kind: "file" },
  ];
  mock.files["b/src/notes.md"] = "# 第二个候选";
  await page.goto("/", { waitUntil: "domcontentloaded" });
  await page.getByRole("button", { name: "Notes", exact: true }).click();
  const dialog = preview(page);
  const choices = dialog.locator(".file-preview-candidates button");
  await expect(choices).toHaveCount(2);
  await expect(choices.first()).toHaveText("a/src/notes.md");
  await choices.nth(1).click();
  await expect(dialog.locator(".file-preview-markdown h1")).toHaveText(
    "第二个候选",
  );
  await expect(dialog.locator(".file-preview-path")).toContainText(
    "b/src/notes.md",
  );
  expect(mock.unexpected).toEqual([]);
});

test("workbook parsing keeps formats, dates and every sheet", async () => {
  const book = await readWorkbook(new Uint8Array(sheetBytes));
  expect(book.sheets.map((sheet) => sheet.name)).toEqual(["预算", "备注"]);
  const [budget] = book.sheets;
  expect(budget.rows[0]).toEqual(["项目", "金额", "日期"]);
  expect(budget.rows[1][1]).toBe("1250");
  // `raw: false` keeps the date format instead of the numeric serial.
  expect(budget.rows[1][2]).toMatch(/^\d{1,2}\/\d{1,2}\/\d{2,4}$/);
  expect(book.truncated).toBe(false);
  await expect(
    readWorkbook(new Uint8Array([0x50, 0x4b, 0x03, 0x04])),
  ).rejects.toThrow();
});

test("a mis-nested PDF path is corrected before the viewer opens", async ({
  page,
}) => {
  const mock = await host(page, "[Report](src/report.pdf)", project.path);
  mock.missing.add("src/report.pdf");
  mock.listings[""] = [{ name: "docs", path: "docs", kind: "directory" }];
  mock.listings["docs"] = [
    { name: "report.pdf", path: "docs/report.pdf", kind: "file" },
  ];
  await page.goto("/", { waitUntil: "domcontentloaded" });
  await page.getByRole("button", { name: "Report", exact: true }).click();
  const dialog = preview(page);
  await expect(dialog.locator(".file-preview-pdf")).toBeVisible();
  await expect(dialog.locator(".file-preview-path")).toContainText(
    "/workspace/links/docs/report.pdf",
  );
  expect(mock.documents.map((read) => read.path)).toEqual([
    "src/report.pdf",
    "docs/report.pdf",
  ]);
  expect(mock.unexpected).toEqual([]);
});

test("xlsx links render a read-only sheet table with tabs", async ({
  page,
}) => {
  const mock = await host(page, "[Budget](docs/budget.xlsx)", project.path);
  await page.goto("/", { waitUntil: "domcontentloaded" });
  await page.getByRole("button", { name: "Budget", exact: true }).click();
  const dialog = preview(page);
  const sheet = dialog.locator(".file-preview-sheet");
  await expect(sheet).toBeVisible();
  await expect(sheet.locator("tr").nth(1)).toContainText("差旅");
  await expect(sheet.locator("tr").nth(1)).toContainText("1250");
  const tabs = sheet.getByRole("tab");
  await expect(tabs).toHaveCount(2);
  await expect(tabs.first()).toHaveAttribute("aria-selected", "true");
  await tabs.nth(1).click();
  await expect(sheet.locator("tr").nth(1)).toContainText("第二张表");
  await expect(dialog.locator(".file-preview-office")).toHaveCount(0);
  await expect(dialog.locator(".file-preview-code")).toHaveCount(0);
  expect(mock.documents).toEqual([
    { project: project.id, path: "docs/budget.xlsx", root: null },
  ]);
  expect(mock.reads).toEqual([]);
  expect(mock.unexpected).toEqual([]);
});

test("a binary that cannot be previewed downloads instead", async ({
  page,
}) => {
  const mock = await host(page, "[Archive](assets/manual.bin)", project.path);
  await page.goto("/", { waitUntil: "domcontentloaded" });
  await page.getByRole("button", { name: "Archive", exact: true }).click();
  const dialog = preview(page);
  await expect(dialog.getByRole("alert")).toContainText(
    "Binary file cannot be previewed as text",
  );
  await dialog.getByRole("button", { name: "Download", exact: true }).click();
  await expect(dialog.getByRole("status")).toContainText("Download started");
  expect(mock.downloads).toEqual([{ path: "assets/manual.bin", root: null }]);
  expect(mock.unexpected).toEqual([]);
});

test("Office cards download the original document", async ({ page }) => {
  const mock = await host(page, "[Spec](docs/spec.docx)", project.path);
  await page.goto("/", { waitUntil: "domcontentloaded" });
  await page.getByRole("button", { name: "Spec", exact: true }).click();
  const dialog = preview(page);
  await expect(dialog.locator(".file-preview-office")).toBeVisible();
  await dialog.getByRole("button", { name: "Download", exact: true }).click();
  await expect(dialog.getByRole("status")).toContainText("Download started");
  expect(mock.downloads).toEqual([{ path: "docs/spec.docx", root: null }]);
  expect(mock.unexpected).toEqual([]);
});

test("an older host explains that it cannot download either", async ({
  page,
}) => {
  const mock = await host(page, "[Archive](assets/manual.bin)", project.path);
  mock.unsupportedDownloads();
  await page.goto("/", { waitUntil: "domcontentloaded" });
  await page.getByRole("button", { name: "Archive", exact: true }).click();
  const dialog = preview(page);
  await dialog.getByRole("button", { name: "Download", exact: true }).click();
  await expect(dialog.getByRole("alert")).toContainText(
    "This service cannot download files",
  );
  expect(mock.unexpected).toEqual([]);
});

test("a file that only exists in a linked worktree resolves there", async ({
  page,
}) => {
  const worktree = "/workspace/links-wt";
  const mock = await host(page, "[Main](src/main.ts)", project.path, "en", [
    worktree,
  ]);
  mock.missing.add("src/main.ts");
  mock.worktreeListings[`${worktree}:`] = [
    { name: "src", path: "src", kind: "directory" },
  ];
  mock.worktreeListings[`${worktree}:src`] = [
    { name: "main.ts", path: "src/main.ts", kind: "file" },
  ];
  await page.goto("/", { waitUntil: "domcontentloaded" });
  await page.getByRole("button", { name: "Main", exact: true }).click();
  const dialog = preview(page);
  await expect(dialog.locator(".file-preview-code")).toBeVisible();
  await expect(dialog.locator(".file-preview-path")).toContainText(
    `${worktree}/src/main.ts`,
  );
  expect(mock.reads).toEqual([
    { project: project.id, path: "src/main.ts", root: null },
    { project: project.id, path: "src/main.ts", root: worktree },
  ]);
  expect(mock.unexpected).toEqual([]);
});

test("matches from two checkouts are offered with their worktree", async ({
  page,
}) => {
  const worktree = "/workspace/links-wt";
  const mock = await host(page, "[Main](src/main.ts)", project.path, "en", [
    worktree,
  ]);
  mock.missing.add("src/main.ts");
  mock.listings[""] = [{ name: "src", path: "src", kind: "directory" }];
  mock.listings["src"] = [
    { name: "main.ts", path: "src/main.ts", kind: "file" },
  ];
  mock.worktreeListings[`${worktree}:`] = [
    { name: "src", path: "src", kind: "directory" },
  ];
  mock.worktreeListings[`${worktree}:src`] = [
    { name: "main.ts", path: "src/main.ts", kind: "file" },
  ];
  await page.goto("/", { waitUntil: "domcontentloaded" });
  await page.getByRole("button", { name: "Main", exact: true }).click();
  const dialog = preview(page);
  const choices = dialog.locator(".file-preview-candidates button");
  await expect(choices).toHaveCount(2);
  // The project root is listed first, the worktree carries its label.
  await expect(choices.first()).toContainText("src/main.ts");
  await expect(choices.nth(1)).toContainText("worktree");
  await choices.nth(1).click();
  await expect(dialog.locator(".file-preview-path")).toContainText(
    `${worktree}/src/main.ts`,
  );
  await expect(dialog.locator(".file-preview-code")).toBeVisible();
  expect(mock.reads).toEqual([
    { project: project.id, path: "src/main.ts", root: null },
    { project: project.id, path: "src/main.ts", root: worktree },
  ]);
  expect(mock.unexpected).toEqual([]);
});
