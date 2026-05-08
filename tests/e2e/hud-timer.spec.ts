import { expect, test } from "@playwright/test";
import { fireEvent, installMocks } from "./_mock";

// Regressions for #481: the HUD pill is a persistent Tauri window
// (hidden/shown, not torn down between sessions), so the elapsed-
// time counter has to anchor to the backend's `startedAtMs` payload
// to reset cleanly across back-to-back recordings. Pre-#481 the
// frontend seeded `recordingStartedAt = Date.now()` at the moment
// the listener saw the event, which (a) drifted by show/emit race
// latency and (b) silently kept the previous session's start time
// whenever the listener missed the new event.
//
// These specs drive the `hud:state` event directly through the
// `__hush_e2e_event_bus` test seam (same path Tauri's `listen`
// uses in the mocked runtime) and assert the rendered elapsed label.

test.describe("HUD timer reset across sessions (#481)", () => {
  test("recording event with startedAtMs anchors the elapsed label", async ({
    page,
  }) => {
    await installMocks(page);
    await page.goto("/hud");

    // Wait for the dismiss button — always in the template — to confirm
    // SvelteKit has bootstrapped and onMount's listen() has registered
    // before we fire the first event.
    await expect(page.locator("button.hud-dismiss")).toBeVisible();
    const elapsed = page.locator('[data-testid="hud-elapsed"]');

    // Seed a recording that "started" exactly 65 seconds ago.
    // The label should round to 1:05 on the very next animation
    // frame regardless of how long the listener took to register.
    const sixtyFiveSecondsAgo = Date.now() - 65_000;
    await fireEvent(page, "hud:state", {
      state: "recording",
      startedAtMs: sixtyFiveSecondsAgo,
    });

    await expect(elapsed).toBeVisible();
    await expect(elapsed).toHaveText(/^1:0[5-7]$/);
  });

  test("second recording event resets the timer to 0:00", async ({ page }) => {
    await installMocks(page);
    await page.goto("/hud");

    // Wait for SvelteKit bootstrap before firing any events.
    await expect(page.locator("button.hud-dismiss")).toBeVisible();
    const elapsed = page.locator('[data-testid="hud-elapsed"]');

    // First session: pretend it started 30s ago.
    await fireEvent(page, "hud:state", {
      state: "recording",
      startedAtMs: Date.now() - 30_000,
    });
    await expect(elapsed).toHaveText(/^0:[23]\d$/);

    // First session ends → Processing freezes the readout.
    await fireEvent(page, "hud:state", { state: "processing" });

    // Second session begins NOW. Timer must reset to 0:00, not
    // continue counting from the previous start. Pre-#481 this
    // was the race-condition repro: the same persistent window
    // kept its old `recordingStartedAt` and the timer drifted
    // forward across sessions.
    await fireEvent(page, "hud:state", {
      state: "recording",
      startedAtMs: Date.now(),
    });

    await expect(elapsed).toHaveText(/^0:0\d$/);
  });

  test("processing event freezes (does not reset) the timer", async ({
    page,
  }) => {
    await installMocks(page);
    await page.goto("/hud");

    await expect(page.locator("button.hud-dismiss")).toBeVisible();
    const elapsed = page.locator('[data-testid="hud-elapsed"]');
    await fireEvent(page, "hud:state", {
      state: "recording",
      startedAtMs: Date.now() - 12_000,
    });
    await expect(elapsed).toHaveText(/^0:1[2-4]$/);

    // Processing transition: the elapsed counter is HIDDEN in
    // processing mode (the markup gates it on hudState ===
    // "recording"), so the assertion is on the absence of the
    // testid — which is the user-visible behaviour: the digits
    // disappear at the same moment the shimmer takes over.
    await fireEvent(page, "hud:state", { state: "processing" });
    await expect(elapsed).toHaveCount(0);
  });
});

