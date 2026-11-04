import { expect, test, type Page } from "@playwright/test";

const childId = "tpchild-11111111-1111-4111-8111-111111111111";
const origin = {
  runId: "native-run",
  assistantEntryId: "assistant",
  toolIndex: 0,
};
const summary = (id = childId, rootCallId = "real-parent") => ({
  version: 1,
  childId: id,
  seq: 100,
  origin,
  originVerification: "nativeCheckpoint",
  rootCallId,
  rootToolName: "program",
  toolName: "read",
  ordinal: 0,
  parentChildId: null,
  state: "unknown",
});
const descriptor = {
  descriptorId: "descriptor",
  childId,
  version: 1,
  bodyId: "body",
  kind: "final",
  codec: "native-json-v1",
  encoding: "base64",
  byteLength: 65540,
  chunkBytes: 32768,
  chunks: 3,
  sha256: "a".repeat(64),
};
const phase = {
  entryId: "final-phase",
  seq: 101,
  version: 1,
  childId,
  index: 2,
  phase: "final",
  descriptor,
  isError: false,
  cancelled: false,
  failed: null,
  interrupted: null,
};
const cursor = {
  sessionId: "s",
  lane: "main",
  leafId: "leaf",
  direction: "newestFirst",
  childId: null,
  afterEntryId: childId,
  afterSeq: 100,
};
function snapshot() {
  return {
    sessionId: "s",
    lane: "main",
    page: {
      leafId: "leaf",
      children: [
        summary(),
        summary("foreign-child", "other-parent"),
        { ...summary("wrong-tool-child"), rootToolName: "not-program" },
      ],
      nextCursor: cursor,
      scanExhausted: false,
    },
    phases: [
      {
        leafId: "leaf",
        childId,
        phases: [phase],
        nextCursor: {
          ...cursor,
          childId,
          afterEntryId: "final-phase",
          afterSeq: 101,
        },
        scanned: 1,
        scanExhausted: false,
        state: "unknown",
      },
    ],
    observations: [
      {
        childId,
        latestPhase: "final",
        finalPhase: phase,
        closedPhase: null,
        outcome: "finalObserved",
      },
    ],
    startSeq: 1,
    lastSeq: 5,
    events: [],
    resyncRequired: false,
  };
}
type Json = Record<string, any>;
type Bridge = Window & {
  childReads: { url: string; aborted: boolean }[];
  childStreams: {
    url: string;
    cancelled: boolean;
    controller: ReadableStreamDefaultController<Uint8Array>;
  }[];
  emitChild: (name: string, value: unknown) => void;
  disconnectChild: () => void;
};
async function fixture(page: Page, status = 200, transcriptToolName = "program") {
  const calls: { path: string; body: Json; headers: Record<string, string> }[] =
    [];
  const unexpected: string[] = [];
  await page.addInitScript(() => {
    localStorage.setItem("rpi-web-language", "en");
    // Ordinary transcript SSE is separate; no child events enter it.
    class MockSource extends EventTarget {
      close() {}
      onopen = null;
      onerror = null;
    }
    window.EventSource = MockSource as unknown as typeof EventSource;
    const w = window as unknown as Bridge;
    w.childStreams = [];
    w.childReads = [];
    w.emitChild = (name, value) => {
      const feed = w.childStreams.at(-1)!;
      feed.controller.enqueue(
        new TextEncoder().encode(
          `event: ${name}\nid: 9999\ndata: ${JSON.stringify(value)}\n\n`,
        ),
      );
    };
    w.disconnectChild = () => w.childStreams.at(-1)!.controller.close();
    const original = window.fetch;
    window.fetch = async (input, init) => {
      const url = String(input);
      if (!url.includes("/tool-program/events")) {
        if (url.includes("/tool-program/") && init?.signal) {
          const read = { url, aborted: false };
          w.childReads.push(read);
          init.signal.addEventListener(
            "abort",
            () => {
              read.aborted = true;
            },
            { once: true },
          );
        }
        return original(input, init);
      }
      if (
        init?.method !== "GET" ||
        !init.signal ||
        new Headers(init.headers).get("X-Rpi-Web") !== "1"
      )
        throw new Error("unsafe child stream");
      if (url.includes("?") || new Headers(init.headers).has("Last-Event-ID"))
        throw new Error("foreign replay cursor");
      const feed = {
        url,
        cancelled: false,
        controller:
          null as unknown as ReadableStreamDefaultController<Uint8Array>,
      };
      const stream = new ReadableStream<Uint8Array>({
        start(c) {
          feed.controller = c;
        },
        cancel() {
          feed.cancelled = true;
        },
      });
      init.signal.addEventListener(
        "abort",
        () => {
          feed.cancelled = true;
        },
        { once: true },
      );
      w.childStreams.push(feed);
      return new Response(stream, {
        headers: { "Content-Type": "text/event-stream" },
      });
    };
  });
  await page.route("**/api/v1/**", async (route) => {
    const req = route.request(),
      path = new URL(req.url()).pathname.replace("/api/v1", "");
    const body = req.postDataJSON() ?? {};
    calls.push({ path, body, headers: req.headers() });
    const send = (json: unknown, code = 200) =>
      route.fulfill({ status: code, json });
    if (path === "/auth") return send({ required: false, authenticated: true });
    const reads = ["get_messages", "get_state"];
    if (path === "/capabilities")
      return send({
        protocol: "rpi",
        supportedCommands: reads,
        supportedActions: [],
        parityComplete: false,
      });
    if (path === "/projects")
      return send({
        projects: [{ id: "p", name: "Fixture", path: "fixture" }],
        defaultProjectId: "p",
      });
    const session = {
      sessionId: "s",
      projectId: "p",
      title: "Child fixture",
      status: "ready",
      createdAt: 1,
      updatedAt: 1,
      lastSeq: 77,
      error: null,
    };
    if (path === "/sessions")
      return send({
        sessions: [
          session,
          { ...session, sessionId: "s2", title: "Second fixture" },
        ],
      });
    if (path === "/sessions/s2/open" || path === "/sessions/s2")
      return send({ ...session, sessionId: "s2", title: "Second fixture" });
    if (path === "/sessions/s2/snapshot")
      return send({
        sessionId: "s2",
        status: "ready",
        messages: [
          {
            role: "user",
            content: [{ type: "text", text: "Second session transcript" }],
          },
        ],
        events: [],
        lastSeq: 88,
        hasMore: false,
      });
    if (path === "/sessions/s2/commands" && reads.includes(body.type))
      return send({
        type: "response",
        command: body.type,
        success: true,
        data: {},
      });
    if (path === "/sessions/s2/run-timings/query")
      return send({ timings: [], complete: true, cursor: null });
    if (path === "/sessions/s/open" || path === "/sessions/s")
      return send(session);
    if (path === "/sessions/s/snapshot")
      return send({
        sessionId: "s",
        status: "ready",
        messages: [
          {
            role: "assistant",
            entryId: "assistant",
            content: [
              {
                type: "toolCall",
                id: "real-parent",
                name: transcriptToolName,
                arguments: { code: "not executed" },
              },
            ],
          },
          {
            role: "toolResult",
            toolCallId: "real-parent",
            toolName: transcriptToolName,
            content: [{ type: "text", text: "Ordinary parent output remains" }],
          },
        ],
        total: 2,
        hasMore: false,
        truncated: false,
        events: [],
        lastSeq: 77,
      });
    if (path === "/sessions/s/commands" && reads.includes(body.type))
      return send({
        type: "response",
        command: body.type,
        success: true,
        data: body.type === "get_state" ? {} : { messages: [] },
      });
    if (path === "/sessions/s/run-timings/query")
      return send({ timings: [], complete: true, cursor: null });
    if (path === "/sessions/s/tool-program/snapshot")
      return send(
        status === 200 ? snapshot() : { error: "Fixture rejected" },
        status,
      );
    if (path === "/sessions/s/tool-program/children/query")
      return send({
        leafId: "leaf",
        children: [
          {
            ...summary("older-child"),
            seq: 50,
            originVerification: "forkCopy",
          },
        ],
        nextCursor: null,
        scanExhausted: true,
      });
    if (path === "/sessions/s/tool-program/phases/query")
      return send({
        leafId: "leaf",
        childId: body.childId,
        phases: [],
        nextCursor: null,
        scanned: 0,
        scanExhausted: true,
        state: "unknown",
      });
    if (path === "/sessions/s/tool-program/body/query") {
      const next = !!body.cursor;
      const data = next ? Buffer.from("tail") : Buffer.alloc(65536, 97);
      const chunks = next
        ? [
            {
              index: 2,
              byteOffset: 65536,
              byteLength: 4,
              data: data.toString("base64"),
              sha256: "a".repeat(64),
            },
          ]
        : [0, 1].map((i) => ({
            index: i,
            byteOffset: i * 32768,
            byteLength: 32768,
            data: data.subarray(i * 32768, (i + 1) * 32768).toString("base64"),
            sha256: "a".repeat(64),
          }));
      return send({
        childId,
        phaseEntryId: "final-phase",
        body: {
          descriptor,
          leafId: "leaf",
          byteOffset: next ? 65536 : 0,
          byteLength: data.length,
          totalBytes: 65540,
          chunks,
          nextCursor: null,
        },
        nextCursor: next
          ? null
          : {
              direction: "oldestFirst",
              childId,
              phaseEntryId: "final-phase",
              body: {
                sessionId: "s",
                lane: "main",
                leafId: "leaf",
                descriptorId: "descriptor",
                nextIndex: 2,
              },
            },
      });
    }
    if (path === "/projects/p/git/status")
      return send({ isRepository: false, branch: null, files: [] });
    unexpected.push(`${req.method()} ${path}`);
    return send({ error: "Unexpected fixture request" }, 500);
  });
  await page.goto("/?session=s");
  if (transcriptToolName === "program") {
    await expect(
      page.getByRole("button", { name: "Inspect child calls" }),
    ).toBeVisible();
  }
  return {
    calls,
    unexpected,
    count: (suffix: string) =>
      calls.filter((c) => c.path.endsWith(suffix)).length,
  };
}
async function open(page: Page) {
  await page.getByRole("button", { name: "Inspect child calls" }).click();
  await expect
    .poll(() =>
      page.evaluate(() => (window as unknown as Bridge).childStreams.length),
    )
    .toBe(1);
  await page.evaluate(
    (s) =>
      (window as unknown as Bridge).emitChild("tool_program_child_snapshot", s),
    snapshot(),
  );
}

