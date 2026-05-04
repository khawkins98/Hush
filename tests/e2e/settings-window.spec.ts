import { expect, test } from "@playwright/test";
import { installMocks } from "./_mock";

// E2E coverage for the standalone Settings window
// (`src/routes/settings/+page.svelte`). The window is a sibling
// route on the same Vite dev server, so Playwright reaches it via
// `page.goto("/settings")` — no IPC `open_settings` round-trip
// needed. Specs that exercise inter-window flows (deep-link from
// the main window to a Settings tab) live in `meeting-panel` /
// `error-states` style files when those flows land.

test.describe("settings window — toolbar nav", () => {
  test("renders all seven tabs and lands on General by default", async ({
    page,
  }) => {
    await installMocks(page);
    await page.goto("/");
    await page.locator(`[data-testid="sidebar-nav-settings"]`).click();

    // Toolbar tabs use stable testIds, so the spec is robust to
    // label copy changes — the test asserts the tabs exist + the
    // active one is General without locking the visible text.
    for (const key of [
      "general",
      "model",
      "vocabulary",
      "replacements",
      "meeting",
      "permissions",
      "about",
    ]) {
      await expect(
        page.locator(`[data-testid="settings-tab-${key}"]`),
      ).toBeVisible();
    }

    const general = page.locator('[data-testid="settings-tab-general"]');
    await expect(general).toHaveAttribute("aria-current", "page");
  });

  test("clicking a tab makes it active + reveals its body", async ({
    page,
  }) => {
    await installMocks(page);
    await page.goto("/");
    await page.locator(`[data-testid="sidebar-nav-settings"]`).click();

    await page.locator('[data-testid="settings-tab-vocabulary"]').click();
    await expect(
      page.locator('[data-testid="settings-tab-vocabulary"]'),
    ).toHaveAttribute("aria-current", "page");
    await expect(page.locator("section.panel-vocabulary")).toBeVisible();

    await page.locator('[data-testid="settings-tab-replacements"]').click();
    await expect(
      page.locator('[data-testid="settings-tab-replacements"]'),
    ).toHaveAttribute("aria-current", "page");
    await expect(page.locator("section.panel-replacements")).toBeVisible();
  });

  test("settings:goto-tab event flips the active tab (deep-link)", async ({
    page,
  }) => {
    // The main window's "Open the Permissions diagnostic" button
    // sequences `invoke('open_settings')` then `emit('settings:goto-tab', 'permissions')`
    // — verify the listener side picks up the event and switches tabs.
    await installMocks(page);
    await page.goto("/");
    await page.locator(`[data-testid="sidebar-nav-settings"]`).click();

    // Wait for onMount to register the `settings:goto-tab`
    // listener. The General tab being marked aria-current is a
    // proxy for "page mounted, listener attached" since the
    // listener registration runs in the same onMount that does
    // initial loaders. Without this wait the fire below races.
    await expect(
      page.locator('[data-testid="settings-tab-general"]'),
    ).toHaveAttribute("aria-current", "page");

    await page.evaluate(() => {
      const bus = (
        window as unknown as {
          __hush_e2e_event_bus?: { fire: (n: string, p: unknown) => void };
        }
      ).__hush_e2e_event_bus;
      bus?.fire("settings:goto-tab", "permissions");
    });

    await expect(
      page.locator('[data-testid="settings-tab-permissions"]'),
    ).toHaveAttribute("aria-current", "page");
  });
});

