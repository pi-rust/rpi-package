import { test as base, expect, type Page } from "@playwright/test";

// Deliberately independent of dev-web.spec.ts: every API request is intercepted,
// including read-shaped POSTs; an accidental prompt/mutation fails the fixture.
const sessionId = "timeline-session";
const otherSessionId = "timeline-other";
const nodes = (page: Page) => page.locator("nav.timeline-panel .timeline-node");
const list = (page: Page) => page.locator(".timeline-panel ol");
const earlier = (page: Page) => page.locator(".timeline-load-earlier");
const currentCursor = "native-current-cursor-not-a-row-id";
const users = (prefix: string, count: number) =>
  Array.from({ length: count }, (_, index) => ({
    role: "user",
    entryId: `${prefix}-${index}`,
    content:
      `Question ${prefix} ${index}\n` +
      Array.from(
        { length: 8 },
        (_, line) => `Line ${line}: bounded transcript anchor content.`,
      ).join("\n"),
  }));
const tools = (prefix: string) =>
  Array.from({ length: 48 }, (_, index) => ({
    role: "assistant",
    entryId: `${prefix}-${index}`,
    content: [
      {
        type: "toolCall",
        id: `${prefix}-call-${index}`,
        name: "read",
        arguments: { path: `src/file-${index}.ts` },
      },
    ],
  }));
type MessagePage = { messages: unknown[]; firstId: string; hasMore: boolean };
function gate() {
  let release!: () => void;
  const promise = new Promise<void>((resolve) => {
    release = resolve;
  });
  return { promise, release };
}
type Gate = ReturnType<typeof gate>;
type Reply = { data?: MessagePage; status?: number; wait?: Gate };
type Host = {
  initial: MessagePage;
  legacy: boolean;
  stateGate?: Gate;
  stateReads: number;
  pages: Reply[];
  reads: Array<{
    session: string;
    type: string;
    beforeId: string;
    limit: number;
  }>;
  unexpected: string[];
  inFlight: number;
  maxInFlight: number;
  cleanup: () => void;
};
const envelope = (data: unknown, command: string) => ({
  type: "response",
  command,
  success: true,
  data,
});
const summary = (id: string) => ({
  sessionId: id,
  projectId: "timeline-project",
  title: id === sessionId ? "Timeline session" : "Other timeline session",
  tags: [],
  status: "ready",
  createdAt: 1_700_000_000_000,
  updatedAt: id === sessionId ? 1_700_000_000_100 : 1_700_000_000_000,
  lastSeq: 4,
  error: null,
  relation: null,
  parentSessionId: null,
  forkEntryId: null,
  latestSettlement: null,
});

