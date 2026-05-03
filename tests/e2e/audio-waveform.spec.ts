import { expect, test } from "@playwright/test";
import { installMocks } from "./_mock";

// Smoke coverage for the extracted AudioWaveform component
// (#411 phase B + F1 moods). The component is leaf-level and does
// its own audio:level subscription internally, so the spec
// exercises the two consumers (HUD pill, ControlsSection row) only
// at the "is the markup present where we expect it, with the right
// mood?" level. The inner ring-buffer behaviour stays untested at
// the e2e layer since simulating audio:level events through the
// mocked IPC would just be testing the mock.

test.describe("AudioWaveform — mount points", () => {
  test("HUD page renders the waveform on first paint", async ({ page }) => {
    await installMocks(page);
    await page.goto("/hud");
    // Default hudState is "recording" before any backend event,
    // so the component should be on the page immediately, with
    // mode="recording" wired by the HUD consumer.
    const waveform = page.locator('[data-testid="audio-waveform"]');
    await expect(waveform).toBeVisible();
    await expect(waveform).toHaveAttribute("data-mode", "recording");
    // 14 bars per the component constant — guard against accidental
    // changes by asserting the count.
    await expect(waveform.locator("span")).toHaveCount(14);
  });

  test("main page renders an idle waveform when not recording", async ({ page }) => {
    await installMocks(page);
    await page.goto("/");
    // F1: the waveform is now always mounted on the main page so
    // the breathing idle bars give the page a continuous live
    // feel. Filter out the HUD test fixture if it leaks into the
    // same page tree by anchoring on data-mode.
    const idleWaveform = page.locator(
      '[data-testid="audio-waveform"][data-mode="idle"]',
    );
    await expect(idleWaveform).toBeVisible();
    await expect(idleWaveform.locator("span")).toHaveCount(14);
  });
});
