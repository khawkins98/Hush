// Hands-off UX walkthrough — captures a screenshot of every
// significant screen / state so a reviewer can flip through them
// and flag visual / interaction polish work. NOT part of the CI
// suite (the `_` prefix excludes it from the default testMatch).
//
// Run: `npx playwright test tests/e2e/_uxwalk.spec.ts --reporter=list`
// Output: PNGs in /tmp/hush-uxwalk-shots/
//
// Each step covers one branch worth showing: empty state, populated
// state, error state, dialog open, click-to-confirm armed, etc.
// Designed to be re-runnable as the UI evolves.

import { expect, test } from "@playwright/test";
import { installMocks } from "./_mock";

const SHOT_DIR = "/tmp/hush-uxwalk-shots";

// Each test is independent; no `serial` so a single failure
// doesn't skip the rest of the walkthrough.
test.use({ viewport: { width: 1280, height: 800 } });

async function shot(page: import("@playwright/test").Page, name: string) {
  // Wait for the network to settle (Promise.all of mount fetches in
  // +page.svelte) and then a beat for any reactive class updates
  // (sidebar nav clicks, settings tab clicks) to land before the
  // screenshot. Cheap; not running in CI.
  await page.waitForLoadState("networkidle").catch(() => undefined);
  await page.waitForTimeout(400);
  await page.screenshot({ path: `${SHOT_DIR}/${name}.png`, fullPage: false });
}

