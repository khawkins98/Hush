import { expect, test } from "@playwright/test";
import { fireEvent, installMocks } from "./_mock";

// Unit-level coverage for the RecordingPhase state machine in
// dictation.svelte.ts. Each test exercises one row of the transition
// table from issue #562. The suite runs in HUSH_E2E=1 mode (real
// browser, mocked IPC) — no real audio device or Tauri runtime needed.
//
// State diagram (abbreviated):
//
//   idle ──start──▶ starting ──ok──▶ recording ──stop()──▶ stopping
//               └──fail──▶ idle              └──ok──▶ transcribing──▶ idle
//                                            └──fail + session live──▶ recording
//                                            └──fail + session gone──▶ idle

// Default meeting session ID returned by the default meeting_start_manual mock.
const DEFAULT_SESSION_ID = 1;

// ---------------------------------------------------------------------------
// starting → idle
// ---------------------------------------------------------------------------

test.describe("starting → idle", () => {
  test("generic IPC failure clears recording state and shows error", async ({ page }) => {
    await installMocks(page, {
      meeting_start_manual: () => {
        throw { kind: "audio", message: "device unavailable" };
      },
      // PTT / toggle path — same error shape, same expectations.
      start_dictation: () => {
        throw { kind: "audio", message: "device unavailable" };
      },
    });
    await page.goto("/");

    await expect(page.getByRole("button", { name: "Start recording" })).toBeEnabled();
    await page.getByRole("button", { name: "Start recording" }).click();

    // Phase: starting → idle (error). "Start recording" re-enabled.
    await expect(page.getByRole("button", { name: "Start recording" })).toBeEnabled();
    await expect(page.getByRole("alert").first()).toBeVisible();
    // Stop button must not appear — phase is idle, not recording.
    await expect(
      page.getByRole("button", { name: "Stop recording and transcribe" }),
    ).toHaveCount(0);
  });

  test("permission-shaped error in multi-source start shows permissions dialog intro", async ({
    page,
  }) => {
    await installMocks(page, {
      // systemAudio.isSupported → true triggers willRecordMeeting = true,
      // which shows the meeting button and makes startRecord include system-audio.
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
        throw { kind: "permission-denied", message: "microphone" };
      },
    });
    await page.goto("/");

    // Wait for meeting mode to be reflected in the button — audio_list_sources
    // is async so isSupported starts false and becomes true after the first IPC.
    const meetingStartBtn = page.getByRole("button", {
      name: "Record meeting (mic plus system audio)",
    });
    await expect(meetingStartBtn).toBeEnabled();
    await meetingStartBtn.click();

    // Phase: starting → idle (permission error, multi-source).
    // pendingPermissionsDialogIntro triggers the permissions dialog.
    // The dialog has role="dialog" aria-labelledby="perm-dialog-heading" ("Permissions").
    await expect(page.getByRole("dialog", { name: "Permissions" })).toBeVisible();
    await expect(
      page.getByRole("button", { name: "Stop recording and transcribe" }),
    ).toHaveCount(0);
  });
});

// ---------------------------------------------------------------------------
// recording → transcribing → idle (happy path stop)
// ---------------------------------------------------------------------------

test("recording → transcribing → idle: successful stop passes through transcribing", async ({
  page,
}) => {
  await installMocks(page, {
    // Delay session_get so the transcribing phase is observable before idle.
    meeting_session_get: async () => {
      await new Promise((r) => setTimeout(r, 80));
      return {
        session: {
          id: DEFAULT_SESSION_ID,
          appName: "manual",
          appKind: "other",
          startedAt: "2026-04-26T15:00:00Z",
          endedAt: "2026-04-26T15:01:00Z",
          speakerCount: null,
          utteranceCount: 1,
          notes: null,
          sources: ["mic"],
          appTitle: null,
        },
        utterances: [
          {
            id: 1,
            sessionId: DEFAULT_SESSION_ID,
            startedAtMs: 0,
            endedAtMs: 1000,
            speakerLabel: null,
            text: "hello world",
            isFinal: true,
          },
        ],
        currentPartials: [],
      };
    },
  });
  await page.goto("/");

  await page.getByRole("button", { name: "Start recording" }).click();
  const stopBtn = page.getByRole("button", { name: "Stop recording and transcribe" });
  await expect(stopBtn).toBeVisible();

  await stopBtn.click();

  // Transient: stopping + trailing silence (500 ms) → transcribing → session_get
  // (80 ms delay). During both phases busy=true and the button reads "Working".
  await expect(page.getByRole("button", { name: "Working" })).toBeVisible();

  // Final: idle. Start button re-enabled; stop button gone.
  await expect(page.getByRole("button", { name: "Start recording" })).toBeEnabled();
  await expect(
    page.getByRole("button", { name: "Stop recording and transcribe" }),
  ).toHaveCount(0);
  // No error surfaced on the happy path.
  await expect(page.getByRole("alert")).toHaveCount(0);
});