async function installHost(page: Page): Promise<Host> {
  const held = new Set<Gate>();
  const host: Host = {
    initial: {
      messages: users("current", 11),
      firstId: currentCursor,
      hasMore: true,
    },
    legacy: false,
    stateReads: 0,
    pages: [],
    reads: [],
    unexpected: [],
    inFlight: 0,
    maxInFlight: 0,
    cleanup: () => {
      host.stateGate?.release();
      for (const item of held) item.release();
    },
  };
  await page.addInitScript(() =>
    localStorage.setItem("rpi-web-language", "en"),
  );
  await page.route("**/api/**", async (route) => {
    const request = route.request();
    const path = new URL(request.url()).pathname;
    const method = request.method();
    const reject = async (detail = "") => {
      host.unexpected.push(`${method} ${path} ${detail}`.trim());
      await route.fulfill({
        status: 500,
        json: { error: "Unexpected API in timeline mock" },
      });
    };
    if (method === "GET" && path === "/api/v1/auth")
      return route.fulfill({ json: { required: false, authenticated: true } });
    if (method === "GET" && path === "/api/v1/capabilities")
      return route.fulfill({
        json: {
          protocol: "rpi",
          piCompatibilityTarget: "1.2.0",
          parityComplete: false,
          supportedCommands: [
            "get_state",
            "get_messages",
            "get_history",
            "get_stats",
            "get_queue",
          ],
          supportedActions: ["projects"],
        },
      });
    if (method === "GET" && path === "/api/v1/projects")
      return route.fulfill({
        json: {
          projects: [
            {
              id: "timeline-project",
              name: "Timeline project",
              path: "/mock/timeline",
              archived: false,
            },
          ],
          defaultProjectId: "timeline-project",
          projectsRoot: "/mock",
        },
      });
    if (method === "GET" && path === "/api/v1/sessions")
      return route.fulfill({
        json: { sessions: [summary(sessionId), summary(otherSessionId)] },
      });
    const match = path.match(/^\/api\/v1\/sessions\/([^/]+)\/(.+)$/);
    if (!match || ![sessionId, otherSessionId].includes(match[1]))
      return reject();
    const [, id, action] = match;
    if (method === "POST" && action === "open")
      return route.fulfill({ json: summary(id) });
    if (method === "GET" && action === "snapshot") {
      const data =
        id === sessionId
          ? host.initial
          : {
              messages: users("other", 2),
              firstId: "other-native-cursor",
              hasMore: false,
            };
      return route.fulfill({
        json: {
          sessionId: id,
          ...data,
          ...(host.legacy ? {} : { status: "ready" }),
          events: [],
          lastSeq: 4,
          leafId: "leaf",
          pendingResume: false,
        },
      });
    }
    if (method === "GET" && action === "events")
      return route.fulfill({
        contentType: "text/event-stream",
        body: 'retry: 60000\nevent: ready\ndata: {"type":"ready"}\n\n',
      });
    if (method === "POST" && action === "run-timings/query")
      return route.fulfill({
        json: {
          timings: [],
          complete: true,
          cursor: null,
          scannedBytes: 0,
          totalBytes: 0,
        },
      });
    if (method !== "POST" || action !== "commands") return reject();
    const body = request.postDataJSON();
    switch (body.type) {
      case "get_state":
        host.stateReads++;
        if (host.stateGate) await host.stateGate.promise;
        return route.fulfill({
          json: envelope(
            {
              sessionId: id,
              leafId: "leaf",
              activeOperation: null,
              queue: { steeringCount: 0, followUpCount: 0 },
              stats: {},
            },
            body.type,
          ),
        });
      case "get_queue":
        return route.fulfill({
          json: envelope({ items: [], count: 0 }, body.type),
        });
      case "get_stats":
        return route.fulfill({
          json: envelope(
            { messageCount: 0, totalTokens: 0, costTotal: 0 },
            body.type,
          ),
        });
      case "get_history":
        return route.fulfill({
          json: envelope(
            { records: [], total: 0, hasMore: false, openOperations: [] },
            body.type,
          ),
        });
      case "get_messages": {
        if (!body.beforeId || body.limit !== 48)
          return reject(JSON.stringify(body));
        host.reads.push({ session: id, ...body });
        const reply = host.pages.shift();
        if (!reply) return reject(`unplanned page ${body.beforeId}`);
        host.maxInFlight = Math.max(host.maxInFlight, ++host.inFlight);
        try {
          if (reply.wait) {
            held.add(reply.wait);
            await reply.wait.promise;
          }
          if (reply.status)
            return await route.fulfill({
              status: reply.status,
              json: { error: "Mock earlier page unavailable" },
            });
          return await route.fulfill({ json: envelope(reply.data, body.type) });
        } finally {
          host.inFlight--;
        }
      }
      default:
        return reject(JSON.stringify(body));
    }
  });
  return host;
}

