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

// Estimated transcription progress. whisper.cpp only reports progress
// per 30 s window, so the backend emits one `transcription:estimate`
// (`{ audioMs, expectedMs }`) and the HUD animates locally from it:
// "Processing…" until the estimate, then a shimmer for short runs or an
// advancing bar for long ones, and "Copied!" only once the text is ready.
// whisper's own `transcription:progress` is a floor, never a number.
test.describe("HUD estimated transcription progress", () => {
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

  const progressOf = (page: Parameters<typeof installMocks>[0]) =>
    page
      .locator('[data-testid="hud-progress"]')
      .getAttribute("data-progress")
      .then((v) => Number(v));

  test("shows 'Processing…' with a shimmer before the estimate", async ({ page }) => {
    await bootstrap(page);
    await expect(page.locator(".hud-label")).toHaveText("Processing…");
    await expect(page.locator('[data-testid="hud-shimmer"]')).toBeVisible();
  });

  test("short run: plain 'Transcribing…' and a shimmer, no bar", async ({ page }) => {
    await bootstrap(page);
    await fireEvent(page, "transcription:estimate", { audioMs: 4000, expectedMs: 900 });
    await expect(page.locator(".hud-label")).toHaveText("Transcribing…");
    await expect(page.locator('[data-testid="hud-shimmer"]')).toBeVisible();
    await expect(page.locator('[data-testid="hud-progress"]')).toHaveCount(0);
  });

  test("long run: audio-length copy and a bar that advances but stays short of full", async ({
    page,
  }) => {
    await bootstrap(page);
    await fireEvent(page, "transcription:estimate", { audioMs: 42_000, expectedMs: 4000 });
    await expect(page.locator(".hud-label")).toHaveText("Transcribing 42 s of audio…");
    await expect(page.locator('[data-testid="hud-shimmer"]')).toHaveCount(0);
    await expect(page.locator('[data-testid="hud-progress"]')).toBeVisible();

    await expect.poll(() => progressOf(page)).toBeGreaterThan(0.1);
    const early = await progressOf(page);
    await expect.poll(() => progressOf(page)).toBeGreaterThan(early);
    // whisper's own 100 is only a floor — the bar never fills before
    // the text is actually ready.
    await fireEvent(page, "transcription:progress", 100);
    await expect.poll(() => progressOf(page)).toBeGreaterThanOrEqual(0.95);
    expect(await progressOf(page)).toBeLessThan(1);
  });

  test("done: snaps to 'Copied!' and drops the bar", async ({ page }) => {
    await bootstrap(page);
    await fireEvent(page, "transcription:estimate", { audioMs: 42_000, expectedMs: 4000 });
    await expect(page.locator('[data-testid="hud-progress"]')).toBeVisible();
    await fireEvent(page, "hud:state", { state: "done" });
    await expect(page.locator(".hud-label")).toHaveText("Copied!");
    await expect(page.locator(".hud-done-check")).toBeVisible();
    await expect(page.locator('[data-testid="hud-progress"]')).toHaveCount(0);
  });

  test("estimate resets on the next recording cycle", async ({ page }) => {
    await bootstrap(page);
    await fireEvent(page, "transcription:estimate", { audioMs: 42_000, expectedMs: 4000 });
    await expect(page.locator(".hud-label")).toHaveText("Transcribing 42 s of audio…");

    // A new cycle must start clean rather than flashing the previous
    // session's estimate on the next Processing transition.
    await fireEvent(page, "hud:state", {
      state: "recording",
      startedAtMs: Date.now(),
    });
    await fireEvent(page, "hud:state", { state: "processing" });
    await expect(page.locator(".hud-label")).toHaveText("Processing…");
    await expect(page.locator('[data-testid="hud-progress"]')).toHaveCount(0);
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
      show_main_window: (args: unknown) => {
        // Routed to the Transcribe screen, where the live recording is.
        if ((args as { section?: string } | undefined)?.section !== "dictation") return;
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
