// Fixed-theme UI screenshot export — captures every significant screen
// using the single brand palette so designers can use them as reference
// material without needing a running Tauri build.
//
// Run:    npm run test:screenshots
// Output: tmp/uxwalk/brand/*.png (tmp/ is .gitignored)
//
// Viewport matches the actual macOS window (800×600) unless overridden
// per describe block (e.g. HUD is 290×60).

import * as fs from "fs";
import * as path from "path";
import { expect, test } from "@playwright/test";
import { installMocks } from "./_mock";

const OUT_DIR = path.join(process.cwd(), "tmp", "uxwalk");

for (const theme of ["brand"] as const) {
  const dir = path.join(OUT_DIR, theme);

  async function shot(page: import("@playwright/test").Page, name: string) {
    fs.mkdirSync(dir, { recursive: true });
    await page.waitForLoadState("networkidle").catch(() => undefined);
    await page.waitForTimeout(400);
    await page.screenshot({ path: path.join(dir, `${name}.png`), fullPage: false });
  }

  // Fixed single-palette theme — retained as a no-op helper so the
  // screenshot call sites stay linear.
  async function forceTheme(_page: import("@playwright/test").Page) {}

  test.describe(`${theme} mode — main window`, () => {
    test.use({ viewport: { width: 800, height: 600 } });

    test("dictation: idle, no model installed", async ({ page }) => {
      await forceTheme(page);
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
            isDownloaded: false,
            isSelected: false,
            expectedPath: "/Users/k/Library/Application Support/Hush/models/ggml-base.bin",
          },
        ],
      });
      await page.goto("/");
      await expect(page.getByRole("heading", { name: "Transcribe" })).toBeVisible();
      await shot(page, "01-dictation-no-model");
    });

    test("dictation: idle, model installed, perms granted", async ({ page }) => {
      await forceTheme(page);
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
          bundleId: "io.github.khawkins98.hush",
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
      await expect(page.getByRole("heading", { name: "Transcribe" })).toBeVisible();
      await shot(page, "02-dictation-perms-ok");
    });

    test("dictation: permission denied warning", async ({ page }) => {
      await forceTheme(page);
      await installMocks(page, {
        diagnose_macos_permissions: () => ({
          bundleId: "io.github.khawkins98.hush",
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
      await forceTheme(page);
      await installMocks(page, {
        get_first_run_completed: () => false,
      });
      await page.goto("/");
      await expect(page.getByRole("heading", { name: "Permissions" })).toBeVisible();
      await shot(page, "04-first-run-modal");
    });

    test("history: empty state", async ({ page }) => {
      await forceTheme(page);
      await installMocks(page);
      await page.goto("/");
      await page.locator(`[data-testid="sidebar-nav-history"]`).click();
      await shot(page, "08-history-empty");
    });

    test("history: populated", async ({ page }) => {
      await forceTheme(page);
      await installMocks(page, {
        history_count: () => 4,
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
            ignored: false,
            model: "ggml-base.bin",
            durationMs: 4200,
            source: "Mail",
          },
          {
            id: 2,
            transcript:
              "Reminder: pick up groceries on the way home, including milk, eggs, and bread.",
            createdAt: "2026-04-29T09:30:00Z",
            ignored: false,
            model: "ggml-base.bin",
            durationMs: 9800,
            source: null,
          },
          {
            id: 3,
            transcript: "Hello world.",
            createdAt: "2026-04-28T16:15:00Z",
            ignored: false,
            model: "ggml-base.bin",
            durationMs: 600,
            source: "Slack",
          },
          {
            id: 4,
            transcript:
              "A longer transcript that wraps over two lines so we can see the row's vertical rhythm.",
            createdAt: "2026-04-28T11:00:00Z",
            ignored: false,
            model: "ggml-base.bin",
            durationMs: 14500,
            source: null,
          },
          {
            id: 5,
            transcript: "",
            createdAt: "2026-04-28T10:55:00Z",
            ignored: true,
            model: "",
            durationMs: 300,
            source: null,
          },
        ],
      });
      await page.goto("/");
      await page.locator(`[data-testid="sidebar-nav-history"]`).click();
      await shot(page, "09-history-populated");
    });

    test("history: row delete armed", async ({ page }) => {
      await forceTheme(page);
      await installMocks(page, {
        history_count: () => 1,
        history_search: () => [
          {
            id: 1,
            transcript: "About to delete this.",
            createdAt: "2026-04-29T11:00:00Z",
            ignored: false,
            model: "ggml-base.bin",
            durationMs: 1500,
            source: null,
          },
        ],
      });
      await page.goto("/");
      await page.locator(`[data-testid="sidebar-nav-history"]`).click();
      await expect(page.locator(".history-row").first()).toBeVisible();
      await page.locator('[data-testid="history-delete-1"]').click();
      await shot(page, "10-history-row-delete-armed");
    });

    test("history: clear all confirm", async ({ page }) => {
      await forceTheme(page);
      await installMocks(page, {
        history_count: () => 5,
        history_search: () => [
          {
            id: 1,
            transcript: "x",
            createdAt: "2026-04-29T11:00:00Z",
            ignored: false,
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

  test.describe(`${theme} mode — settings`, () => {
    test.use({ viewport: { width: 800, height: 600 } });

    test("settings: General tab", async ({ page }) => {
      await forceTheme(page);
      await installMocks(page);
      await page.goto("/");
      await page.locator(`[data-testid="sidebar-nav-settings"]`).click();
      await expect(page.getByRole("heading", { name: "General" })).toBeVisible();
      await shot(page, "20-settings-general");
    });

    test("settings: Model tab", async ({ page }) => {
      await forceTheme(page);
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

    test("settings: Vocabulary tab", async ({ page }) => {
      await forceTheme(page);
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

    test("settings: Replacements tab", async ({ page }) => {
      await forceTheme(page);
      await installMocks(page, {
        replacements_list: () => [
          {
            id: 1,
            findText: "btw",
            replaceText: "by the way",
            createdAt: "2026-04-01T00:00:00Z",
          },
          {
            id: 2,
            findText: "imo",
            replaceText: "in my opinion",
            createdAt: "2026-04-01T00:00:00Z",
          },
          {
            id: 3,
            findText: "asap",
            replaceText: "as soon as possible",
            createdAt: "2026-04-01T00:00:00Z",
          },
        ],
      });
      await page.goto("/");
      await page.locator(`[data-testid="sidebar-nav-settings"]`).click();
      await page.locator('[data-testid="settings-tab-replacements"]').click();
      await shot(page, "23-settings-replacements");
    });

    test("settings: Meeting tab", async ({ page }) => {
      await forceTheme(page);
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
      await forceTheme(page);
      await installMocks(page, {
        diagnose_macos_permissions: () => ({
          bundleId: "io.github.khawkins98.hush",
          microphoneHint:
            "Open System Settings → Privacy & Security → Microphone and enable Hush.",
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

    test("settings: About", async ({ page }) => {
      await forceTheme(page);
      await installMocks(page);
      await page.goto("/");
      await page.locator(`[data-testid="sidebar-nav-about"]`).click();
      await shot(page, "26-settings-about");
    });
  });

  test.describe(`${theme} mode — HUD`, () => {
    test.use({ viewport: { width: 290, height: 60 } });

    test("HUD: idle pill", async ({ page }) => {
      await forceTheme(page);
      await installMocks(page);
      await page.goto("/hud");
      await page.waitForTimeout(200);
      await shot(page, "30-hud");
    });
  });
}
