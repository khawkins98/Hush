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

    // Returning users should never see the wizard. After #609 the
    // wizard opens on the Permissions step rather than Welcome —
    // assert against the heading the wizard now starts on.
    await expect(page.getByRole("heading", { name: "Permissions" })).toHaveCount(0);
  });

  test("renders for fresh installs and dismisses on Finish (#511 wizard, #609 reordered)", async ({ page }) => {
    let markCalled = false;
    await installMocks(page, {
      get_first_run_completed: () => false,
      // Sentinel via global so the test can read it after dismissal.
      mark_first_run_completed: () => {
        (globalThis as unknown as { __markCalled: boolean }).__markCalled = true;
      },
    });
    await page.goto("/");

    // Wizard now opens on Permissions (#609). Get the mandatory grants
    // out of the way before the explainer.
    const permsHeading = page.getByRole("heading", { name: "Permissions" });
    await expect(permsHeading).toBeVisible();

    // Step 1 (permissions) → Continue advances to step 2 (welcome).
    await page.locator('[data-testid="wizard-continue-permissions"]').click();
    await expect(
      page.getByRole("heading", { name: "Welcome to Hush" }),
    ).toBeVisible();

    // Step 2 → "Start using Hush" dismisses the wizard. The IPC
    // testid is unchanged (`wizard-finish`) since it's the same
    // primary action — it just lives on a different step now.
    await page.locator('[data-testid="wizard-finish"]').click();
    await expect(
      page.getByRole("heading", { name: "Welcome to Hush" }),
    ).toHaveCount(0);

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

    // Wizard opens on Permissions step (#609).
    const heading = page.getByRole("heading", { name: "Permissions" });
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

    await expect(page.getByRole("heading", { name: "Permissions" })).toBeVisible();

    // Step 1 (permissions) has the same two-button footer shape as the
    // pre-#609 welcome step: Skip setup (ghost) + Continue (primary).
    // Drive the focus trap by manually focusing the first button —
    // the modal's $effect autofocus runs at mount but Playwright's
    // page-load can race ahead of the focus call, leaving the trap
    // precondition (`active === first`) unsatisfied. Once we focus
    // Skip setup explicitly, Shift+Tab triggers the wrap to Continue.
    const skipBtn = page.getByRole("button", { name: "Skip setup" });
    await skipBtn.focus();
    await expect(skipBtn).toBeFocused();
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
  //
  // After #609 the wizard opens directly on the permissions step,
  // so this helper no longer needs the welcome → permissions
  // navigation it had pre-#609.

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
    // Wizard opens directly on permissions step post-#609.
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
  // The PermissionsDialog opens when a dictation or meeting start
  // hits a permission-denied error (the +page.svelte $effect blocks
  // that watch `dictation.pendingPermissionsDialogIntro` /
  // `meeting.pendingPermissionsDialogIntro` set
  // `showPermissionsDialog = true`).
  //
  // Pre-#609 the dialog also auto-opened after first-run wizard
  // dismissal — that was the redundant third "permissions" surface
  // the wizard now obviates. This test now drives the dialog open
  // via the permission-error path, which is the remaining live
  // trigger. The assertion (perm-dialog-refresh visible in the
  // dialog header) is unchanged.
  test("perm-dialog-refresh button is visible in the permissions dialog", async ({
    page,
  }) => {
    // Drive the dialog open via the same shape as
    // recording-phase.spec.ts: list system-audio as supported, then
    // throw a permission-denied error from meeting_start_manual.
    // The +page.svelte $effect on `meeting.pendingPermissionsDialogIntro`
    // sets `showPermissionsDialog = true` so the dialog appears.
    await installMocks(page, {
      audio_list_sources: () => [
        {
          kind: "microphone",
          id: "Built-in Microphone",
          name: "Built-in Microphone",
          isDefault: true,
          isSupported: true,
        },
        {
          kind: "system-audio",
          id: "system",
          name: "System audio",
          isDefault: false,
          isSupported: true,
        },
      ],
      meeting_start_manual: () => {
        throw { kind: "permission-denied", message: "screen-recording" };
      },
    });
    await page.goto("/");

    const meetingStartBtn = page.getByRole("button", {
      name: "Record meeting (mic plus system audio)",
    });
    await expect(meetingStartBtn).toBeEnabled();
    await meetingStartBtn.click();

    await expect(
      page.locator('[data-testid="perm-dialog-refresh"]'),
    ).toBeVisible();
  });
});