test.describe("settings window — General tab", () => {
  test("autostart toggle reflects the plugin's reported state", async ({
    page,
  }) => {
    // Default mock has the autostart plugin returning false; the
    // checkbox should mount unchecked.
    await installMocks(page);
    await page.goto("/");
    await page.locator(`[data-testid="sidebar-nav-settings"]`).click();

    const toggle = page.locator('[data-testid="settings-autostart-toggle"]');
    await expect(toggle).toBeVisible();
    await expect(toggle).not.toBeChecked();
  });

  test("autostart toggle starts checked when the plugin reports enabled", async ({
    page,
  }) => {
    // Override the autostart plugin's `is_enabled` to return true —
    // the checkbox must mount checked.
    await installMocks(page, {
      "plugin:autostart|is_enabled": () => true,
    });
    await page.goto("/");
    await page.locator(`[data-testid="sidebar-nav-settings"]`).click();

    const toggle = page.locator('[data-testid="settings-autostart-toggle"]');
    await expect(toggle).toBeChecked();
  });

  test("HUD toggle reflects the persisted value and fires set_hud_enabled on click", async ({
    page,
  }) => {
    // Backend reports HUD is currently OFF; checkbox mounts
    // unchecked.
    await installMocks(page, {
      get_hud_enabled: () => false,
    });
    await page.goto("/");
    await page.locator(`[data-testid="sidebar-nav-settings"]`).click();

    const toggle = page.locator('[data-testid="settings-hud-toggle"]');
    await expect(toggle).toBeVisible();
    await expect(toggle).not.toBeChecked();

    // Click to enable; checkbox flips to checked. The mock's
    // default `set_hud_enabled` is a no-op `() => undefined` so
    // the optimistic update sticks.
    await toggle.click();
    await expect(toggle).toBeChecked();
  });

  test("inference-threads slider mounts at the persisted value and updates the value label", async ({
    page,
  }) => {
    // Backend reports 8 threads persisted. Slider should mount at 8
    // and the inline value label next to the slider should match.
    await installMocks(page, {
      get_inference_threads: () => 8,
    });
    await page.goto("/");
    await page.locator(`[data-testid="sidebar-nav-settings"]`).click();

    // Performance lives behind the Advanced disclosure (#427 Item 2).
    await page
      .locator('[data-testid="settings-general-advanced-toggle"]')
      .click();

    const slider = page.locator(
      '[data-testid="settings-inference-threads-slider"]',
    );
    await expect(slider).toBeVisible();
    await expect(slider).toHaveValue("8");

    const label = page.locator(
      '[data-testid="settings-inference-threads-value"]',
    );
    await expect(label).toHaveText("8");
  });

  test("first-run reset button shows confirmation copy after click", async ({
    page,
  }) => {
    await installMocks(page);
    await page.goto("/");
    await page.locator(`[data-testid="sidebar-nav-settings"]`).click();

    // First-run welcome lives behind the Advanced disclosure (#427 Item 2).
    await page
      .locator('[data-testid="settings-general-advanced-toggle"]')
      .click();

    const button = page.locator('[data-testid="settings-reset-first-run"]');
    await expect(button).toContainText(/Show welcome on next launch/i);
    await button.click();
    // The component swaps the label to the confirmation message
    // for ~3 s before reverting. Asserting the copy here pins the
    // success path without waiting on the timer.
    await expect(button).toContainText(/Welcome will show on next launch/i);
  });

  test("autostart path-stale warning hidden when status is clean (#317)", async ({
    page,
  }) => {
    await installMocks(page);
    await page.goto("/");
    await page.locator(`[data-testid="sidebar-nav-settings"]`).click();
    await expect(
      page.locator('[data-testid="autostart-path-stale-warning"]'),
    ).toHaveCount(0);
  });

  test("autostart path-stale warning surfaces + retry clears it (#317)", async ({
    page,
  }) => {
    let retryCalls = 0;
    await page.exposeFunction("__hush_record_autostart_retry", () => {
      retryCalls += 1;
    });
    await installMocks(page, {
      get_autostart_path_status: () => ({ stale: true }),
      retry_autostart_registration: () => {
        (
          window as unknown as {
            __hush_record_autostart_retry: () => void;
          }
        ).__hush_record_autostart_retry();
        // First call returns `true` → frontend clears the flag
        // and the warning disappears.
        return true;
      },
    });
    await page.goto("/");
    await page.locator(`[data-testid="sidebar-nav-settings"]`).click();

    const warning = page.locator(
      '[data-testid="autostart-path-stale-warning"]',
    );
    await expect(warning).toBeVisible();
    await expect(warning).toContainText(/out of date/i);

    await page
      .locator('[data-testid="autostart-retry-button"]')
      .click();

    await expect.poll(() => retryCalls).toBe(1);
    // Successful retry clears the flag → warning disappears.
    await expect(warning).toHaveCount(0);
  });

  test("autostart retry failure surfaces an error sub-row (#317)", async ({
    page,
  }) => {
    await installMocks(page, {
      get_autostart_path_status: () => ({ stale: true }),
      // Mock returns false → frontend keeps the warning visible
      // and shows the retry-failed sub-error.
      retry_autostart_registration: () => false,
    });
    await page.goto("/");
    await page.locator(`[data-testid="sidebar-nav-settings"]`).click();

    await page
      .locator('[data-testid="autostart-retry-button"]')
      .click();
    await expect(
      page.locator('[data-testid="autostart-retry-error"]'),
    ).toBeVisible();
    // Warning row still visible (retry didn't clear the flag).
    await expect(
      page.locator('[data-testid="autostart-path-stale-warning"]'),
    ).toBeVisible();
  });
});

