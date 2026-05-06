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

test.describe("first-run permissions step — wizard-perm and wizard-allow testids", () => {
  // The permissions step shows wizard-perm-* rows unconditionally.
  // wizard-allow-* buttons appear only when the matching permission
  // is NOT granted (i.e. status !== "granted" && status !== "not-applicable").
  // Overriding diagnose_macos_permissions to return "not-determined"
  // for all three permissions causes all Allow buttons to render.

  async function openPermissionsStep(page: import("@playwright/test").Page) {
    await installMocks(page, {
      get_first_run_completed: () => false,
      diagnose_macos_permissions: () => ({
        bundleId: "io.github.khawkins98.hush",
        microphoneHint: null,
        inputMonitoringHint: null,
        canReset: false,
        statuses: {
          microphone: "not-determined",
          screenRecording: "not-determined",
          inputMonitoring: "not-determined",
        },
      }),
      // Prevent real OS calls from the allow buttons.
      request_microphone_permission: () => undefined,
      request_input_monitoring_permission: () => false,
      prime_screen_recording_permission: () => undefined,
    });
    await page.goto("/");
    // Advance from welcome step to permissions step.
    await page.locator('[data-testid="wizard-continue-welcome"]').click();
    await expect(
      page.getByRole("heading", { name: "Permissions" }),
    ).toBeVisible();
  }

  test("wizard-perm-* rows are visible in the permissions step", async ({
    page,
  }) => {
    await openPermissionsStep(page);

    for (const key of ["microphone", "input-monitoring"]) {
      await expect(
        page.locator(`[data-testid="wizard-perm-${key}"]`),
      ).toBeVisible();
    }
  });

  test("wizard-allow-* buttons are visible when permissions are not-determined", async ({
    page,
  }) => {
    await openPermissionsStep(page);

    await expect(
      page.locator('[data-testid="wizard-allow-microphone"]'),
    ).toBeVisible();
    await expect(
      page.locator('[data-testid="wizard-allow-input-monitoring"]'),
    ).toBeVisible();
  });

  test("wizard-allow-* buttons are absent when permissions are already granted", async ({
    page,
  }) => {
    // Default mock: statuses are all "not-applicable" → isGranted() returns true.
    await installMocks(page, {
      get_first_run_completed: () => false,
    });
    await page.goto("/");
    await page.locator('[data-testid="wizard-continue-welcome"]').click();
    await expect(
      page.getByRole("heading", { name: "Permissions" }),
    ).toBeVisible();

    // Both Allow buttons should be absent — ✓ badges shown instead.
    await expect(
      page.locator('[data-testid="wizard-allow-microphone"]'),
    ).toHaveCount(0);
    await expect(
      page.locator('[data-testid="wizard-allow-input-monitoring"]'),
    ).toHaveCount(0);
  });
});

test.describe("permissions dialog — perm-dialog-refresh", () => {
  // The PermissionsDialog opens automatically after the first-run
  // wizard is dismissed (dismissFirstRun() in +page.svelte sets
  // showPermissionsDialog = true). The perm-dialog-refresh button
  // is always rendered in the dialog header.
  test("perm-dialog-refresh button is visible in the permissions dialog", async ({
    page,
  }) => {
    await installMocks(page, {
      get_first_run_completed: () => false,
    });
    await page.goto("/");

    // Complete the wizard to open the permissions dialog.
    await page.locator('[data-testid="wizard-continue-welcome"]').click();
    await page.locator('[data-testid="wizard-finish"]').click();

    await expect(
      page.locator('[data-testid="perm-dialog-refresh"]'),
    ).toBeVisible();
  });
});

