import { expect, test } from "@playwright/test";
import { installMocks } from "./_mock";

// Meeting panel UX specs spanning Phase 1–3 of the meeting-mode
// roadmap (#122). The panel:
//
//  - Renders an inline mic dropdown + a "Also record system audio"
//    checkbox (Phase 3 multi-source). The dictation source picker
//    in `section.controls` is independent — these tests scope to
//    `section.panel-meetings`.
//  - Defaults the system-audio toggle to ON when the backend reports
//    the entry as `isSupported: true`, OFF otherwise.
//  - Active-session view replaces the picker with an "Auto-recording
//    from <sources>" line + live utterance counter.

test.describe("meeting panel — multi-source picker", () => {
  test("idle panel renders mic dropdown + system-audio checkbox", async ({
    page,
  }) => {
    await installMocks(page);
    await page.goto("/");

    const panel = page.locator("section.panel-meetings");
    await expect(panel).toBeVisible();

    // Mic dropdown is single-select; the meeting-vs-system axis is
    // mic-vs-system-audio rather than mic-vs-mic, so the dropdown
    // only contains mics (no system-audio optgroup, unlike the
    // dictation picker).
    const micSelect = panel.locator("select");
    await expect(micSelect).toBeVisible();
    await expect(panel.locator('optgroup[label="Microphone"]')).toHaveCount(0);
    await expect(panel.locator('optgroup[label="System audio"]')).toHaveCount(
      0,
    );

    // System-audio toggle is a checkbox, disabled in the default
    // mock state (isSupported: false) with a "coming soon" hint.
    const sysCheckbox = panel.locator('input[type="checkbox"]');
    await expect(sysCheckbox).toBeVisible();
    await expect(sysCheckbox).toBeDisabled();
    await expect(panel).toContainText(/coming soon/i);

    // Phase 3 hint copy: explains auto-recording, not hotkey-driven.
    await expect(panel).toContainText(/Click Start to begin auto-recording/i);
  });

  test("system-audio toggle becomes enabled and default-on when backend reports support", async ({
    page,
  }) => {
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
    });
    await page.goto("/");

    const panel = page.locator("section.panel-meetings");
    const sysCheckbox = panel.locator('input[type="checkbox"]');
    await expect(sysCheckbox).toBeEnabled();
    // Default-ON when the backend reports support — meetings want
    // mic + system audio in parallel by default.
    await expect(sysCheckbox).toBeChecked();
    // The "(coming soon)" hint next to the checkbox is gone when
    // SCK ships. Scope to the controls group rather than the whole
    // panel — the no-sessions placeholder elsewhere in the panel has
    // unrelated "coming soon" copy about live transcripts overall.
    const controls = panel.locator(
      '[aria-label="Meeting session controls"]',
    );
    await expect(controls).not.toContainText(/coming soon/i);
  });

  test("Start invokes meeting_start_manual with both sources when SCK is on", async ({
    page,
  }) => {
    // Capture the args passed to meeting_start_manual so we can pin
    // the wire shape — a future regression that drops the system-
    // audio source from the list would silently break the meeting's
    // primary feature without this test.
    const seen: { sources: unknown; appName: unknown }[] = [];
    await page.exposeFunction("__hush_record_meeting_start", (args: unknown) => {
      seen.push(args as { sources: unknown; appName: unknown });
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
      meeting_start_manual: (args: unknown) => {
        (
          window as unknown as {
            __hush_record_meeting_start: (a: unknown) => void;
          }
        ).__hush_record_meeting_start(args);
        return {
          id: 1,
          appName: "manual",
          appKind: "other",
          startedAt: "2026-04-26T15:00:00Z",
          endedAt: null,
          speakerCount: null,
          utteranceCount: 0,
          notes: null,
        };
      },
    });
    await page.goto("/");

    const panel = page.locator("section.panel-meetings");
    await panel.getByRole("button", { name: "Start a session" }).click();

    await expect.poll(() => seen.length).toBeGreaterThan(0);
    expect(seen[0]).toMatchObject({
      sources: [
        { kind: "microphone", deviceId: "Built-in Microphone" },
        { kind: "system-audio" },
      ],
      appName: null,
    });
  });

  test("active session shows auto-recording line + utterance counter", async ({
    page,
  }) => {
    await installMocks(page, {
      meeting_active_session: () => ({ active: 42 }),
      meeting_sessions_list: () => [
        {
          id: 42,
          appName: "manual",
          appKind: "other",
          startedAt: "2026-04-26T15:00:00Z",
          endedAt: null,
          speakerCount: null,
          utteranceCount: 3,
          notes: null,
        },
      ],
      meeting_session_get: () => ({
        session: {
          id: 42,
          appName: "manual",
          appKind: "other",
          startedAt: "2026-04-26T15:00:00Z",
          endedAt: null,
          speakerCount: null,
          utteranceCount: 0,
          notes: null,
        },
        utterances: [],
      }),
    });
    await page.goto("/");

    const panel = page.locator("section.panel-meetings");

    // Live utterance counter reads from the active session row.
    await expect(panel).toContainText(/3 utterances so far/i);
    // Phase 3 active-session copy — auto-recording, not hotkey-driven.
    await expect(panel).toContainText(/Auto-recording/i);
    // Picker is hidden once the session is active.
    await expect(panel.locator("select")).toHaveCount(0);
    await expect(panel.locator('input[type="checkbox"]')).toHaveCount(0);

    // Stop button replaces Start.
    await expect(
      panel.getByRole("button", { name: "Stop session" }),
    ).toBeVisible();
  });

  test("active session renders the live transcript with mic / system speaker badges", async ({
    page,
  }) => {
    // PR4 (live transcript view) polls `meeting_session_get` every
    // ~3s while a session is in flight and renders each utterance
    // with a coarse "You" / "Remote" badge tied to the source the
    // chunk came from. Pin the rendered shape so a regression that
    // drops the badge or mis-tags the source surfaces here.
    await installMocks(page, {
      meeting_active_session: () => ({ active: 99 }),
      meeting_sessions_list: () => [
        {
          id: 99,
          appName: "us.zoom.xos",
          appKind: "meeting",
          startedAt: "2026-04-26T15:00:00Z",
          endedAt: null,
          speakerCount: null,
          utteranceCount: 2,
          notes: null,
        },
      ],
      meeting_session_get: () => ({
        session: {
          id: 99,
          appName: "us.zoom.xos",
          appKind: "meeting",
          startedAt: "2026-04-26T15:00:00Z",
          endedAt: null,
          speakerCount: null,
          utteranceCount: 2,
          notes: null,
        },
        utterances: [
          {
            id: 1,
            sessionId: 99,
            startedAtMs: 0,
            endedAtMs: 10_000,
            speakerLabel: "mic",
            text: "Hello, can you hear me?",
            isFinal: true,
          },
          {
            id: 2,
            sessionId: 99,
            startedAtMs: 10_000,
            endedAtMs: 20_000,
            speakerLabel: "system",
            text: "Yes, loud and clear.",
            isFinal: true,
          },
        ],
      }),
    });
    await page.goto("/");

    const panel = page.locator("section.panel-meetings");
    const transcript = panel.locator("ol.live-transcript");
    await expect(transcript).toBeVisible();

    // Two utterances rendered, in order.
    const items = transcript.locator("li.utterance");
    await expect(items).toHaveCount(2);

    // Mic-side utterance shows "You" badge + the right text.
    const micRow = items.filter({ has: page.locator(".speaker-mic") }).first();
    await expect(micRow).toContainText("You");
    await expect(micRow).toContainText("Hello, can you hear me?");
    await expect(micRow).toContainText("0:00");

    // System-side utterance shows "Remote" badge + the right text.
    const sysRow = items
      .filter({ has: page.locator(".speaker-system") })
      .first();
    await expect(sysRow).toContainText("Remote");
    await expect(sysRow).toContainText("Yes, loud and clear.");
    await expect(sysRow).toContainText("0:10");
  });

  test("historical session row expands inline to show its transcript", async ({
    page,
  }) => {
    // PR5 (historical expand) lazy-loads `meeting_session_get` on
    // first click of the per-row "Show transcript" button. Pin the
    // lazy-load + render-inline shape so a regression that always
    // pre-fetches every detail or fails to surface the transcript
    // stays caught.
    //
    // The default mock's `meeting_session_get` throws — we override
    // to a working impl below. Mock functions are serialised via
    // `toString()` and rebuilt in the page context, so they can't
    // close over variables declared on the test side; any counters
    // / capture buffers must go through `page.exposeFunction`.
    let detailFetches = 0;
    await page.exposeFunction("__hush_record_detail_fetch", () => {
      detailFetches += 1;
    });
    await installMocks(page, {
      // No active session so the panel renders the historical list.
      meeting_active_session: () => ({ active: null }),
      meeting_sessions_list: () => [
        {
          id: 17,
          appName: "Zoom",
          appKind: "meeting",
          startedAt: "2026-04-26T15:00:00Z",
          endedAt: "2026-04-26T15:30:00Z",
          speakerCount: null,
          utteranceCount: 1,
          notes: null,
        },
      ],
      meeting_session_get: () => {
        (
          window as unknown as {
            __hush_record_detail_fetch: () => void;
          }
        ).__hush_record_detail_fetch();
        return {
          session: {
            id: 17,
            appName: "Zoom",
            appKind: "meeting",
            startedAt: "2026-04-26T15:00:00Z",
            endedAt: "2026-04-26T15:30:00Z",
            speakerCount: null,
            utteranceCount: 1,
            notes: null,
          },
          utterances: [
            {
              id: 100,
              sessionId: 17,
              startedAtMs: 0,
              endedAtMs: 5_000,
              speakerLabel: "mic",
              text: "This was the past meeting talking.",
              isFinal: true,
            },
          ],
        };
      },
    });
    await page.goto("/");

    const panel = page.locator("section.panel-meetings");
    const row = panel.locator("li.session-row").first();
    await expect(row).toBeVisible();

    // No transcript yet — the lazy load only fires on click.
    await expect(row.locator("ol.live-transcript")).toHaveCount(0);
    expect(detailFetches).toBe(0);

    // First click triggers the fetch and renders the transcript.
    await row.getByRole("button", { name: /Show transcript/i }).click();
    await expect(row.locator("ol.live-transcript")).toBeVisible();
    await expect(row).toContainText("This was the past meeting talking.");
    await expect(row).toContainText("You");
    expect(detailFetches).toBeGreaterThanOrEqual(1);

    // Toggle closes the inline view. Re-open re-fetches today
    // (collapse drops the cached detail) — pinning that shape so
    // a future change that adds a cache layer is an explicit
    // decision, not a silent semantic shift.
    await row.getByRole("button", { name: /Hide transcript/i }).click();
    await expect(row.locator("ol.live-transcript")).toHaveCount(0);
  });

  test("active session shows listening placeholder when no utterances yet", async ({
    page,
  }) => {
    // First poll lands an empty utterances array — the panel must
    // make it visible that recording is in progress and the user
    // should expect text shortly. Pinned so a regression that
    // suppresses the empty-state line silently doesn't leave the
    // panel looking broken at session-start.
    await installMocks(page, {
      meeting_active_session: () => ({ active: 7 }),
      meeting_sessions_list: () => [
        {
          id: 7,
          appName: "manual",
          appKind: "other",
          startedAt: "2026-04-26T15:00:00Z",
          endedAt: null,
          speakerCount: null,
          utteranceCount: 0,
          notes: null,
        },
      ],
      meeting_session_get: () => ({
        session: {
          id: 7,
          appName: "manual",
          appKind: "other",
          startedAt: "2026-04-26T15:00:00Z",
          endedAt: null,
          speakerCount: null,
          utteranceCount: 0,
          notes: null,
        },
        utterances: [],
      }),
    });
    await page.goto("/");

    const panel = page.locator("section.panel-meetings");
    await expect(panel).toContainText(/Listening/i);
    await expect(panel.locator("ol.live-transcript")).toHaveCount(0);
  });
});