// ---------------------------------------------------------------------------
// stopping → recording / idle (stop failure recovery)
// ---------------------------------------------------------------------------

// Both recovery tests expose a Node-side function that the mock reads via
// `page.exposeFunction`, so the session-alive/gone value can be changed
// after recording starts but before stop completes. Closure capture does
// not cross the mock serialisation boundary — only window properties do.

test("stopping → recording: stop failure with live session restores recording state", async ({
  page,
}) => {
  // Start with no active session (for the mount-time refresh query).
  let activeSessionResult: { active: number | null } = { active: null };
  await page.exposeFunction("hushGetActiveSession", () => activeSessionResult);

  await installMocks(page, {
    meeting_stop_manual: () => {
      throw { kind: "unknown", message: "stop failed" };
    },
    // Dynamic: reads from the Node-side closure via exposeFunction so we can
    // change the response after recording starts without a new addInitScript.
    meeting_active_session: async () => {
      return await (window as unknown as Record<string, () => unknown>)["hushGetActiveSession"]();
    },
  });
  await page.goto("/");

  // Start recording (default mock: id = 1).
  await page.getByRole("button", { name: "Start recording" }).click();
  await expect(
    page.getByRole("button", { name: "Stop recording and transcribe" }),
  ).toBeVisible();

  // Session is still live on the backend when stop fails.
  activeSessionResult = { active: DEFAULT_SESSION_ID };

  await page.getByRole("button", { name: "Stop recording and transcribe" }).click();

  // Phase recovery: stopping → (stop throws) → active query returns session →
  // restore to recording. Stop button must reappear.
  await expect(
    page.getByRole("button", { name: "Stop recording and transcribe" }),
  ).toBeVisible();
  // Error is shown so the user knows the stop attempt failed.
  await expect(page.getByRole("alert").first()).toBeVisible();
});

test("stopping → idle: stop failure with gone session clears to idle", async ({ page }) => {
  let activeSessionResult: { active: number | null } = { active: null };
  await page.exposeFunction("hushGetActiveSession", () => activeSessionResult);

  await installMocks(page, {
    meeting_stop_manual: () => {
      throw { kind: "unknown", message: "stop failed" };
    },
    meeting_active_session: async () => {
      return await (window as unknown as Record<string, () => unknown>)["hushGetActiveSession"]();
    },
  });
  await page.goto("/");

  await page.getByRole("button", { name: "Start recording" }).click();
  await expect(
    page.getByRole("button", { name: "Stop recording and transcribe" }),
  ).toBeVisible();

  // Session gone: activeSessionResult stays { active: null } (≠ DEFAULT_SESSION_ID).
  await page.getByRole("button", { name: "Stop recording and transcribe" }).click();

  // Phase: stopping → idle (session gone).
  await expect(page.getByRole("button", { name: "Start recording" })).toBeEnabled();
  await expect(
    page.getByRole("button", { name: "Stop recording and transcribe" }),
  ).toHaveCount(0);
  await expect(page.getByRole("alert").first()).toBeVisible();
});

// ---------------------------------------------------------------------------
// Guards (structural — proven by button visibility, not method invocation)
// ---------------------------------------------------------------------------

test("idle guard: stop button absent when idle", async ({ page }) => {
  await installMocks(page);
  await page.goto("/");

  // No active recording → stop button must not be in the DOM.
  await expect(
    page.getByRole("button", { name: "Stop recording and transcribe" }),
  ).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Start recording" })).toBeEnabled();
});

