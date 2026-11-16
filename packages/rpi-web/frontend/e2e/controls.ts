import { expect, type Page } from "@playwright/test";

/** Select an option through the custom menu, just as a browser user would. */
export async function chooseMenuOption(
  page: Page,
  name: string,
  value: string | { label: string },
) {
  const labels: Record<string, string> = {
    en: "English",
    zh: "简体中文",
    high: "high",
    offline: "offline",
  };
  const label =
    typeof value === "object"
      ? value.label
      : value === ""
        ? name === "Filter by tag"
          ? "All tags"
          : "All statuses"
        : (labels[value] ?? value);
  await page.getByRole("button", { name, exact: true }).click();
  await page
    .getByRole("listbox", { name, exact: true })
    .getByRole("option", { name: label, exact: true })
    .click();
}

/**
 * Open the single right-hand runtime panel.
 *
 * The panel owns runtime settings and the read-only diagnostics that used to
 * live in a separate inspector. It is reached from the composer's "details"
 * jump (always present) or from the runtime entry at the top of the model menu.
 */
export async function openRuntimePanel(page: Page) {
  await page.locator(".slim-metrics").getByRole("button").click();
  await expect(page.locator("aside.inspector")).toBeVisible();
}

/**
 * Language lives in Settings; the footer dropdown is gone.
 *
 * The Settings shortcut is registered by the workspace, so the caller must
 * already be past the login screen. `openSettings` waits for that workspace
 * instead of firing the shortcut into a page that cannot answer it.
 */
export async function chooseLanguage(page: Page, value: "en" | "zh") {
  const dialog = await openSettings(page);
  const label = (await dialog
    .getByRole("button", { name: "Language", exact: true })
    .count())
    ? "Language"
    : "语言";
  await chooseMenuOption(page, label, value);
  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog")).toHaveCount(0);
}

/**
 * Open the Settings dialog from the workspace.
 *
 * Settings is a workspace dialog: on the login screen the shortcut is ignored
 * because no dialog exists yet. Waiting for the workspace footer first keeps
 * the shortcut deterministic instead of racing the initial auth/state render.
 */
export async function openSettings(page: Page) {
  await expect(
    page.locator(".workspace, .workspace--immersive").first(),
  ).toBeVisible();
  await page.keyboard.press("Control+,");
  const dialog = page.getByRole("dialog");
  await expect(dialog).toBeVisible();
  return dialog;
}

/** Manual list refresh lives in Settings now. */
export async function refreshChats(page: Page) {
  const dialog = await openSettings(page);
  await dialog.getByRole("button", { name: /Refresh chats|刷新会话/ }).click();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog")).toHaveCount(0);
}

/**
 * Click a sidebar row action (`+`, `…`) the way a user reaches it: move the
 * pointer onto the row first. Hidden row actions carry `pointer-events: none`
 * so an invisible control cannot be clicked by accident; the row hover reveals
 * them, exactly as it does for a real pointer.
 */
export async function clickRowAction(
  page: Page,
  name: string | RegExp,
  options: { exact?: boolean } = { exact: true },
) {
  const button = page.getByRole("button", { name, exact: options.exact });
  await expect(button).toHaveCount(1);
  const box = await button.boundingBox();
  expect(box).not.toBeNull();
  await page.mouse.move(box!.x - 12, box!.y + box!.height / 2);
  await expect(button).toHaveCSS("pointer-events", "auto");
  await button.click();
}