test.describe("settings window — PTT editor", () => {
  test("renders the persisted combo as kbd chips and the enable toggle", async ({
    page,
  }) => {
    await installMocks(page);
    await page.goto("/");
    await page.locator(`[data-testid="sidebar-nav-settings"]`).click();

    // The default mock returns `combo: ["RightMeta"], enabled: false`.
    const display = page.locator('[data-testid="ptt-combo-display"]');
    await expect(display).toBeVisible();
    await expect(display.locator("kbd")).toHaveCount(1);

    const enable = page.locator(
      '[data-testid="ptt-enabled-toggle"] input[type="checkbox"]',
    );
    await expect(enable).toBeVisible();
    await expect(enable).not.toBeChecked();
  });

  test("multi-key combos render one kbd chip per key", async ({ page }) => {
    await installMocks(page, {
      ptt_get_config: () => ({
        combo: ["RightMeta", "RightShift"],
        enabled: true,
        listenerRunning: true,
      }),
    });
    await page.goto("/");
    await page.locator(`[data-testid="sidebar-nav-settings"]`).click();

    const display = page.locator('[data-testid="ptt-combo-display"]');
    await expect(display.locator("kbd")).toHaveCount(2);

    const enable = page.locator(
      '[data-testid="ptt-enabled-toggle"] input[type="checkbox"]',
    );
    await expect(enable).toBeChecked();
  });

  test("Record-new-combo button enters capture mode", async ({ page }) => {
    await installMocks(page);
    await page.goto("/");
    await page.locator(`[data-testid="sidebar-nav-settings"]`).click();

    const record = page.locator('[data-testid="ptt-record-button"]');
    await expect(record).toBeVisible();
    await record.click();

    // In capture mode, the prompt copy appears and the record
    // button is replaced with Save / Cancel actions.
    await expect(page.locator("text=Press your combo")).toBeVisible();
    await expect(record).toHaveCount(0);
    await expect(page.getByRole("button", { name: /Cancel/i })).toBeVisible();
  });
});

