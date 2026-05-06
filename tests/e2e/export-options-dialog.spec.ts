import { expect, test } from "@playwright/test";
import { installMocks } from "./_mock";

// E2E coverage for ExportOptionsDialog testids:
// export-options-dialog-backdrop, export-cancel, export-confirm,
// export-kind-auto, export-kind-dictation, export-kind-meetings, export-kind-both,
// export-fmt-text, export-fmt-csv, export-fmt-json
//
// The dialog opens when the user clicks history-export-bundle.
// That button only renders when onExportBundle is wired (it is in +page.svelte)
// AND historyTotalCount > 0 (or meetingSessions.length > 0).
// We override history_count → 1 to satisfy that condition.

async function openExportDialog(page: import("@playwright/test").Page) {
  await installMocks(page, {
    history_count: () => 1,
    history_list: () => [
      {
        id: 1,
        kind: "dictation",
        text: "Test transcript.",
        durationMs: 1200,
        createdAt: "2026-04-26T15:00:00Z",
      },
    ],
    // history_export_bundle is a no-op (we never actually pick a folder in tests)
    history_export_bundle: () => undefined,
  });
  await page.goto("/");
  await page.locator('[data-testid="sidebar-nav-history"]').click();
  await page.locator('[data-testid="history-export-bundle"]').click();
}

test.describe("export options dialog", () => {
  test("backdrop and dialog body appear on open", async ({ page }) => {
    await openExportDialog(page);

    await expect(
      page.locator('[data-testid="export-options-dialog-backdrop"]'),
    ).toBeVisible();
    // Both confirm and cancel buttons are in the dialog body.
    await expect(
      page.locator('[data-testid="export-cancel"]'),
    ).toBeVisible();
    await expect(
      page.locator('[data-testid="export-confirm"]'),
    ).toBeVisible();
  });

  test("cancel button closes the dialog", async ({ page }) => {
    await openExportDialog(page);

    await page.locator('[data-testid="export-cancel"]').click();
    await expect(
      page.locator('[data-testid="export-options-dialog-backdrop"]'),
    ).toHaveCount(0);
  });

  test("backdrop click closes the dialog", async ({ page }) => {
    await openExportDialog(page);

    // Click on the backdrop area itself (not on the dialog card).
    // Playwright's locator click hits the element's bounding-box centre
    // which may land inside the card — force a click on the very top
    // edge of the backdrop instead.
    await page
      .locator('[data-testid="export-options-dialog-backdrop"]')
      .click({ position: { x: 5, y: 5 } });
    await expect(
      page.locator('[data-testid="export-options-dialog-backdrop"]'),
    ).toHaveCount(0);
  });

  test("all four kind radio inputs are visible and auto is checked by default", async ({
    page,
  }) => {
    await openExportDialog(page);

    for (const kind of ["auto", "dictation", "meetings", "both"]) {
      await expect(
        page.locator(`[data-testid="export-kind-${kind}"]`),
      ).toBeVisible();
    }
    await expect(
      page.locator('[data-testid="export-kind-auto"]'),
    ).toBeChecked();
  });

  test("kind selection changes which radio is checked", async ({ page }) => {
    await openExportDialog(page);

    await page.locator('[data-testid="export-kind-dictation"]').click();
    await expect(
      page.locator('[data-testid="export-kind-dictation"]'),
    ).toBeChecked();
    await expect(
      page.locator('[data-testid="export-kind-auto"]'),
    ).not.toBeChecked();
  });

  test("all three meeting-format radio inputs are visible and text is checked by default", async ({
    page,
  }) => {
    await openExportDialog(page);

    for (const fmt of ["text", "csv", "json"]) {
      await expect(
        page.locator(`[data-testid="export-fmt-${fmt}"]`),
      ).toBeVisible();
    }
    await expect(
      page.locator('[data-testid="export-fmt-text"]'),
    ).toBeChecked();
  });

  test("meeting format selection changes which radio is checked", async ({
    page,
  }) => {
    await openExportDialog(page);

    await page.locator('[data-testid="export-fmt-csv"]').click();
    await expect(
      page.locator('[data-testid="export-fmt-csv"]'),
    ).toBeChecked();
    await expect(
      page.locator('[data-testid="export-fmt-text"]'),
    ).not.toBeChecked();
  });
});
