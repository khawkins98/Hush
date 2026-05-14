import { expect, test } from "@playwright/test";
import { gotoSection, installMocks } from "./_mock";

// E2E coverage for diarizer model section testids in Settings → Meeting tab:
// diarizer-model-ready, diarizer-source-link,
// diarizer-remove-button, diarizer-remove-confirm, diarizer-remove-cancel,
// diarizer-model-not-installed, diarizer-download-button,
// diarizer-download-error
//
// Also covers the remove_diarizer_model IPC invocation path (#712).

async function openMeetingTab(page: import("@playwright/test").Page) {
  await page.goto("/");
  await gotoSection(page, "configuration");
  await page.locator('[data-testid="settings-tab-meeting"]').click();
}

test.describe("diarizer model section — model installed", () => {
  // Default mock has downloaded: true → renders diarizer-model-ready section.
  test("model-ready section is visible", async ({ page }) => {
    await installMocks(page);
    await openMeetingTab(page);
    await expect(
      page.locator('[data-testid="diarizer-model-ready"]'),
    ).toBeVisible();
  });

  test("source link is present inside model details", async ({ page }) => {
    await installMocks(page);
    await openMeetingTab(page);

    // diarizer-source-link is inside a <details> — open it first.
    await page.locator(".diarizer-installed-details").click();
    await expect(
      page.locator('[data-testid="diarizer-source-link"]'),
    ).toBeVisible();
  });

  test("remove button is visible and clicking it shows confirm/cancel", async ({
    page,
  }) => {
    await installMocks(page);
    await openMeetingTab(page);

    // Initial state: remove button visible, confirm/cancel absent.
    await expect(
      page.locator('[data-testid="diarizer-remove-button"]'),
    ).toBeVisible();
    await expect(
      page.locator('[data-testid="diarizer-remove-confirm"]'),
    ).toHaveCount(0);
    await expect(
      page.locator('[data-testid="diarizer-remove-cancel"]'),
    ).toHaveCount(0);

    // Click remove → enter two-stage confirmation.
    await page.locator('[data-testid="diarizer-remove-button"]').click();
    await expect(
      page.locator('[data-testid="diarizer-remove-confirm"]'),
    ).toBeVisible();
    await expect(
      page.locator('[data-testid="diarizer-remove-cancel"]'),
    ).toBeVisible();
    await expect(
      page.locator('[data-testid="diarizer-remove-button"]'),
    ).toHaveCount(0);
  });

  test("cancel from confirm state restores the remove button", async ({
    page,
  }) => {
    await installMocks(page);
    await openMeetingTab(page);

    await page.locator('[data-testid="diarizer-remove-button"]').click();
    await page.locator('[data-testid="diarizer-remove-cancel"]').click();

    await expect(
      page.locator('[data-testid="diarizer-remove-button"]'),
    ).toBeVisible();
    await expect(
      page.locator('[data-testid="diarizer-remove-confirm"]'),
    ).toHaveCount(0);
  });

  test("confirming removal calls remove_diarizer_model and shows not-installed state", async ({
    page,
  }) => {
    // Use exposeFunction to track calls across the page/Node boundary.
    // remove_diarizer_model awaits the exposed fn so Node-side removeCalls
    // increments before loadDiarizerModelStatus() runs — preventing the race
    // where get_diarizer_model_status might still return downloaded:true.
    let removeCalls = 0;
    await page.exposeFunction("__hush_record_remove", () => {
      removeCalls += 1;
    });
    await page.exposeFunction("__hush_was_remove_called", () => removeCalls > 0);

    await installMocks(page, {
      remove_diarizer_model: async () => {
        await (
          window as unknown as { __hush_record_remove: () => Promise<void> }
        ).__hush_record_remove();
      },
      // Returns downloaded:false after removal so the UI transitions to
      // the not-installed section.
      get_diarizer_model_status: async () => {
        const removed = await (
          window as unknown as {
            __hush_was_remove_called: () => Promise<boolean>;
          }
        ).__hush_was_remove_called();
        return removed
          ? {
              downloaded: false,
              displayName: "wespeaker ResNet34-LM",
              sizeMb: 26,
              sha256:
                "7bb2f06e9df17cdf1ef14ee8a15ab08ed28e8d0ef5054ee135741560df2ec068",
              expectedPath:
                "/Users/test/Library/Application Support/com.hush.dev/models/voxceleb_resnet34_LM.onnx",
              sourceUrl:
                "https://huggingface.co/Wespeaker/wespeaker-voxceleb-resnet34-LM",
            }
          : {
              downloaded: true,
              displayName: "wespeaker ResNet34-LM",
              sizeMb: 26,
              sha256:
                "7bb2f06e9df17cdf1ef14ee8a15ab08ed28e8d0ef5054ee135741560df2ec068",
              expectedPath:
                "/Users/test/Library/Application Support/com.hush.dev/models/voxceleb_resnet34_LM.onnx",
              sourceUrl:
                "https://huggingface.co/Wespeaker/wespeaker-voxceleb-resnet34-LM",
            };
      },
    });
    await openMeetingTab(page);

    await page.locator('[data-testid="diarizer-remove-button"]').click();
    await page.locator('[data-testid="diarizer-remove-confirm"]').click();

    await expect.poll(() => removeCalls).toBeGreaterThan(0);
    await expect(
      page.locator('[data-testid="diarizer-model-not-installed"]'),
    ).toBeVisible();
  });
});

test.describe("diarizer model section — model not installed", () => {
  // Override downloaded: false → renders diarizer-model-not-installed section.
  test("not-installed section shows download button", async ({ page }) => {
    await installMocks(page, {
      get_diarizer_model_status: () => ({
        downloaded: false,
        displayName: "wespeaker ResNet34-LM",
        sizeMb: 26,
        sha256:
          "7bb2f06e9df17cdf1ef14ee8a15ab08ed28e8d0ef5054ee135741560df2ec068",
        expectedPath:
          "/Users/test/Library/Application Support/com.hush.dev/models/voxceleb_resnet34_LM.onnx",
        sourceUrl:
          "https://huggingface.co/Wespeaker/wespeaker-voxceleb-resnet34-LM",
      }),
    });
    await openMeetingTab(page);

    await expect(
      page.locator('[data-testid="diarizer-model-not-installed"]'),
    ).toBeVisible();
    await expect(
      page.locator('[data-testid="diarizer-download-button"]'),
    ).toBeVisible();
  });

  test("download error message appears when download_diarizer_model throws", async ({
    page,
  }) => {
    await installMocks(page, {
      get_diarizer_model_status: () => ({
        downloaded: false,
        displayName: "wespeaker ResNet34-LM",
        sizeMb: 26,
        sha256:
          "7bb2f06e9df17cdf1ef14ee8a15ab08ed28e8d0ef5054ee135741560df2ec068",
        expectedPath:
          "/Users/test/Library/Application Support/com.hush.dev/models/voxceleb_resnet34_LM.onnx",
        sourceUrl:
          "https://huggingface.co/Wespeaker/wespeaker-voxceleb-resnet34-LM",
      }),
      // Simulate a download failure.
      download_diarizer_model: () => {
        throw new Error("Network unreachable");
      },
    });
    await openMeetingTab(page);

    await page.locator('[data-testid="diarizer-download-button"]').click();
    await expect(
      page.locator('[data-testid="diarizer-download-error"]'),
    ).toBeVisible();
  });
});