test.describe("settings window — Meeting tab (Phase E #112)", () => {
  test("renders the empty-state hint when no overrides exist", async ({
    page,
  }) => {
    await installMocks(page);
    await page.goto("/");
    await page.locator(`[data-testid="sidebar-nav-settings"]`).click();
    await page.locator('[data-testid="settings-tab-meeting"]').click();
    await expect(
      page.locator('[data-testid="settings-tab-meeting"]'),
    ).toHaveAttribute("aria-current", "page");
    await expect(page.locator("section.panel-overrides")).toBeVisible();
    await expect(page.locator(".empty-history")).toContainText(
      /No overrides yet/i,
    );
  });

  test("auto-start dropdown reflects the persisted mode and updates on change", async ({
    page,
  }) => {
    // Backend reports "always"; dropdown mounts with that value.
    await installMocks(page, {
      get_meeting_autostart_mode: () => "always",
    });
    await page.goto("/");
    await page.locator(`[data-testid="sidebar-nav-settings"]`).click();
    await page.locator('[data-testid="settings-tab-meeting"]').click();

    const dropdown = page.locator('[data-testid="settings-meeting-autostart"]');
    await expect(dropdown).toBeVisible();
    await expect(dropdown).toHaveValue("always");

    // Switching to "off" optimistically updates the dropdown
    // (the default mock's `set_meeting_autostart_mode` is a
    // no-op `() => undefined` so the optimistic value sticks).
    await dropdown.selectOption("off");
    await expect(dropdown).toHaveValue("off");
  });

  test("submitting the form invokes upsert with trimmed app_name", async ({
    page,
  }) => {
    // The trimming is enforced backend-side too, but pinning it on
    // the frontend keeps the wire contract honest — a typo with
    // trailing whitespace shouldn't sneak in.
    const calls: Array<{ appName: string; kind: string }> = [];
    await page.exposeFunction("__hush_record_upsert", (args: unknown) => {
      calls.push(args as { appName: string; kind: string });
    });
    await installMocks(page, {
      meeting_app_override_upsert: (args) => {
        const { appName, kind } = (args ?? {}) as {
          appName: string;
          kind: string;
        };
        (
          window as unknown as {
            __hush_record_upsert: (a: { appName: string; kind: string }) => void;
          }
        ).__hush_record_upsert({ appName, kind });
        return {
          appName,
          kind,
          createdAt: "2026-04-28T00:00:00Z",
        };
      },
    });
    await page.goto("/");
    await page.locator(`[data-testid="sidebar-nav-settings"]`).click();
    await page.locator('[data-testid="settings-tab-meeting"]').click();

    await page
      .getByLabel("App identifier")
      .fill("  com.example.huddle  ");
    await page.getByLabel("Classification", { exact: true }).selectOption("meeting");
    await page.getByRole("button", { name: "Add" }).click();

    await expect.poll(() => calls.length).toBeGreaterThan(0);
    expect(calls[0]).toEqual({
      appName: "com.example.huddle",
      kind: "meeting",
    });
  });

  test("renders pre-existing overrides as rows", async ({ page }) => {
    await installMocks(page, {
      meeting_app_override_list: () => [
        {
          appName: "alpha.app",
          kind: "meeting",
          createdAt: "2026-04-26T00:00:00Z",
        },
        {
          appName: "zebra.app",
          kind: "media",
          createdAt: "2026-04-27T00:00:00Z",
        },
      ],
    });
    await page.goto("/");
    await page.locator(`[data-testid="sidebar-nav-settings"]`).click();
    await page.locator('[data-testid="settings-tab-meeting"]').click();

    const rows = page.locator(".override-row");
    await expect(rows).toHaveCount(2);
    await expect(rows.nth(0).locator(".override-name")).toHaveText("alpha.app");
    await expect(rows.nth(1).locator(".override-name")).toHaveText("zebra.app");
  });

  test("built-in defaults disclosure renders Meeting + Media sections (#320)", async ({
    page,
  }) => {
    // Default mock returns a small representative subset so the
    // assertions are stable. Real production has ~70 entries.
    await installMocks(page);
    await page.goto("/");
    await page.locator(`[data-testid="sidebar-nav-settings"]`).click();
    await page.locator('[data-testid="settings-tab-meeting"]').click();

    const disclosure = page.locator('[data-testid="override-defaults"]');
    await expect(disclosure).toBeVisible();
    await disclosure.locator("summary").click();
    // Sections are present + entries from the default mock land in
    // the right groups.
    await expect(
      disclosure.locator(".override-defaults-heading", { hasText: "Meeting" }),
    ).toBeVisible();
    await expect(
      disclosure.locator(".override-defaults-heading", { hasText: "Media" }),
    ).toBeVisible();
    await expect(disclosure.getByText("us.zoom.xos")).toBeVisible();
    await expect(disclosure.getByText("com.spotify.client")).toBeVisible();
  });

  test("variant-suggestion box surfaces matching defaults + batch-adds (#320 part 2)", async ({
    page,
  }) => {
    const upserts: Array<{ appName: string; kind: string }> = [];
    await page.exposeFunction("__hush_record_upsert_variants", (args: unknown) => {
      upserts.push(args as { appName: string; kind: string });
    });
    await installMocks(page, {
      meeting_app_override_upsert: (args) => {
        const { appName, kind } = (args ?? {}) as {
          appName: string;
          kind: string;
        };
        (
          window as unknown as {
            __hush_record_upsert_variants: (a: {
              appName: string;
              kind: string;
            }) => void;
          }
        ).__hush_record_upsert_variants({ appName, kind });
        return { appName, kind, createdAt: "2026-05-01T00:00:00Z" };
      },
    });
    await page.goto("/");
    await page.locator(`[data-testid="sidebar-nav-settings"]`).click();
    await page.locator('[data-testid="settings-tab-meeting"]').click();

    // Suggestion box hidden until the user types a substring
    // matching multiple defaults.
    await expect(
      page.locator('[data-testid="override-variant-suggestions"]'),
    ).toHaveCount(0);

    // Type "zoom" — matches 2 entries in the default mock
    // (us.zoom.xos + Zoom.exe).
    await page.getByLabel("App identifier").fill("zoom");
    const box = page.locator('[data-testid="override-variant-suggestions"]');
    await expect(box).toBeVisible();
    await expect(box.getByText("us.zoom.xos")).toBeVisible();
    await expect(box.getByText("Zoom.exe")).toBeVisible();

    // Submit batch — both pre-checked, kind defaults to Meeting.
    await page
      .locator('[data-testid="override-variant-submit"]')
      .click();
    // Two upserts happen in parallel; assert both landed with the
    // right shape (order isn't guaranteed because of Promise.all).
    await expect.poll(() => upserts.length).toBe(2);
    const names = new Set(upserts.map((u) => u.appName));
    expect(names.has("us.zoom.xos")).toBe(true);
    expect(names.has("Zoom.exe")).toBe(true);
    for (const u of upserts) {
      expect(u.kind).toBe("meeting");
    }
  });

  test("redundant-override warning surfaces when typing a default app_name (#320)", async ({
    page,
  }) => {
    await installMocks(page);
    await page.goto("/");
    await page.locator(`[data-testid="sidebar-nav-settings"]`).click();
    await page.locator('[data-testid="settings-tab-meeting"]').click();

    // Pre-warning: the input is empty, no notice.
    await expect(
      page.locator('[data-testid="override-redundant-note"]'),
    ).toHaveCount(0);

    // Type a default app_name → notice appears with the right
    // classification.
    await page.getByLabel("App identifier").fill("us.zoom.xos");
    const note = page.locator('[data-testid="override-redundant-note"]');
    await expect(note).toBeVisible();
    await expect(note).toContainText(/already classified as/i);
    await expect(note).toContainText(/Meeting/i);
    await expect(note).toContainText("us.zoom.xos");

    // Type a non-default app_name → notice disappears.
    await page.getByLabel("App identifier").fill("com.example.unknown");
    await expect(
      page.locator('[data-testid="override-redundant-note"]'),
    ).toHaveCount(0);
  });
});

