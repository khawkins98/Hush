import { expect, test } from "@playwright/test";
import { gotoSection, installMocks } from "./_mock";

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
