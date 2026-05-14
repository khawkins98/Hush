import { expect, test } from "@playwright/test";
import { gotoSection, installMocks } from "./_mock";

// E2E coverage for the History panel's "Clear all" affordance
// (#198). The button uses a click-to-confirm dance: first click
// reveals a danger-styled confirm, second click fires the IPC,
// Cancel reverts. Mock function bodies must self-contain (the
// installer toString()s and rebuilds them in the page context, so
// closure variables don't survive the bridge — every test below
// inlines its own fixture data).

test.describe("history clear-all", () => {
  test("button is hidden when history is empty", async ({ page }) => {
    // Default mock has empty history + count=0 — the button must not
    // render so an empty-state user can't second-guess what they're
    // about to delete.
    await installMocks(page);
    await page.goto("/");
    await gotoSection(page, "history");
    await expect(
      page.locator('[data-testid="history-clear-all"]'),
    ).toHaveCount(0);
  });

  test("first click reveals confirm prompt with the row count", async ({
    page,
  }) => {
    await installMocks(page, {
      history_search: () => [
        {
          id: 1,
          transcript: "first",
          appName: "TestApp",
          windowTitle: null,
          model: "ggml-base.bin",
          durationMs: 1234,
          createdAt: "2026-04-26T15:00:00Z",
          ignored: false,
        },
        {
          id: 2,
          transcript: "second",
          appName: null,
          windowTitle: null,
          model: "ggml-base.bin",
          durationMs: 5678,
          createdAt: "2026-04-26T15:01:00Z",
          ignored: false,
        },
      ],
      history_count: () => 2,
    });
    await page.goto("/");
    await gotoSection(page, "history");

    // Wait for the rows to render before reaching for the clear
    // button — `historyTotalCount` lands via the same Promise.all
    // refresh as the entries themselves, so visible rows prove
    // the count fetch resolved.
    await expect(page.locator(".history-row")).toHaveCount(2);

    const clearBtn = page.locator('[data-testid="history-clear-all"]');
    await expect(clearBtn).toBeVisible();
    await clearBtn.click();

    // Confirm prompt replaces the button. Copy must include the
    // total count so the user knows the scope.
    await expect(
      page.locator('[data-testid="history-clear-all"]'),
    ).toHaveCount(0);
    await expect(
      page.locator('[data-testid="history-clear-confirm"]'),
    ).toBeVisible();
    await expect(page.locator(".clear-confirm-text")).toContainText(
      "Delete all 2?",
    );
  });

  test("Cancel reverts without firing the IPC", async ({ page }) => {
    const calls: string[] = [];
    await page.exposeFunction("__hush_record_clear", () => {
      calls.push("clear");
    });
    await installMocks(page, {
      history_search: () => [
        {
          id: 1,
          transcript: "first",
          appName: null,
          windowTitle: null,
          model: "ggml-base.bin",
          durationMs: 1234,
          createdAt: "2026-04-26T15:00:00Z",
          ignored: false,
        },
      ],
      history_count: () => 1,
      history_clear: () => {
        (
          window as unknown as { __hush_record_clear: () => void }
        ).__hush_record_clear();
        return 1;
      },
    });
    await page.goto("/");
    await gotoSection(page, "history");
    await expect(page.locator(".history-row")).toHaveCount(1);

    await page.locator('[data-testid="history-clear-all"]').click();
    await page.locator('[data-testid="history-clear-cancel"]').click();

    await expect(
      page.locator('[data-testid="history-clear-all"]'),
    ).toBeVisible();
    expect(calls).toEqual([]);
  });

  test("second click fires history_clear and empties the list", async ({
    page,
  }) => {
    await installMocks(page, {
      history_search: () => [
        {
          id: 1,
          transcript: "first",
          appName: null,
          windowTitle: null,
          model: "ggml-base.bin",
          durationMs: 1234,
          createdAt: "2026-04-26T15:00:00Z",
          ignored: false,
        },
        {
          id: 2,
          transcript: "second",
          appName: null,
          windowTitle: null,
          model: "ggml-base.bin",
          durationMs: 5678,
          createdAt: "2026-04-26T15:01:00Z",
          ignored: false,
        },
      ],
      history_count: () => 2,
      history_clear: () => 2,
    });
    await page.goto("/");
    await gotoSection(page, "history");
    await expect(page.locator(".history-row")).toHaveCount(2);

    await page.locator('[data-testid="history-clear-all"]').click();
    await page.locator('[data-testid="history-clear-confirm"]').click();

    // Optimistic empty + the empty-state placeholder takes over.
    // `clearAllHistory` zeroes both `historyEntries` and
    // `historyTotalCount` immediately so the panel re-renders
    // without waiting on a refresh round-trip.
    await expect(page.locator(".history-row")).toHaveCount(0);
    await expect(page.locator(".empty-history")).toBeVisible();
    // Clear button is gone again because the total is now 0.
    await expect(
      page.locator('[data-testid="history-clear-all"]'),
    ).toHaveCount(0);
  });
});