const test = base.extend<{ host: Host }>({
  host: async ({ page }, use) => {
    const host = await installHost(page);
    try {
      await use(host);
    } finally {
      host.cleanup();
      expect(
        host.unexpected,
        "All API must remain mocked, read-only and explicitly planned",
      ).toEqual([]);
    }
  },
});
async function open(page: Page, count = 11) {
  // Public HMR websocket connections must not hold navigation open.
  await page.goto("/", { waitUntil: "domcontentloaded" });
  await expect(nodes(page)).toHaveCount(count);
  await expect(page.locator(".conversation-timeline")).toBeVisible();
}
async function settle(page: Page, milliseconds = 350) {
  // Negative assertions need to span both the 250ms wheel burst and RAF work.
  await page.waitForTimeout(milliseconds);
}
async function railTop(page: Page, top: number) {
  await list(page).evaluate(async (element, value) => {
    element.scrollTop = value;
    element.dispatchEvent(new Event("scroll", { bubbles: true }));
    await new Promise<void>((resolve) =>
      requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
    );
  }, top);
}
async function overRail(page: Page) {
  const box = (await list(page).boundingBox())!;
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
}
async function atTop(page: Page) {
  await overRail(page);
  await railTop(page, 0);
}
async function wheelUp(page: Page, delta = -80) {
  await page.mouse.wheel(0, delta);
}
async function transcriptAnchor(page: Page) {
  return page.locator(".transcript").evaluate((element) => {
    const top = element.getBoundingClientRect().top;
    const row = Array.from(
      element.querySelectorAll<HTMLElement>("[data-transcript-row]"),
    ).find((item) => item.getBoundingClientRect().bottom > top + 20);
    if (!row) throw new Error("Missing visible transcript anchor");
    return {
      id: row.dataset.transcriptRow!,
      offset: row.getBoundingClientRect().top - top,
    };
  });
}
async function expectTranscriptAnchor(
  page: Page,
  anchor: { id: string; offset: number },
) {
  await expect
    .poll(() =>
      page.locator(".transcript").evaluate((element, saved) => {
        const row = Array.from(
          element.querySelectorAll<HTMLElement>("[data-transcript-row]"),
        ).find((item) => item.dataset.transcriptRow === saved.id);
        return row
          ? Math.abs(
              row.getBoundingClientRect().top -
                element.getBoundingClientRect().top -
                saved.offset,
            )
          : Infinity;
      }, anchor),
    )
    .toBeLessThanOrEqual(3);
}
const older = (prefix = "older", count = 48, hasMore = true): MessagePage => ({
  messages: users(prefix, count),
  firstId: `native-${prefix}-cursor`,
  hasMore,
});

for (const count of [1, 10, 11]) {
  test(`${count} user nodes: default-visible left rail and ten-node geometry at desktop and 390px`, async ({
    page,
    host,
  }) => {
    host.initial = {
      messages: users("geometry", count),
      firstId: currentCursor,
      hasMore: true,
    };
    for (const width of [1280, 390]) {
      await page.setViewportSize({ width, height: 900 });
      await open(page, count);
      await expect(page.getByRole("tooltip")).toHaveCount(0);
      const geometry = await list(page).evaluate((element) => {
        const box = element.getBoundingClientRect();
        const marks = Array.from(
          element.querySelectorAll<HTMLElement>(".timeline-mark"),
        );
        const buttons = Array.from(
          element.querySelectorAll<HTMLElement>(".timeline-node"),
        );
        return {
          height: box.height,
          scrollHeight: element.scrollHeight,
          clientHeight: element.clientHeight,
          visible: buttons.filter((button) => {
            const b = button.getBoundingClientRect();
            return b.top >= box.top - 1 && b.bottom <= box.bottom + 1;
          }).length,
          opacity: marks.map((mark) => {
            let opacity = 1;
            for (
              let node: HTMLElement | null = mark;
              node;
              node = node.parentElement
            )
              opacity *= Number(getComputedStyle(node).opacity);
            return opacity;
          }),
          overflow: getComputedStyle(element).overflowY,
        };
      });
      expect(Math.min(...geometry.opacity)).toBeGreaterThanOrEqual(0.75);
      expect(geometry.height).toBeLessThanOrEqual(247);
      if (count <= 10) {
        expect(geometry.visible).toBe(count);
        expect(geometry.scrollHeight).toBeLessThanOrEqual(
          geometry.clientHeight + 1,
        );
      } else {
        expect(geometry.height).toBeGreaterThanOrEqual(245);
        expect(geometry.overflow).toBe("auto");
        expect(geometry.scrollHeight).toBeGreaterThan(geometry.clientHeight);
      }
      const frame = (await page.locator(".transcript-frame").boundingBox())!;
      const rail = (await page
        .locator(".conversation-timeline")
        .boundingBox())!;
      const arrow = (await earlier(page).boundingBox())!;
      const ol = (await list(page).boundingBox())!;
      expect(rail.width).toBeLessThanOrEqual(28);
      expect(rail.x).toBeGreaterThanOrEqual(frame.x);
      expect(rail.x + rail.width).toBeLessThan(frame.x + frame.width / 2);
      expect(arrow.height).toBeCloseTo(24, 0);
      expect(arrow.y + arrow.height).toBeLessThanOrEqual(ol.y + 1);
      expect(
        await earlier(page).evaluate((button) => !button.closest("ol")),
      ).toBe(true);
      expect(
        await page.evaluate(
          () => document.documentElement.scrollWidth <= innerWidth,
        ),
      ).toBe(true);
      await settle(page);
      expect(host.reads).toHaveLength(0);
      await page.screenshot({
        path: test.info().outputPath(`timeline-${count}-${width}.png`),
      });
    }
  });
}

