import { expect, test } from "@playwright/test";
import { fireEvent, gotoSection, installMocks } from "./_mock";

// Meeting row e2e specs — unified History feed (#357 phase 2).
//
// The standalone Meetings panel was removed in #357 phase 1; meetings
// now appear as rows inside the unified History feed alongside
// dictation entries. These specs cover:
//
//   1. Row rendering — metadata (app name, duration, utterance count)
//   2. Transcript expand: calls meeting_session_get, shows utterances
//   3. Transcript expand: empty session shows appropriate message
//   4. Expand toggles (second click collapses)
//   5. Meetings-only filter chip hides dictation rows
//
// Export (row CSV/JSON/text) is covered by row-export.spec.ts.
// Delete (two-click confirm) is covered by destructive-confirms.spec.ts.
//
// IMPORTANT: mock functions are serialized via toString() and rebuilt
// inside the page context, so they cannot close over variables defined
// outside the function body. All mock return values must be inline
// literals. See learnings.md (2026-05-06 closure-capture entry).

test.describe("meeting rows in unified History feed", () => {
  test("meeting session row renders app name and utterance count", async ({
    page,
  }) => {
    await installMocks(page, {
      meeting_sessions_list: () => [
        {
          id: 55,
          appName: "us.zoom.xos",
          appKind: "meeting",
          startedAt: "2026-05-01T14:00:00Z",
          endedAt: "2026-05-01T14:30:00Z",
          speakerCount: 2,
          utteranceCount: 3,
          notes: null,
          sources: ["mic", "system"],
          appTitle: null,
        },
      ],
    });
    await page.goto("/");
    await gotoSection(page, "history");

    const row = page.locator('[data-meeting-id="55"]');
    await expect(row).toBeVisible();
    await expect(row).toContainText("us.zoom.xos");
    await expect(row).toContainText("3 utterances");
  });

  test("clicking the row content expands and calls meeting_session_get", async ({
    page,
  }) => {
    const calls: number[] = [];
    await page.exposeFunction(
      "__hush_record_session_get",
      (id: number) => {
        calls.push(id);
      },
    );
    await installMocks(page, {
      meeting_sessions_list: () => [
        {
          id: 55,
          appName: "us.zoom.xos",
          appKind: "meeting",
          startedAt: "2026-05-01T14:00:00Z",
          endedAt: "2026-05-01T14:30:00Z",
          speakerCount: 2,
          utteranceCount: 3,
          notes: null,
          sources: ["mic", "system"],
          appTitle: null,
        },
      ],
      meeting_session_get: (args: unknown) => {
        const { id } = (args ?? {}) as { id: number };
        (
          window as unknown as {
            __hush_record_session_get: (id: number) => void;
          }
        ).__hush_record_session_get(id);
        return {
          session: {
            id: 55,
            appName: "us.zoom.xos",
            appKind: "meeting",
            startedAt: "2026-05-01T14:00:00Z",
            endedAt: "2026-05-01T14:30:00Z",
            speakerCount: 2,
            utteranceCount: 3,
            notes: null,
            sources: ["mic", "system"],
            appTitle: null,
          },
          utterances: [
            {
              id: 1,
              sessionId: 55,
              startedAtMs: 0,
              endedAtMs: 5000,
              speakerLabel: "mic",
              text: "Hello from mic.",
              isFinal: true,
            },
            {
              id: 2,
              sessionId: 55,
              startedAtMs: 5000,
              endedAtMs: 10000,
              speakerLabel: "system",
              text: "Hello from system.",
              isFinal: true,
            },
          ],
          currentPartials: [],
        };
      },
    });
    await page.goto("/");
    await gotoSection(page, "history");

    const row = page.locator('[data-meeting-id="55"]');
    await row.locator('[role="button"]').first().click();

    await expect.poll(() => calls.length).toBeGreaterThan(0);
    expect(calls[0]).toBe(55);
    // Transcript is visible
    await expect(row).toContainText("Hello from mic.");
    await expect(row).toContainText("Hello from system.");
  });

  test("expanded row shows speaker labels when ≥2 distinct speakers", async ({
    page,
  }) => {
    await installMocks(page, {
      meeting_sessions_list: () => [
        {
          id: 55,
          appName: "us.zoom.xos",
          appKind: "meeting",
          startedAt: "2026-05-01T14:00:00Z",
          endedAt: "2026-05-01T14:30:00Z",
          speakerCount: 2,
          utteranceCount: 2,
          notes: null,
          sources: ["mic", "system"],
          appTitle: null,
        },
      ],
      meeting_session_get: () => ({
        session: {
          id: 55,
          appName: "us.zoom.xos",
          appKind: "meeting",
          startedAt: "2026-05-01T14:00:00Z",
          endedAt: "2026-05-01T14:30:00Z",
          speakerCount: 2,
          utteranceCount: 2,
          notes: null,
          sources: ["mic", "system"],
          appTitle: null,
        },
        utterances: [
          {
            id: 1,
            sessionId: 55,
            startedAtMs: 0,
            endedAtMs: 5000,
            speakerLabel: "mic",
            text: "Hello from mic.",
            isFinal: true,
          },
          {
            id: 2,
            sessionId: 55,
            startedAtMs: 5000,
            endedAtMs: 10000,
            speakerLabel: "system",
            text: "Hello from system.",
            isFinal: true,
          },
        ],
        currentPartials: [],
      }),
    });
    await page.goto("/");
    await gotoSection(page, "history");

    const row = page.locator('[data-meeting-id="55"]');
    await row.locator('[role="button"]').first().click();

    const transcript = row.locator('[aria-label="Meeting transcript"]');
    await expect(transcript).toBeVisible();
    // Speaker labels "You" and "Remote" appear when ≥2 distinct
    // speakerLabel values are present.
    await expect(transcript).toContainText("You");
    await expect(transcript).toContainText("Remote");
  });

  test("session with no utterances shows appropriate message after expand", async ({
    page,
  }) => {
    await installMocks(page, {
      meeting_sessions_list: () => [
        {
          id: 55,
          appName: "us.zoom.xos",
          appKind: "meeting",
          startedAt: "2026-05-01T14:00:00Z",
          endedAt: "2026-05-01T14:30:00Z",
          speakerCount: null,
          utteranceCount: 0,
          notes: null,
          sources: null,
          appTitle: null,
        },
      ],
      meeting_session_get: () => ({
        session: {
          id: 55,
          appName: "us.zoom.xos",
          appKind: "meeting",
          startedAt: "2026-05-01T14:00:00Z",
          endedAt: "2026-05-01T14:30:00Z",
          speakerCount: null,
          utteranceCount: 0,
          notes: null,
          sources: null,
          appTitle: null,
        },
        utterances: [],
        currentPartials: [],
      }),
    });
    await page.goto("/");
    await gotoSection(page, "history");

    const row = page.locator('[data-meeting-id="55"]');
    await row.locator('[role="button"]').first().click();

    await expect(row).toContainText("didn't capture any speech");
    await expect(row.locator('[aria-label="Meeting transcript"]')).toHaveCount(
      0,
    );
  });

  test("clicking the expand button again collapses the transcript", async ({
    page,
  }) => {
    await installMocks(page, {
      meeting_sessions_list: () => [
        {
          id: 55,
          appName: "us.zoom.xos",
          appKind: "meeting",
          startedAt: "2026-05-01T14:00:00Z",
          endedAt: "2026-05-01T14:30:00Z",
          speakerCount: 2,
          utteranceCount: 2,
          notes: null,
          sources: ["mic", "system"],
          appTitle: null,
        },
      ],
      meeting_session_get: () => ({
        session: {
          id: 55,
          appName: "us.zoom.xos",
          appKind: "meeting",
          startedAt: "2026-05-01T14:00:00Z",
          endedAt: "2026-05-01T14:30:00Z",
          speakerCount: 2,
          utteranceCount: 2,
          notes: null,
          sources: ["mic", "system"],
          appTitle: null,
        },
        utterances: [
          {
            id: 1,
            sessionId: 55,
            startedAtMs: 0,
            endedAtMs: 5000,
            speakerLabel: "mic",
            text: "Hello from mic.",
            isFinal: true,
          },
          {
            id: 2,
            sessionId: 55,
            startedAtMs: 5000,
            endedAtMs: 10000,
            speakerLabel: "system",
            text: "Hello from system.",
            isFinal: true,
          },
        ],
        currentPartials: [],
      }),
    });
    await page.goto("/");
    await gotoSection(page, "history");

    const row = page.locator('[data-meeting-id="55"]');
    const expandBtn = row.locator('[data-testid="meeting-show-transcript-55"]');

    // First click expands.
    await expandBtn.click();
    await expect(row.locator('[aria-label="Meeting transcript"]')).toBeVisible();

    // Second click collapses.
    await expandBtn.click();
    await expect(
      row.locator('[aria-label="Meeting transcript"]'),
    ).toHaveCount(0);
  });

  test("Meetings filter chip hides dictation rows and shows only meeting rows", async ({
    page,
  }) => {
    await installMocks(page, {
      history_search: () => [
        {
          id: 1,
          transcript: "A dictation entry",
          appName: null,
          windowTitle: null,
          model: "base",
          durationMs: 2000,
          createdAt: "2026-05-01T13:00:00Z",
          ignored: false,
        },
      ],
      history_count: () => 1,
      meeting_sessions_list: () => [
        {
          id: 55,
          appName: "us.zoom.xos",
          appKind: "meeting",
          startedAt: "2026-05-01T14:00:00Z",
          endedAt: "2026-05-01T14:30:00Z",
          speakerCount: 2,
          utteranceCount: 3,
          notes: null,
          sources: ["mic", "system"],
          appTitle: null,
        },
      ],
    });
    await page.goto("/");
    await gotoSection(page, "history");

    // Both kinds visible initially.
    await expect(page.locator('[data-kind="dictation"]')).toBeVisible();
    await expect(page.locator('[data-kind="meeting"]')).toBeVisible();

    // Apply meetings-only filter.
    await page
      .locator('[data-testid="history-filter-meetings"]')
      .click();

    await expect(page.locator('[data-kind="dictation"]')).toHaveCount(0);
    await expect(page.locator('[data-kind="meeting"]')).toBeVisible();
  });
});