test("ordinary tools do not advertise child inspection", async ({ page }) => {
  await fixture(page, 200, "read");
  await expect(page.getByRole("button", { name: "Inspect child calls", exact: true })).toHaveCount(0);
});

test("exact parent binding, bounded explicit pages, body fragments and core outcome", async ({
  page,
}) => {
  const f = await fixture(page);
  expect(f.count("tool-program/snapshot")).toBe(0);
  await open(page);
  const window = page.locator('[data-parent-tool-call-id="real-parent"]');
  await expect(window.locator("[data-child-id]")).toHaveCount(1);
  await expect(window.locator("summary").last()).toContainText(
    "Final observed (closure not established)",
  );
  expect(f.count("body/query")).toBe(0);
  const snapshotCall = f.calls.find((c) =>
    c.path.endsWith("tool-program/snapshot"),
  )!;
  expect(snapshotCall.body).toEqual({
    children: {
      leafId: null,
      cursor: null,
      direction: "newestFirst",
      limit: 8,
      maxBytes: 131072,
    },
    phaseLimit: 8,
  });
  await window.locator(".tool-program-child > summary").click();
  expect(f.count("body/query")).toBe(0);
  await window
    .getByRole("button", { name: "Read body page", exact: true })
    .click();
  await expect(window.getByText("Bytes: 0–65536 / 65540")).toBeVisible();
  expect(f.count("body/query")).toBe(1);
  await window.getByRole("button", { name: "Read next body page" }).click();
  await expect(window.locator("pre")).toHaveText("tail");
  const bodyCalls = f.calls.filter((c) => c.path.endsWith("body/query"));
  expect(bodyCalls[0].body).toEqual({
    childId,
    phaseEntryId: "final-phase",
    descriptorId: "descriptor",
    leafId: "leaf",
    cursor: null,
    limit: 2,
    maxBytes: 65536,
  });
  expect(bodyCalls[1].body.cursor).toEqual({
    direction: "oldestFirst",
    childId,
    phaseEntryId: "final-phase",
    body: {
      sessionId: "s",
      lane: "main",
      leafId: "leaf",
      descriptorId: "descriptor",
      nextIndex: 2,
    },
  });
  await window.getByRole("button", { name: "Earlier phases page" }).click();
  await expect(
    window.getByText("No phases in this bounded scan; lifecycle unknown."),
  ).toBeVisible();
  expect(
    f.calls.find((c) => c.path.endsWith("phases/query"))!.body.cursor,
  ).toEqual(snapshot().phases[0].nextCursor);
  await window.getByRole("button", { name: "Earlier children page" }).click();
  await expect(window.locator('[data-child-id="older-child"]')).toBeVisible();
  expect(f.calls.find((c) => c.path.endsWith("children/query"))!.body).toEqual({
    leafId: "leaf",
    cursor,
    direction: "newestFirst",
    limit: 8,
    maxBytes: 32768,
  });
  await window.locator(".tool-program-child > summary").click();
  await expect(
    window.getByText("Historical fork copy — not execution in this session"),
  ).toBeVisible();
  await window.getByRole("button", { name: "Load phases" }).click();
  await expect.poll(() => f.count("phases/query")).toBe(2);
  expect(f.count("body/query")).toBe(2);
  expect(f.unexpected).toEqual([]);
  expect(
    f.calls
      .filter((c) => c.path.endsWith("/commands"))
      .every((c) => ["get_state", "get_messages"].includes(c.body.type)),
  ).toBeTruthy();
});