test("legacy snapshot paints timeline while get_state still holds loadingSession", async ({
  page,
  host,
}) => {
  host.legacy = true;
  host.stateGate = gate();
  await open(page);
  await expect.poll(() => host.stateReads).toBe(1);
  await expect(page.locator(".message.user")).toHaveCount(11);
  await expect(earlier(page)).toBeVisible();
  await settle(page);
  expect(host.reads).toHaveLength(0);
  host.stateGate.release();
  await expect(page.locator(".conversation-timeline")).toBeVisible();
});

test("initial reveal, programmatic scroll and focus never auto-page", async ({
  page,
  host,
}) => {
  await open(page);
  await settle(page);
  await atTop(page);
  await railTop(page, 60);
  await railTop(page, 0);
  await nodes(page).first().focus();
  await nodes(page).last().focus();
  await page.keyboard.press("End");
  await page.keyboard.press("ArrowDown");
  await settle(page);
  expect(host.reads).toHaveLength(0);
});

test("native upward wheel is edge-gated, works already at top, and deduplicates a burst/in-flight page", async ({
  page,
  host,
}) => {
  const pending = gate();
  host.initial.messages = users("current", 20);
  host.pages.push({ wait: pending, data: older() });
  await open(page, 20);
  await overRail(page);
  await railTop(page, 100);
  await wheelUp(page, -1);
  await settle(page);
  expect(host.reads).toHaveLength(0);
  // No scroll transition is required: the wheel is delivered at scrollTop=0.
  await railTop(page, 0);
  await wheelUp(page);
  await expect.poll(() => host.reads.length).toBe(1);
  for (let index = 0; index < 4; index++) await wheelUp(page, -30);
  await settle(page);
  expect(host.reads).toEqual([
    {
      session: sessionId,
      type: "get_messages",
      beforeId: currentCursor,
      limit: 48,
    },
  ]);
  await expect(earlier(page)).toBeDisabled();
  pending.release();
  await expect(nodes(page)).toHaveCount(68);
  await settle(page);
  expect(host.reads).toHaveLength(1);
  expect(host.maxInFlight).toBe(1);
});

test("prepend preserves a visible old rail node offset and the parent transcript anchor", async ({
  page,
  host,
}) => {
  const pending = gate();
  host.initial.messages = users("current", 20);
  host.pages.push({ wait: pending, data: older() });
  await open(page, 20);
  await overRail(page);
  await railTop(page, 24);
  const saved = await nodes(page)
    .nth(2)
    .evaluate((button) => ({
      id: button.dataset.timelineTarget!,
      offset:
        button.getBoundingClientRect().top -
        button.closest("ol")!.getBoundingClientRect().top,
    }));
  const anchor = await transcriptAnchor(page);
  const oldIds = await nodes(page).evaluateAll((elements) =>
    elements.map((node) => node.getAttribute("data-timeline-target")),
  );
  await wheelUp(page, -1);
  await expect.poll(() => host.reads.length).toBe(1);
  // A native -1 wheel may move the list by one pixel before the request returns.
  const before = await nodes(page)
    .nth(2)
    .evaluate(
      (button) =>
        button.getBoundingClientRect().top -
        button.closest("ol")!.getBoundingClientRect().top,
    );
  pending.release();
  await expect(nodes(page)).toHaveCount(68);
  await expect
    .poll(() =>
      nodes(page).evaluateAll(
        (elements, target) => {
          const button = elements.find(
            (node) => node.getAttribute("data-timeline-target") === target.id,
          )!;
          return Math.abs(
            button.getBoundingClientRect().top -
              button.closest("ol")!.getBoundingClientRect().top -
              target.offset,
          );
        },
        { id: saved.id, offset: before },
      ),
    )
    .toBeLessThanOrEqual(2);
  await expectTranscriptAnchor(page, anchor);
  expect(
    (
      await nodes(page).evaluateAll((elements) =>
        elements.map((node) => node.getAttribute("data-timeline-target")),
      )
    ).slice(48),
  ).toEqual(oldIds);
  await settle(page);
  await expectTranscriptAnchor(page, anchor);
  expect(host.reads).toHaveLength(1);
});

