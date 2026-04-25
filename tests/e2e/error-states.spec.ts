import { expect, test } from "@playwright/test";
import { installMocks } from "./_mock";

// Error-copy regression smokes. The frontend maps backend
// `IpcError::*` variants to recovery-oriented copy via switch-on-kind.
// Round-3 reviewer flagged that this dispatch is fragile if future
// edits drift the kind strings — these tests pin the rendered copy
// for the variants the user is most likely to hit.

test.describe("IPC error copy", () => {
  test("transcription-unavailable surfaces the model-path hint", async ({ page }) => {
    await installMocks(page, {
      stop_dictation: () => {
        // Tauri's invoke rejects with the serialised IpcError shape.
        // The frontend's catch block reads `err.kind`.
        throw { kind: "transcription-unavailable" };
      },
    });
    await page.goto("/");

    // Click Start, then Stop — the start succeeds (default mock),
    // the stop throws.
    await page.getByRole("button", { name: "Start recording" }).click();
    await page.getByRole("button", { name: "Stop recording and transcribe" }).click();

    // The recovery copy mentions HUSH_MODEL_PATH or "model" so a
    // user knows what to fix. We assert on a substring rather than
    // pinning the whole string — wording can drift, the *signal*
    // is what matters.
    const errorRegion = page.getByRole("alert");
    await expect(errorRegion).toBeVisible();
    await expect(errorRegion).toContainText(/model|whisper|HUSH_MODEL_PATH/i);
  });
});
