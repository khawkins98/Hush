import { expect, test } from "@playwright/test";
import { installMocks } from "./_mock";

// Smoke coverage for the F5 technical status line (#411 phase F5).
// The toggle lives in Settings → General → Advanced; the line
// itself renders below the main-window waveform when enabled. The
// data plumbed in (audio source label + model name) is already
// asserted by other specs — these tests pin the toggle wiring and
// the conditional render based on the persisted localStorage flag.

test.describe("StatusLine — F5 toggle wiring", () => {
  test("Settings exposes the toggle inside the Advanced disclosure, off by default", async ({
    page,
  }) => {
    await installMocks(page);
    await page.goto("/settings");

    // Toggle is hidden until Advanced is expanded — mirrors the
    // Performance + first-run patterns already in this tab.
    await expect(
      page.locator('[data-testid="settings-status-line-toggle"]'),
    ).toHaveCount(0);

    await page
      .locator('[data-testid="settings-general-advanced-toggle"]')
      .click();

    const toggle = page.locator('[data-testid="settings-status-line-toggle"]');
    await expect(toggle).toBeVisible();
    await expect(toggle).not.toBeChecked();
  });

  test("main window hides the status line when the localStorage flag is unset", async ({
    page,
  }) => {
    await installMocks(page);
    await page.goto("/");
    // Default boot — no localStorage flag, no status line.
    await expect(
      page.locator('[data-testid="audio-status-line"]'),
    ).toHaveCount(0);
  });

  test("main window renders the status line when the flag is persisted", async ({
    page,
  }) => {
    // Seed the localStorage flag *before* the app boots so the
    // ControlsSection's onMount picks it up. addInitScript runs in
    // every navigation in this page, so the next goto sees the
    // value.
    await installMocks(page);
    await page.addInitScript(() => {
      try {
        window.localStorage.setItem("hush.statusLine", "1");
      } catch {
        // Fine — the test will fail loudly via the visibility
        // assertion below if storage isn't accessible.
      }
    });
    await page.goto("/");
    const line = page.locator('[data-testid="audio-status-line"]');
    await expect(line).toBeVisible();
    // The mock backend ships a default model + a default source;
    // the dash placeholder only appears when both are null. Pin
    // the structural separator to lock the format ("device · model").
    await expect(line).toContainText("·");
  });
});
