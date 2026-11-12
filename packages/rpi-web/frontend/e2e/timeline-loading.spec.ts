import { test as base, expect, type Page } from "@playwright/test";
import { translate } from "../src/i18n";

const transcript = (page: Page) => page.locator(".transcript");
const historicalMessage = (page: Page, index: number) =>
  transcript(page)
    .locator(".message")
    .getByText(`Historical native body ${index}`, { exact: true });
const historyPages = (page: Page) =>
  page.getByRole("navigation", { name: translate("en", "historyContextPagination"), exact: true });
const latestMessages = (page: Page) =>
  page.getByRole("button", { name: translate("en", "jumpToBottom"), exact: true });

const timelineToggle = (page: Page) =>
  page.getByRole("button", { name: /^(Conversation timeline|对话时间线)$/ });
async function openTimeline(page: Page) {
  const button = timelineToggle(page);
  await expect(button).toBeVisible();
  await expect(button).toHaveAttribute("aria-controls", "conversation-timeline-navigation");
  if (await button.getAttribute("aria-expanded") === "false") await button.click();
  await expect(button).toHaveAttribute("aria-expanded", "true");
}

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
type QueryReply = { data?: unknown; status?: number; wait?: Gate };
type Host = {
  nativeQueries: boolean;
  nativeLeaf: string;
  summaryPages: QueryReply[];
  windows: QueryReply[];
  queryReads: Array<{ session: string; type: string; params: Record<string, string> }>;
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
    nativeQueries: false, nativeLeaf: "native-pinned-leaf", summaryPages: [], windows: [], queryReads: [],
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
    // The client probes this before treating a closed event stream as a retired
    // host. These sessions stay live, so no reopen is attempted.
    const single = path.match(/^\/api\/v1\/sessions\/([^/]+)$/);
    if (method === "GET" && single && [sessionId, otherSessionId].includes(single[1]))
      return route.fulfill({ json: summary(single[1]) });
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
          ...(host.nativeQueries ? { supportedCommands: ["get_message_summaries", "get_message_window"] } : {}),
          ...(host.legacy ? {} : { status: "ready" }),
          events: [],
          lastSeq: 4,
          leafId: host.nativeQueries ? (id === sessionId ? host.nativeLeaf : "other-leaf") : "leaf",
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
    if (method === "GET" && ["message-summaries", "message-window"].includes(action)) {
      host.queryReads.push({ session: id, type: action, params: Object.fromEntries(new URL(request.url()).searchParams) });
      const reply = (action === "message-summaries" ? host.summaryPages : host.windows).shift();
      if (!reply) return reject("unplanned bounded query");
      if (reply.wait) { held.add(reply.wait); await reply.wait.promise; }
      return route.fulfill({ status: reply.status ?? 200, json: reply.status ? { error: "Mock bounded query unavailable" } : reply.data });
    }
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
  await openTimeline(page);
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
  test(`${count} user nodes: opt-in left rail and ten-node geometry at desktop and 390px`, async ({
    page,
    host,
  }) => {
    host.initial = {
      messages: users("geometry", count),
      firstId: currentCursor,
      hasMore: count > 1,
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
          nodeHeights: buttons.map(button => button.getBoundingClientRect().height),
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
      const step = 15;
      expect(geometry.nodeHeights.every(height => Math.abs(height - step) < 1)).toBe(true);
      expect(geometry.height).toBeLessThanOrEqual(10 * step + 7);
      if (count <= 10) {
        expect(geometry.visible).toBe(count);
        expect(geometry.scrollHeight).toBeLessThanOrEqual(
          geometry.clientHeight + 1,
        );
      } else {
        expect(geometry.height).toBeGreaterThanOrEqual(10 * step + 5);
        expect(geometry.overflow).toBe("auto");
        expect(geometry.scrollHeight).toBeGreaterThan(geometry.clientHeight);
      }
      const frame = (await page.locator(".transcript-frame").boundingBox())!;
      const rail = (await page
        .locator(".conversation-timeline")
        .boundingBox())!;
      const ol = (await list(page).boundingBox())!;
      expect(rail.width).toBeLessThanOrEqual(28);
      expect(rail.x).toBeGreaterThanOrEqual(frame.x);
      expect(rail.x + rail.width).toBeLessThan(frame.x + frame.width / 2);
      if (count > 1) {
        const arrow = (await earlier(page).boundingBox())!;
        expect(arrow.height).toBeCloseTo(24, 0);
        expect(arrow.y + arrow.height).toBeLessThanOrEqual(ol.y + 1);
        expect(
          await earlier(page).evaluate((button) => !button.closest("ol")),
        ).toBe(true);
      } else await expect(earlier(page)).toHaveCount(0);
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

test("initial tool-only pages fill through to real questions without gestures", async ({
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
  await open(page, 1);
  await settle(page, 600);
  expect(host.maxInFlight).toBe(1);
  expect(host.reads.map((read) => read.beforeId)).toEqual([
    currentCursor,
    "native-tools-cursor",
  ]);
  await expect(earlier(page)).toHaveCount(0);
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
  await openTimeline(page);
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

for (const width of [1280, 390]) {
  test(`initial dense timeline fills ten real questions without gestures at ${width}px`, async ({
    page,
    host,
  }) => {
    await page.setViewportSize({ width, height: 900 });
    host.initial.messages = [
      ...users("latest", 1),
      ...tools("latest-tools").slice(0, 47),
    ];
    for (let index = 1; index <= 9; index++) {
      host.pages.push({
        data: {
          messages: [
            ...users(`fill-${index}`, 1),
            ...tools(`fill-tools-${index}`).slice(0, 47),
          ],
          firstId: `fill-cursor-${index}`,
          hasMore: true,
        },
      });
    }
    await open(page, 10);
    await expect
      .poll(() =>
        nodes(page).evaluateAll((buttons) => {
          const box = buttons[0].closest("ol")!.getBoundingClientRect();
          return buttons.filter((button) => {
            const rect = button.getBoundingClientRect();
            return rect.top >= box.top - 1 && rect.bottom <= box.bottom + 1;
          }).length;
        }),
      )
      .toBe(10);
    expect(host.reads.map((read) => read.beforeId)).toEqual([
      currentCursor,
      ...Array.from({ length: 8 }, (_, index) => `fill-cursor-${index + 1}`),
    ]);
    expect(host.reads.every((read) => read.limit === 48)).toBe(true);
    expect(host.maxInFlight).toBe(1);
    await settle(page, 600);
    expect(host.reads).toHaveLength(9);
    await page.screenshot({
      path: test.info().outputPath(`timeline-default-ten-${width}.png`),
    });
  });
}

test("initial fill has a ten-page budget even for tool-only history and remains manually resumable", async ({
  page,
  host,
}) => {
  host.initial.messages = tools("budget-initial");
  for (let index = 1; index <= 10; index++)
    host.pages.push({
      data: {
        messages: tools(`budget-${index}`),
        firstId: `budget-cursor-${index}`,
        hasMore: true,
      },
    });
  host.pages.push({ data: older("manual-after-budget", 1, false) });
  await open(page, 0);
  await expect.poll(() => host.reads.length).toBe(10);
  await expect(earlier(page)).toBeEnabled();
  await settle(page, 700);
  expect(host.reads).toHaveLength(10);
  expect(host.maxInFlight).toBe(1);
  await earlier(page).click();
  await expect(nodes(page)).toHaveCount(1);
  expect(host.reads).toHaveLength(11);
  expect(host.reads[10].beforeId).toBe("budget-cursor-10");
  await expect(earlier(page)).toHaveCount(0);
});

test("initial fill failure stops without retries and the arrow can retry the same cursor", async ({
  page,
  host,
}) => {
  host.initial.messages = users("fill-failure", 1);
  host.pages.push({ status: 503 }, { data: older("fill-retry", 9, false) });
  await open(page, 1);
  await expect.poll(() => host.reads.length).toBe(1);
  await expect(earlier(page)).toBeEnabled();
  await settle(page, 700);
  expect(host.reads).toHaveLength(1);
  await earlier(page).click();
  await expect(nodes(page)).toHaveCount(10);
  expect(host.reads.map((read) => read.beforeId)).toEqual([
    currentCursor,
    currentCursor,
  ]);
});

for (const nextCursor of [currentCursor, ""]) {
  test(`initial fill stops at a nonadvancing or missing cursor ${nextCursor || "missing"}`, async ({
    page,
    host,
  }) => {
    host.initial.messages = users("no-progress", 1);
    host.pages.push({
      data: { messages: [], firstId: nextCursor, hasMore: true },
    });
    await open(page, 1);
    await expect.poll(() => host.reads.length).toBe(1);
    await expect(earlier(page)).toHaveCount(0);
    await settle(page, 700);
    expect(host.reads).toHaveLength(1);
  });
}

test("slow initial fill preserves the transcript reading anchor", async ({
  page,
  host,
}) => {
  const pending = gate();
  host.initial.messages = [
    ...users("anchor-latest", 1),
    ...tools("anchor-latest-tools").slice(0, 47),
  ];
  host.pages.push({ wait: pending, data: older("anchor-fill", 9, false) });
  await open(page, 1);
  await expect.poll(() => host.reads.length).toBe(1);
  await page.locator(".transcript").evaluate((element) => {
    element.scrollTop /= 2;
  });
  await settle(page);
  const anchor = await transcriptAnchor(page);
  pending.release();
  await expect(nodes(page)).toHaveCount(10);
  await settle(page);
  await expectTranscriptAnchor(page, anchor);
  expect(host.reads).toHaveLength(1);
});

test("navigation stops the old initial-fill chain and ignores its held response", async ({
  page,
  host,
}) => {
  const pending = gate();
  host.initial.messages = users("old-initial", 1);
  host.pages.push({ wait: pending, data: older("obsolete-fill", 1, true) });
  await open(page, 1);
  await expect.poll(() => host.reads.length).toBe(1);
  await page
    .locator(".project-session-link")
    .filter({ hasText: "Other timeline session" })
    .click();
  await expect(page.locator(".toolbar h1")).toHaveText(
    "Other timeline session",
  );
  await openTimeline(page);
  await expect(nodes(page)).toHaveCount(2);
  pending.release();
  await settle(page, 700);
  await expect(nodes(page)).toHaveCount(2);
  await expect(page.locator(".transcript")).not.toContainText(
    "Question obsolete-fill",
  );
  expect(host.reads).toHaveLength(1);
});

function nativeSummaryPage(start = 10, count = 10, hasMore = true) {
  const messages = Array.from({ length: count }, (_, i) => ({ entryId: `native-${start + i}`, role: "user", preview: `Native question ${start + i}`, timestamp: start + i, hasImages: false }));
  return { filtered: true, role: "user", messages, firstId: messages[0]?.entryId ?? null, lastId: messages.at(-1)?.entryId ?? null, nextBeforeId: hasMore ? messages[0]?.entryId ?? null : null, hasMore, leafId: "native-pinned-leaf" };
}
function nativeWindowPage(start = 9, count = 3, hasMoreAfter = true) {
  const messages = Array.from({ length: count }, (_, i) => ({ role: "user", entryId: `native-${start + i}`, content: `Historical native body ${start + i}` }));
  return { messages, firstId: messages[0]?.entryId ?? null, lastId: messages.at(-1)?.entryId ?? null, nextBeforeId: start > 0 ? messages[0]?.entryId ?? null : null, nextAfterId: hasMoreAfter ? messages.at(-1)?.entryId ?? null : null, hasMoreBefore: start > 0, hasMoreAfter, leafId: "native-pinned-leaf" };
}
test("hidden native timeline performs no summary reads until explicitly opened", async ({ page, host }) => {
  enableNative(host);
  await page.goto("/", { waitUntil: "domcontentloaded" });
  await expect(transcript(page).locator("[data-transcript-row]")).toHaveCount(49);
  const frames = () => page.evaluate(async () => {
    for (let frame = 0; frame < 4; frame++)
      await new Promise<void>(resolve => requestAnimationFrame(() => resolve()));
  });
  await frames();
  const toggle = timelineToggle(page);
  await expect(toggle).toHaveAttribute("aria-expanded", "false");
  await expect(page.locator(".conversation-timeline")).toHaveCount(0);
  expect(host.queryReads).toEqual([]);
  expect(host.reads).toEqual([]);
  const composer = page.locator(".composer textarea");
  await composer.fill("Keep this native timeline draft");
  await openTimeline(page);
  await expect.poll(() => host.queryReads.length).toBe(1);
  await expect(nodes(page)).toHaveCount(10);
  await expect(page.locator(".conversation-timeline")).toBeVisible();
  expect((await page.locator(".conversation-timeline").boundingBox())!.width).toBeLessThanOrEqual(28);
  await toggle.click();
  await expect(toggle).toHaveAttribute("aria-expanded", "false");
  await expect(page.locator(".conversation-timeline")).toHaveCount(0);
  await frames();
  await expect(page.locator(".conversation-timeline")).toHaveCount(0);
  await expect(toggle).toHaveAttribute("aria-expanded", "false");
  await expect(composer).toHaveValue("Keep this native timeline draft");
  expect(host.queryReads).toEqual([{
    session: sessionId, type: "message-summaries",
    params: { limit: "10", role: "user", leafId: "native-pinned-leaf" },
  }]);
  expect(host.reads).toEqual([]);
});

function enableNative(host: Host) {
  host.nativeQueries = true;
  host.initial = { messages: [...tools("dense-native"), ...users("native", 20).slice(-1)], firstId: currentCursor, hasMore: true };
  host.summaryPages.push({ data: nativeSummaryPage() });
}
for (const width of [1280, 390]) {
  test(`native summaries fetch ten questions in one light paginated query at ${width}px`, async ({ page, host }) => {
    await page.setViewportSize({ width, height: 860 });
    enableNative(host);
    await open(page, 10);
    await settle(page);
    expect(host.queryReads).toHaveLength(1);
    expect(host.queryReads[0]).toEqual({ session: sessionId, type: "message-summaries", params: { limit: "10", role: "user", leafId: "native-pinned-leaf" } });
    expect(host.reads).toHaveLength(0);
    await expect(page.locator(".transcript [data-transcript-row]")).toHaveCount(49);
    await expect(nodes(page).last()).toHaveAttribute("aria-label", /Native question 19/);
    await page.screenshot({ path: test.info().outputPath(`native-summary-${width}.png`) });
  });
}
test("native historical keyboard navigation stays inline without a modal or focus trap", async ({ page, host }) => {
  enableNative(host);
  const held = gate();
  host.windows.push({ data: nativeWindowPage(), wait: held });
  await open(page, 10);
  const question = nodes(page).first();
  await question.focus();
  await page.keyboard.press("Enter");
  await expect(transcript(page)).toHaveAttribute("data-history-window", "true");
  await expect(page.locator(".history-window-status")).toContainText(translate("en", "loadingEarlierMessages"));
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(question).toBeFocused();
  // A delayed result must not pull focus back from the composer or create a trap.
  const composer = page.locator(".composer textarea");
  await composer.focus();
  held.release();
  await expect(historicalMessage(page, 10)).toBeVisible();
  await expect(composer).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(transcript(page)).toHaveAttribute("data-history-window", "true");
  await latestMessages(page).click();
  await expect(transcript(page)).not.toHaveAttribute("data-history-window", "true");
  await expect(transcript(page).locator("[data-transcript-row]")).toHaveCount(49);
  await expect(page.getByRole("dialog")).toHaveCount(0);
  expect(host.queryReads.filter(read => read.type === "message-window")).toHaveLength(1);
});

test("native summary pagination is single-flight, exclusive and pins its branch leaf", async ({ page, host }) => {
  enableNative(host);
  const held = gate(); host.summaryPages.push({ data: nativeSummaryPage(0, 10, false), wait: held });
  await open(page, 10);
  const anchor = await transcriptAnchor(page);
  await earlier(page).click();
  await expect(earlier(page)).toBeDisabled();
  await atTop(page); await wheelUp(page); await settle(page);
  expect(host.queryReads).toHaveLength(2);
  expect(host.queryReads[1].params).toEqual({ limit: "10", role: "user", beforeId: "native-10", leafId: "native-pinned-leaf" });
  held.release();
  await expect(nodes(page)).toHaveCount(20);
  expect(host.reads).toHaveLength(0);
  await expectTranscriptAnchor(page, anchor);
  await expect(earlier(page)).toHaveCount(0);
});
for (const width of [1280, 390]) {
test(`native historical click replaces the main transcript with bounded pages at ${width}px`, async ({ page, host }) => {
  await page.setViewportSize({ width, height: 860 });
  enableNative(host);
  host.windows.push({ data: nativeWindowPage() }, { data: nativeWindowPage(12, 3, false) }, { data: nativeWindowPage() });
  await open(page, 10);
  await nodes(page).first().click();
  await expect(transcript(page)).toHaveAttribute("data-history-window", "true");
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(historicalMessage(page, 10)).toBeVisible();
  await expect(transcript(page).locator("[data-transcript-row]")).toHaveCount(3);
  await expect(transcript(page)).not.toContainText("Question native 19");
  expect(host.queryReads[1].params).toEqual({ limit: "48", anchorId: "native-10", leafId: "native-pinned-leaf" });
  await expect(historyPages(page)).toBeVisible();
  await page.screenshot({ path: test.info().outputPath(`native-inline-history-${width}.png`) });
  await historyPages(page).getByRole("button", { name: "Later page", exact: true }).click();
  await expect(historicalMessage(page, 14)).toBeVisible();
  await expect(historicalMessage(page, 10)).toHaveCount(0);
  await expect(transcript(page).locator("[data-transcript-row]")).toHaveCount(3);
  await expect(historyPages(page).getByRole("button", { name: "Later page", exact: true })).toBeDisabled();
  expect(host.queryReads[2].params).toEqual({ limit: "48", afterId: "native-11", leafId: "native-pinned-leaf" });
  await historyPages(page).getByRole("button", { name: "Earlier page", exact: true }).click();
  await expect(historicalMessage(page, 10)).toBeVisible();
  await expect(historicalMessage(page, 14)).toHaveCount(0);
  expect(host.queryReads[3].params).toEqual({ limit: "48", beforeId: "native-12", leafId: "native-pinned-leaf" });
  await expect(transcript(page).locator("[data-transcript-row]")).toHaveCount(3);
  expect(host.reads).toHaveLength(0);
  await latestMessages(page).click();
  await expect(transcript(page)).not.toHaveAttribute("data-history-window", "true");
  await expect(transcript(page).locator("[data-transcript-row]")).toHaveCount(49);
  await expect(transcript(page)).toContainText("Question native 19");
  await expect(historicalMessage(page, 10)).toHaveCount(0);
  await expect(historyPages(page)).toHaveCount(0);
  expect(host.queryReads).toHaveLength(4);
});
}
for (const width of [1280, 390]) {
  test(`native inline navigation aligns the requested question within a full window at ${width}px`, async ({ page, host }) => {
    await page.setViewportSize({ width, height: 860 });
    enableNative(host);
    host.windows.push({ data: nativeWindowPage(0, 48, false) });
    await open(page, 10);
    await nodes(page).first().click();
    const body = historicalMessage(page, 10);
    await expect(body).toBeVisible();
    await expect(transcript(page).locator("[data-transcript-row]")).toHaveCount(48);
    await expect.poll(() => body.evaluate(element => {
      const top = element.closest(".transcript")!.getBoundingClientRect().top;
      return Math.round(element.closest("[data-transcript-row]")!.getBoundingClientRect().top - top);
    })).toBe(12);
    await expect(page.getByRole("dialog")).toHaveCount(0);
    expect(host.queryReads.filter(read => read.type === "message-window")).toHaveLength(1);
    expect(host.reads).toHaveLength(0);
    await page.screenshot({ path: test.info().outputPath(`native-aligned-inline-${width}.png`) });
  });
}

test("native local timeline selection cancels pending historical paging", async ({ page, host }) => {
  enableNative(host);
  const held = gate();
  host.windows.push({ data: nativeWindowPage() }, { data: nativeWindowPage(12), wait: held });
  await open(page, 10);
  await nodes(page).first().click();
  await expect(historicalMessage(page, 10)).toBeVisible();
  await historyPages(page).getByRole("button", { name: "Later page", exact: true }).click();
  await expect.poll(() => host.queryReads.filter(read => read.type === "message-window").length).toBe(2);
  await page.locator('.timeline-node[aria-label*="Native question 11"]').click();
  held.release();
  await settle(page);
  await expect(historicalMessage(page, 11)).toBeVisible();
  await expect(historicalMessage(page, 14)).toHaveCount(0);
  await expect(historyPages(page).getByRole("button", { name: "Later page", exact: true })).toBeEnabled();
  expect(host.queryReads.filter(read => read.type === "message-window")).toHaveLength(2);
  expect(host.reads).toHaveLength(0);
});

test("native historical footer looks up only authoritative metadata for the displayed window", async ({ page, host }) => {
  enableNative(host);
  host.windows.push({ data: nativeWindowPage() });
  const queried: string[][] = [];
  await page.route(`**/api/v1/sessions/${sessionId}/run-timings/query`, route => {
    const ids = route.request().postDataJSON().entryIds as string[];
    queried.push(ids);
    return route.fulfill({ json: {
      timings: ids.includes("native-11") ? [{ runId: "historical-run", finalEntryId: "native-11", startedAt: 1000, finishedAt: 4000, elapsedMs: 3000, outcome: "completed" }] : [],
      complete: true, cursor: null, scannedBytes: 0, totalBytes: 0,
    } });
  });
  await open(page, 10);
  await nodes(page).first().click();
  await expect(transcript(page).locator(".run-elapsed")).toContainText("3s");
  expect(queried.some(ids => ids.length === 3 && ids.includes("native-9") && ids.includes("native-11"))).toBe(true);
  await latestMessages(page).click();
  await expect(transcript(page).locator(".run-elapsed")).toHaveCount(0);
  expect(host.reads).toHaveLength(0);
});

test("native loaded question navigates locally without fetching a context window", async ({ page, host }) => {
  enableNative(host); await open(page, 10);
  await nodes(page).last().click(); await settle(page);
  expect(host.queryReads).toHaveLength(1);
  expect(host.reads).toHaveLength(0);
  await expect(transcript(page)).not.toHaveAttribute("data-history-window", "true");
  await expect(page.getByRole("dialog")).toHaveCount(0);
});
test("native transient failure is visible and retryable without heavy fallback reads", async ({ page, host }) => {
  enableNative(host); host.summaryPages.unshift({ status: 503 });
  await open(page, 1);
  await expect(page.locator(".timeline-query-error")).toContainText("Mock bounded query unavailable");
  await settle(page, 700);
  expect(host.reads).toHaveLength(0); expect(host.queryReads).toHaveLength(1);
  await page.locator(".timeline-query-error").getByRole("button", { name: "Retry" }).click();
  await expect(nodes(page)).toHaveCount(10);
  expect(host.queryReads).toHaveLength(2);
});
test("native unsupported endpoint falls back only to the existing bounded legacy paginator", async ({ page, host }) => {
  enableNative(host); host.summaryPages = [{ status: 501 }];
  host.pages.push({ data: older("fallback", 9, false) });
  await open(page, 10);
  expect(host.queryReads).toHaveLength(1); expect(host.reads).toHaveLength(1);
  expect(host.reads[0].limit).toBe(48);
});

function otherNativePage() {
  const messages = users("other", 2).map((message, index) => ({ entryId: message.entryId, role: "user", preview: `Other question ${index}`, timestamp: index, hasImages: false }));
  return { filtered: true, role: "user", messages, firstId: "other-0", lastId: "other-1", nextBeforeId: null, hasMore: false, leafId: "other-leaf" };
}
test("native summary pagination aborts stale pages when switching sessions", async ({ page, host }) => {
  enableNative(host);
  const held = gate(); host.summaryPages.push({ data: nativeSummaryPage(0), wait: held }, { data: otherNativePage() });
  await open(page, 10); await earlier(page).click();
  await expect.poll(() => host.queryReads.length).toBe(2);
  await page.locator(".project-session-link").filter({ hasText: "Other timeline session" }).click();
  await expect(page.locator(".toolbar h1")).toHaveText("Other timeline session");
  await openTimeline(page);
  await expect(nodes(page)).toHaveCount(2);
  held.release(); await settle(page);
  await expect(nodes(page)).toHaveCount(2);
  await expect(nodes(page).first()).toHaveAttribute("aria-label", /Other question 0/);
  expect(host.reads).toHaveLength(0);
});
test("native inline history aborts when switching sessions", async ({ page, host }) => {
  enableNative(host);
  const held = gate(); host.windows.push({ data: nativeWindowPage(), wait: held }); host.summaryPages.push({ data: otherNativePage() });
  await open(page, 10); await nodes(page).first().click();
  await expect.poll(() => host.queryReads.length).toBe(2);
  await expect(transcript(page)).toHaveAttribute("data-history-window", "true");
  await page.locator(".project-session-link").filter({ hasText: "Other timeline session" }).click();
  await expect(page.locator(".toolbar h1")).toHaveText("Other timeline session");
  await openTimeline(page);
  await expect(nodes(page)).toHaveCount(2);
  held.release(); await settle(page);
  await expect(transcript(page)).not.toHaveAttribute("data-history-window", "true");
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(transcript(page)).not.toContainText("Historical native body");
  await expect(transcript(page)).toContainText("Question other 1");
  expect(host.reads).toHaveLength(0);
});
test("native context branch conflicts stay inline and retry without a mixed-history fallback", async ({ page, host }) => {
  enableNative(host); host.windows.push({ status: 409 }, { data: nativeWindowPage() });
  await open(page, 10); await nodes(page).first().click();
  await expect(transcript(page)).toHaveAttribute("data-history-window", "true");
  const status = page.locator(".history-window-status");
  await expect(status).toContainText("Mock bounded query unavailable");
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(transcript(page)).not.toContainText("Question native 19");
  await settle(page);
  expect(host.queryReads).toHaveLength(2); expect(host.reads).toHaveLength(0);
  await status.getByRole("button", { name: "Retry", exact: true }).click();
  await expect(historicalMessage(page, 10)).toBeVisible();
  expect(host.queryReads[2].params).toEqual(host.queryReads[1].params);
  await latestMessages(page).click();
  await expect(transcript(page)).not.toHaveAttribute("data-history-window", "true");
  expect(host.queryReads).toHaveLength(3); expect(host.reads).toHaveLength(0);
});

test("native rapid A/B history clicks isolate late success and error responses", async ({ page, host }) => {
  enableNative(host);
  const first = gate(), second = gate(), staleError = gate();
  host.windows.push(
    { data: nativeWindowPage(9), wait: first },
    { data: nativeWindowPage(14), wait: second },
    { status: 503, wait: staleError },
    { data: nativeWindowPage(16) },
  );
  await open(page, 10);
  await nodes(page).first().click();
  await expect.poll(() => host.queryReads.length).toBe(2);
  // B must start while A is still held: single-flight applies per selection,
  // not as a global lock that silently ignores a newer timeline click.
  await nodes(page).nth(5).click();
  await expect.poll(() => host.queryReads.length).toBe(3);
  expect(host.queryReads[2].params.anchorId).toBe("native-15");
  second.release();
  await expect(historicalMessage(page, 15)).toBeVisible();
  first.release();
  await settle(page);
  await expect(historicalMessage(page, 15)).toBeVisible();
  await expect(historicalMessage(page, 10)).toHaveCount(0);
  await expect(transcript(page).locator("[data-transcript-row]")).toHaveCount(3);
  // Repeat with a stale error; it must not replace the newer success with Retry.
  await nodes(page).first().click();
  await expect.poll(() => host.queryReads.length).toBe(4);
  await nodes(page).nth(7).click();
  await expect.poll(() => host.queryReads.length).toBe(5);
  await expect(historicalMessage(page, 17)).toBeVisible();
  staleError.release();
  await settle(page);
  await expect(historicalMessage(page, 17)).toBeVisible();
  await expect(page.locator(".history-window-status")).not.toContainText("Mock bounded query unavailable");
  expect(host.queryReads.filter(read => read.type === "message-window").map(read => read.params.anchorId)).toEqual(["native-10", "native-15", "native-10", "native-17"]);
  expect(host.reads).toHaveLength(0);
});

test("return to latest cancels pending inline history without a late render or focus theft", async ({ page, host }) => {
  enableNative(host);
  const held = gate();
  host.windows.push({ data: nativeWindowPage(), wait: held });
  await open(page, 10);
  await nodes(page).first().click();
  await expect.poll(() => host.queryReads.length).toBe(2);
  await expect(transcript(page)).toHaveAttribute("data-history-window", "true");
  await latestMessages(page).click();
  await expect(transcript(page)).not.toHaveAttribute("data-history-window", "true");
  await expect(transcript(page).locator("[data-transcript-row]")).toHaveCount(49);
  const composer = page.locator(".composer textarea");
  await composer.focus();
  held.release();
  await settle(page, 600);
  await expect(composer).toBeFocused();
  await expect(transcript(page)).not.toHaveAttribute("data-history-window", "true");
  await expect(transcript(page)).not.toContainText("Historical native body");
  await expect(transcript(page)).toContainText("Question native 19");
  await expect(page.locator(".history-window-status")).toHaveCount(0);
  expect(host.queryReads.filter(read => read.type === "message-window")).toHaveLength(1);
  expect(host.reads).toHaveLength(0);
});

async function settleEvent(page: Page, live: Gate) {
  let emitted = false;
  await page.route(`**/api/v1/sessions/${sessionId}/events?*`, async route => {
    if (emitted) return route.fulfill({ contentType: "text/event-stream", body: 'retry: 60000\nevent: ready\ndata: {"type":"ready"}\n\n' });
    emitted = true; await live.promise;
    return route.fulfill({ contentType: "text/event-stream", body: 'retry: 60000\nid: 5\nevent: agent_settled\ndata: {"type":"agent_settled","outcome":"completed","runId":"read-only-fixture"}\n\n' });
  });
}
test("native authoritative leaf refresh discards old summary pages rather than mixing branches", async ({ page, host }) => {
  enableNative(host);
  const live = gate(); await settleEvent(page, live);
  try {
    await open(page, 10);
    const messages = users("fork", 2);
    host.initial = { messages, firstId: "fork-0", hasMore: false };
    host.nativeLeaf = "fork-leaf";
    host.summaryPages.push({ data: { ...otherNativePage(), messages: messages.map((message, timestamp) => ({ entryId: message.entryId, role: "user", preview: `Fork question ${timestamp}`, timestamp, hasImages: false })), firstId: "fork-0", lastId: "fork-1", leafId: "fork-leaf" } });
    live.release();
    await expect(nodes(page)).toHaveCount(2);
    await expect(nodes(page).first()).toHaveAttribute("aria-label", /Fork question 0/);
    expect(host.queryReads).toHaveLength(2);
    expect(host.queryReads[1].params.leafId).toBe("fork-leaf");
    await expect(page.locator(".transcript")).not.toContainText("Question native 19");
    expect(host.reads).toHaveLength(0);
  } finally { live.release(); }
});
for (const toolOnly of [false, true]) {
  test(`native live snapshot refresh preserves the historical page with a ${toolOnly ? "tool-only" : "user"} live tail`, async ({ page, host }) => {
    enableNative(host);
    host.windows.push({ data: nativeWindowPage() }, { data: nativeWindowPage(12, 3, false) });
    const live = gate(), summaries = gate();
    await settleEvent(page, live);
    try {
      await open(page, 10); await nodes(page).first().click();
      const main = transcript(page);
      await expect(main).toHaveAttribute("data-history-window", "true");
      await expect(historicalMessage(page, 10)).toBeVisible();
      await historyPages(page).getByRole("button", { name: "Later page", exact: true }).click();
      await expect(historicalMessage(page, 14)).toBeVisible();
      await main.evaluate(element => { element.dataset.testInstance = "pinned-transcript"; });
      host.initial = { messages: toolOnly ? tools("live-tail") : users("native", 21).slice(-1), firstId: "live-tail-cursor", hasMore: true };
      host.nativeLeaf = "append-leaf";
      host.summaryPages.push({ data: { ...nativeSummaryPage(11), leafId: "append-leaf" }, wait: summaries });
      live.release();
      await expect.poll(() => host.queryReads.filter(read => read.type === "message-summaries").length).toBe(2);
      await expect(main).toHaveAttribute("data-history-window", "true");
      await expect(main).toHaveAttribute("data-test-instance", "pinned-transcript");
      await expect(historicalMessage(page, 14)).toBeVisible();
      await expect(main.locator("[data-transcript-row]")).toHaveCount(3);
      await expect(main).not.toContainText(toolOnly ? "src/file-0.ts" : "Question native 20");
      expect(host.queryReads.filter(read => read.type === "message-window")).toHaveLength(2);
      summaries.release();
      await expect(nodes(page)).toHaveCount(10);
      await settle(page);
      await expect(main).toHaveAttribute("data-history-window", "true");
      await expect(main).toHaveAttribute("data-test-instance", "pinned-transcript");
      await expect(historicalMessage(page, 14)).toBeVisible();
      await expect(main.locator("[data-transcript-row]")).toHaveCount(3);
      expect(host.queryReads.filter(read => read.type === "message-window")).toHaveLength(2);
      expect(host.queryReads.filter(read => read.type === "message-window").every(read => read.params.leafId === "native-pinned-leaf")).toBe(true);
      await latestMessages(page).click();
      await expect(main).not.toHaveAttribute("data-history-window", "true");
      await expect(historicalMessage(page, 14)).toHaveCount(0);
      // Settlement notes can keep an empty wrapper; count rendered native cards,
      // not presentation-only placeholders retained by the live reconciler.
      await expect(main.locator(toolOnly ? ".tool-card" : ".message")).toHaveCount(toolOnly ? 48 : 1);
      if (!toolOnly) await expect(main).toContainText("Question native 20");
      await expect(nodes(page).last()).toHaveAttribute("aria-label", /Native question 20/);
      expect(host.reads).toHaveLength(0);
      expect(host.queryReads.filter(read => read.type === "message-window")).toHaveLength(2);
    } finally { live.release(); summaries.release(); }
  });
}
