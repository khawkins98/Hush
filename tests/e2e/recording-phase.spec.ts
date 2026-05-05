import { expect, test } from "@playwright/test";
import { installMocks } from "./_mock";

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
      // screenRecording "confirmed" → screenRecordingLive = true →
      // startRecord(true) → sources = [mic, system-audio] → isMultiSource = true.
      get_permission_health: () => ({
        health: {
          microphone: "not-applicable",
          screenRecording: "confirmed",
          inputMonitoring: "not-applicable",
        },
      }),
      meeting_start_manual: () => {
        throw { kind: "permission-denied", permission: "screen-recording" };
      },
    });
    await page.goto("/");

    // Wait for meeting mode to be reflected in the button — the
    // permission health IPC is async so screenRecordingLive starts
    // false and becomes true after the first paint. Clicking before
    // the update would exercise the single-source path instead.
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
      return await (window as Record<string, () => unknown>)["hushGetActiveSession"]();
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
      return await (window as Record<string, () => unknown>)["hushGetActiveSession"]();
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