test.describe("settings window — About tab", () => {
  test("renders app name + version + license + repo links", async ({
    page,
  }) => {
    await installMocks(page);
    await page.goto("/");
    await page.locator(`[data-testid="sidebar-nav-settings"]`).click();

    await page.locator('[data-testid="settings-tab-about"]').click();
    await expect(
      page.locator('[data-testid="settings-tab-about"]'),
    ).toHaveAttribute("aria-current", "page");

    // App-info plugin mocks return "Hush" / "0.1.0" / "2.10.3".
    // Fail mode for this assertion is the silent-fallback path
    // (loadAppMetadata threw) — the test would catch a regression
    // where the @tauri-apps/api/app import broke entirely.
    await expect(page.locator(".about-name")).toHaveText("Hush");
    await expect(page.locator(".about-version")).toHaveText(
      /Version\s+0\.1\.0/,
    );
    await expect(page.locator(".about-meta code")).toHaveText("2.10.3");

    // Outbound links the user is most likely to click. Locked to
    // the actual hrefs because a typo in the repo URL silently
    // sends users to a dead page.
    await expect(
      page.locator('.about-meta a[href*="apache.org"]'),
    ).toHaveCount(1);
    await expect(
      page.locator('.about-meta a[href="https://github.com/khawkins98/Hush"]'),
    ).toBeVisible();
    await expect(
      page.locator(
        '.about-meta a[href="https://github.com/khawkins98/Hush/issues/new"]',
      ),
    ).toBeVisible();
  });

  test("Check for updates — up-to-date branch", async ({ page }) => {
    await installMocks(page, {
      check_for_updates: () => ({ kind: "upToDate", current: "0.1.0" }),
    });
    await page.goto("/");
    await page.locator(`[data-testid="sidebar-nav-settings"]`).click();
    await page.locator('[data-testid="settings-tab-about"]').click();

    await page.locator('[data-testid="settings-check-updates"]').click();
    await expect(page.locator(".about-update-ok")).toContainText(
      /You're on 0\.1\.0/,
    );
  });

  test("Check for updates — update-available branch renders the link", async ({
    page,
  }) => {
    await installMocks(page, {
      check_for_updates: () => ({
        kind: "updateAvailable",
        current: "0.1.0",
        latest: "0.2.0",
        releaseUrl: "https://github.com/khawkins98/Hush/releases/tag/v0.2.0",
      }),
    });
    await page.goto("/");
    await page.locator(`[data-testid="sidebar-nav-settings"]`).click();
    await page.locator('[data-testid="settings-tab-about"]').click();

    await page.locator('[data-testid="settings-check-updates"]').click();
    await expect(page.locator(".about-update-available")).toContainText(
      /Update available.*0\.2\.0/,
    );
    // The release-notes link sits inside the install-flow surface
    // alongside the Install button (#10). Scoped to the parent
    // `.about-update-available-block` so this still pins "the
    // link is visible somewhere on the update-available surface"
    // without coupling to which sibling in the surface owns it.
    await expect(
      page.locator(
        '.about-update-available-block a[href$="releases/tag/v0.2.0"]',
      ),
    ).toBeVisible();
    // Install button is the new primary action.
    await expect(
      page.locator('[data-testid="about-install-update"]'),
    ).toBeVisible();
  });

  test("Install flow — success path walks idle → installing → pending (#497)", async ({
    page,
  }) => {
    // Drives the auto-update install state machine through its
    // happy-path branches so a regression in the listener wiring
    // (the chunkLen accumulator, the install-pending handoff,
    // the formatInstallProgress branches) fails this spec rather
    // than passing silently. Pre-#497 only the unavailable-gate
    // branch was covered.
    //
    // The install_pending_update IPC is mocked to "succeed silently"
    // (returns undefined). Real success would relaunch the app, so
    // we don't try to assert the post-relaunch state — only that
    // the UI walks through installing → pending while the events
    // we drive arrive in order.
    await installMocks(page, {
      check_for_updates: () => ({
        kind: "updateAvailable",
        current: "0.1.0",
        latest: "0.2.0",
        releaseUrl: "https://github.com/khawkins98/Hush/releases/tag/v0.2.0",
      }),
      // Per-test override: a Promise that never resolves. In
      // production the IPC doesn't return until the install
      // completes (which relaunches the app), so the
      // `installState = "idle"` reset on Promise.resolve never
      // fires. A sync `() => undefined` would resolve instantly
      // and flip the UI back to idle before the test can drive
      // the events. The pending-forever Promise pins the state
      // machine in `installing` so the listener-driven
      // transitions are observable.
      install_pending_update: () => new Promise(() => {}),
    });
    await page.goto("/");
    await page.locator(`[data-testid="sidebar-nav-settings"]`).click();
    await page.locator('[data-testid="settings-tab-about"]').click();
    await page.locator('[data-testid="settings-check-updates"]').click();

    // Click Install — this fires the IPC (which silently
    // succeeds in the mock) and flips the UI to `installing`.
    await page.locator('[data-testid="about-install-update"]').click();

    // Drive a download-progress event with a known chunk size so
    // the accumulator visibly increments.
    await page.evaluate(() => {
      const bus = (
        window as unknown as {
          __hush_e2e_event_bus?: {
            fire: (n: string, p: unknown) => void;
          };
        }
      ).__hush_e2e_event_bus;
      bus?.fire("updater:download-progress", {
        chunkLen: 524_288,
        total: 5_242_880,
      });
    });

    // Progress readout shows the accumulated bytes as a percent
    // when total is known. 524_288 / 5_242_880 = 10%.
    await expect(
      page.locator('[data-testid="about-install-progress"]'),
    ).toContainText(/Downloading.*10%/);

    // Drive the install-pending handoff event — UI swaps to the
    // "Hush will relaunch" copy.
    await page.evaluate(() => {
      const bus = (
        window as unknown as {
          __hush_e2e_event_bus?: {
            fire: (n: string, p: unknown) => void;
          };
        }
      ).__hush_e2e_event_bus;
      bus?.fire("updater:install-pending", { version: "0.2.0" });
    });

    await expect(
      page.locator('[data-testid="about-install-pending"]'),
    ).toContainText(/Installing.*relaunch/);
  });

  test("Install flow — version mismatch surfaces the rotated version (#497)", async ({
    page,
  }) => {
    // Pin the TOCTOU defence: the IPC refuses to install when
    // the plugin's check resolves to a different version than the
    // user agreed to. The frontend renders the Internal error via
    // ErrorDisplay so the user sees what happened.
    await installMocks(page, {
      check_for_updates: () => ({
        kind: "updateAvailable",
        current: "0.1.0",
        latest: "0.2.0",
        releaseUrl: "https://github.com/khawkins98/Hush/releases/tag/v0.2.0",
      }),
      install_pending_update: () => {
        throw {
          kind: "internal",
          message:
            "update version mismatch: you agreed to install 0.2.0, " +
            "but the latest is now 0.2.1 — please re-check",
        };
      },
    });
    await page.goto("/");
    await page.locator(`[data-testid="sidebar-nav-settings"]`).click();
    await page.locator('[data-testid="settings-tab-about"]').click();
    await page.locator('[data-testid="settings-check-updates"]').click();
    await page.locator('[data-testid="about-install-update"]').click();

    const failed = page.locator('[data-testid="about-install-failed"]');
    await expect(failed).toBeVisible();
    // ErrorDisplay surfaces the message via the `.error-headline`
    // / `.error-details-body` shape (#199 pattern).
    await expect(failed).toContainText(/version mismatch/i);
  });

  test("Check for updates — failed branch surfaces the reason", async ({
    page,
  }) => {
    await installMocks(page, {
      check_for_updates: () => ({
        kind: "checkFailed",
        reason: "GitHub is rate-limiting the request. Try again in a few minutes.",
      }),
    });
    await page.goto("/");
    await page.locator(`[data-testid="sidebar-nav-settings"]`).click();
    await page.locator('[data-testid="settings-tab-about"]').click();

    await page.locator('[data-testid="settings-check-updates"]').click();
    await expect(page.locator(".about-update-failed")).toContainText(
      /rate-limiting/,
    );
  });

  test("falls back to static copy when app-info plugin throws", async ({
    page,
  }) => {
    // If the Tauri app-info plugin fails (older runtime, missing
    // capability), `loadAppMetadata` swallows the error and the
    // About tab still renders the default app name + the static
    // license/source links. Regression guard for the silent-catch
    // path in `loadAppMetadata`.
    await installMocks(page, {
      "plugin:app|name": () => {
        throw new Error("boom");
      },
      "plugin:app|version": () => {
        throw new Error("boom");
      },
      "plugin:app|tauri_version": () => {
        throw new Error("boom");
      },
    });
    await page.goto("/");
    await page.locator(`[data-testid="sidebar-nav-settings"]`).click();

    await page.locator('[data-testid="settings-tab-about"]').click();
    await expect(page.locator(".about-name")).toHaveText("Hush");
    // Version line is gated on a non-empty appVersion — should be hidden.
    await expect(page.locator(".about-version")).toHaveCount(0);
    // The static license link is still there.
    await expect(
      page.locator('.about-meta a[href*="apache.org"]'),
    ).toBeVisible();
  });
});
