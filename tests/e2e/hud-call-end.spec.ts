import { expect, test } from "@playwright/test";
import { fireEvent, installMocks } from "./_mock";

// Tests for the HUD call-may-have-ended prompt.
// Drives `meeting:call-may-have-ended` (what the call-end detector really
// emits — the HUD used to listen only for a `hud:state` form the backend
// never sent) and `meeting:call-end-cancelled` through the test seam.

// Helper: wait for the HUD page to finish mounting (dismiss button visible =
// all `listen()` calls in onMount have fired, initialising the event bus).
// The prompt only interrupts a live *meeting* recording, so every test
// starts from one.
async function waitForHudReady(page: Parameters<typeof fireEvent>[0]) {
  await page.locator("button.hud-dismiss").waitFor({ state: "visible" });
  await fireEvent(page, "hud:state", {
    state: "recording",
    kind: "meeting",
    startedAtMs: Date.now(),
  });
}

test.describe("HUD call-may-have-ended prompt", () => {
  test("shows high-confidence call-ended prompt", async ({ page }) => {
    await installMocks(page);
    await page.goto("/hud");
    await waitForHudReady(page);

    await fireEvent(page, "meeting:call-may-have-ended", {
      confidence: "high",
      signalSummary: "mic inactive + system audio quiet",
    });

    await expect(page.getByText("Call ended?")).toBeVisible();
    await expect(page.getByRole("button", { name: "Stop", exact: true })).toBeVisible();
    await expect(page.getByRole("button", { name: "Keep recording" })).toBeVisible();
  });

  test("shows medium-confidence call-ended prompt with softer copy", async ({ page }) => {
    await installMocks(page);
    await page.goto("/hud");
    await waitForHudReady(page);

    await fireEvent(page, "meeting:call-may-have-ended", {
      confidence: "medium",
      signalSummary: "mic inactive + system audio quiet",
    });

    await expect(page.getByText("Call winding down?")).toBeVisible();
    await expect(page.getByRole("button", { name: "Stop", exact: true })).toBeVisible();
    await expect(page.getByRole("button", { name: "Keep recording" })).toBeVisible();
  });

  test("Keep recording returns to recording state and hides prompt", async ({ page }) => {
    await installMocks(page);
    await page.goto("/hud");
    await waitForHudReady(page);

    await fireEvent(page, "hud:state", {
      state: "recording",
      kind: "meeting",
      startedAtMs: Date.now(),
    });
    await fireEvent(page, "meeting:call-may-have-ended", {
      confidence: "high",
      signalSummary: "mic inactive + system audio quiet",
    });

    await page.getByRole("button", { name: "Keep recording" }).click();

    await expect(page.getByText("Call ended?")).not.toBeVisible();
    // Stop recording button should be back
    await expect(page.getByRole("button", { name: "Stop recording" })).toBeVisible();
  });

  test("Stop calls meeting_stop_manual", async ({ page }) => {
    let stopped = false;
    await page.exposeFunction("__hush_on_call_end_stop", () => {
      stopped = true;
    });
    await installMocks(page, {
      meeting_stop_manual: () => {
        (window as unknown as { __hush_on_call_end_stop: () => void }).__hush_on_call_end_stop();
      },
    });
    await page.goto("/hud");
    await waitForHudReady(page);

    await fireEvent(page, "meeting:call-may-have-ended", {
      confidence: "high",
      signalSummary: "mic inactive + system audio quiet",
    });

    await page.getByRole("button", { name: "Stop", exact: true }).click();

    await expect(async () => {
      expect(stopped).toBe(true);
    }).toPass({ timeout: 2000 });
  });

  test("CallEndCancelled event dismisses the prompt and restores recording state", async ({ page }) => {
    await installMocks(page);
    await page.goto("/hud");
    await waitForHudReady(page);

    await fireEvent(page, "hud:state", {
      state: "recording",
      kind: "meeting",
      startedAtMs: Date.now(),
    });
    await fireEvent(page, "meeting:call-may-have-ended", {
      confidence: "high",
      signalSummary: "mic inactive + system audio quiet",
    });

    await expect(page.getByText("Call ended?")).toBeVisible();

    await fireEvent(page, "meeting:call-end-cancelled", null);

    await expect(page.getByText("Call ended?")).not.toBeVisible();
  });

  test("suppresses subsequent call-end prompts after Keep recording", async ({ page }) => {
    await installMocks(page);
    await page.goto("/hud");
    await waitForHudReady(page);

    await fireEvent(page, "hud:state", {
      state: "recording",
      kind: "meeting",
      startedAtMs: Date.now(),
    });
    await fireEvent(page, "meeting:call-may-have-ended", {
      confidence: "high",
      signalSummary: "mic inactive + system audio quiet",
    });
    await page.getByRole("button", { name: "Keep recording" }).click();

    // A second call-end event should be suppressed
    await fireEvent(page, "meeting:call-may-have-ended", {
      confidence: "high",
      signalSummary: "mic inactive + system audio quiet",
    });

    await expect(page.getByText("Call ended?")).not.toBeVisible();
  });

  test("ignores call-end while a dictation is recording", async ({ page }) => {
    await installMocks(page);
    await page.goto("/hud");
    await waitForHudReady(page);
    await fireEvent(page, "hud:state", {
      state: "recording",
      kind: "dictation",
      startedAtMs: Date.now(),
    });
    await fireEvent(page, "meeting:call-may-have-ended", {
      confidence: "high",
      signalSummary: "mic inactive + system audio quiet",
    });
    await page.waitForTimeout(200);
    await expect(page.getByText("Call ended?")).toHaveCount(0);
  });
});