test.describe("UX walkthrough — main window", () => {
  test("dictation: idle, no model installed", async ({ page }) => {
    await installMocks(page, {
      // No models downloaded → first-run setup banner is visible.
      model_list: () => [
        {
          id: "whisper-base",
          displayName: "Whisper Base",
          filename: "ggml-base.bin",
          sizeMb: 142,
          speedRating: 4,
          accuracyRating: 3,
          description: "Recommended starter model.",
          isDefault: true,
          isDownloaded: false,
          isSelected: false,
          expectedPath: "/Users/k/Library/Application Support/Hush/models/ggml-base.bin",
        },
      ],
    });
    await page.goto("/");
    await expect(page.getByRole("heading", { name: "Dictation" })).toBeVisible();
    await shot(page, "01-dictation-no-model");
  });

  test("dictation: idle, model installed, perms granted (green pill)", async ({ page }) => {
    await installMocks(page, {
      model_list: () => [
        {
          id: "whisper-base",
          displayName: "Whisper Base",
          filename: "ggml-base.bin",
          sizeMb: 142,
          speedRating: 4,
          accuracyRating: 3,
          description: "Recommended starter model.",
          isDefault: true,
          isDownloaded: true,
          isSelected: true,
          expectedPath: "/Users/k/Library/Application Support/Hush/models/ggml-base.bin",
        },
      ],
      diagnose_macos_permissions: () => ({
        bundleId: "com.khawkins.hush",
        microphoneHint: "",
        inputMonitoringHint: "",
        canReset: true,
        statuses: {
          microphone: "granted",
          screenRecording: "granted",
          inputMonitoring: "granted",
        },
      }),
    });
    await page.goto("/");
    await expect(page.getByRole("heading", { name: "Dictation" })).toBeVisible();
    await shot(page, "02-dictation-perms-ok");
  });

  test("dictation: yellow recovery hint when a perm is denied", async ({ page }) => {
    await installMocks(page, {
      diagnose_macos_permissions: () => ({
        bundleId: "com.khawkins.hush",
        microphoneHint: "",
        inputMonitoringHint: "",
        canReset: true,
        statuses: {
          microphone: "denied",
          screenRecording: "not-determined",
          inputMonitoring: "not-determined",
        },
      }),
    });
    await page.goto("/");
    await shot(page, "03-dictation-perms-denied");
  });

  test("first-run welcome modal", async ({ page }) => {
    await installMocks(page, {
      get_first_run_completed: () => false,
    });
    await page.goto("/");
    await expect(
      page.getByRole("heading", { name: "Welcome to Hush" }),
    ).toBeVisible();
    await shot(page, "04-first-run-modal");
  });

  // Phase 1 of #357 dropped the standalone Meetings panel from the
  // main-window sidebar (Dictation/History only now). The three
  // meetings: shots below are skipped until Phase 2 reintroduces
  // meetings as part of the unified History feed; the spec bodies
  // stay checked-in so the screenshots can be re-baselined when
  // the surface returns.
  test.skip("meetings: empty state", async ({ page }) => {
    await installMocks(page);
    await page.goto("/");
    await page.locator("button", { hasText: "Meetings" }).click();
    await expect(
      page.getByRole("heading", { name: "Meetings", exact: true }),
    ).toBeVisible();
    await shot(page, "05-meetings-empty");
  });

  test.skip("meetings: populated list with search visible", async ({ page }) => {
    await installMocks(page, {
      meeting_sessions_list: () => [
        {
          id: 1,
          appName: "Zoom",
          appKind: "meeting",
          startedAt: "2026-04-29T10:00:00Z",
          endedAt: "2026-04-29T10:45:00Z",
          utteranceCount: 184,
          notes: "Weekly product sync — discussed roadmap and Q3 OKRs.",
        },
        {
          id: 2,
          appName: "Microsoft Teams",
          appKind: "meeting",
          startedAt: "2026-04-28T14:00:00Z",
          endedAt: "2026-04-28T14:30:00Z",
          utteranceCount: 92,
          notes: null,
        },
        {
          id: 3,
          appName: "Discord",
          appKind: "meeting",
          startedAt: "2026-04-27T20:00:00Z",
          endedAt: "2026-04-27T21:15:00Z",
          utteranceCount: 312,
          notes: "Late-night design call.",
        },
      ],
    });
    await page.goto("/");
    await page.locator("button", { hasText: "Meetings" }).click();
    await shot(page, "06-meetings-populated");
  });

  test.skip("meetings: search active with no matches", async ({ page }) => {
    await installMocks(page, {
      meeting_sessions_list: () => [
        {
          id: 1,
          appName: "Zoom",
          appKind: "meeting",
          startedAt: "2026-04-29T10:00:00Z",
          endedAt: "2026-04-29T10:45:00Z",
          utteranceCount: 184,
          notes: "Sync.",
        },
      ],
    });
    await page.goto("/");
    await page.locator("button", { hasText: "Meetings" }).click();
    // Wait for the populated list to render so the search input
    // (gated on `sessions.length > 0`) is in the DOM.
    await expect(page.locator(".session-app").first()).toBeVisible();
    await page.getByPlaceholder("Filter by app or notes…").fill("xyzzy");
    await shot(page, "07-meetings-search-no-match");
  });

  test("history: empty state", async ({ page }) => {
    await installMocks(page);
    await page.goto("/");
    await page.locator(`[data-testid="sidebar-nav-history"]`).click();
    await shot(page, "08-history-empty");
  });

  test("history: populated", async ({ page }) => {
    await installMocks(page, {
      history_count: () => 4,
      // Dictation stats (#293) — visible above the list when
      // session_count > 0. Numbers shaped to land "11h 52m saved"
      // and "~148,200 keystrokes" on the stats bar so the
      // walkthrough screenshot exercises the populated state.
      get_dictation_stats: () => ({
        sessionCount: 142,
        wordCount: 28450,
        totalRecordingMs: 8 * 60 * 60 * 1000,
        totalChars: 148200,
      }),
      history_search: () => [
        {
          id: 1,
          transcript: "This is a quick note about the new feature.",
          createdAt: "2026-04-29T11:00:00Z",
          model: "ggml-base.bin",
          durationMs: 4200,
          source: "Mail",
        },
        {
          id: 2,
          transcript: "Reminder: pick up groceries on the way home, including milk, eggs, and bread.",
          createdAt: "2026-04-29T09:30:00Z",
          model: "ggml-base.bin",
          durationMs: 9800,
          source: null,
        },
        {
          id: 3,
          transcript: "Hello world.",
          createdAt: "2026-04-28T16:15:00Z",
          model: "ggml-base.bin",
          durationMs: 600,
          source: "Slack",
        },
        {
          id: 4,
          transcript: "A longer transcript that wraps over two lines so we can see the row's vertical rhythm and the action buttons stay aligned regardless of length.",
          createdAt: "2026-04-28T11:00:00Z",
          model: "ggml-base.bin",
          durationMs: 14500,
          source: null,
        },
      ],
    });
    await page.goto("/");
    await page.locator(`[data-testid="sidebar-nav-history"]`).click();
    await shot(page, "09-history-populated");
  });

  test("history: row delete armed (click-to-confirm)", async ({ page }) => {
    await installMocks(page, {
      history_count: () => 1,
      history_search: () => [
        {
          id: 1,
          transcript: "About to delete this.",
          createdAt: "2026-04-29T11:00:00Z",
          model: "ggml-base.bin",
          durationMs: 1500,
          source: null,
        },
      ],
    });
    await page.goto("/");
    await page.locator(`[data-testid="sidebar-nav-history"]`).click();
    // Wait for the row to mount before targeting its delete btn.
    await expect(page.locator(".history-row").first()).toBeVisible();
    await page.locator('[data-testid="history-delete-1"]').click();
    await shot(page, "10-history-row-delete-armed");
  });

  test("history: clear all confirm prompt", async ({ page }) => {
    await installMocks(page, {
      history_count: () => 5,
      history_search: () => [
        {
          id: 1,
          transcript: "x",
          createdAt: "2026-04-29T11:00:00Z",
          model: "ggml-base.bin",
          durationMs: 1500,
          source: null,
        },
      ],
    });
    await page.goto("/");
    await page.locator(`[data-testid="sidebar-nav-history"]`).click();
    await page.locator('[data-testid="history-clear-all"]').click();
    await shot(page, "11-history-clear-all-confirm");
  });
});

