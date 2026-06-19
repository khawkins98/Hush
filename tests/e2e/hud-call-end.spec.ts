import { expect, test } from "@playwright/test";
import { fireEvent, installMocks } from "./_mock";

// Tests for the HUD call-may-have-ended prompt.
// Drives `hud:state` and `meeting:call-end-cancelled` events through
// the test seam and asserts the rendered prompt UI and IPC calls.

// Helper: wait for the HUD page to finish mounting (dismiss button visible =
// all `listen()` calls in onMount have fired, initialising the event bus).
async function waitForHudReady(page: Parameters<typeof fireEvent>[0]) {
  await page.locator("button.hud-dismiss").waitFor({ state: "visible" });
}

test.describe("HUD call-may-have-ended prompt", () => {
  test("shows high-confidence call-ended prompt", async ({ page }) => {
    await installMocks(page);
    await page.goto("/hud");
    await waitForHudReady(page);

    await fireEvent(page, "hud:state", {
      state: "call-may-have-ended",
      confidence: "high",
    });

    await expect(page.getByText("Your call has likely ended")).toBeVisible();
    await expect(page.getByRole("button", { name: "Stop now" })).toBeVisible();
    await expect(page.getByRole("button", { name: "Keep recording" })).toBeVisible();
  });

  test("shows medium-confidence call-ended prompt with softer copy", async ({ page }) => {
    await installMocks(page);
    await page.goto("/hud");
    await waitForHudReady(page);

    await fireEvent(page, "hud:state", {
      state: "call-may-have-ended",
      confidence: "medium",
    });

    await expect(page.getByText("Your call may be winding down")).toBeVisible();
    await expect(page.getByRole("button", { name: "Stop now" })).toBeVisible();
    await expect(page.getByRole("button", { name: "Keep recording" })).toBeVisible();
  });

  test("Keep recording returns to recording state and hides prompt", async ({ page }) => {
    await installMocks(page);
    await page.goto("/hud");
    await waitForHudReady(page);

    await fireEvent(page, "hud:state", {
      state: "recording",
      startedAtMs: Date.now(),
    });
    await fireEvent(page, "hud:state", {
      state: "call-may-have-ended",
      confidence: "high",
    });

    await page.getByRole("button", { name: "Keep recording" }).click();

    await expect(page.getByText("Your call has likely ended")).not.toBeVisible();
    // Stop recording button should be back
    await expect(page.getByRole("button", { name: "Stop recording" })).toBeVisible();
  });

  test("Stop now calls meeting_stop_manual", async ({ page }) => {
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

    await fireEvent(page, "hud:state", {
      state: "call-may-have-ended",
      confidence: "high",
    });

    await page.getByRole("button", { name: "Stop now" }).click();

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
      startedAtMs: Date.now(),
    });
    await fireEvent(page, "hud:state", {
      state: "call-may-have-ended",
      confidence: "high",
    });

    await expect(page.getByText("Your call has likely ended")).toBeVisible();

    await fireEvent(page, "meeting:call-end-cancelled", null);

    await expect(page.getByText("Your call has likely ended")).not.toBeVisible();
  });

  test("suppresses subsequent call-end prompts after Keep recording", async ({ page }) => {
    await installMocks(page);
    await page.goto("/hud");
    await waitForHudReady(page);

    await fireEvent(page, "hud:state", {
      state: "recording",
      startedAtMs: Date.now(),
    });
    await fireEvent(page, "hud:state", {
      state: "call-may-have-ended",
      confidence: "high",
    });
    await page.getByRole("button", { name: "Keep recording" }).click();

    // A second call-end event should be suppressed
    await fireEvent(page, "hud:state", {
      state: "call-may-have-ended",
      confidence: "high",
    });

    await expect(page.getByText("Your call has likely ended")).not.toBeVisible();
  });
});
