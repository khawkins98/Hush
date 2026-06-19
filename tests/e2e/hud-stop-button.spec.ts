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
      startedAtMs: Date.now(),
    });

    await page.getByRole("button", { name: "Stop recording" }).click();
    await page.getByRole("button", { name: "Stop" }).click();

    await expect(async () => {
      expect(stopped).toBe(true);
    }).toPass({ timeout: 2000 });
  });
});