test.describe("UX walkthrough — settings window", () => {
  test("settings: General tab default", async ({ page }) => {
    await installMocks(page);
    await page.goto("/");
    await page.locator(`[data-testid="sidebar-nav-settings"]`).click();
    await expect(page.getByRole("heading", { name: "General" })).toBeVisible();
    await shot(page, "20-settings-general");
  });

  test("settings: Model tab with mixed download states", async ({ page }) => {
    await installMocks(page, {
      model_list: () => [
        {
          id: "whisper-tiny",
          displayName: "Whisper Tiny",
          filename: "ggml-tiny.bin",
          sizeMb: 39,
          speedRating: 5,
          accuracyRating: 1,
          description: "Fastest. Reasonable for quick notes.",
          isDefault: false,
          isDownloaded: true,
          isSelected: true,
          expectedPath: "/x/y",
        },
        {
          id: "whisper-base",
          displayName: "Whisper Base",
          filename: "ggml-base.bin",
          sizeMb: 142,
          speedRating: 4,
          accuracyRating: 3,
          description: "Recommended starter. Solid balance.",
          isDefault: true,
          isDownloaded: true,
          isSelected: false,
          expectedPath: "/x/y",
        },
        {
          id: "whisper-medium",
          displayName: "Whisper Medium",
          filename: "ggml-medium.bin",
          sizeMb: 1530,
          speedRating: 2,
          accuracyRating: 4,
          description: "Better accuracy at the cost of speed.",
          isDefault: false,
          isDownloaded: false,
          isSelected: false,
          expectedPath: "/x/y",
        },
        {
          id: "whisper-large-v3",
          displayName: "Whisper Large v3",
          filename: "ggml-large-v3.bin",
          sizeMb: 3094,
          speedRating: 1,
          accuracyRating: 5,
          description: "Highest accuracy. Slow on CPU.",
          isDefault: false,
          isDownloaded: false,
          isSelected: false,
          expectedPath: "/x/y",
        },
      ],
    });
    await page.goto("/");
    await page.locator(`[data-testid="sidebar-nav-settings"]`).click();
    await page.locator('[data-testid="settings-tab-model"]').click();
    await expect(
      page.locator('[data-testid="settings-tab-model"]'),
    ).toHaveAttribute("aria-current", "page");
    await shot(page, "21-settings-model");
  });

  test("settings: Vocabulary tab with terms", async ({ page }) => {
    await installMocks(page, {
      vocabulary_list: () => [
        { id: 1, term: "Hush", createdAt: "2026-04-01T00:00:00Z" },
        { id: 2, term: "Whisper", createdAt: "2026-04-01T00:00:00Z" },
        { id: 3, term: "Tauri", createdAt: "2026-04-01T00:00:00Z" },
        { id: 4, term: "Khawkins", createdAt: "2026-04-01T00:00:00Z" },
      ],
    });
    await page.goto("/");
    await page.locator(`[data-testid="sidebar-nav-settings"]`).click();
    await page.locator('[data-testid="settings-tab-vocabulary"]').click();
    await shot(page, "22-settings-vocabulary");
  });

  test("settings: Replacements tab with rules", async ({ page }) => {
    await installMocks(page, {
      replacements_list: () => [
        { id: 1, findText: "btw", replaceText: "by the way", createdAt: "2026-04-01T00:00:00Z" },
        { id: 2, findText: "imo", replaceText: "in my opinion", createdAt: "2026-04-01T00:00:00Z" },
        { id: 3, findText: "asap", replaceText: "as soon as possible", createdAt: "2026-04-01T00:00:00Z" },
      ],
    });
    await page.goto("/");
    await page.locator(`[data-testid="sidebar-nav-settings"]`).click();
    await page.locator('[data-testid="settings-tab-replacements"]').click();
    await shot(page, "23-settings-replacements");
  });

  test("settings: Meeting tab with auto-start dropdown + overrides", async ({ page }) => {
    await installMocks(page, {
      get_meeting_autostart_mode: () => "always",
      meeting_app_override_list: () => [
        { appName: "com.acme.huddle", kind: "meeting", createdAt: "2026-04-01T00:00:00Z" },
        { appName: "Notion", kind: "other", createdAt: "2026-04-01T00:00:00Z" },
      ],
    });
    await page.goto("/");
    await page.locator(`[data-testid="sidebar-nav-settings"]`).click();
    await page.locator('[data-testid="settings-tab-meeting"]').click();
    await shot(page, "24-settings-meeting");
  });

  test("settings: Permissions tab", async ({ page }) => {
    await installMocks(page, {
      diagnose_macos_permissions: () => ({
        bundleId: "com.khawkins.hush",
        microphoneHint: "Open System Settings → Privacy & Security → Microphone and enable Hush.",
        inputMonitoringHint:
          "Open System Settings → Privacy & Security → Input Monitoring and enable Hush.",
        canReset: true,
        statuses: {
          microphone: "granted",
          screenRecording: "denied",
          inputMonitoring: "not-determined",
        },
      }),
    });
    await page.goto("/");
    await page.locator(`[data-testid="sidebar-nav-settings"]`).click();
    await page.locator('[data-testid="settings-tab-permissions"]').click();
    await shot(page, "25-settings-permissions");
  });

  test("settings: About tab", async ({ page }) => {
    await installMocks(page);
    await page.goto("/");
    await page.locator(`[data-testid="sidebar-nav-settings"]`).click();
    await page.locator('[data-testid="settings-tab-about"]').click();
    await shot(page, "26-settings-about");
  });
});

test.describe("UX walkthrough — HUD", () => {
  test("HUD page in isolation", async ({ page }) => {
    await installMocks(page);
    await page.goto("/hud");
    // HUD has a transparent body in production; on this dev path we
    // just want to see the pill markup.
    await page.waitForTimeout(200);
    await shot(page, "30-hud");
  });
});