test("dedicated SSE snapshot wins, native/core sequence domains, resync and disconnect refresh", async ({
  page,
}) => {
  const f = await fixture(page);
  await open(page);
  const updatePhase = {
    ...phase,
    entryId: "closed-phase",
    seq: 102,
    index: 3,
    phase: "closed",
    descriptor: null,
    failed: false,
    interrupted: false,
  };
  const event = {
    type: "tool_program_child_phase",
    schemaVersion: 1,
    sourceSessionId: "s",
    lane: "main",
    entryId: updatePhase.entryId,
    parentEntryId: "final-phase",
    seq: 102,
    customType: "rpi.tool_program.child_phase.v1",
    data: Object.fromEntries(
      Object.entries(updatePhase).filter(
        ([key]) => !["entryId", "seq"].includes(key),
      ),
    ),
  };
  await page.evaluate(
    ({ event, observation }) =>
      (window as unknown as Bridge).emitChild("tool_program_child_update", {
        type: "update",
        seq: 9,
        event,
        observation,
        disposition: "applied",
      }),
    {
      event,
      observation: {
        ...snapshot().observations[0],
        closedPhase: updatePhase,
        latestPhase: "closed",
        outcome: "interrupted",
      },
    },
  );
  // Render the authoritative observation, not a UI-derived success from final+closed.
  await expect(page.locator(".tool-program-child > summary")).toContainText(
    "Interrupted",
  );
  await page.evaluate(() =>
    (window as unknown as Bridge).emitChild("resync_required", {
      type: "resyncRequired",
      seq: 9,
      reason: "branchChanged",
    }),
  );
  await expect.poll(() => f.count("tool-program/snapshot")).toBe(2);
  await expect
    .poll(() =>
      page.evaluate(() => (window as unknown as Bridge).childStreams.length),
    )
    .toBe(2);
  await page.evaluate(
    (s) =>
      (window as unknown as Bridge).emitChild("tool_program_child_snapshot", {
        ...s,
        observations: [{ ...s.observations[0], outcome: "unknown" }],
      }),
    snapshot(),
  );
  await expect(page.locator(".tool-program-child > summary")).toContainText(
    "Unknown",
  );
  await page.evaluate(() => (window as unknown as Bridge).disconnectChild());
  await expect.poll(() => f.count("tool-program/snapshot")).toBe(3);
  await expect
    .poll(() =>
      page.evaluate(() => (window as unknown as Bridge).childStreams.length),
    )
    .toBe(3);
  expect(
    await page.evaluate(() =>
      (window as unknown as Bridge).childStreams
        .slice(0, 2)
        .every((s) => s.cancelled),
    ),
  ).toBeTruthy();
  expect(f.count("body/query")).toBe(0);
  expect(f.unexpected).toEqual([]);
});

