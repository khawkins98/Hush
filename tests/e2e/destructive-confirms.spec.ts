import { expect, test } from "@playwright/test";
import { gotoSection, installMocks } from "./_mock";

// Coverage for the per-row click-to-confirm pattern (#204) on
// destructive Delete buttons across panels:
//   - Vocabulary Delete (Settings → Vocabulary)
//   - Replacements Delete (Settings → Replacements)
//   - History row Delete (main → History)
//   - Meeting session Delete (main → Meetings)
//
// History's Clear-all is covered separately in history-clear.spec.ts;
// this file exercises the per-row confirm dance on the surfaces
// that previously fired Delete on a single click.
//
// Mock function bodies must self-contain (the installer toString()s
// and rebuilds them in the page context, so closure variables don't
// survive the bridge — every test below inlines its own fixture
// data).

test.describe("destructive confirm — Vocabulary Delete", () => {
  test("first click flips Delete to 'Click to confirm'", async ({ page }) => {
    await installMocks(page, {
      vocabulary_list: () => [{ id: 1, term: "Tauri" }],
    });
    await page.goto("/settings");
    await page.locator('[data-testid="settings-tab-vocabulary"]').click();

    const btn = page.locator('[data-testid="vocab-delete-1"]');
    await expect(btn).toHaveText("Delete");
    await btn.click();
    await expect(btn).toHaveText(/Click to confirm/i);
    await expect(btn).toHaveAttribute("aria-label", /Click again to confirm/i);
  });

  test("second click fires vocabulary_delete; first click alone doesn't", async ({
    page,
  }) => {
    const calls: number[] = [];
    await page.exposeFunction("__hush_record_vocab_delete", (id: number) => {
      calls.push(id);
    });
    await installMocks(page, {
      vocabulary_list: () => [
        { id: 7, term: "Tauri" },
        { id: 8, term: "ggml" },
      ],
      vocabulary_delete: (args) => {
        const { id } = (args ?? {}) as { id: number };
        (
          window as unknown as {
            __hush_record_vocab_delete: (id: number) => void;
          }
        ).__hush_record_vocab_delete(id);
        return undefined;
      },
    });
    await page.goto("/settings");
    await page.locator('[data-testid="settings-tab-vocabulary"]').click();

    // First click — armed, no IPC.
    await page.locator('[data-testid="vocab-delete-7"]').click();
    expect(calls).toEqual([]);

    // Second click on the SAME row — fires.
    await page.locator('[data-testid="vocab-delete-7"]').click();
    await expect.poll(() => calls.length).toBe(1);
    expect(calls[0]).toBe(7);
  });
});

test.describe("destructive confirm — Replacements Delete", () => {
  test("two-click confirm flow fires the IPC exactly once", async ({
    page,
  }) => {
    const calls: number[] = [];
    await page.exposeFunction("__hush_record_replace_delete", (id: number) => {
      calls.push(id);
    });
    await installMocks(page, {
      replacements_list: () => [
        { id: 4, findText: "um ", replaceText: "", sortOrder: 0 },
      ],
      replacement_delete: (args) => {
        const { id } = (args ?? {}) as { id: number };
        (
          window as unknown as {
            __hush_record_replace_delete: (id: number) => void;
          }
        ).__hush_record_replace_delete(id);
        return undefined;
      },
    });
    await page.goto("/settings");
    await page.locator('[data-testid="settings-tab-replacements"]').click();

    const btn = page.locator('[data-testid="replacement-delete-4"]');
    await btn.click();
    await expect(btn).toHaveText(/Click to confirm/i);
    expect(calls).toEqual([]);
    await btn.click();
    await expect.poll(() => calls.length).toBe(1);
    expect(calls[0]).toBe(4);
  });
});

test.describe("destructive confirm — History row Delete", () => {
  test("two-click confirm flow fires history_delete exactly once", async ({
    page,
  }) => {
    const calls: number[] = [];
    await page.exposeFunction("__hush_record_hist_delete", (id: number) => {
      calls.push(id);
    });
    await installMocks(page, {
      history_search: () => [
        {
          id: 11,
          transcript: "first",
          appName: null,
          windowTitle: null,
          model: "ggml-base.bin",
          durationMs: 1234,
          createdAt: "2026-04-26T15:00:00Z",
        },
      ],
      history_count: () => 1,
      history_delete: (args) => {
        const { id } = (args ?? {}) as { id: number };
        (
          window as unknown as {
            __hush_record_hist_delete: (id: number) => void;
          }
        ).__hush_record_hist_delete(id);
        return undefined;
      },
    });
    await page.goto("/");
    await gotoSection(page, "history");
    await expect(page.locator(".history-row")).toHaveCount(1);

    const btn = page.locator('[data-testid="history-delete-11"]');
    await btn.click();
    await expect(btn).toHaveText(/Click to confirm/i);
    expect(calls).toEqual([]);
    await btn.click();
    await expect.poll(() => calls.length).toBe(1);
    expect(calls[0]).toBe(11);
  });
});

// Phase 1 of #357 dropped the standalone Meetings panel from the
// main-window sidebar; the meeting-session Delete affordance lives
// in that panel. Skip until Phase 2 reintroduces meetings as part
// of the unified History feed (the Delete + two-click confirm
// pattern will move with the row component).
test.describe.skip("destructive confirm — Meeting session Delete", () => {
  test("two-click confirm flow fires meeting_session_delete exactly once", async ({
    page,
  }) => {
    const calls: number[] = [];
    await page.exposeFunction(
      "__hush_record_meeting_delete",
      (id: number) => {
        calls.push(id);
      },
    );
    await installMocks(page, {
      meeting_sessions_list: () => [
        {
          id: 22,
          appName: "us.zoom.xos",
          appKind: "meeting",
          startedAt: "2026-04-26T15:00:00Z",
          endedAt: "2026-04-26T15:30:00Z",
          speakerCount: null,
          utteranceCount: 12,
          notes: null,
        },
      ],
      meeting_session_delete: (args) => {
        const { id } = (args ?? {}) as { id: number };
        (
          window as unknown as {
            __hush_record_meeting_delete: (id: number) => void;
          }
        ).__hush_record_meeting_delete(id);
        return undefined;
      },
    });
    await page.goto("/");
    await gotoSection(page, "meetings");

    const btn = page.locator('[data-testid="meeting-session-delete-22"]');
    await expect(btn).toBeVisible();
    await btn.click();
    await expect(btn).toHaveText(/Click to confirm/i);
    expect(calls).toEqual([]);
    await btn.click();
    await expect.poll(() => calls.length).toBe(1);
    expect(calls[0]).toBe(22);
  });
});
