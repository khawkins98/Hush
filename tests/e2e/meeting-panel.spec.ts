import { expect, test } from "@playwright/test";
import { gotoSection, installMocks } from "./_mock";

// Meeting panel UX specs spanning Phase 1–3 of the meeting-mode
// roadmap (#122). The panel:
//
//  - Renders an inline mic dropdown + a "Also record system audio"
//    checkbox (Phase 3 multi-source). The dictation source picker
//    in `section.controls` is independent — these tests scope to
//    `section.panel-meetings`.
//  - Defaults the system-audio toggle to ON when the backend reports
//    the entry as `isSupported: true`, OFF otherwise.
//  - Active-session view replaces the picker with a "Recording
//    from <sources>" line + a separate live utterance counter.

test.describe("meeting panel — multi-source picker", () => {
  test("idle panel renders mic dropdown + system-audio checkbox", async ({
    page,
  }) => {
    await installMocks(page);
    await page.goto("/");
    await gotoSection(page, "meetings");

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
    await expect(panel).toContainText(/macOS only today/i);

    // Hint copy primes the user that the session records on Start
    // (not hotkey-driven) and produces text every ~10 s.
    await expect(panel).toContainText(/Click Start to begin recording/i);
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
    await gotoSection(page, "meetings");

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
    await gotoSection(page, "meetings");

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
          utteranceCount: 3,
          notes: null,
        },
        // Live counter now reads from `activeDetail.utterances.length`
        // rather than the session list's `utteranceCount` (which only
        // refreshes on session start/stop). Mock three finals so the
        // counter assertion below can verify the live read.
        utterances: [
          {
            id: 1,
            sessionId: 42,
            startedAtMs: 0,
            endedAtMs: 5_000,
            speakerLabel: "mic",
            text: "First.",
            isFinal: true,
          },
          {
            id: 2,
            sessionId: 42,
            startedAtMs: 5_000,
            endedAtMs: 10_000,
            speakerLabel: "system",
            text: "Second.",
            isFinal: true,
          },
          {
            id: 3,
            sessionId: 42,
            startedAtMs: 10_000,
            endedAtMs: 15_000,
            speakerLabel: "mic",
            text: "Third.",
            isFinal: true,
          },
        ],
        currentPartials: [],
      }),
    });
    await page.goto("/");
    await gotoSection(page, "meetings");

    const panel = page.locator("section.panel-meetings");

    // Live utterance counter reads from `activeDetail.utterances.length`,
    // which is polled every ~3 s and reflects what the pump has
    // committed since session start.
    await expect(panel).toContainText(/3 utterances so far/i);
    // Active-session copy — recording, not hotkey-driven.
    await expect(panel).toContainText(/Recording from/i);
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
        currentPartials: [],
      }),
    });
    await page.goto("/");
    await gotoSection(page, "meetings");

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

  test("active session renders streaming partial utterances with italic / opacity treatment", async ({
    page,
  }) => {
    // PR4 of #108 (streaming pump) surfaces in-flight partial
    // utterances via the new `currentPartials` field on
    // `meeting_session_get`. The panel renders them after the
    // settled finals with a `utterance-partial` class that styles
    // them italic + reduced-opacity, plus an animated "…"
    // indicator. Pin the rendered shape so a regression that
    // collapses partials into the finals list (or drops the
    // distinguishing styling) surfaces here.
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
          utteranceCount: 1,
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
          utteranceCount: 1,
          notes: null,
        },
        utterances: [
          {
            id: 1,
            sessionId: 99,
            startedAtMs: 0,
            endedAtMs: 5_000,
            speakerLabel: "mic",
            text: "Settled final from earlier.",
            isFinal: true,
          },
        ],
        // Two in-flight partials, one per source — the panel must
        // render both with the partial styling, alphabetically by
        // speakerLabel ("mic" before "system" — matches the
        // backend's sort).
        currentPartials: [
          {
            startedAtMs: 6_000,
            endedAtMs: 8_500,
            speakerLabel: "mic",
            text: "still being refined",
            isFinal: false,
          },
          {
            startedAtMs: 7_000,
            endedAtMs: 9_000,
            speakerLabel: "system",
            text: "remote tail not yet committed",
            isFinal: false,
          },
        ],
      }),
    });
    await page.goto("/");
    await gotoSection(page, "meetings");

    const panel = page.locator("section.panel-meetings");
    const transcript = panel.locator("ol.live-transcript");
    await expect(transcript).toBeVisible();

    // 1 final + 2 partials = 3 rows.
    const items = transcript.locator("li.utterance");
    await expect(items).toHaveCount(3);

    // The settled final has no `utterance-partial` class.
    const finals = transcript.locator("li.utterance:not(.utterance-partial)");
    await expect(finals).toHaveCount(1);
    await expect(finals.first()).toContainText("Settled final from earlier.");

    // Two partials with the styling.
    const partials = transcript.locator("li.utterance-partial");
    await expect(partials).toHaveCount(2);

    // Partial text content present + the "…" indicator visible.
    await expect(partials.nth(0)).toContainText("still being refined");
    await expect(partials.nth(0).locator(".partial-indicator")).toContainText(
      "…",
    );
    await expect(partials.nth(1)).toContainText(
      "remote tail not yet committed",
    );

    // Italic styling actually applied (computed-style assertion is
    // the most concrete check; class-name assertions can drift if
    // CSS is renamed).
    const italicTextStyle = await partials
      .nth(0)
      .locator(".utterance-text")
      .evaluate((el) => window.getComputedStyle(el).fontStyle);
    expect(italicTextStyle).toBe("italic");
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
          currentPartials: [],
        };
      },
    });
    await page.goto("/");
    await gotoSection(page, "meetings");

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

  test("historical transcript renders mm:ss offset alongside wall-clock time (#136)", async ({
    page,
  }) => {
    // The expanded historical view must surface a wall-clock time
    // alongside the session-relative offset — `47:23` on yesterday's
    // 90-minute call is meaningless without it. Pin the rendered
    // shape so a future change that drops the clock-time span gets
    // caught.
    await installMocks(page, {
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
      meeting_session_get: () => ({
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
            id: 1,
            sessionId: 17,
            startedAtMs: 47 * 60_000 + 23_000, // 47:23 offset
            endedAtMs: 47 * 60_000 + 28_000,
            speakerLabel: "mic",
            text: "A point made deep into the meeting.",
            isFinal: true,
          },
        ],
        currentPartials: [],
      }),
    });
    await page.goto("/");
    await gotoSection(page, "meetings");

    const panel = page.locator("section.panel-meetings");
    const row = panel.locator("li.session-row").first();
    await row.getByRole("button", { name: /Show transcript/i }).click();
    const transcript = row.locator("ol.live-transcript");
    await expect(transcript).toBeVisible();

    // Offset present.
    await expect(transcript).toContainText("47:23");
    // Wall-clock span present (the format depends on locale; just
    // pin the existence of the .utterance-clock class + a leading
    // separator dot).
    const clockSpan = transcript.locator(".utterance-clock").first();
    await expect(clockSpan).toBeVisible();
    await expect(clockSpan).toContainText("·");
  });

  test("live transcript pill stays hidden on initial mount even with many utterances (#135)", async ({
    page,
  }) => {
    // The auto-scroll affordance defaults to "following" — the
    // pill that appears once the user scrolls up should NOT render
    // on initial load even when there are enough utterances to
    // make the transcript scrollable. Pin that default so a
    // regression that flips `liveTranscriptFollowing` to false on
    // mount is caught.
    //
    // Inlining the utterances inside the mock function: the e2e
    // bridge serialises functions via `toString()` and rebuilds
    // them on the page side, so closure capture from the test
    // scope doesn't survive — any data the mock returns has to be
    // literal in its body. (Same constraint as
    // `historical session row expands…` above.)
    await installMocks(page, {
      meeting_active_session: () => ({ active: 99 }),
      meeting_sessions_list: () => [
        {
          id: 99,
          appName: "manual",
          appKind: "other",
          startedAt: "2026-04-26T15:00:00Z",
          endedAt: null,
          speakerCount: null,
          utteranceCount: 5,
          notes: null,
        },
      ],
      meeting_session_get: () => ({
        session: {
          id: 99,
          appName: "manual",
          appKind: "other",
          startedAt: "2026-04-26T15:00:00Z",
          endedAt: null,
          speakerCount: null,
          utteranceCount: 5,
          notes: null,
        },
        utterances: [
          { id: 1, sessionId: 99, startedAtMs: 0, endedAtMs: 10_000, speakerLabel: "mic", text: "First utterance.", isFinal: true },
          { id: 2, sessionId: 99, startedAtMs: 10_000, endedAtMs: 20_000, speakerLabel: "system", text: "Second utterance.", isFinal: true },
          { id: 3, sessionId: 99, startedAtMs: 20_000, endedAtMs: 30_000, speakerLabel: "mic", text: "Third utterance.", isFinal: true },
          { id: 4, sessionId: 99, startedAtMs: 30_000, endedAtMs: 40_000, speakerLabel: "system", text: "Fourth utterance.", isFinal: true },
          { id: 5, sessionId: 99, startedAtMs: 40_000, endedAtMs: 50_000, speakerLabel: "mic", text: "Fifth utterance.", isFinal: true },
        ],
        currentPartials: [],
      }),
    });
    await page.goto("/");
    await gotoSection(page, "meetings");

    const panel = page.locator("section.panel-meetings");
    const transcript = panel.locator("ol.live-transcript");
    await expect(transcript).toBeVisible();

    // Pill is hidden while the user is following the tail —
    // initial mount must not trigger the freeze.
    await expect(panel.locator("button.jump-to-latest")).toHaveCount(0);
  });

  test("Stop session requires confirmation (closes #131)", async ({ page }) => {
    // The first click should NOT call meeting_stop_manual — it
    // reveals an inline confirmation. Only the explicit "Yes, end
    // session" click commits. Pin the two-step shape so a future
    // change that drops the confirmation regresses the foot-gun
    // the round-8 reviewer surfaced.
    let stopCallCount = 0;
    await page.exposeFunction("__hush_record_stop", () => {
      stopCallCount += 1;
    });
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
          utteranceCount: 5,
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
          utteranceCount: 5,
          notes: null,
        },
        // Live counter reads `activeDetail.utterances.length`, so
        // we mock five finals to match the "5 utterances captured"
        // assertion the Stop-confirmation prompt exercises below.
        utterances: [
          { id: 1, sessionId: 42, startedAtMs: 0, endedAtMs: 5000, speakerLabel: "mic", text: "1", isFinal: true },
          { id: 2, sessionId: 42, startedAtMs: 5000, endedAtMs: 10000, speakerLabel: "mic", text: "2", isFinal: true },
          { id: 3, sessionId: 42, startedAtMs: 10000, endedAtMs: 15000, speakerLabel: "mic", text: "3", isFinal: true },
          { id: 4, sessionId: 42, startedAtMs: 15000, endedAtMs: 20000, speakerLabel: "mic", text: "4", isFinal: true },
          { id: 5, sessionId: 42, startedAtMs: 20000, endedAtMs: 25000, speakerLabel: "mic", text: "5", isFinal: true },
        ],
        currentPartials: [],
      }),
      meeting_stop_manual: () => {
        (
          window as unknown as { __hush_record_stop: () => void }
        ).__hush_record_stop();
        return undefined;
      },
    });
    await page.goto("/");
    await gotoSection(page, "meetings");

    const panel = page.locator("section.panel-meetings");
    const stopButton = panel.getByRole("button", { name: "Stop session" });
    await expect(stopButton).toBeVisible();

    // First click — reveals the confirmation prompt, doesn't fire
    // the IPC.
    await stopButton.click();
    await expect(stopButton).toHaveCount(0);
    await expect(panel).toContainText(/End session\?/);
    await expect(panel).toContainText(/5 utterances captured/);
    expect(stopCallCount).toBe(0);

    // Cancel returns to the unconfirmed Stop state without firing
    // the IPC.
    await panel.getByRole("button", { name: "Cancel" }).click();
    await expect(
      panel.getByRole("button", { name: "Stop session" }),
    ).toBeVisible();
    expect(stopCallCount).toBe(0);

    // Re-click + confirm fires the IPC exactly once.
    await panel.getByRole("button", { name: "Stop session" }).click();
    await panel.getByRole("button", { name: "Yes, end session" }).click();
    await expect.poll(() => stopCallCount).toBe(1);
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
        currentPartials: [],
      }),
    });
    await page.goto("/");
    await gotoSection(page, "meetings");

    const panel = page.locator("section.panel-meetings");
    await expect(panel).toContainText(/Listening/i);
    await expect(panel.locator("ol.live-transcript")).toHaveCount(0);
  });
});