for (const status of [401, 501])
  test(`HTTP ${status} is explicit, never empty success or exec`, async ({
    page,
  }) => {
    const f = await fixture(page, status);
    await page.getByRole("button", { name: "Inspect child calls" }).click();
    await expect(
      page.getByText(
        status === 401
          ? "Child inspection failed"
          : "Child inspection unavailable on this backend or native host.",
        { exact: false },
      ),
    ).toBeVisible();
    await expect(
      page.getByText("No children for this exact parent call", {
        exact: false,
      }),
    ).toHaveCount(0);
    expect(f.count("tool-program/snapshot")).toBe(1);
    expect(f.count("body/query")).toBe(0);
    expect(f.unexpected).toEqual([]);
  });

test("reload restores parent and reopening uses a fresh child snapshot", async ({
  page,
}) => {
  const f = await fixture(page);
  await open(page);
  await page.reload();
  await expect(
    page.getByRole("button", { name: "Inspect child calls" }),
  ).toBeVisible();
  expect(f.count("tool-program/snapshot")).toBe(1);
  await open(page);
  expect(f.count("tool-program/snapshot")).toBe(2);
  await expect(page.locator(".tool-program-child > summary")).toContainText(
    "Final observed",
  );
  expect(f.count("body/query")).toBe(0);
});

