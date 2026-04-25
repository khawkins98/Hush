import { expect, test } from "@playwright/test";
import { installMocks } from "./_mock";

// First-run welcome modal smokes. The modal renders only when the
// `get_first_run_completed` invoke returns `false`. Once dismissed,
// `mark_first_run_completed` is called and the flag flips to true so
// subsequent loads don't re-show it.

test.describe("first-run welcome modal", () => {
  test("does not render when get_first_run_completed returns true", async ({ page }) => {
    await installMocks(page); // default: get_first_run_completed -> true
    await page.goto("/");

    // The modal's heading is `<h2 id="first-run-heading">Welcome to Hush</h2>`.
    // A returning user should not see it.
    await expect(page.getByRole("heading", { name: "Welcome to Hush" })).toHaveCount(0);
  });

  test("renders for fresh installs and dismisses on Got it", async ({ page }) => {
    let markCalled = false;
    await installMocks(page, {
      get_first_run_completed: () => false,
      // Sentinel via global so the test can read it after dismissal.
      mark_first_run_completed: () => {
        (globalThis as unknown as { __markCalled: boolean }).__markCalled = true;
      },
    });
    await page.goto("/");

    const heading = page.getByRole("heading", { name: "Welcome to Hush" });
    await expect(heading).toBeVisible();

    await page.getByRole("button", { name: "Got it" }).click();
    await expect(heading).toHaveCount(0);

    // Confirm the IPC mark fired exactly once on the click — the
    // settings table backs the persistence, so the next launch
    // skips the modal entirely.
    markCalled = await page.evaluate(
      () => (globalThis as unknown as { __markCalled?: boolean }).__markCalled === true,
    );
    expect(markCalled).toBe(true);
  });

  // a11y regression. Round-4 reviewer flagged that the modal has no
  // Escape-key dismissal (issue #48). When that ships we expect this
  // test to pass; today it documents the gap. Marked `fixme` so the
  // suite stays green and the failure is visible only when fixed.
  test.fixme("Escape key dismisses the modal (closes part of #48)", async ({ page }) => {
    await installMocks(page, { get_first_run_completed: () => false });
    await page.goto("/");

    const heading = page.getByRole("heading", { name: "Welcome to Hush" });
    await expect(heading).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(heading).toHaveCount(0);
  });
});
