import { expect, test } from "@playwright/test";
import { gotoSection, installMocks } from "./_mock";

// History load failure (#1013 item 9): the error card must offer a
// Retry that re-runs the load, and the "Nothing here yet" empty
// state must not render underneath it — we don't know the feed is
// empty when we couldn't read it.

test.describe("history load error", () => {
  test("shows Retry and no empty state; Retry reloads the feed", async ({
    page,
  }) => {
    await installMocks(page, {
      // First call fails, later calls succeed. The counter lives on
      // `window` because mock bodies are toString()'d into the page
      // and can't close over test-side variables.
      history_search: () => {
        const w = window as unknown as { __historyCalls?: number };
        w.__historyCalls = (w.__historyCalls ?? 0) + 1;
        if (w.__historyCalls === 1) {
          throw { kind: "history", message: "database is locked" };
        }
        return [
          {
            id: 1,
            transcript: "recovered row",
            appName: null,
            windowTitle: null,
            model: "ggml-base.bin",
            durationMs: 1200,
            createdAt: "2026-09-30T11:00:00Z",
            ignored: false,
            name: null,
          },
        ];
      },
      history_count: () => 1,
    });
    await page.goto("/");
    await gotoSection(page, "history");

    const alert = page.getByRole("alert").filter({ hasText: "Dictation history" });
    await expect(alert).toBeVisible();
    await expect(page.locator(".empty-history")).toHaveCount(0);
    await expect(page.getByText("Nothing here yet.")).toHaveCount(0);

    await alert.getByRole("button", { name: "Retry" }).click();

    await expect(page.locator(".history-row")).toHaveCount(1);
    await expect(page.getByText("recovered row")).toBeVisible();
    await expect(alert).toHaveCount(0);
  });

  test("non-load errors don't get a Retry button", async ({ page }) => {
    // A failed row delete reuses the same error slot; "retry the
    // list" would be the wrong recovery there.
    await installMocks(page, {
      history_search: () => [
        {
          id: 1,
          transcript: "row",
          appName: null,
          windowTitle: null,
          model: "ggml-base.bin",
          durationMs: 1200,
          createdAt: "2026-09-30T11:00:00Z",
          ignored: false,
          name: null,
        },
      ],
      history_count: () => 1,
      history_delete: () => {
        throw { kind: "history", message: "delete failed" };
      },
    });
    await page.goto("/");
    await gotoSection(page, "history");
    await page.locator('[data-testid="history-delete-1"]').click();
    await page.locator('[data-testid="history-delete-1"]').click();

    const alert = page.getByRole("alert").filter({ hasText: "Dictation history" });
    await expect(alert).toBeVisible();
    await expect(alert.getByRole("button", { name: "Retry" })).toHaveCount(0);
  });
});
