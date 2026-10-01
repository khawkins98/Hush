import { expect, test } from "@playwright/test";
import { gotoSection, installMocks } from "./_mock";

// Confidence shading in the live meeting transcript (#1013).
//
// The backend only attaches word confidences to partials when it runs
// with HUSH_CONFIDENCE_SHADING=1; the mock simulates both shapes. Mock
// functions are serialized via toString(), so the two variants are
// written out in full rather than sharing a closure-captured helper.

async function startMeeting(page: import("@playwright/test").Page) {
  await page.goto("/");
  await gotoSection(page, "dictation");
  await page.locator('[data-testid="record-start-btn"]').click();
  await expect(page.locator('[data-testid="live-transcript"]')).toBeVisible();
}

const baseMocks = {
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
    (window as unknown as { __hush_active_id: number | null }).__hush_active_id = 1;
    return {
      id: 1,
      appName: "manual",
      appKind: "other",
      startedAt: "2026-05-01T15:00:00Z",
      endedAt: null,
      speakerCount: null,
      utteranceCount: 0,
      notes: null,
      sources: ["mic", "system"],
      appTitle: null,
    };
  },
  meeting_active_session: () => ({
    active: (window as unknown as { __hush_active_id: number | null }).__hush_active_id,
  }),
};

test.describe("live transcript confidence shading", () => {
  test.beforeEach(async ({ page }) => {
    await page.addInitScript(() => {
      (window as unknown as { __hush_active_id: number | null }).__hush_active_id = null;
    });
  });

  test("dims low-confidence words in a partial that carries confidences", async ({ page }) => {
    await installMocks(page, {
      ...baseMocks,
      meeting_session_get: () => ({
        session: {
          id: 1,
          appName: "manual",
          appKind: "other",
          startedAt: "2026-05-01T15:00:00Z",
          endedAt: null,
          speakerCount: null,
          utteranceCount: 0,
          notes: null,
          sources: ["mic", "system"],
          appTitle: null,
        },
        utterances: [],
        currentPartials: [
          {
            startedAtMs: 0,
            endedAtMs: 1000,
            speakerLabel: "mic",
            text: "Ship the quarterly forecast",
            isFinal: false,
            words: [
              { word: "Ship", p: 0.95 },
              { word: "the", p: 0.97 },
              { word: "quarterly", p: 0.31 },
              { word: "forecast", p: 0.9 },
            ],
          },
        ],
      }),
    });
    await startMeeting(page);
    const pane = page.locator('[data-testid="live-transcript"]');
    await expect(pane).toContainText("Ship the quarterly forecast");
    const low = pane.locator('[data-testid="low-confidence-word"]');
    await expect(low).toHaveCount(1);
    await expect(low).toHaveText("quarterly");
  });

  test("renders plain text when partials carry no confidences (shading off)", async ({
    page,
  }) => {
    await installMocks(page, {
      ...baseMocks,
      meeting_session_get: () => ({
        session: {
          id: 1,
          appName: "manual",
          appKind: "other",
          startedAt: "2026-05-01T15:00:00Z",
          endedAt: null,
          speakerCount: null,
          utteranceCount: 0,
          notes: null,
          sources: ["mic", "system"],
          appTitle: null,
        },
        utterances: [],
        currentPartials: [
          {
            startedAtMs: 0,
            endedAtMs: 1000,
            speakerLabel: "mic",
            text: "Ship the quarterly forecast",
            isFinal: false,
          },
        ],
      }),
    });
    await startMeeting(page);
    const pane = page.locator('[data-testid="live-transcript"]');
    await expect(pane).toContainText("Ship the quarterly forecast");
    await expect(pane.locator('[data-testid="low-confidence-word"]')).toHaveCount(0);
  });
});
