import { expect, test } from "@playwright/test";
import { installMocks } from "./_mock";

test.describe("fixed theme", () => {
  test("ignores stored theme overrides and omits the settings picker", async ({
    page,
  }) => {
    await installMocks(page);
    await page.addInitScript(() => {
      try {
        localStorage.setItem("hush.theme", "dark");
      } catch {
        // localStorage access is best-effort in test environments.
      }
    });

    await page.goto("/");
    await page.locator(`[data-testid="sidebar-nav-settings"]`).click();

    await expect(page.locator("html")).not.toHaveAttribute("data-theme", /.*/);
    await expect(page.locator('[data-testid="settings-theme-row"]')).toHaveCount(0);
  });
});
