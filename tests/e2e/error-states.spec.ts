import { expect, test } from "@playwright/test";
import { installMocks } from "./_mock";

// Error-copy regression smokes. The frontend maps backend
// `IpcError::*` variants to recovery-oriented copy via switch-on-kind.
// Round-3 reviewer flagged that this dispatch is fragile if future
// edits drift the kind strings — these tests pin the rendered copy
// for the variants the user is most likely to hit.

test.describe("IPC error copy", () => {
  test("transcription-unavailable points at the model picker", async ({ page }) => {
    // The default mock `model_list` has Whisper Base as `isDownloaded: true`
    // — that suppresses the no-model banner so we exercise the
    // post-click-error path, not the never-let-the-user-click-Start path.
    // (The banner-instead-of-error path is covered in
    // first-run.spec.ts's no-model test.)
    await installMocks(page, {
      stop_dictation: () => {
        // Tauri's invoke rejects with the serialised IpcError shape.
        // The frontend's catch block reads `err.kind`.
        throw { kind: "transcription-unavailable" };
      },
    });
    await page.goto("/");

    await page.getByRole("button", { name: "Start recording" }).click();
    await page.getByRole("button", { name: "Stop recording and transcribe" }).click();

    // The new copy points at the in-app Models section and mentions
    // Download — replaces the stale HUSH_MODEL_PATH instruction. Match
    // on substring so the wording can drift without breaking the test.
    const errorRegion = page.getByRole("alert").first();
    await expect(errorRegion).toBeVisible();
    await expect(errorRegion).toContainText(/model/i);
    await expect(errorRegion).toContainText(/download/i);
    // Regression: the old copy mentioned HUSH_MODEL_PATH and a
    // milestone-deferred picker. New copy must not.
    await expect(errorRegion).not.toContainText(/HUSH_MODEL_PATH/);
    await expect(errorRegion).not.toContainText(/coming in/i);
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

    // Banner is visible with the action button. Match exact text to
    // disambiguate from the disabled Start button's aria-label
    // "Choose a model first" — both contain the substring "Choose a model".
    await expect(page.getByText("Set up your first model")).toBeVisible();
    await expect(
      page.getByRole("button", { name: "Choose a model", exact: true }),
    ).toBeVisible();

    // Start is disabled — clicking through to a "no model" error is
    // worse UX than not letting them click at all.
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
