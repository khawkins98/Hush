import { expect, test } from "@playwright/test";
import { installMocks } from "./_mock";

// Smoke coverage for the extracted AudioWaveform component
// (#411 phase B). The component is leaf-level and does its own
// audio:level subscription internally, so the spec exercises the
// two consumers (HUD pill, ControlsSection recording row) only at
// the "is the markup present where we expect it?" level. The
// inner ring-buffer behaviour stays untested at the e2e layer
// since simulating audio:level events through the mocked IPC
// would just be testing the mock.

test.describe("AudioWaveform — mount points", () => {
  test("HUD page renders the waveform on first paint", async ({ page }) => {
    await installMocks(page);
    await page.goto("/hud");
    // Default hudState is "recording" before any backend event,
    // so the component should be on the page immediately.
    const waveform = page.locator('[data-testid="audio-waveform"]');
    await expect(waveform).toBeVisible();
    // 14 bars per the component constant — guard against accidental
    // changes by asserting the count.
    await expect(waveform.locator("span")).toHaveCount(14);
  });

  test("main page hides the waveform when not recording", async ({ page }) => {
    await installMocks(page);
    await page.goto("/");
    // Idle state — no Record button engaged. The waveform sits
    // inside the recording-status row's `{#if recording}` guard.
    const waveform = page.locator(
      '[data-testid="audio-waveform"]:not(:has(*[data-tauri-drag-region]))',
    );
    await expect(waveform).toHaveCount(0);
  });
});
