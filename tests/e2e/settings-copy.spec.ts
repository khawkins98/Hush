import { expect, test, type Page } from "@playwright/test";
import { installMocks } from "./_mock";

// Copy / layout fixes from #1013 item 9: the fixed toggle hotkey
// row, the model-picker legend + English-only badge, Personal
// Vocabulary ordering, and the trimmed meeting auto-start card.

async function openTab(page: Page, tab: string) {
  await page.goto("/");
  await page.locator(`[data-testid="sidebar-nav-settings"]`).click();
  await page.locator(`[data-testid="settings-tab-${tab}"]`).click();
}

test("General: toggle hotkey row states the fixed chord", async ({ page }) => {
  await installMocks(page);
  await page.goto("/");
  await page.locator(`[data-testid="sidebar-nav-settings"]`).click();

  const row = page.locator('[data-testid="settings-toggle-hotkey-row"]');
  // `@tauri-apps/plugin-os` platform() throws outside Tauri, so the
  // e2e page may render either form; both must name the same chord.
  await expect(row).toContainText(/⌃⌥H|Ctrl \+ Alt \+ H/);
  await expect(row).toContainText("(fixed)");
  await expect(row).toContainText("push-to-talk");
  await expect(row).not.toContainText("Not currently editable");
});

test("Model: legend explains compact + English only; badge on .en cards", async ({
  page,
}) => {
  await installMocks(page, {
    model_list: () => [
      {
        id: "whisper-small-q8_0",
        displayName: "Whisper Small (compact)",
        filename: "ggml-small-q8_0.bin",
        sizeMb: 264,
        speedRating: 7,
        accuracyRating: 8,
        description: "Recommended default.",
        isDefault: true,
        downloadUrl: "https://example.test/a.bin",
        sha256: "abc",
        englishOnly: false,
        isDownloaded: true,
        isSelected: true,
        expectedPath: "/tmp/a.bin",
      },
      {
        id: "whisper-small.en-q8_0",
        displayName: "Whisper Small (English only, compact)",
        filename: "ggml-small.en-q8_0.bin",
        sizeMb: 264,
        speedRating: 7,
        accuracyRating: 8,
        description: "English-only build of Small.",
        isDefault: false,
        downloadUrl: "https://example.test/b.bin",
        sha256: "abc",
        englishOnly: true,
        isDownloaded: false,
        isSelected: false,
        expectedPath: "/tmp/b.bin",
      },
    ],
  });
  await openTab(page, "model");

  const legend = page.locator('[data-testid="model-legend"]');
  await expect(legend).toContainText("quantized");
  await expect(legend).toContainText("can't transcribe other languages");

  const badges = page.locator('[data-testid="model-english-only-badge"]');
  await expect(badges).toHaveCount(1);
  await expect(
    page.locator(".model-card", { hasText: "Whisper Small (English only, compact)" })
      .locator('[data-testid="model-english-only-badge"]'),
  ).toBeVisible();
});

test("Vocabulary: Personal Vocabulary is the first section", async ({ page }) => {
  await installMocks(page);
  await openTab(page, "vocabulary");

  const headings = page.locator(".settings-content h2");
  await expect(headings.first()).toHaveText("Personal Vocabulary");
  await expect(page.getByText("Replacements above")).toHaveCount(0);
  await expect(page.getByText("Unlike Replacements (its own tab)")).toBeVisible();
});

test("Meeting: auto-start detail lives behind a How it works disclosure", async ({
  page,
}) => {
  await installMocks(page);
  await openTab(page, "meeting");

  const how = page.locator('[data-testid="settings-meeting-autostart-how"]');
  await expect(how).toBeVisible();
  await expect(how).not.toHaveAttribute("open", /.*/);
  await how.locator("summary").click();
  await expect(how).toContainText("releases the mic");
  // The select keeps its test id for existing specs.
  await expect(page.locator('[data-testid="settings-meeting-autostart"]')).toBeVisible();
});