test("failed cursor blocks more wheel bursts until move-away/reentry, with manual arrow retry", async ({
  page,
  host,
}) => {
  host.initial.messages = users("current", 20);
  host.pages.push(
    { status: 503 },
    { status: 503 },
    { data: older("retry", 1, false) },
  );
  await open(page, 20);
  await atTop(page);
  await wheelUp(page);
  await expect.poll(() => host.reads.length).toBe(1);
  await expect(earlier(page)).toBeEnabled();
  await settle(page);
  await wheelUp(page);
  await settle(page);
  expect(host.reads).toHaveLength(1);
  // Move away with real user scrolling, then return to the same native cursor.
  await page.mouse.wheel(0, 150);
  await expect
    .poll(() => list(page).evaluate((element) => element.scrollTop))
    .toBeGreaterThan(24);
  await settle(page);
  await wheelUp(page, -500);
  await expect.poll(() => host.reads.length).toBe(2);
  await expect(earlier(page)).toBeEnabled();
  await settle(page);
  await wheelUp(page);
  await settle(page);
  expect(host.reads).toHaveLength(2);
  await earlier(page).click();
  await expect(nodes(page)).toHaveCount(21);
  expect(host.reads.map((read) => read.beforeId)).toEqual([
    currentCursor,
    currentCursor,
    currentCursor,
  ]);
});

test("tool-only pages wheel through the arrow area once per gesture without auto-chaining", async ({
  page,
  host,
}) => {
  host.initial.messages = tools("initial-tools");
  host.pages.push(
    {
      data: {
        messages: tools("older-tools"),
        firstId: "native-tools-cursor",
        hasMore: true,
      },
    },
    { data: older("visible", 1, false) },
  );
  await open(page, 0);
  await expect(earlier(page)).toBeVisible();
  await settle(page);
  expect(host.reads).toHaveLength(0);
  const anchor = await transcriptAnchor(page);
  await earlier(page).hover();
  await wheelUp(page);
  await expect.poll(() => host.reads.length).toBe(1);
  await expect(earlier(page)).toBeEnabled();
  await expect(nodes(page)).toHaveCount(0);
  await settle(page, 600);
  expect(host.reads).toHaveLength(1);
  await expectTranscriptAnchor(page, anchor);
  await wheelUp(page);
  await expect(nodes(page)).toHaveCount(1);
  expect(host.reads.map((read) => read.beforeId)).toEqual([
    currentCursor,
    "native-tools-cursor",
  ]);
  await expect(earlier(page)).toHaveCount(0);
  await expectTranscriptAnchor(page, anchor);
});

test("hasMore=false and a no-progress native cursor both stop further automatic paging", async ({
  page,
  host,
}) => {
  host.initial.hasMore = false;
  await open(page);
  await atTop(page);
  await wheelUp(page);
  await nodes(page).first().focus();
  await page.keyboard.press("ArrowUp");
  await settle(page);
  await expect(earlier(page)).toHaveCount(0);
  expect(host.reads).toHaveLength(0);
  host.initial.hasMore = true;
  host.pages.push({
    data: { messages: [], firstId: currentCursor, hasMore: true },
  });
  await open(page);
  await atTop(page);
  await wheelUp(page);
  await expect.poll(() => host.reads.length).toBe(1);
  await expect(earlier(page)).toHaveCount(0);
  await settle(page);
  await wheelUp(page);
  await settle(page);
  expect(host.reads).toHaveLength(1);
});

test("first-node ArrowUp and Home request one native page without navigating the transcript", async ({
  page,
  host,
}) => {
  const anchorData: Array<{ key: string; cursor: string }> = [];
  host.pages.push(
    { data: older("keyboard-one", 2) },
    { data: older("keyboard-two", 2, false) },
  );
  await open(page);
  for (const key of ["ArrowUp", "Home"]) {
    await nodes(page).first().focus();
    const anchor = await transcriptAnchor(page);
    const previous = host.reads.length;
    await page.keyboard.press(key);
    await expect.poll(() => host.reads.length).toBe(previous + 1);
    await expect(nodes(page)).toHaveCount(11 + (previous + 1) * 2);
    await settle(page);
    await expectTranscriptAnchor(page, anchor);
    expect(host.reads).toHaveLength(previous + 1);
    anchorData.push({ key, cursor: host.reads[previous].beforeId });
  }
  expect(anchorData).toEqual([
    { key: "ArrowUp", cursor: currentCursor },
    { key: "Home", cursor: "native-keyboard-one-cursor" },
  ]);
});