test("session switch aborts feed and pending body, stale results never cross navigation", async ({
  page,
}) => {
  const f = await fixture(page);
  let release!: () => void;
  const blocked = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.route("**/tool-program/body/query", async (route) => {
    await blocked;
    await route
      .fulfill({ json: { error: "obsolete" }, status: 500 })
      .catch(() => {});
  });
  await open(page);
  await page.locator(".tool-program-child > summary").click();
  await page
    .getByRole("button", { name: "Read body page", exact: true })
    .click();
  await page.getByText("Second fixture", { exact: true }).click();
  await expect(
    page.getByText("Second session transcript", { exact: true }),
  ).toBeVisible();
  await expect
    .poll(() =>
      page.evaluate(
        () => (window as unknown as Bridge).childStreams[0].cancelled,
      ),
    )
    .toBeTruthy();
  expect(
    await page.evaluate(() =>
      (window as unknown as Bridge).childReads
        .filter((r) => r.url.endsWith("body/query"))
        .every((r) => r.aborted),
    ),
  ).toBeTruthy();
  release();
  await expect(page.locator(".tool-program")).toHaveCount(0);
  await expect(page.getByText("obsolete", { exact: false })).toHaveCount(0);
  expect(f.count("tool-program/snapshot")).toBe(1);
  expect(f.unexpected).toEqual([]);
});

test("SSE auth_required stops without silently reconnecting or reporting empty", async ({
  page,
}) => {
  const f = await fixture(page);
  await open(page);
  await page.evaluate(() =>
    (window as unknown as Bridge).emitChild("auth_required", {
      code: "authentication_required",
    }),
  );
  await expect(page.getByRole("alert")).toContainText(
    "authentication required",
  );
  await page.waitForTimeout(1200);
  expect(f.count("tool-program/snapshot")).toBe(1);
  await expect(
    page.getByText("No children for this exact parent call", { exact: false }),
  ).toHaveCount(0);
});

