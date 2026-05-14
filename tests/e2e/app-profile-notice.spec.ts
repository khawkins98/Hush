import { expect, test } from "@playwright/test";
import { fireEvent, gotoSection, installMocks } from "./_mock";

// Coverage for app-profile-notice (data-testid="app-profile-notice").
//
// The notice appears in the History section when the backend emits an
// `app:profile-activated` event (e.g. when the user switches focus to an
// app that has a saved per-app audio profile). It auto-dismisses after ~3 s
// and the user can dismiss early with the × button.
//
// The notice fires via the Tauri listen("app:profile-activated") handler
// in +page.svelte. In the test environment the e2e event bus carries it.

test.describe("app profile notice", () => {
  test("notice appears in History after app:profile-activated event", async ({
    page,
  }) => {
    // A payload with null sources skips IPC calls for model_select /
    // refreshModels, so the only visible effect is the notice copy.
    await installMocks(page);
    await page.goto("/");

    // Navigate to History so the notice is in the DOM.
    await gotoSection(page, "history");

    await fireEvent(page, "app:profile-activated", {
      appName: "Zoom",
      preferredAudioSource: null,
      preferredModelId: null,
    });

    const notice = page.locator('[data-testid="app-profile-notice"]');
    await expect(notice).toBeVisible();
    await expect(notice).toContainText(/Zoom/);
  });

  test("dismissing the notice hides it", async ({ page }) => {
    await installMocks(page);
    await page.goto("/");
    await gotoSection(page, "history");

    await fireEvent(page, "app:profile-activated", {
      appName: "Slack",
      preferredAudioSource: null,
      preferredModelId: null,
    });

    const notice = page.locator('[data-testid="app-profile-notice"]');
    await expect(notice).toBeVisible();

    await notice.getByTestId("app-profile-notice-dismiss").click();
    await expect(notice).toHaveCount(0);
  });
});
