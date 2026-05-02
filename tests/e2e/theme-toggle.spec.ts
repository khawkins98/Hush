import { expect, test } from "@playwright/test";
import { installMocks } from "./_mock";

// E2E coverage for the Settings → General → Appearance picker
// (#411 phase A). The picker writes to localStorage and emits a
// Tauri event; the root layout listens and applies a `data-theme`
// attribute on `<html>`. We assert the attribute round-trips
// through the click → emit → listener cycle within the same page,
// since cross-window propagation needs a real Tauri runtime.

test.describe("settings → appearance picker", () => {
  test("System / Light / Dark options flip data-theme on <html>", async ({
    page,
  }) => {
    await installMocks(page);
    // Start fresh — any leftover preference from a previous spec
    // would pre-paint the override and mask the System default.
    await page.addInitScript(() => {
      try {
        localStorage.removeItem("hush.theme");
        document.documentElement.removeAttribute("data-theme");
      } catch {
        // localStorage can throw under some sandbox configs; the
        // spec runs in standard Playwright Chromium so this is
        // defensive.
      }
    });

    await page.goto("/settings");

    // System default — no attribute present.
    await expect(page.locator("html")).not.toHaveAttribute("data-theme", /.*/);

    const systemBtn = page.locator('[data-testid="settings-theme-system"]');
    const lightBtn = page.locator('[data-testid="settings-theme-light"]');
    const darkBtn = page.locator('[data-testid="settings-theme-dark"]');

    await expect(systemBtn).toHaveAttribute("aria-checked", "true");

    await lightBtn.click();
    await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
    await expect(lightBtn).toHaveAttribute("aria-checked", "true");
    await expect(systemBtn).toHaveAttribute("aria-checked", "false");

    await darkBtn.click();
    await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
    await expect(darkBtn).toHaveAttribute("aria-checked", "true");

    await systemBtn.click();
    await expect(page.locator("html")).not.toHaveAttribute("data-theme", /.*/);
    await expect(systemBtn).toHaveAttribute("aria-checked", "true");
  });

  test("preference persists across reload via localStorage", async ({
    page,
  }) => {
    await installMocks(page);
    await page.addInitScript(() => {
      try {
        localStorage.setItem("hush.theme", "dark");
      } catch {
        /* see above */
      }
    });

    await page.goto("/settings");

    // The layout's synchronous applyThemeAttribute call must have
    // fired before children mounted, so the attribute is on
    // <html> by first paint — no flash-of-unstyled-content.
    await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
    await expect(
      page.locator('[data-testid="settings-theme-dark"]'),
    ).toHaveAttribute("aria-checked", "true");
  });
});