test("mouse upper-edge scrub never loads while held, then loads exactly one page on release", async ({
  page,
  host,
}) => {
  host.pages.push({ data: older("mouse", 2) });
  await open(page);
  await atTop(page);
  const box = (await list(page).boundingBox())!;
  const start = (await nodes(page).nth(3).boundingBox())!;
  const x = box.x + box.width / 2;
  await page.mouse.move(x, start.y + start.height / 2);
  await page.mouse.down();
  try {
    await page.mouse.move(x, box.y + 2, { steps: 6 });
    await expect(list(page)).toHaveAttribute("data-scrubbing", "true");
    await settle(page, 600);
    expect(host.reads).toHaveLength(0);
    const anchor = await transcriptAnchor(page); // Scrubbing itself intentionally navigates.
    await page.mouse.up();
    await expect.poll(() => host.reads.length).toBe(1);
    await expect(nodes(page)).toHaveCount(13);
    await expect(list(page)).not.toHaveAttribute("data-scrubbing");
    await settle(page);
    expect(host.reads).toHaveLength(1);
    await expectTranscriptAnchor(page, anchor);
  } finally {
    await page.mouse.up();
  }
});

test("real 390px touch upper-edge scrub pages on touchEnd only", async ({
  browser,
  browserName,
  baseURL,
}) => {
  test.skip(
    browserName !== "chromium",
    "Real touch injection requires Chromium CDP",
  );
  const context = await browser.newContext({
    baseURL,
    isMobile: true,
    hasTouch: true,
    viewport: { width: 390, height: 844 },
  });
  const page = await context.newPage();
  const host = await installHost(page);
  try {
    host.pages.push({ data: older("touch", 2) });
    await open(page);
    expect(
      await page.evaluate(
        () =>
          navigator.maxTouchPoints > 0 &&
          matchMedia("(pointer: coarse)").matches,
      ),
    ).toBe(true);
    await settle(page);
    await railTop(page, 0);
    const box = (await list(page).boundingBox())!;
    const start = (await nodes(page).nth(3).boundingBox())!;
    const x = box.x + box.width / 2;
    const cdp = await context.newCDPSession(page);
    await cdp.send("Input.dispatchTouchEvent", {
      type: "touchStart",
      touchPoints: [{ x, y: start.y + start.height / 2, id: 1 }],
    });
    await cdp.send("Input.dispatchTouchEvent", {
      type: "touchMove",
      touchPoints: [{ x, y: box.y + 2, id: 1 }],
    });
    await expect(list(page)).toHaveAttribute("data-scrubbing", "true");
    await settle(page, 600);
    expect(host.reads).toHaveLength(0);
    const anchor = await transcriptAnchor(page);
    await cdp.send("Input.dispatchTouchEvent", {
      type: "touchEnd",
      touchPoints: [],
    });
    await expect.poll(() => host.reads.length).toBe(1);
    await expect(nodes(page)).toHaveCount(13);
    await settle(page);
    expect(host.reads).toHaveLength(1);
    await expectTranscriptAnchor(page, anchor);
    await cdp.detach();
  } finally {
    host.cleanup();
    await context.close();
    expect(host.unexpected).toEqual([]);
  }
});