test("foreign cursor is rejected before issuing a rebased query", async ({
  page,
}) => {
  const f = await fixture(page);
  await page.route("**/tool-program/snapshot", (route) =>
    route.fulfill({
      json: {
        ...snapshot(),
        page: {
          ...snapshot().page,
          nextCursor: { ...cursor, sessionId: "foreign-session" },
        },
      },
    }),
  );
  await page.getByRole("button", { name: "Inspect child calls" }).click();
  await expect(
    page.getByText("Invalid child cursor binding", { exact: false }),
  ).toBeVisible();
  expect(f.count("children/query")).toBe(0);
  expect(f.count("body/query")).toBe(0);
});

test("SSE parser handles split UTF-8/CRLF data frames and ignores transport IDs", async ({
  page,
}) => {
  await fixture(page);
  await open(page);
  await page.evaluate((s) => {
    const feed = (window as unknown as Bridge).childStreams.at(-1)!;
    const bytes = new TextEncoder().encode(
      `: keepalive\r\nevent: tool_program_child_snapshot\r\nid: 999999\r\ndata: ${JSON.stringify({ ...s, page: { ...s.page, children: [{ ...s.page.children[0], toolName: "读取" }] } })}\r\n\r\n`,
    );
    for (let index = 0; index < bytes.length; index += 7)
      feed.controller.enqueue(bytes.slice(index, index + 7));
  }, snapshot());
  await expect(page.locator(".tool-program-child > summary")).toContainText(
    "读取",
  );
});

test("empty bounded phase page keeps the opaque continuation, never auto scans", async ({
  page,
}) => {
  const f = await fixture(page);
  const earlierCursor = {
    ...snapshot().phases[0].nextCursor,
    afterEntryId: "unrelated-native-entry",
    afterSeq: 90,
  };
  let phaseReads = 0;
  await page.route("**/tool-program/phases/query", async (route) => {
    phaseReads++;
    const query = route.request().postDataJSON();
    expect(query.cursor).toEqual(
      phaseReads === 1 ? snapshot().phases[0].nextCursor : earlierCursor,
    );
    await route.fulfill({
      json: {
        leafId: "leaf",
        childId,
        phases: [],
        scanned: 128,
        scanExhausted: phaseReads === 2,
        state: "unknown",
        nextCursor: phaseReads === 1 ? earlierCursor : null,
      },
    });
  });
  await open(page);
  await page.locator(".tool-program-child > summary").click();
  await page.getByRole("button", { name: "Earlier phases page" }).click();
  await expect(
    page.getByText("No phases in this bounded scan; lifecycle unknown."),
  ).toBeVisible();
  expect(phaseReads).toBe(1);
  await page.getByRole("button", { name: "Earlier phases page" }).click();
  await expect(
    page.getByText("Phase scan exhausted — not proof of completion."),
  ).toBeVisible();
  expect(phaseReads).toBe(2);
  expect(f.count("body/query")).toBe(0);
  await expect(page.locator(".tool-program-child > summary")).toContainText(
    "Final observed",
  );
});

test("snapshot replay adds bounded metadata but uses the reconciled core observation", async ({
  page,
}) => {
  await fixture(page);
  const replayPhase = {
    ...phase,
    entryId: "closed-phase",
    seq: 102,
    index: 3,
    phase: "closed",
    descriptor: null,
  };
  const event = {
    type: "tool_program_child_phase",
    schemaVersion: 1,
    sourceSessionId: "s",
    lane: "main",
    entryId: "closed-phase",
    parentEntryId: "final-phase",
    seq: 102,
    customType: "rpi.tool_program.child_phase.v1",
    data: Object.fromEntries(
      Object.entries(replayPhase).filter(
        ([key]) => !["entryId", "seq"].includes(key),
      ),
    ),
  };
  await open(page);
  await page.evaluate(
    ({ s, event }) =>
      (window as unknown as Bridge).emitChild("tool_program_child_snapshot", {
        ...s,
        observations: [{ ...s.observations[0], outcome: "failed" }],
        events: [{ seq: 5, disposition: "applied", event }],
      }),
    { s: snapshot(), event },
  );
  await expect(page.locator(".tool-program-child > summary")).toContainText(
    "Failed",
  );
  await page.locator(".tool-program-child > summary").click();
  await expect(page.getByText("Closed #3", { exact: true })).toBeVisible();
});