test("recording guard: start button replaced by stop when recording", async ({ page }) => {
  await installMocks(page);
  await page.goto("/");

  await page.getByRole("button", { name: "Start recording" }).click();

  // Phase: recording. Start button gone; stop button takes its place.
  await expect(
    page.getByRole("button", { name: "Stop recording and transcribe" }),
  ).toBeVisible();
  await expect(page.getByRole("button", { name: "Start recording" })).toHaveCount(0);
});

test("record-mode-badge hidden when Screen Recording health is not-applicable (default)", async ({
  page,
}) => {
  // Default mock returns screenRecording: "not-applicable"; badge must be absent.
  await installMocks(page);
  await page.goto("/");
  await expect(
    page.locator('[data-testid="record-mode-badge"]'),
  ).toHaveCount(0);
});

test("live-transcript panel appears during recording when utterances are available", async ({
  page,
}) => {
  // This test covers the "combined mode": dictation is recording AND a meeting
  // session activates mid-session (via the meeting:session-started event).
  // RecordPanel's showLiveTranscript then renders the live utterances.
  //
  // Start without an active meeting so RecordPanel is visible. After Record is
  // clicked (dictation running), fire meeting:session-started to simulate the
  // meeting activating, then confirm the live-transcript panel appears.
  //
  // meeting_active_session must switch to 99 when fireEvent is called so that
  // the listener's meeting.refresh() doesn't overwrite the just-set activeId
  // back to null.
  let activeMeetingId: number | null = null;
  await page.exposeFunction("hushGetActiveMeetingForLT", () => activeMeetingId);

  await installMocks(page, {
    meeting_active_session: async () => {
      const id = await (window as unknown as Record<string, () => unknown>)["hushGetActiveMeetingForLT"]();
      return { active: id };
    },
    meeting_session_get: () => ({
      session: {
        id: 99,
        appName: "manual",
        appKind: "other",
        startedAt: "2026-05-05T10:00:00Z",
        endedAt: null,
        speakerCount: null,
        utteranceCount: 1,
        notes: null,
        sources: ["mic"],
        appTitle: null,
      },
      utterances: [
        {
          id: 1,
          sessionId: 99,
          startedAtMs: 0,
          endedAtMs: 3000,
          speakerLabel: "mic",
          text: "Hello world.",
          isFinal: true,
        },
      ],
      currentPartials: [],
    }),
  });
  await page.goto("/");

  // Dictation starts — meetingOnlyActive becomes false (recording=true)
  // so RecordPanel is visible even when meeting later activates.
  await page.getByRole("button", { name: "Start recording" }).click();
  await expect(
    page.getByRole("button", { name: "Stop recording and transcribe" }),
  ).toBeVisible();

  // Switch the meeting_active_session mock to return 99, then fire the event.
  // The listener sets meeting.activeId=99 and calls refresh(); refresh() will
  // now also see active=99 so the id stays set.
  activeMeetingId = 99;
  await fireEvent(page, "meeting:session-started", { sessionId: 99 });

  // RecordPanel's liveTranscriptText derived becomes non-empty →
  // showLiveTranscript renders the live-transcript section.
  const livePanel = page.locator('[data-testid="live-transcript"]');
  await expect(livePanel).toBeVisible();
  await expect(livePanel).toContainText("Hello world.");
});

