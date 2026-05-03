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

  test("clicking start emits the hotkey:toggle event", async ({ page }) => {
    // The popover delegates to the main window's listener via
    // the `hotkey:toggle` event (same path the tray uses), rather
    // than invoking `start_dictation` directly. The main window
    // owns the recording state machine; going through the event
    // keeps `ui:recording-state` as the single source of truth.
    //
    // The bus initialises lazily on first `listen()` / `emit()`,
    // so we install via `addInitScript` and poll for the bus to
    // come up before wrapping `fire()` to count emissions —
    // mirroring the same pattern audio-source-picker.spec.ts uses
    // for ui:recording-state.
    await page.addInitScript(() => {
      (
        window as unknown as { __hush_toggle_emit_count: number }
      ).__hush_toggle_emit_count = 0;
      const interval = window.setInterval(() => {
        const bus = (
          window as unknown as {
            __hush_e2e_event_bus?: {
              fire: (n: string, p: unknown) => void;
            };
          }
        ).__hush_e2e_event_bus;
        if (!bus) return;
        const original = bus.fire.bind(bus);
        bus.fire = (name: string, payload: unknown) => {
          if (name === "hotkey:toggle") {
            (
              window as unknown as { __hush_toggle_emit_count: number }
            ).__hush_toggle_emit_count += 1;
          }
          original(name, payload);
        };
        window.clearInterval(interval);
      }, 5);
    });

    await installMocks(page);
    await page.goto("/menu-bar");

    await page.locator('[data-testid="popover-toggle"]').click();

    await expect
      .poll(async () =>
        page.evaluate(
          () =>
            (window as unknown as { __hush_toggle_emit_count: number })
              .__hush_toggle_emit_count,
        ),
      )
      .toBe(1);
  });
});