// Active-session flow (#782): start → active → live transcript → stop.
//
// These specs exercise the dictation panel (not the history feed). They
// verify that clicking the record button in meeting mode starts a
// session, the stop button appears, and the live-transcript pane shows
// partial text. They also verify the source-failed and session-ended
// event paths.
//
// System audio must be marked `isSupported: true` so `willRecordMeeting`
// is true and the record button shows in meeting mode. The `__hush_active_id`
// window variable is used to control what `meeting_active_session` returns
// before vs. after the session starts — `meeting_start_manual` sets it
// as a side effect so the subsequent `meeting.refresh()` sees the new id.
test.describe("active meeting session flow", () => {
  test("clicking record button in meeting mode starts an active session", async ({
    page,
  }) => {
    // Seed the window variable the mocks read — must exist before the
    // init script runs, so use addInitScript rather than page.evaluate.
    await page.addInitScript(() => {
      (window as unknown as { __hush_active_id: number | null }).__hush_active_id =
        null;
    });

    await installMocks(page, {
      // Enable system audio so `willRecordMeeting` is true.
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
      // meeting_start_manual sets the shared window variable as a side
      // effect so the next meeting_active_session call (in refresh())
      // returns the active id rather than null.
      meeting_start_manual: () => {
        (window as unknown as { __hush_active_id: number | null }).__hush_active_id =
          1;
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
        active: (window as unknown as { __hush_active_id: number | null })
          .__hush_active_id,
      }),
      // refreshActiveDetail calls meeting_session_get once activeId is set.
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
        currentPartials: [],
      }),
    });

    await page.goto("/");
    await gotoSection(page, "dictation");

    // Before clicking: start button in meeting mode.
    const startBtn = page.locator(
      '[data-testid="record-start-btn"][data-record-mode="meeting"]',
    );
    await expect(startBtn).toBeVisible();

    await startBtn.click();

    // After start: stop button replaces the start button.
    // Note: button-click path sets `recording=true` (dictation phase),
    // not `meetingOnlyActive=true`, so the aria-label is "Stop recording and transcribe".
    const stopBtn = page.locator("button.record-btn.recording");
    await expect(stopBtn).toBeVisible();
    // Start button is gone.
    await expect(
      page.locator('[data-testid="record-start-btn"]'),
    ).toHaveCount(0);
  });

  test("active meeting session shows live transcript with partial text", async ({
    page,
  }) => {
    await page.addInitScript(() => {
      (window as unknown as { __hush_active_id: number | null }).__hush_active_id =
        null;
    });

    await installMocks(page, {
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
        (window as unknown as { __hush_active_id: number | null }).__hush_active_id =
          1;
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
        active: (window as unknown as { __hush_active_id: number | null })
          .__hush_active_id,
      }),
      // Return a partial utterance so the live-transcript pane is visible.
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
            id: -1,
            sessionId: 1,
            startedAtMs: 0,
            endedAtMs: null,
            speakerLabel: "mic",
            text: "Testing one two three.",
            isFinal: false,
          },
        ],
      }),
    });

    await page.goto("/");
    await gotoSection(page, "dictation");

    await page.locator('[data-testid="record-start-btn"]').click();

    // Live transcript section appears and shows the partial text.
    await expect(
      page.locator('[data-testid="live-transcript"]'),
    ).toBeVisible();
    await expect(page.locator('[data-testid="live-transcript"]')).toContainText(
      "Testing one two three.",
    );
  });

  test("stopping an active session clears active state", async ({ page }) => {
    await page.addInitScript(() => {
      (window as unknown as { __hush_active_id: number | null }).__hush_active_id =
        null;
    });

    await installMocks(page, {
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
        (window as unknown as { __hush_active_id: number | null }).__hush_active_id =
          1;
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
      // meeting_stop_manual clears the active id so the subsequent
      // refresh() call returns null and the UI returns to idle.
      meeting_stop_manual: () => {
        (window as unknown as { __hush_active_id: number | null }).__hush_active_id =
          null;
      },
      meeting_active_session: () => ({
        active: (window as unknown as { __hush_active_id: number | null })
          .__hush_active_id,
      }),
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
        currentPartials: [],
      }),
    });

    await page.goto("/");
    await gotoSection(page, "dictation");

    // Start the session.
    await page.locator('[data-testid="record-start-btn"]').click();
    // Button-click path: recording=true, meetingOnlyActive=false.
    const stopBtn = page.locator("button.record-btn.recording");
    await expect(stopBtn).toBeVisible();

    // Stop the session.
    await stopBtn.click();

    // UI returns to idle: start button in meeting mode is back.
    await expect(
      page.locator('[data-testid="record-start-btn"][data-record-mode="meeting"]'),
    ).toBeVisible();
  });

  test("meeting:source-failed event shows source-failed banner", async ({
    page,
  }) => {
    await installMocks(page, {
      // The session-started event sets activeId; the active session mock
      // must confirm it so refresh() doesn't immediately clear it.
      meeting_active_session: () => ({ active: 1 }),
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
        currentPartials: [],
      }),
    });

    await page.goto("/");
    await gotoSection(page, "dictation");

    // Fire session-started to put the UI into active mode.
    await fireEvent(page, "meeting:session-started", { sessionId: 1 });

    // Stop button should appear.
    await expect(
      page.locator('button[aria-label="Stop meeting recording"]'),
    ).toBeVisible();

    // Fire source-failed for the mic source (multi-source session → "still recording" message).
    await fireEvent(page, "meeting:source-failed", {
      sessionId: 1,
      sourceKind: "mic",
      reason: "device lost",
      deviceLost: true,
    });

    const banner = page.locator('[data-testid="source-failed-banner"]');
    await expect(banner).toBeVisible();
    await expect(banner).toContainText("Microphone");
  });

  test("meeting:session-ended event clears active session state", async ({
    page,
  }) => {
    await installMocks(page, {
      meeting_active_session: () => ({ active: null }),
      meeting_session_get: () => ({
        session: {
          id: 1,
          appName: "manual",
          appKind: "other",
          startedAt: "2026-05-01T15:00:00Z",
          endedAt: "2026-05-01T15:05:00Z",
          speakerCount: null,
          utteranceCount: 0,
          notes: null,
          sources: ["mic"],
          appTitle: null,
        },
        utterances: [],
        currentPartials: [],
      }),
    });

    await page.goto("/");
    await gotoSection(page, "dictation");

    // Put the UI into active mode via the event.
    // We need meeting_active_session to return active: 1 while the session
    // is live so refresh() doesn't immediately clear it. Patch via evaluate
    // after the first page load (which uses the null default).
    await page.evaluate(() => {
      const stub = (
        window as unknown as {
          __hush_e2e?: {
            invoke: Record<string, (args?: unknown) => Promise<unknown>>;
          };
        }
      ).__hush_e2e;
      if (stub) {
        stub.invoke["meeting_active_session"] = async () => ({ active: 1 });
      }
    });
    await fireEvent(page, "meeting:session-started", { sessionId: 1 });

    const stopBtn = page.locator('button[aria-label="Stop meeting recording"]');
    await expect(stopBtn).toBeVisible();

    // Patch back to null so refresh() after session-ended sees no active session.
    await page.evaluate(() => {
      const stub = (
        window as unknown as {
          __hush_e2e?: {
            invoke: Record<string, (args?: unknown) => Promise<unknown>>;
          };
        }
      ).__hush_e2e;
      if (stub) {
        stub.invoke["meeting_active_session"] = async () => ({ active: null });
      }
    });

    // Fire session-ended — should clear active state.
    await fireEvent(page, "meeting:session-ended", { sessionId: 1 });

    // Stop button is gone; UI is idle again (start button visible).
    await expect(stopBtn).toHaveCount(0);
  });
});
