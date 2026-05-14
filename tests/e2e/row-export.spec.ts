import { expect, test } from "@playwright/test";
import { gotoSection, installMocks } from "./_mock";

// E2E coverage for row-level export IPCs (#723).
//
// history_export_row_csv  — invoked by HistoryDictationRow when the
//   user picks "CSV" from the per-row export popover.
// meeting_session_export  — invoked by HistoryMeetingRow when the
//   user picks a format from the meeting export popover.
//
// Both flows call `plugin:dialog|save` first to get a path. The
// default mock returns "/Users/test/hush-export.csv" so the IPC
// fires without a real OS dialog. Per-test exposeFunction handlers
// capture the invoked command + args across the page/Node boundary.

test.describe("row-level export IPCs", () => {
  test("CSV export button on a dictation row calls history_export_row_csv with the row id", async ({
    page,
  }) => {
    let exportArgs: Record<string, unknown> | null = null;
    await page.exposeFunction("__hush_record_row_export", (args: unknown) => {
      exportArgs = args as Record<string, unknown>;
    });
    await page.exposeFunction("__hush_get_row_export_args", () => exportArgs);

    await installMocks(page, {
      history_search: () => [
        {
          id: 42,
          transcript: "Export me please.",
          appName: null,
          windowTitle: null,
          model: "base",
          durationMs: 2100,
          createdAt: "2026-05-01T10:00:00Z",
          ignored: false,
        },
      ],
      history_count: () => 1,
      history_export_row_csv: async (args) => {
        await (
          window as unknown as {
            __hush_record_row_export: (a: unknown) => Promise<void>;
          }
        ).__hush_record_row_export(args);
      },
    });

    await page.goto("/");
    await gotoSection(page, "history");

    // Open the export popover for row 42.
    const exportToggle = page.locator('[data-testid="history-export-42"]');
    await expect(exportToggle).toBeVisible();
    await exportToggle.click();

    // Click the CSV option.
    const csvItem = page.locator('[data-testid="history-export-csv-42"]');
    await expect(csvItem).toBeVisible();
    await csvItem.click();

    // Wait for the IPC to fire and verify the args.
    await expect
      .poll(() =>
        page.evaluate(() =>
          (
            window as unknown as {
              __hush_get_row_export_args: () => Promise<Record<
                string,
                unknown
              > | null>;
            }
          ).__hush_get_row_export_args(),
        ),
      )
      .not.toBeNull();

    const recorded = await page.evaluate(() =>
      (
        window as unknown as {
          __hush_get_row_export_args: () => Promise<Record<
            string,
            unknown
          > | null>;
        }
      ).__hush_get_row_export_args(),
    );

    expect(recorded).toMatchObject({ id: 42 });
  });

  test("meeting export button calls meeting_session_export with the session id and format", async ({
    page,
  }) => {
    let meetingExportArgs: Record<string, unknown> | null = null;
    await page.exposeFunction(
      "__hush_record_meeting_export",
      (args: unknown) => {
        meetingExportArgs = args as Record<string, unknown>;
      },
    );
    await page.exposeFunction(
      "__hush_get_meeting_export_args",
      () => meetingExportArgs,
    );

    await installMocks(page, {
      meeting_sessions_search: () => [
        {
          id: 99,
          appName: "Zoom",
          appKind: "conferencing",
          startedAt: "2026-05-01T14:00:00Z",
          endedAt: "2026-05-01T15:00:00Z",
          speakerCount: 2,
          utteranceCount: 10,
          notes: null,
          sources: ["mic", "system"],
          appTitle: null,
        },
      ],
      meeting_session_export: async (args) => {
        await (
          window as unknown as {
            __hush_record_meeting_export: (a: unknown) => Promise<void>;
          }
        ).__hush_record_meeting_export(args);
      },
    });

    await page.goto("/");
    await gotoSection(page, "history");

    // Switch to the Meetings sub-filter so the row renders.
    const meetingsFilter = page.locator(
      '[data-testid="history-filter-meetings"]',
    );
    if (await meetingsFilter.isVisible()) {
      await meetingsFilter.click();
    }

    // Open the export popover for session 99.
    const exportToggle = page.locator(
      '[data-testid="meeting-export-toggle-99"]',
    );
    await expect(exportToggle).toBeVisible();
    await exportToggle.click();

    // Click the CSV export option.
    const csvItem = page.locator('[data-testid="meeting-export-csv-99"]');
    await expect(csvItem).toBeVisible();
    await csvItem.click();

    // Wait for the IPC to fire.
    await expect
      .poll(() =>
        page.evaluate(() =>
          (
            window as unknown as {
              __hush_get_meeting_export_args: () => Promise<Record<
                string,
                unknown
              > | null>;
            }
          ).__hush_get_meeting_export_args(),
        ),
      )
      .not.toBeNull();

    const recorded = await page.evaluate(() =>
      (
        window as unknown as {
          __hush_get_meeting_export_args: () => Promise<Record<
            string,
            unknown
          > | null>;
        }
      ).__hush_get_meeting_export_args(),
    );

    expect(recorded).toMatchObject({ id: 99, format: "csv" });
  });
});