test("a held old-session page cannot prepend into a newly selected session", async ({
  page,
  host,
}) => {
  const pending = gate();
  host.pages.push({ wait: pending, data: older("obsolete", 48, false) });
  await open(page);
  await atTop(page);
  await wheelUp(page);
  await expect.poll(() => host.reads.length).toBe(1);
  await page
    .locator(".project-session-link")
    .filter({ hasText: "Other timeline session" })
    .click();
  await expect(page.locator(".toolbar h1")).toHaveText(
    "Other timeline session",
  );
  await expect(nodes(page)).toHaveCount(2);
  const targets = await nodes(page).evaluateAll((elements) =>
    elements.map((node) => node.getAttribute("data-timeline-target")),
  );
  const anchor = await transcriptAnchor(page);
  pending.release();
  await settle(page, 600);
  expect(
    await nodes(page).evaluateAll((elements) =>
      elements.map((node) => node.getAttribute("data-timeline-target")),
    ),
  ).toEqual(targets);
  await expect(page.locator(".transcript")).not.toContainText(
    "Question obsolete",
  );
  await expectTranscriptAnchor(page, anchor);
  await expect(earlier(page)).toHaveCount(0);
  expect(host.reads).toEqual([
    {
      session: sessionId,
      type: "get_messages",
      beforeId: currentCursor,
      limit: 48,
    },
  ]);
});

test("Home from a nonfirst node cannot reuse a recent away-from-edge wheel to page", async ({
  page,
  host,
}) => {
  host.initial.messages = users("current", 20);
  await open(page, 20);
  await list(page).evaluate(async (element) => {
    const buttons =
      element.querySelectorAll<HTMLButtonElement>(".timeline-node");
    buttons[6].focus({ preventScroll: true });
    element.scrollTop = 100;
    // Same browser task: Home must see a fresh (<250ms) wheel intent, without
    // Playwright round trips or a native wheel's asynchronous default scrolling.
    element.dispatchEvent(
      new WheelEvent("wheel", { deltaY: -1, bubbles: true }),
    );
    buttons[6].dispatchEvent(
      new KeyboardEvent("keydown", {
        key: "Home",
        bubbles: true,
        cancelable: true,
      }),
    );
    await new Promise<void>((resolve) =>
      requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
    );
  });
  await expect(nodes(page).first()).toBeFocused();
  // reveal aligns to the first node's offset, after the 3px list padding.
  expect(
    await list(page).evaluate((element) => element.scrollTop),
  ).toBeLessThanOrEqual(3);
  expect(host.reads).toHaveLength(0);
  await settle(page);
  expect(host.reads).toHaveLength(0);
});

for (const outcome of ["failure", "tool-only"] as const) {
  test(`${outcome} page with unchanged user targets releases the rail anchor for transcript following`, async ({
    page,
    host,
  }) => {
    host.initial.messages = users("current", 20);
    host.pages.push(
      outcome === "failure"
        ? { status: 503 }
        : {
            data: {
              messages: tools("anchor-tools"),
              firstId: "native-anchor-tools-cursor",
              hasMore: true,
            },
          },
    );
    await open(page, 20);
    const originalTargets = await nodes(page).evaluateAll((elements) =>
      elements.map((node) => node.getAttribute("data-timeline-target")),
    );
    await atTop(page);
    await wheelUp(page);
    await expect.poll(() => host.reads.length).toBe(1);
    await expect(earlier(page)).toBeEnabled();
    await expect.poll(() => host.inFlight).toBe(0);
    await expect(nodes(page)).toHaveCount(20);
    expect(
      await nodes(page).evaluateAll((elements) =>
        elements.map((node) => node.getAttribute("data-timeline-target")),
      ),
    ).toEqual(originalTargets);
    const composer = page.locator(".composer textarea");
    await composer.hover();
    await composer.focus();
    // Let wheel browsing protection expire, without advancing/faking RAF time.
    await settle(page, 1100);
    await railTop(page, 0);
    const target = originalTargets[15]!;
    await page.locator(".transcript").evaluate(async (element, id) => {
      const row = Array.from(
        element.querySelectorAll<HTMLElement>("[data-transcript-row]"),
      ).find((item) => item.dataset.transcriptRow === id);
      if (!row) throw new Error("Missing loaded question to scroll to");
      // Scroll the transcript directly: a timeline click would clear the leaked
      // anchor itself and conceal the regression this test needs to exercise.
      element.scrollTop +=
        row.getBoundingClientRect().top -
        element.getBoundingClientRect().top -
        48;
      element.dispatchEvent(new Event("scroll", { bubbles: true }));
      await new Promise<void>((resolve) =>
        requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
      );
    }, target);
    await expect(nodes(page).nth(15)).toHaveAttribute(
      "aria-current",
      "location",
    );
    await expect
      .poll(() =>
        nodes(page)
          .nth(15)
          .evaluate((button) => {
            const box = button.getBoundingClientRect();
            const rail = button.closest("ol")!.getBoundingClientRect();
            return box.top >= rail.top - 1 && box.bottom <= rail.bottom + 1;
          }),
      )
      .toBe(true);
    expect(host.reads).toHaveLength(1);
  });
}

