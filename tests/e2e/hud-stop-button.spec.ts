import { expect, test } from "@playwright/test";
import { fireEvent, installMocks } from "./_mock";

// Tests for the HUD stop button + inline confirmation flow.
// Drives `hud:state` events through the test seam and asserts
// the rendered confirmation UI and IPC calls.

test.describe("HUD stop button", () => {
  test("shows stop button when recording", async ({ page }) => {
    await installMocks(page);
    await page.goto("/hud");

    await expect(page.locator("button.hud-dismiss")).toBeVisible();

    await fireEvent(page, "hud:state", {
      state: "recording",
      kind: "meeting",
      startedAtMs: Date.now(),
    });

    await expect(
      page.getByRole("button", { name: "Stop recording" }),
    ).toBeVisible();
  });

  test("stop button shows inline confirmation", async ({ page }) => {
    await installMocks(page);
    await page.goto("/hud");

    await expect(page.locator("button.hud-dismiss")).toBeVisible();

    await fireEvent(page, "hud:state", {
      state: "recording",
      kind: "meeting",
      startedAtMs: Date.now(),
    });

    await page.getByRole("button", { name: "Stop recording" }).click();

    await expect(page.getByText("Stop recording?")).toBeVisible();
    await expect(page.getByRole("button", { name: "Stop" })).toBeVisible();
    await expect(
      page.getByRole("button", { name: "Keep recording" }),
    ).toBeVisible();
  });

  test("Keep recording returns to recording state", async ({ page }) => {
    await installMocks(page);
    await page.goto("/hud");

    await expect(page.locator("button.hud-dismiss")).toBeVisible();

    await fireEvent(page, "hud:state", {
      state: "recording",
      kind: "meeting",
      startedAtMs: Date.now(),
    });

    await page.getByRole("button", { name: "Stop recording" }).click();
    await page.getByRole("button", { name: "Keep recording" }).click();

    await expect(page.getByText("Stop recording?")).not.toBeVisible();
    await expect(
      page.getByRole("button", { name: "Stop recording" }),
    ).toBeVisible();
  });

  test("Stop calls meeting_stop_manual", async ({ page }) => {
    let stopped = false;
    await page.exposeFunction("__hush_on_stop_called", () => {
      stopped = true;
    });
    await installMocks(page, {
      meeting_stop_manual: () => {
        (
          window as unknown as { __hush_on_stop_called: () => void }
        ).__hush_on_stop_called();
      },
    });
    await page.goto("/hud");

    await expect(page.locator("button.hud-dismiss")).toBeVisible();

    await fireEvent(page, "hud:state", {
      state: "recording",
      kind: "meeting",
      startedAtMs: Date.now(),
    });

    await page.getByRole("button", { name: "Stop recording" }).click();
    await page.getByRole("button", { name: "Stop" }).click();

    await expect(async () => {
      expect(stopped).toBe(true);
    }).toPass({ timeout: 2000 });
  });

  test("dictation recordings have no stop button", async ({ page }) => {
    // Pre-fix ■ called meeting_stop_manual during a dictation: the HUD
    // hid itself and the dictation kept recording.
    await installMocks(page);
    await page.goto("/hud");
    await expect(page.locator("button.hud-dismiss")).toBeVisible();

    await fireEvent(page, "hud:state", {
      state: "recording",
      kind: "dictation",
      startedAtMs: Date.now(),
    });

    await expect(page.getByText("Recording", { exact: true })).toBeVisible();
    await expect(
      page.getByRole("button", { name: "Stop recording" }),
    ).toHaveCount(0);
  });

  test("Stop shows Stopping… then the backend's Stopped confirmation", async ({
    page,
  }) => {
    await installMocks(page);
    await page.goto("/hud");
    await expect(page.locator("button.hud-dismiss")).toBeVisible();
    await fireEvent(page, "hud:state", {
      state: "recording",
      kind: "meeting",
      startedAtMs: Date.now(),
    });

    await page.getByRole("button", { name: "Stop recording" }).click();
    await page.getByRole("button", { name: "Stop", exact: true }).click();
    await expect(page.locator(".hud-label")).toHaveText("Stopping…");

    await fireEvent(page, "hud:state", { state: "stopped" });
    await expect(page.locator(".hud-label")).toHaveText(
      "Stopped · saving transcript",
    );
  });

  test("a failed stop returns to recording with an error", async ({ page }) => {
    await installMocks(page, {
      meeting_stop_manual: () => {
        throw { kind: "meeting-sessions", message: "stop_manual: boom" };
      },
    });
    await page.goto("/hud");
    await expect(page.locator("button.hud-dismiss")).toBeVisible();
    await fireEvent(page, "hud:state", {
      state: "recording",
      kind: "meeting",
      startedAtMs: Date.now(),
    });

    await page.getByRole("button", { name: "Stop recording" }).click();
    await page.getByRole("button", { name: "Stop", exact: true }).click();

    await expect(page.locator(".hud-label")).toHaveText(
      "Couldn't stop — try again",
    );
    await expect(
      page.getByRole("button", { name: "Stop recording" }),
    ).toBeVisible();
  });

  test("double-clicking ■ neither raises the main window nor stops", async ({
    page,
  }) => {
    // The first click swaps ■ for the confirm strip, so the second click
    // lands on the pill and the dblclick bubbles to the root. Pre-fix
    // that raised the main window, which read as "stopped" while the
    // meeting kept recording.
    let raised = 0;
    let stopped = 0;
    await page.exposeFunction("__hushRaised", () => { raised++; });
    await page.exposeFunction("__hushStopped", () => { stopped++; });
    await installMocks(page, {
      show_main_window: () => {
        (window as unknown as { __hushRaised: () => void }).__hushRaised();
      },
      meeting_stop_manual: () => {
        (window as unknown as { __hushStopped: () => void }).__hushStopped();
      },
    });
    await page.goto("/hud");
    await expect(page.locator("button.hud-dismiss")).toBeVisible();
    await fireEvent(page, "hud:state", {
      state: "recording",
      kind: "meeting",
      startedAtMs: Date.now(),
    });

    await page.getByRole("button", { name: "Stop recording" }).dblclick();

    await expect(page.getByText("Stop recording?")).toBeVisible();
    await page.waitForTimeout(400);
    expect(raised).toBe(0);
    expect(stopped).toBe(0);
  });

  test("backend stop-failed resumes the live timer with an error", async ({
    page,
  }) => {
    await installMocks(page);
    await page.goto("/hud");
    await expect(page.locator("button.hud-dismiss")).toBeVisible();
    await fireEvent(page, "hud:state", {
      state: "recording",
      kind: "meeting",
      startedAtMs: Date.now() - 65_000,
    });
    // A stop from elsewhere (main window / auto-stop) that fails.
    await fireEvent(page, "hud:state", { state: "stopping" });
    await expect(page.locator(".hud-label")).toHaveText("Stopping…");
    await fireEvent(page, "hud:state", { state: "stop-failed" });

    await expect(page.locator(".hud-label")).toHaveText(
      "Couldn't stop — try again",
    );
    // Timer resumes from the original anchor rather than freezing/resetting.
    await expect(page.locator('[data-testid="hud-elapsed"]')).toHaveText(
      /^1:0[5-9]$/,
      { timeout: 6000 },
    );
  });
});
