import { expect, test } from "@playwright/test";
import { installMocks } from "./_mock";

// E2E coverage for the standalone Settings window
// (`src/routes/settings/+page.svelte`). The window is a sibling
// route on the same Vite dev server, so Playwright reaches it via
// `page.goto("/settings")` — no IPC `open_settings` round-trip
// needed. Specs that exercise inter-window flows (deep-link from
// the main window to a Settings tab) live in `meeting-panel` /
// `error-states` style files when those flows land.

test.describe("settings window — toolbar nav", () => {
  test("renders all six tabs and lands on General by default", async ({
    page,
  }) => {
    await installMocks(page);
    await page.goto("/settings");

    // Toolbar tabs use stable testIds, so the spec is robust to
    // label copy changes — the test asserts the tabs exist + the
    // active one is General without locking the visible text.
    for (const key of [
      "general",
      "model",
      "vocabulary",
      "replacements",
      "permissions",
      "about",
    ]) {
      await expect(
        page.locator(`[data-testid="settings-tab-${key}"]`),
      ).toBeVisible();
    }

    const general = page.locator('[data-testid="settings-tab-general"]');
    await expect(general).toHaveAttribute("aria-current", "page");
  });

  test("clicking a tab makes it active + reveals its body", async ({
    page,
  }) => {
    await installMocks(page);
    await page.goto("/settings");

    await page.locator('[data-testid="settings-tab-vocabulary"]').click();
    await expect(
      page.locator('[data-testid="settings-tab-vocabulary"]'),
    ).toHaveAttribute("aria-current", "page");
    await expect(page.locator("section.panel-vocabulary")).toBeVisible();

    await page.locator('[data-testid="settings-tab-replacements"]').click();
    await expect(
      page.locator('[data-testid="settings-tab-replacements"]'),
    ).toHaveAttribute("aria-current", "page");
    await expect(page.locator("section.panel-replacements")).toBeVisible();
  });

  test("settings:goto-tab event flips the active tab (deep-link)", async ({
    page,
  }) => {
    // The main window's "Open the Permissions diagnostic" button
    // sequences `invoke('open_settings')` then `emit('settings:goto-tab', 'permissions')`
    // — verify the listener side picks up the event and switches tabs.
    await installMocks(page);
    await page.goto("/settings");

    // Wait for onMount to register the `settings:goto-tab`
    // listener. The General tab being marked aria-current is a
    // proxy for "page mounted, listener attached" since the
    // listener registration runs in the same onMount that does
    // initial loaders. Without this wait the fire below races.
    await expect(
      page.locator('[data-testid="settings-tab-general"]'),
    ).toHaveAttribute("aria-current", "page");

    await page.evaluate(() => {
      const bus = (
        window as unknown as {
          __hush_e2e_event_bus?: { fire: (n: string, p: unknown) => void };
        }
      ).__hush_e2e_event_bus;
      bus?.fire("settings:goto-tab", "permissions");
    });

    await expect(
      page.locator('[data-testid="settings-tab-permissions"]'),
    ).toHaveAttribute("aria-current", "page");
  });
});

test.describe("settings window — General tab", () => {
  test("autostart toggle reflects the plugin's reported state", async ({
    page,
  }) => {
    // Default mock has the autostart plugin returning false; the
    // checkbox should mount unchecked.
    await installMocks(page);
    await page.goto("/settings");

    const toggle = page.locator('[data-testid="settings-autostart-toggle"]');
    await expect(toggle).toBeVisible();
    await expect(toggle).not.toBeChecked();
  });

  test("autostart toggle starts checked when the plugin reports enabled", async ({
    page,
  }) => {
    // Override the autostart plugin's `is_enabled` to return true —
    // the checkbox must mount checked.
    await installMocks(page, {
      "plugin:autostart|is_enabled": () => true,
    });
    await page.goto("/settings");

    const toggle = page.locator('[data-testid="settings-autostart-toggle"]');
    await expect(toggle).toBeChecked();
  });

  test("first-run reset button shows confirmation copy after click", async ({
    page,
  }) => {
    await installMocks(page);
    await page.goto("/settings");

    const button = page.locator('[data-testid="settings-reset-first-run"]');
    await expect(button).toContainText(/Show welcome on next launch/i);
    await button.click();
    // The component swaps the label to the confirmation message
    // for ~3 s before reverting. Asserting the copy here pins the
    // success path without waiting on the timer.
    await expect(button).toContainText(/Welcome will show on next launch/i);
  });
});

test.describe("settings window — PTT editor", () => {
  test("renders the persisted combo as kbd chips and the enable toggle", async ({
    page,
  }) => {
    await installMocks(page);
    await page.goto("/settings");

    // The default mock returns `combo: ["RightMeta"], enabled: false`.
    const display = page.locator('[data-testid="ptt-combo-display"]');
    await expect(display).toBeVisible();
    await expect(display.locator("kbd")).toHaveCount(1);

    const enable = page.locator(
      '[data-testid="ptt-enabled-toggle"] input[type="checkbox"]',
    );
    await expect(enable).toBeVisible();
    await expect(enable).not.toBeChecked();
  });

  test("multi-key combos render one kbd chip per key", async ({ page }) => {
    await installMocks(page, {
      ptt_get_config: () => ({
        combo: ["RightMeta", "RightShift"],
        enabled: true,
        listenerRunning: true,
      }),
    });
    await page.goto("/settings");

    const display = page.locator('[data-testid="ptt-combo-display"]');
    await expect(display.locator("kbd")).toHaveCount(2);

    const enable = page.locator(
      '[data-testid="ptt-enabled-toggle"] input[type="checkbox"]',
    );
    await expect(enable).toBeChecked();
  });

  test("Record-new-combo button enters capture mode", async ({ page }) => {
    await installMocks(page);
    await page.goto("/settings");

    const record = page.locator('[data-testid="ptt-record-button"]');
    await expect(record).toBeVisible();
    await record.click();

    // In capture mode, the prompt copy appears and the record
    // button is replaced with Save / Cancel actions.
    await expect(page.locator("text=Press your combo")).toBeVisible();
    await expect(record).toHaveCount(0);
    await expect(page.getByRole("button", { name: /Cancel/i })).toBeVisible();
  });
});
