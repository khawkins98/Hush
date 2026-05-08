import { expect, test } from "@playwright/test";
import { installMocks } from "./_mock";

// Error-copy regression smokes. The frontend maps backend
// `IpcError::*` variants to recovery-oriented copy via switch-on-kind.
// Round-3 reviewer flagged that this dispatch is fragile if future
// edits drift the kind strings — these tests pin the rendered copy
// for the variants the user is most likely to hit.

test.describe("IPC error copy", () => {
  test("transcription-unavailable points at Settings → Model", async ({ page }) => {
    // Post-#195 the pre-flight in `start_dictation` returns
    // TranscriptionUnavailable BEFORE audio capture opens, so the
    // user sees the error at click time rather than after a wasted
    // recording. Mock the start path here (was `stop_dictation`).
    //
    // The default mock `model_list` has Whisper Base as
    // `isDownloaded: true` — that suppresses the no-model banner so
    // we exercise the post-click-error path, not the never-let-the-
    // user-click-Start path. (The banner path is covered below.)
    await installMocks(page, {
      // Click-record now goes through `meeting_start_manual`
      // (#468 r3); same error path, just a different IPC.
      meeting_start_manual: () => {
        throw { kind: "transcription-unavailable" };
      },
      start_dictation: () => {
        throw { kind: "transcription-unavailable" };
      },
    });
    await page.goto("/");

    await page.getByRole("button", { name: "Start recording" }).click();

    // The new copy points the user at Settings → Model and explicitly
    // mentions "no restart needed" (the model hot-swap shipped under
    // a separate PR; the old copy still said "prompt you to restart"
    // — pinned out here so a regression in either direction surfaces).
    const errorRegion = page.getByRole("alert").first();
    await expect(errorRegion).toBeVisible();
    await expect(errorRegion).toContainText(/Settings.*Model/i);
    await expect(errorRegion).toContainText(/without a restart/i);
    // Regression: the old copy mentioned HUSH_MODEL_PATH and a
    // milestone-deferred picker. New copy must not.
    await expect(errorRegion).not.toContainText(/HUSH_MODEL_PATH/);
    await expect(errorRegion).not.toContainText(/coming in/i);
  });

  test("error renders as headline + hint with technical details collapsed (#199)", async ({
    page,
  }) => {
    // Pin the unified ErrorDisplay shape: friendly headline, action-
    // oriented hint, raw technical message tucked inside a closed
    // <details>. Pre-#199 the error rendered as one wall of nested
    // context strings — the hint and the failure point were
    // indistinguishable.
    //
    // The mock's body is .toString()'d and rebuilt in the page
    // context, so closure variables don't survive — every literal
    // is inlined.
    await installMocks(page, {
      meeting_start_manual: () => {
        throw {
          kind: "audio",
          message: "deeply: nested: context: chain: with low-level error",
        };
      },
      start_dictation: () => {
        throw {
          kind: "audio",
          message: "deeply: nested: context: chain: with low-level error",
        };
      },
    });
    await page.goto("/");

    await page.getByRole("button", { name: "Start recording" }).click();

    const errorRegion = page.getByRole("alert").first();
    await expect(errorRegion).toBeVisible();

    // Headline is what the user reads first.
    await expect(errorRegion.locator(".error-headline")).toBeVisible();
    await expect(errorRegion.locator(".error-headline")).toContainText(
      /microphone access failed/i,
    );

    // Hint surfaces the actionable copy.
    await expect(errorRegion.locator(".error-hint")).toBeVisible();

    // Technical details are inside a closed <details> element. The
    // raw message appears in the DOM but the body is hidden until
    // the user expands. Pin both: the summary is visible, the body
    // contains the technical chain.
    const details = errorRegion.locator(".error-details");
    await expect(details).toBeVisible();
    await expect(details.locator("summary")).toContainText(
      /technical details/i,
    );
    // Open the disclosure and assert the body contains the raw
    // message — the panel ships closed by default.
    await details.locator("summary").click();
    await expect(details.locator(".error-details-body")).toContainText(
      "deeply: nested: context",
    );
  });

  test("audio-device-lost surfaces 'Microphone disconnected' with the device name (#587)", async ({
    page,
  }) => {
    // Distinct from the generic `audio` bucket above: when the cpal
    // backend reports `StreamError::DeviceNotAvailable`, the audio
    // module wraps a typed `DeviceLost` and the IPC layer routes to
    // `IpcError::AudioDeviceLost`. Frontend shows a clear
    // "Microphone disconnected" headline naming the lost device,
    // not the generic "Microphone access failed."
    // Mock both start paths the same way the generic-audio test
    // above does — the recording button can route to either
    // `meeting_start_manual` or `start_dictation` depending on
    // which audio sources are selected, and we want the test to
    // pin the error rendering regardless of route.
    await installMocks(page, {
      meeting_start_manual: () => {
        throw {
          kind: "audio-device-lost",
          message: "MacBook Pro Microphone",
        };
      },
      start_dictation: () => {
        throw {
          kind: "audio-device-lost",
          message: "MacBook Pro Microphone",
        };
      },
    });
    await page.goto("/");

    await page.getByRole("button", { name: "Start recording" }).click();

    const errorRegion = page.getByRole("alert").first();
    await expect(errorRegion).toBeVisible();
    await expect(errorRegion.locator(".error-headline")).toContainText(
      /microphone disconnected/i,
    );
    // Hint includes the device name so the user knows which input
    // walked away when they have several plugged in.
    await expect(errorRegion.locator(".error-hint")).toContainText(
      /MacBook Pro Microphone/,
    );
  });
});

test.describe("first-time setup banner", () => {
  test("shows when no model is downloaded and disables Start", async ({ page }) => {
    await installMocks(page, {
      // Override the default catalog mock to simulate fresh install:
      // no card has `isDownloaded: true`.
      // Field shape mirrors the Rust ModelCard serde rename — see
      // tests/e2e/_mock.ts for the canonical default and the rationale.
      model_list: () => [
        {
          id: "whisper-base",
          displayName: "Whisper Base",
          filename: "ggml-base.bin",
          sizeMb: 142,
          speedRating: 9,
          accuracyRating: 6,
          description: "Default. Fast, decent for dictation.",
          isDefault: true,
          downloadUrl: "https://example.test/ggml-base.bin",
          sha256: "abc",
          isDownloaded: false,
          isSelected: false,
          expectedPath: "/tmp/models/ggml-base.bin",
        },
      ],
    });
    await page.goto("/");

    // Banner is visible with the action button.
    await expect(page.getByText("Set up your first model")).toBeVisible();
    await expect(
      page.getByRole("button", { name: "Choose a model", exact: true }),
    ).toBeVisible();

    // Start is disabled — clicking through to a "no model" error is
    // worse UX than not letting them click at all. Match by aria-label
    // (the visible label is "● Start recording" with a leading dot).
    await expect(
      page.getByRole("button", { name: "Choose a model first", exact: true }),
    ).toBeDisabled();
  });

  test("disappears when at least one model is downloaded", async ({ page }) => {
    // Default mocks have isDownloaded: true on Whisper Base.
    await installMocks(page);
    await page.goto("/");

    await expect(page.getByText("Set up your first model")).toHaveCount(0);
    await expect(page.getByRole("button", { name: "Start recording" })).toBeEnabled();
  });
});
