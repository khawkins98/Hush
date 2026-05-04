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

  test("renders for fresh installs and dismisses on Finish (#511 wizard)", async ({ page }) => {
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

    // Step 1 (welcome) → Continue advances to step 2 (permissions).
    await page.locator('[data-testid="wizard-continue-welcome"]').click();
    await expect(
      page.getByRole("heading", { name: "Permissions" }),
    ).toBeVisible();

    // Step 2 → Finish dismisses the wizard. Continue is never
    // hard-blocked even when permissions aren't granted (mic
    // ungrant just shows a soft warning footer).
    await page.locator('[data-testid="wizard-finish"]').click();
    await expect(heading).toHaveCount(0);

    // Confirm the IPC mark fired exactly once on the click — the
    // settings table backs the persistence, so the next launch
    // skips the wizard entirely.
    markCalled = await page.evaluate(
      () => (globalThis as unknown as { __markCalled?: boolean }).__markCalled === true,
    );
    expect(markCalled).toBe(true);
  });

  test("Escape key dismisses the modal (closes part of #48)", async ({ page }) => {
    let markCalled = false;
    await installMocks(page, {
      get_first_run_completed: () => false,
      mark_first_run_completed: () => {
        (globalThis as unknown as { __markCalled: boolean }).__markCalled = true;
      },
    });
    await page.goto("/");

    const heading = page.getByRole("heading", { name: "Welcome to Hush" });
    await expect(heading).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(heading).toHaveCount(0);

    // Escape should also persist dismissal — the user expects
    // "I dismissed this" to mean "and don't show it again",
    // regardless of which control they used to dismiss.
    markCalled = await page.evaluate(
      () => (globalThis as unknown as { __markCalled?: boolean }).__markCalled === true,
    );
    expect(markCalled).toBe(true);
  });

  test("Tab cycles within the modal instead of escaping (closes focus-trap part of #48)", async ({ page }) => {
    await installMocks(page, { get_first_run_completed: () => false });
    await page.goto("/");

    await expect(page.getByRole("heading", { name: "Welcome to Hush" })).toBeVisible();

    // Step 1 (welcome) has two focusable buttons in DOM order:
    //   1) Skip setup (ghost)
    //   2) Continue (primary)
    // Auto-focus lands on #1; one Shift+Tab from there must wrap
    // to #2, not escape to whatever was on the page behind the
    // backdrop.
    await page.keyboard.press("Shift+Tab");
    await expect(page.getByRole("button", { name: "Continue" })).toBeFocused();

    // Tab from "Continue" must wrap forward to the first button.
    await page.keyboard.press("Tab");
    await expect(
      page.getByRole("button", { name: "Skip setup" }),
    ).toBeFocused();
  });
});
