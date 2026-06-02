import { expect, test } from "@playwright/test";
import { gotoSection, installMocks } from "./_mock";

// Regression spec for #990: the Transcribe panel's recording timer must
// survive tab navigation. Pre-#990, RecordPanel stamped its start time as
// component-local state on mount; navigating Settings/History → Transcribe
// remounted the panel and reset the visible counter to zero while the
// actual recording kept running.
//
// The fix anchors the timer to remount-safe sources (dictation state
// module / the meeting session's backend-persisted startedAt), so a
// remount re-derives the same elapsed value. This spec drives the
// auto-detected-meeting path: `meeting_active_session` reports an active
// session whose `startedAt` is ten minutes in the past, and the timer
// must show ~10:00 — including after a round-trip through History.

test.describe("recording timer survives tab navigation (#990)", () => {
  test("timer shows session-anchored elapsed, not time-since-mount, across navigation", async ({
    page,
  }) => {
    await installMocks(page, {
      // An auto-detected meeting session is active from app launch.
      meeting_active_session: () => ({ active: 1 }),
      // The session started ten minutes ago (computed in the page
      // context at call time — mocks cannot capture closure variables,
      // but they can call Date in the page).
      meeting_session_get: () => ({
        session: {
          id: 1,
          appName: "Microsoft Teams",
          appKind: "meeting",
          startedAt: new Date(Date.now() - 10 * 60_000).toISOString(),
          endedAt: null,
          speakerCount: null,
          utteranceCount: 0,
          notes: null,
          sources: ["mic", "system"],
          appTitle: null,
          name: null,
        },
        utterances: [],
        currentPartials: [],
      }),
    });

    await page.goto("/");
    await gotoSection(page, "dictation");

    const timer = page.locator(".record-time");
    await expect(timer).toBeVisible();

    // The meeting-detail poll runs on an interval; wait for the timer to
    // pick up the session-anchored elapsed (~10 minutes). 9:5x–10:0x
    // tolerates poll latency and test runtime.
    await expect(timer).toHaveText(/^(9:5\d|10:0\d)$/, { timeout: 10_000 });

    // Navigate away (History) and back. This unmounts and remounts
    // RecordPanel — the pre-#990 bug zeroed the timer here.
    await gotoSection(page, "history");
    await gotoSection(page, "dictation");

    // The timer must still show the session-anchored elapsed, NOT a
    // freshly-restarted counter near 0:00.
    await expect(page.locator(".record-time")).toBeVisible();
    await expect(page.locator(".record-time")).toHaveText(/^(9:5\d|10:0\d)$/, {
      timeout: 10_000,
    });
  });
});