test("export-picker appears in ResultBlock after successful single-source stop", async ({
  page,
}) => {
  // Single-source (mic only, no screen recording confirmed) → mode = "dictation"
  // → result is hydrated from the session utterances after meeting_stop_manual.
  // NOTE: mock functions are serialised via toString() and rebuilt in the page
  // context — they cannot close over module-level variables like DEFAULT_SESSION_ID.
  // All values inside mock functions must be literals.
  await installMocks(page, {
    meeting_session_get: () => ({
      session: {
        id: 1,
        appName: "manual",
        appKind: "other",
        startedAt: "2026-04-26T15:00:00Z",
        endedAt: "2026-04-26T15:01:00Z",
        speakerCount: null,
        utteranceCount: 1,
        notes: null,
        sources: ["mic"],
        appTitle: null,
      },
      utterances: [
        {
          id: 1,
          sessionId: 1,
          startedAtMs: 0,
          endedAtMs: 2000,
          speakerLabel: null,
          text: "Hello Playwright.",
          isFinal: true,
        },
      ],
      currentPartials: [],
    }),
  });
  await page.goto("/");

  await page.getByRole("button", { name: "Start recording" }).click();
  await page.getByRole("button", { name: "Stop recording and transcribe" }).click();

  // Wait for idle: result block renders with the transcript text.
  await expect(page.getByRole("button", { name: "Start recording" })).toBeEnabled();

  // ResultBlock: export-picker group visible with at least one format button.
  const picker = page.locator('[data-testid="export-picker"]');
  await expect(picker).toBeVisible();
  // "Copy as:" label and the Plain format button should be present.
  await expect(picker).toContainText("Copy as:");
  await expect(
    picker.locator('[data-testid="export-format-plain"]'),
  ).toBeVisible();
});

test("meeting-copy-notice appears after stop when utterances are present", async ({
  page,
}) => {
  // After _stopMeeting completes, the state machine calls
  // meeting.setNotice() from the clipboard write path. Whether
  // navigator.clipboard.writeText succeeds or fails, one of the two
  // notice variants appears. We mock it to succeed so the success
  // variant renders deterministically.
  await page.addInitScript(() => {
    Object.defineProperty(navigator, "clipboard", {
      value: { writeText: () => Promise.resolve() },
      writable: true,
      configurable: true,
    });
  });
  await installMocks(page, {
    meeting_session_get: () => ({
      session: {
        id: 1,
        appName: "manual",
        appKind: "other",
        startedAt: "2026-04-26T15:00:00Z",
        endedAt: "2026-04-26T15:01:00Z",
        speakerCount: null,
        utteranceCount: 1,
        notes: null,
        sources: ["mic"],
        appTitle: null,
      },
      utterances: [
        {
          id: 1,
          sessionId: 1,
          startedAtMs: 0,
          endedAtMs: 2000,
          speakerLabel: null,
          text: "This is a test transcript.",
          isFinal: true,
        },
      ],
      currentPartials: [],
    }),
  });
  await page.goto("/");

  await page.getByRole("button", { name: "Start recording" }).click();
  await page.getByRole("button", { name: "Stop recording and transcribe" }).click();

  // Wait for the recording to complete.
  await expect(page.getByRole("button", { name: "Start recording" })).toBeEnabled();

  // MeetingSection (which renders the copy notice) lives inside the History
  // section block ({#if nav.activeSection === "history"}), so we navigate
  // there before asserting. The notice is already set by _stopMeeting before
  // phase returns to idle, so it persists through the navigation.
  await page.locator('[data-testid="sidebar-nav-history"]').click();

  const notice = page.locator('[data-testid="meeting-copy-notice"]');
  await expect(notice).toBeVisible();
  // Success variant: confirms the copy went to clipboard.
  await expect(notice).toContainText(/Copied to clipboard/);
});

test("perms-pill-ok renders when all macOS permissions are granted", async ({
  page,
}) => {
  // MacosPermsPill shows the green pill when capable=true (canReset: true)
  // and allGranted is derived from permStatuses (all three = "granted").
  await installMocks(page, {
    diagnose_macos_permissions: () => ({
      bundleId: "io.github.khawkins98.hush",
      microphoneHint: "",
      inputMonitoringHint: "",
      canReset: true,
      statuses: {
        microphone: "granted",
        screenRecording: "granted",
        inputMonitoring: "granted",
      },
    }),
  });
  await page.goto("/");

  const pill = page.locator('[data-testid="perms-pill-ok"]');
  await expect(pill).toBeVisible();
  await expect(pill).toContainText(/permissions OK/i);
});

