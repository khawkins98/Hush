import { expect, test } from "@playwright/test";
import { fireEvent, installMocks } from "./_mock";

// Tests for the HUD pending state: countdown bar, label, and cancel button.

test.describe("HUD pending state", () => {
  test("shows 'Meeting detected' label in pending state", async ({ page }) => {
    await installMocks(page);
    await page.goto("/hud");

    await expect(page.locator("button.hud-dismiss")).toBeVisible();

    await fireEvent(page, "hud:state", {
      state: "pending",
      endsAtMs: Date.now() + 3000,
    });

    await expect(page.getByText("Meeting detected")).toBeVisible();
  });

  test("shows countdown bar in pending state", async ({ page }) => {
    await installMocks(page);
    await page.goto("/hud");

    await expect(page.locator("button.hud-dismiss")).toBeVisible();

    await fireEvent(page, "hud:state", {
      state: "pending",
      endsAtMs: Date.now() + 3000,
    });

    await expect(page.locator("[data-testid='hud-countdown-bar']")).toBeVisible();
  });

  test("shows cancel button in pending state", async ({ page }) => {
    await installMocks(page);
    await page.goto("/hud");

    await expect(page.locator("button.hud-dismiss")).toBeVisible();

    await fireEvent(page, "hud:state", {
      state: "pending",
      endsAtMs: Date.now() + 3000,
    });

    await expect(
      page.getByRole("button", { name: "Cancel auto-recording" }),
    ).toBeVisible();
  });

  test("cancel button calls meeting_cancel_pending", async ({ page }) => {
    let cancelled = false;
    await page.exposeFunction("__hush_on_cancel_called", () => {
      cancelled = true;
    });
    await installMocks(page, {
      meeting_cancel_pending: () => {
        (
          window as unknown as { __hush_on_cancel_called: () => void }
        ).__hush_on_cancel_called();
      },
    });
    await page.goto("/hud");

    await expect(page.locator("button.hud-dismiss")).toBeVisible();

    await fireEvent(page, "hud:state", {
      state: "pending",
      endsAtMs: Date.now() + 3000,
    });

    await page.getByRole("button", { name: "Cancel auto-recording" }).click();

    await expect(async () => {
      expect(cancelled).toBe(true);
    }).toPass({ timeout: 2000 });
  });

  test("stop button is not shown in pending state", async ({ page }) => {
    await installMocks(page);
    await page.goto("/hud");

    await expect(page.locator("button.hud-dismiss")).toBeVisible();

    await fireEvent(page, "hud:state", {
      state: "pending",
      endsAtMs: Date.now() + 3000,
    });

    await expect(
      page.getByRole("button", { name: "Stop recording" }),
    ).not.toBeVisible();
  });

  test("cancel button is not shown in recording state", async ({ page }) => {
    await installMocks(page);
    await page.goto("/hud");

    await expect(page.locator("button.hud-dismiss")).toBeVisible();

    await fireEvent(page, "hud:state", {
      state: "recording",
      startedAtMs: Date.now(),
    });

    await expect(
      page.getByRole("button", { name: "Cancel auto-recording" }),
    ).not.toBeVisible();
  });
});