test("a full phase window refreshes instead of skipping an evicted continuation", async ({
  page,
}) => {
  const f = await fixture(page);
  await open(page);
  const full: Json = snapshot();
  full.page = { ...full.page, nextCursor: null, scanExhausted: true };
  full.phases = [
    {
      ...full.phases[0],
      phases: Array.from({ length: 8 }, (_, index) => ({
        ...phase,
        entryId: `phase-${index}`,
        seq: 100 + index,
        index,
        phase: index === 7 ? "final" : "update",
      })),
      nextCursor: null,
      scanExhausted: true,
    },
  ];
  await page.evaluate(
    (value) =>
      (window as unknown as Bridge).emitChild(
        "tool_program_child_snapshot",
        value,
      ),
    full,
  );
  const appended = { ...phase, entryId: "phase-8", seq: 108, index: 8 };
  const event = {
    type: "tool_program_child_phase",
    schemaVersion: 1,
    sourceSessionId: "s",
    lane: "main",
    entryId: appended.entryId,
    parentEntryId: "phase-7",
    seq: appended.seq,
    customType: "rpi.tool_program.child_phase.v1",
    data: Object.fromEntries(
      Object.entries(appended).filter(([key]) => !["entryId", "seq"].includes(key)),
    ),
  };
  await page.evaluate(
    (value) =>
      (window as unknown as Bridge).emitChild("tool_program_child_update", value),
    { type: "update", seq: 9, event, observation: null, disposition: "applied" },
  );
  await expect(page.getByText("Child feed stopped; refreshing a bounded snapshot…")).toBeVisible();
  await expect.poll(() => f.count("tool-program/snapshot")).toBe(2);
  await expect(page.getByText("More children exist outside this window.")).toBeVisible();
});

test("synthetic transcript row ID is not a native parent toolCallId", async ({
  page,
}) => {
  const f = await fixture(page);
  await page.route("**/sessions/s/snapshot*", (route) =>
    route.fulfill({
      json: {
        sessionId: "s",
        status: "ready",
        messages: [
          {
            role: "toolResult",
            entryId: "real-parent",
            toolName: "program",
            content: [
              { type: "text", text: "Result without native call identity" },
            ],
          },
        ],
        events: [],
        lastSeq: 77,
        hasMore: false,
      },
    }),
  );
  await page.reload();
  await expect(
    page.locator('[data-transcript-row="call-real-parent"]'),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Inspect child calls" }),
  ).toHaveCount(0);
  expect(f.count("tool-program/snapshot")).toBe(0);
});

