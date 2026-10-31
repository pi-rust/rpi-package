import { test, expect } from "@playwright/test";
import { createHash } from "node:crypto";

const lightVariantHash =
  "67f5fd2e2791443e3dace12dc02219a387248f4948e2bd840bf34c88bba36744";
for (const width of [1280, 390]) {
  for (const authenticated of [false, true]) {
    test(`official rpi logo ${authenticated ? "workspace" : "login"} at ${width}px`, async ({
      page,
    }) => {
      await page.setViewportSize({ width, height: 900 });
      await page.addInitScript(() =>
        localStorage.setItem("rpi-web-language", "en"),
      );
      const summary = {
        sessionId: "branding",
        projectId: "logo",
        title: "Logo fixture",
        status: "ready",
        tags: [],
        createdAt: 1700000000000,
        updatedAt: 1700000000000,
        lastSeq: 0,
        error: null,
        latestSettlement: null,
      };
      const errors: string[] = [];
      page.on("pageerror", (error) => errors.push(error.message));
      await page.route("**/api/**", async (route) => {
        const path = new URL(route.request().url()).pathname;
        if (path.endsWith("/commands")) {
          const type = route.request().postDataJSON().type;
          expect([
            "get_state",
            "get_stats",
            "get_history",
            "get_queue",
          ]).toContain(type);
          return route.fulfill({
            json: {
              type: "response",
              command: type,
              success: true,
              data:
                type === "get_state"
                  ? { stats: {}, activeOperation: null, queue: {} }
                  : type === "get_queue"
                    ? { items: [], count: 0 }
                    : {},
            },
          });
        }
        if (path.endsWith("/events"))
          return route.fulfill({
            contentType: "text/event-stream",
            body: 'retry: 60000\nevent: ready\ndata: {"type":"ready"}\n\n',
          });
        const json =
          path === "/api/v1/auth"
            ? { required: true, authenticated }
            : path === "/api/v1/projects"
              ? {
                  projects: [
                    { id: "logo", name: "Logo", path: "/workspace/logo" },
                  ],
                  projectsRoot: "/workspace",
                  defaultProjectId: "logo",
                }
              : path === "/api/v1/sessions"
                ? { sessions: [summary] }
                : path.endsWith("/open")
                  ? summary
                  : path.endsWith("/snapshot")
                    ? {
                        sessionId: "branding",
                        messages: [
                          {
                            role: "assistant",
                            entryId: "logo-message",
                            content: [{ type: "text", text: "Brand preview" }],
                          },
                        ],
                        events: [],
                        lastSeq: 0,
                        hasMore: false,
                        pendingResume: false,
                      }
                    : path.endsWith("/run-timings/query")
                      ? { timings: [], complete: true }
                      : path === "/api/v1/capabilities"
                        ? {
                            protocol: "rpi",
                            supportedCommands: [
                              "get_state",
                              "get_stats",
                              "get_history",
                              "get_queue",
                            ],
                            supportedActions: ["projects"],
                          }
                        : {};
        await route.fulfill({ json });
      });
      await page.goto("/");
      const selector = authenticated
        ? ".sidebar-brand .rpi-logo"
        : ".login-header .rpi-logo";
      if (authenticated && width === 390) {
        await page
          .getByRole("button", { name: "Toggle sidebar", exact: true })
          .click();
      }
      await expect(page.locator(selector)).toBeVisible();
      await expect
        .poll(() =>
          page
            .locator(selector)
            .evaluate((el) => (el as HTMLImageElement).naturalWidth),
        )
        .toBeGreaterThan(0);
      const src = await page.locator(selector).getAttribute("src");
      expect(src).toBe("/favicon.svg");
      const response = await page.request.get(src!);
      expect(
        createHash("sha256")
          .update((await response.text()).replace(/\r\n/g, "\n"))
          .digest("hex"),
      ).toBe(lightVariantHash);
      expect(await page.locator('link[rel="icon"]').getAttribute("href")).toBe(
        src,
      );
      if (!authenticated)
        await expect(page.locator(".login-mark .rpi-logo")).toBeVisible();
      expect(
        await page.evaluate(
          () => document.documentElement.scrollWidth <= innerWidth,
        ),
      ).toBe(true);
      expect(errors).toEqual([]);
    });
  }
}