// Transcription progress indicator (#566): the label shows "Processing…"
// without a percentage until the first `transcription:progress` event
// arrives, then updates to "Processing… N%". Progress resets between
// recording cycles so back-to-back sessions don't show a stale percentage.
test.describe("HUD transcription progress indicator (#566)", () => {
  async function bootstrap(page: Parameters<typeof installMocks>[0]) {
    await installMocks(page);
    await page.goto("/hud");
    await expect(page.locator("button.hud-dismiss")).toBeVisible();
    // Drive into processing state (the only state where the label is visible).
    await fireEvent(page, "hud:state", {
      state: "recording",
      startedAtMs: Date.now(),
    });
    await fireEvent(page, "hud:state", { state: "processing" });
  }

  test("shows 'Processing…' before any progress event", async ({ page }) => {
    await bootstrap(page);
    await expect(page.locator(".hud-label")).toHaveText("Processing…");
  });

  test("updates label to 'Processing… N%' on transcription:progress event", async ({
    page,
  }) => {
    await bootstrap(page);
    await fireEvent(page, "transcription:progress", 42);
    await expect(page.locator(".hud-label")).toHaveText("Processing… 42%");
  });

  test("label updates as progress increases", async ({ page }) => {
    await bootstrap(page);
    await fireEvent(page, "transcription:progress", 25);
    await expect(page.locator(".hud-label")).toHaveText("Processing… 25%");
    await fireEvent(page, "transcription:progress", 75);
    await expect(page.locator(".hud-label")).toHaveText("Processing… 75%");
    await fireEvent(page, "transcription:progress", 100);
    await expect(page.locator(".hud-label")).toHaveText("Processing… 100%");
  });

  test("progress resets to 'Processing…' on next recording cycle", async ({
    page,
  }) => {
    await bootstrap(page);
    await fireEvent(page, "transcription:progress", 80);
    await expect(page.locator(".hud-label")).toHaveText("Processing… 80%");

    // New recording cycle — progress must clear so the next Processing
    // transition starts clean rather than flashing the previous session's
    // final percentage.
    await fireEvent(page, "hud:state", {
      state: "recording",
      startedAtMs: Date.now(),
    });
    await fireEvent(page, "hud:state", { state: "processing" });
    await expect(page.locator(".hud-label")).toHaveText("Processing…");
  });
});

// Double-click to raise main window (#662): double-clicking the HUD pill
// calls `show_main_window` so the user can surface the Hush app without
// leaving their active document.
test.describe("HUD double-click raises main window", () => {
  test("dblclick on pill body invokes show_main_window", async ({ page }) => {
    let callCount = 0;
    await page.exposeFunction("__hushTestTrackShowMain", () => {
      callCount++;
    });
    await installMocks(page, {
      // Must be an inline literal — no outer-scope variable capture.
      show_main_window: () => {
        (window as unknown as { __hushTestTrackShowMain: () => void }).__hushTestTrackShowMain();
      },
    });
    await page.goto("/hud");
    await expect(page.locator("button.hud-dismiss")).toBeVisible();

    await page.locator(".hud-root").dblclick();

    await expect
      .poll(() => callCount, { timeout: 2000 })
      .toBeGreaterThanOrEqual(1);
  });

  test("dblclick on dismiss button does not invoke show_main_window", async ({
    page,
  }) => {
    let callCount = 0;
    await page.exposeFunction("__hushTestTrackShowMain2", () => {
      callCount++;
    });
    await installMocks(page, {
      show_main_window: () => {
        (window as unknown as { __hushTestTrackShowMain2: () => void }).__hushTestTrackShowMain2();
      },
    });
    await page.goto("/hud");
    await expect(page.locator("button.hud-dismiss")).toBeVisible();

    // Double-click the dismiss button — should NOT bubble to .hud-root.
    await page.locator("button.hud-dismiss").dblclick();

    // Give a short window for any erroneous call to arrive.
    await page.waitForTimeout(300);
    expect(callCount).toBe(0);
  });
});
