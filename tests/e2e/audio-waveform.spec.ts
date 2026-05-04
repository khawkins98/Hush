import { expect, test } from "@playwright/test";
import { fireEvent, installMocks } from "./_mock";

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
    // hudState starts as null so the AudioWaveform only mounts after
    // the first genuine hud:state = recording event (this prevents
    // WebKit from throttling rAF while the window is still hidden).
    // Wait for the dismiss button — always in the template — to
    // confirm SvelteKit has bootstrapped and onMount has registered
    // the event listener before we fire the first event.
    await expect(page.locator("button.hud-dismiss")).toBeVisible();
    await fireEvent(page, "hud:state", {
      state: "recording",
      startedAtMs: Date.now(),
    });
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
    // F4: the main-page consumer opts in to metering. The peak-
    // hold marker only paints once a real level sample lands, so
    // we don't assert its presence here — just that the wrapper
    // is in metering mode so a future regression that drops the
    // prop fails this spec instead of a manual smoke.
    await expect(idleWaveform).toHaveAttribute("data-metering", "on");
  });

  test("HUD page is not in metering mode", async ({ page }) => {
    await installMocks(page);
    await page.goto("/hud");
    // F4: the HUD pill stays compact — peak-hold + clip warning
    // would over-decorate the menu-bar overlay. Lock that in so a
    // future "wire metering everywhere" change has to revisit
    // this trade-off explicitly.
    await expect(page.locator("button.hud-dismiss")).toBeVisible();
    await fireEvent(page, "hud:state", {
      state: "recording",
      startedAtMs: Date.now(),
    });
    const waveform = page.locator('[data-testid="audio-waveform"]');
    await expect(waveform).toBeVisible();
    await expect(waveform).not.toHaveAttribute("data-metering", "on");
  });
});
