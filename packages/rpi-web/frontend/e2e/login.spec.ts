import { test, expect } from "@playwright/test";

test("login supports retry, password reveal, pending state and successful access", async ({
  page,
}) => {
  let authenticated = false;
  let finishLogin: (() => void) | undefined;
  await page.route("**/api/v1/**", async (route) => {
    const path = new URL(route.request().url()).pathname;
    if (path === "/api/v1/auth") {
      if (route.request().method() === "GET")
        return route.fulfill({ json: { required: true, authenticated } });
      if (route.request().postDataJSON().password === "wrong")
        return route.fulfill({
          status: 401,
          json: { error: "authentication failed" },
        });
      await new Promise<void>((resolve) => {
        finishLogin = resolve;
      });
      authenticated = true;
      return route.fulfill({ json: { authenticated: true } });
    }
    if (path === "/api/v1/capabilities")
      return route.fulfill({
        json: { supportedCommands: [], supportedActions: [] },
      });
    if (path === "/api/v1/projects")
      return route.fulfill({ json: { projects: [], defaultProjectId: "" } });
    return route.fulfill({ json: { sessions: [] } });
  });
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.goto("/");
  const password = page.getByLabel("Workspace password", { exact: true });
  const submit = page.getByRole("button", { name: "Sign in", exact: true });
  await expect(submit).toBeDisabled();
  await expect(password).toBeFocused();
  await password.fill("wrong");
  await page.getByRole("button", { name: "Show password" }).click();
  await expect(password).toHaveAttribute("type", "text");
  await page.getByRole("button", { name: "Hide password" }).click();
  await expect(password).toHaveAttribute("type", "password");
  await password.press("Enter");
  await expect(page.getByRole("alert")).toContainText("Check your password");
  await expect(password).toHaveValue("wrong");
  await expect(password).toHaveAttribute("aria-invalid", "true");
  await password.fill("test-only-password");
  await expect(page.getByRole("alert")).toHaveCount(0);
  await submit.click();
  await expect(
    page.getByRole("button", { name: "Signing in…" }),
  ).toBeDisabled();
  await expect(password).toBeDisabled();
  await expect.poll(() => !!finishLogin).toBe(true);
  finishLogin!();
  await expect(page.locator(".workspace")).toBeVisible();
  expect(errors).toEqual([]);
});

test("mobile login localizes errors and preserves appearance preferences", async ({
  page,
}) => {
  await page.setViewportSize({ width: 375, height: 667 });
  await page.route("**/api/v1/auth", (route) =>
    route.request().method() === "GET"
      ? route.fulfill({ json: { required: true, authenticated: false } })
      : route.fulfill({
          status: 401,
          json: { error: "authentication failed" },
        }),
  );
  await page.goto("/");
  await page.getByRole("combobox", { name: "Language" }).selectOption("zh");
  await page.getByRole("button", { name: "切换到深色模式" }).click();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await page
    .getByLabel("工作区密码", { exact: true })
    .fill("test-only-password");
  await page.getByRole("button", { name: "登录", exact: true }).click();
  await expect(page.getByRole("alert")).toContainText("请检查密码");
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
  ).toBe(true);
  await page.reload();
  await expect(page.getByRole("heading", { name: "登录 rpi" })).toBeVisible();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await expect(page.getByLabel("工作区密码", { exact: true })).toHaveValue("");
});
