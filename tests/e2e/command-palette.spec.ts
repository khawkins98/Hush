import { expect, test } from "@playwright/test";
import { installMocks } from "./_mock";

// Smoke coverage for the ⌘K command palette (#411 phase F3). The
// palette is a frontend-only leaf — every action's `run` is wired
// in the page, so this spec exercises:
//   1. The keyboard binding opens the palette.
//   2. Esc closes it.
//   3. The action list mirrors the page state (start enabled when
//      idle, stop disabled while idle).
//   4. Substring filtering works.
//   5. Clicking an action runs it (Settings: General → opens
//      settings — covered via the existing open_settings mock).

test.describe("CommandPalette — F3 ⌘K", () => {
  test("⌘K opens the palette and Esc closes it", async ({ page }) => {
    await installMocks(page);
    await page.goto("/");

    // Click the app body so the page has focus before the
    // keyboard event — Playwright won't deliver window-level
    // keystrokes to a brand-new tab without something to focus
    // first.
    await page.locator("header.app-bar").click();

    const palette = page.locator('[data-testid="command-palette"]');
    await expect(palette).toHaveCount(0);

    // Cmd+K on macOS, Ctrl+K elsewhere — Playwright's
    // ControlOrMeta resolves to the platform's modifier.
    await page.keyboard.press("ControlOrMeta+k");
    await expect(palette).toBeVisible();

    await page.keyboard.press("Escape");
    await expect(palette).toHaveCount(0);
  });

  test("input is autofocused on open and filter narrows the list", async ({
    page,
  }) => {
    await installMocks(page);
    await page.goto("/");

    // Click the app-bar (non-interactive) so the page has focus,
    // then send the keystroke. Clicking body.click() would hit
    // the Record button at the centre of the dictation section
    // and flip the page into recording state, skewing the
    // palette's enabled-action set.
    await page.locator("header.app-bar").click();
    await page.keyboard.press("ControlOrMeta+k");
    const input = page.locator('[data-testid="command-palette-input"]');
    await expect(input).toBeFocused();

    // Default action set has Start dictation, Stop dictation,
    // Show History, plus a Settings group of seven entries.
    const allRows = page.locator('[data-testid="command-palette-row"]');
    const initialCount = await allRows.count();
    expect(initialCount).toBeGreaterThan(5);

    await input.fill("permissions");
    await expect(allRows).toHaveCount(1);
    await expect(allRows.first()).toContainText(/Permissions/i);
  });

  test("Stop dictation is disabled while idle", async ({ page }) => {
    await installMocks(page);
    await page.goto("/");

    // Click the app-bar (non-interactive) so the page has focus,
    // then send the keystroke. Clicking body.click() would hit
    // the Record button at the centre of the dictation section
    // and flip the page into recording state, skewing the
    // palette's enabled-action set.
    await page.locator("header.app-bar").click();
    await page.keyboard.press("ControlOrMeta+k");
    const stopRow = page.locator(
      '[data-testid="command-palette-row"][data-action-id="dictation.stop"]',
    );
    await expect(stopRow).toBeVisible();
    await expect(stopRow).toHaveAttribute("aria-disabled", "true");
  });

  test("clicking a Settings row dispatches open_settings", async ({ page }) => {
    let openCalls = 0;
    await page.exposeFunction("__incrementOpenSettings", () => {
      openCalls += 1;
    });
    await installMocks(page, {
      // Override the mock for open_settings to bump the counter.
      // The default mock's other settings IPCs still apply via the
      // base mock; this one mirrors the open_settings shape.
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      open_settings: () => {
        // The mock function is serialized via toString(), so we
        // call into the page-level exposed binding.
        // @ts-expect-error — exposed function on window
        return window.__incrementOpenSettings();
      },
    });
    await page.goto("/");

    // Click the app-bar (non-interactive) so the page has focus,
    // then send the keystroke. Clicking body.click() would hit
    // the Record button at the centre of the dictation section
    // and flip the page into recording state, skewing the
    // palette's enabled-action set.
    await page.locator("header.app-bar").click();
    await page.keyboard.press("ControlOrMeta+k");
    await page
      .locator(
        '[data-testid="command-palette-row"][data-action-id="settings.general"]',
      )
      .click();

    // Palette closes after run, and open_settings was invoked.
    await expect(
      page.locator('[data-testid="command-palette"]'),
    ).toHaveCount(0);
    // Give the async run a tick.
    await page.waitForFunction(() => true);
    expect(openCalls).toBeGreaterThanOrEqual(1);
  });
});