for (const mode of ["live", "replay"]) {
  test(`${mode} phase newer than pinned snapshot reads body on a fresh leaf, then pins continuation`, async ({
    page,
  }) => {
    const f = await fixture(page);
    const initial: Json = snapshot();
    initial.phases[0].phases = [
      {
        ...phase,
        entryId: "prepared-L0",
        index: 1,
        phase: "prepared",
        descriptor: { ...descriptor, kind: "prepared" },
      },
    ];
    initial.observations = [
      {
        childId,
        latestPhase: "prepared",
        finalPhase: null,
        closedPhase: null,
        outcome: "unknown",
      },
    ];
    await page.route("**/sessions/s/tool-program/snapshot", (route) =>
      route.fulfill({ json: initial }),
    );
    const appended = { ...phase, entryId: "final-L1", seq: 102 };
    const event = {
      type: "tool_program_child_phase",
      schemaVersion: 1,
      sourceSessionId: "s",
      lane: "main",
      entryId: appended.entryId,
      parentEntryId: "prepared-L0",
      seq: appended.seq,
      customType: "rpi.tool_program.child_phase.v1",
      data: Object.fromEntries(
        Object.entries(appended).filter(
          ([key]) => !["entryId", "seq"].includes(key),
        ),
      ),
    };
    const observation = {
      childId,
      latestPhase: "final",
      finalPhase: appended,
      closedPhase: null,
      outcome: "finalObserved",
    };
    const reads: Json[] = [];
    await page.route("**/sessions/s/tool-program/body/query", async (route) => {
      const q = route.request().postDataJSON();
      reads.push(q);
      // Native ancestry rejects F under L0. This fixture deliberately enforces
      // the constraint that the earlier mock did not model.
      if (
        q.phaseEntryId !== appended.entryId ||
        (q.leafId && q.leafId !== "leaf-after-final")
      )
        return route.fulfill({
          status: 409,
          json: { error: "phase not visible under requested leaf" },
        });
      const next = !!q.cursor;
      const data = next ? Buffer.from("tail") : Buffer.alloc(65536, 97);
      const chunks = next
        ? [
            {
              index: 2,
              byteOffset: 65536,
              byteLength: 4,
              data: data.toString("base64"),
              sha256: "a".repeat(64),
            },
          ]
        : [0, 1].map((i) => ({
            index: i,
            byteOffset: i * 32768,
            byteLength: 32768,
            data: data.subarray(i * 32768, (i + 1) * 32768).toString("base64"),
            sha256: "a".repeat(64),
          }));
      return route.fulfill({
        json: {
          childId,
          phaseEntryId: appended.entryId,
          body: {
            descriptor,
            leafId: "leaf-after-final",
            byteOffset: next ? 65536 : 0,
            byteLength: data.length,
            totalBytes: descriptor.byteLength,
            chunks,
            nextCursor: null,
          },
          nextCursor: next
            ? null
            : {
                direction: "oldestFirst",
                childId,
                phaseEntryId: appended.entryId,
                body: {
                  sessionId: "s",
                  lane: "main",
                  leafId: "leaf-after-final",
                  descriptorId: descriptor.descriptorId,
                  nextIndex: 2,
                },
              },
        },
      });
    });
    await page.getByRole("button", { name: "Inspect child calls" }).click();
    await expect
      .poll(() =>
        page.evaluate(() => (window as unknown as Bridge).childStreams.length),
      )
      .toBe(1);
    await page.evaluate(
      ({ initial, mode, event, observation }) => {
        const w = window as unknown as Bridge;
        w.emitChild(
          "tool_program_child_snapshot",
          mode === "replay"
            ? {
                ...initial,
                observations: [observation],
                lastSeq: 7,
                events: [{ seq: 7, event, disposition: "applied" }],
              }
            : initial,
        );
        if (mode === "live")
          w.emitChild("tool_program_child_update", {
            type: "update",
            seq: 7,
            event,
            observation,
            disposition: "applied",
          });
      },
      { initial, mode, event, observation },
    );
    const card = page.locator(`[data-child-id="${childId}"]`);
    await card.locator(":scope > summary").click();
    const final = card
      .locator(".tool-program-phases > li")
      .filter({ hasText: "Final #2" });
    await expect(final).toBeVisible();
    expect(reads).toHaveLength(0);
    await final
      .getByRole("button", { name: "Read body page", exact: true })
      .click();
    await expect(
      final.getByRole("button", { name: "Read next body page" }),
    ).toBeVisible();
    expect(reads[0].leafId).toBeNull();
    expect(reads[0].phaseEntryId).toBe(appended.entryId);
    await final.getByRole("button", { name: "Read next body page" }).click();
    await expect(final.locator("pre")).toHaveText("tail");
    expect(reads[1].leafId).toBe("leaf-after-final");
    expect(reads[1].cursor.body.leafId).toBe("leaf-after-final");
    expect(f.count("phases/query")).toBe(0);
    expect(f.unexpected).toEqual([]);
  });
}
