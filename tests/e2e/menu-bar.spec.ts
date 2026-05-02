import { expect, test } from "@playwright/test";
import { installMocks } from "./_mock";

// Smoke coverage for the menu-bar quick-access popover (#427
// Item 1). The popover lives in a separate Tauri window in
// production, summoned from the tray menu. Playwright reaches
// it as a sibling SvelteKit route so we can validate the UI
// end-to-end without a real Tauri runtime — the tray-handler
// integration is dev-launch-smoke territory and lives outside
// CI's reach.

test.describe("menu-bar popover", () => {
  test("renders Ready state with start button when not recording", async ({
    page,
  }) => {
    await installMocks(page);
    await page.goto("/menu-bar");

    const root = page.locator('[data-testid="menu-bar-root"]');
    await expect(root).toBeVisible();

    // State indicator + label default to Ready.
    await expect(page.locator(".state-label")).toHaveText("Ready");

    // Primary action is the start button.
    const toggle = page.locator('[data-testid="popover-toggle"]');
    await expect(toggle).toContainText(/start dictation/i);
    await expect(toggle).not.toBeDisabled();

    // Open Hush link is present.
    await expect(
      page.locator('[data-testid="popover-open-main"]'),
    ).toBeVisible();
  });

  test("clicking start invokes start_dictation IPC", async ({ page }) => {
    let startCalls = 0;
    await page.exposeFunction("__hush_record_start_dictation", () => {
      startCalls += 1;
    });
    await installMocks(page, {
      start_dictation: () => {
        (
          window as unknown as {
            __hush_record_start_dictation: () => void;
          }
        ).__hush_record_start_dictation();
        return undefined;
      },
    });
    await page.goto("/menu-bar");

    await page.locator('[data-testid="popover-toggle"]').click();
    // The popover doesn't optimistically flip its `recording`
    // state — it waits for the `ui:recording-state` broadcast
    // from elsewhere. So we assert on the IPC call, not the
    // visible-state change.
    await expect.poll(() => startCalls).toBe(1);
  });
});