test("slow prepend preserves the old rail offset after pointer departure and browsing grace expiry", async ({
  page,
  host,
}) => {
  const pending = gate();
  host.initial.messages = users("current", 20);
  host.pages.push({ wait: pending, data: older("slow-prepend") });
  await open(page, 20);
  await overRail(page);
  await railTop(page, 24);
  const anchor = await transcriptAnchor(page);
  await wheelUp(page, -1);
  await expect.poll(() => host.reads.length).toBe(1);
  const composer = page.locator(".composer textarea");
  await composer.hover();
  await composer.focus();
  await settle(page, 1100);
  // Capture after the wheel's native default scroll, immediately before release.
  const saved = await nodes(page)
    .nth(2)
    .evaluate((button) => ({
      id: button.dataset.timelineTarget!,
      offset:
        button.getBoundingClientRect().top -
        button.closest("ol")!.getBoundingClientRect().top,
    }));
  pending.release();
  await expect(nodes(page)).toHaveCount(68);
  await list(page).evaluate(async () => {
    // Include both the prepend layout and subsequent active/ResizeObserver work.
    for (let frame = 0; frame < 4; frame++)
      await new Promise<void>((resolve) =>
        requestAnimationFrame(() => resolve()),
      );
  });
  const offsetError = () =>
    nodes(page).evaluateAll((elements, previous) => {
      const button = elements.find(
        (node) => node.getAttribute("data-timeline-target") === previous.id,
      );
      if (!button) return Infinity;
      return Math.abs(
        button.getBoundingClientRect().top -
          button.closest("ol")!.getBoundingClientRect().top -
          previous.offset,
      );
    }, saved);
  expect(await offsetError()).toBeLessThanOrEqual(2);
  await settle(page);
  expect(await offsetError()).toBeLessThanOrEqual(2);
  await expectTranscriptAnchor(page, anchor);
  expect(host.reads).toHaveLength(1);
});

test("live appended tick cannot consume a pending earlier-page rail anchor", async ({
  page,
  host,
}) => {
  const pending = gate();
  const live = gate();
  host.initial.messages = users("current", 20);
  host.pages.push({ wait: pending, data: older("live-race") });
  let emitted = false;
  await page.route(
    `**/api/v1/sessions/${sessionId}/events?*`,
    async (route) => {
      if (emitted)
        return route.fulfill({
          contentType: "text/event-stream",
          body: 'retry: 60000\nevent: ready\ndata: {"type":"ready"}\n\n',
        });
      emitted = true;
      await live.promise;
      await route.fulfill({
        contentType: "text/event-stream",
        body: `retry: 60000\nid: 5\nevent: message_start\ndata: ${JSON.stringify({ type: "message_start", message: users("live", 1)[0] })}\n\n`,
      });
    },
  );
  try {
    await open(page, 20);
    await overRail(page);
    await railTop(page, 0);
    await wheelUp(page, -1);
    await expect.poll(() => host.reads.length).toBe(1);
    await settle(page);
    const saved = await nodes(page)
      .nth(2)
      .evaluate((button) => ({
        id: button.dataset.timelineTarget!,
        offset:
          button.getBoundingClientRect().top -
          button.closest("ol")!.getBoundingClientRect().top,
      }));
    live.release();
    await expect(nodes(page)).toHaveCount(21);
    await settle(page);
    pending.release();
    await expect(nodes(page)).toHaveCount(69);
    await settle(page);
    const offsetError = await nodes(page).evaluateAll((elements, previous) => {
      const button = elements.find(
        (node) => node.getAttribute("data-timeline-target") === previous.id,
      );
      if (!button) return Infinity;
      return Math.abs(
        button.getBoundingClientRect().top -
          button.closest("ol")!.getBoundingClientRect().top -
          previous.offset,
      );
    }, saved);
    expect(offsetError).toBeLessThanOrEqual(2);
    expect(host.reads).toHaveLength(1);
  } finally {
    live.release();
    pending.release();
  }
});