test("perms-hint-yellow renders when a macOS permission is denied", async ({
  page,
}) => {
  // MacosPermsPill shows the yellow banner when capable=true and
  // anyDenied (at least one permission status = "denied").
  await installMocks(page, {
    diagnose_macos_permissions: () => ({
      bundleId: "io.github.khawkins98.hush",
      microphoneHint: "",
      inputMonitoringHint: "",
      canReset: true,
      statuses: {
        microphone: "denied",
        screenRecording: "not-determined",
        inputMonitoring: "not-determined",
      },
    }),
  });
  await page.goto("/");

  const banner = page.locator('[data-testid="perms-hint-yellow"]');
  await expect(banner).toBeVisible();
  await expect(banner).toContainText(/Permission needed/i);
});

// ---------------------------------------------------------------------------
// Meeting-active banner — dictation section
// ---------------------------------------------------------------------------

test("meeting-active banner appears when meeting is running but dictation is idle", async ({
  page,
}) => {
  // Backend reports an active meeting session from the start.
  // Dictation is NOT recording — only the meeting pump is running.
  await installMocks(page, {
    meeting_active_session: () => ({ active: 42 }),
    meeting_session_get: () => ({
      session: {
        id: 42,
        appName: "zoom",
        appKind: "video-call",
        startedAt: "2026-05-13T16:00:00Z",
        endedAt: null,
        speakerCount: null,
        utteranceCount: 2,
        notes: null,
        sources: ["mic", "system"],
        appTitle: null,
      },
      utterances: [
        {
          id: 1,
          sessionId: 42,
          startedAtMs: 0,
          endedAtMs: 4000,
          speakerLabel: null,
          text: "Hello from the meeting.",
          isFinal: true,
        },
      ],
      currentPartials: [],
    }),
  });
  await page.goto("/");

  // The banner must be visible on the Transcribe (dictation) panel without
  // the user needing to navigate anywhere.
  const banner = page.locator('[aria-label="Meeting recording in progress"]');
  await expect(banner).toBeVisible();

  // Stop button is accessible.
  const stopBtn = page.getByRole("button", { name: "Stop meeting recording" });
  await expect(stopBtn).toBeVisible();

  // Live transcript from the poll appears once meeting_session_get is called.
  const transcript = page.locator('[data-testid="meeting-active-transcript"]');
  await expect(transcript).toBeVisible();
  await expect(transcript).toContainText("Hello from the meeting.");
});

test("meeting-active banner stop button calls meeting_stop_manual", async ({
  page,
}) => {
  let stopCalled = false;
  await page.exposeFunction("hushRecordStop", () => {
    stopCalled = true;
  });

  await installMocks(page, {
    meeting_active_session: () => ({ active: 7 }),
    meeting_stop_manual: async () => {
      await (window as unknown as Record<string, () => unknown>)["hushRecordStop"]();
    },
    meeting_session_get: () => ({
      session: {
        id: 7,
        appName: "teams",
        appKind: "video-call",
        startedAt: "2026-05-13T16:00:00Z",
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

  const stopBtn = page.getByRole("button", { name: "Stop meeting recording" });
  await expect(stopBtn).toBeVisible();
  await stopBtn.click();

  // meeting_stop_manual must have been called.
  await expect.poll(() => stopCalled).toBe(true);
});

test("meeting-active banner hides once meeting stops", async ({ page }) => {
  let activeResult: { active: number | null } = { active: 7 };
  await page.exposeFunction("hushGetActiveMeeting", () => activeResult);
  await page.exposeFunction("hushStopMeeting", () => {
    activeResult = { active: null };
  });

  await installMocks(page, {
    meeting_active_session: async () => {
      return await (window as unknown as Record<string, () => unknown>)["hushGetActiveMeeting"]();
    },
    meeting_stop_manual: async () => {
      await (window as unknown as Record<string, () => unknown>)["hushStopMeeting"]();
    },
    meeting_session_get: () => ({
      session: {
        id: 7,
        appName: "teams",
        appKind: "video-call",
        startedAt: "2026-05-13T16:00:00Z",
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

  const banner = page.locator('[aria-label="Meeting recording in progress"]');
  await expect(banner).toBeVisible();

  await page.getByRole("button", { name: "Stop meeting recording" }).click();

  // Banner must disappear once meeting_stop_manual clears the active session.
  await expect(banner).not.toBeVisible();
});
