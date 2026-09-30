import { expect, test, type Locator } from "@playwright/test";
import { gotoSection, installMocks } from "./_mock";

// Hover must not undo a component's own state styling. Both regressions
// came from a broad scoped hover rule out-specifying a class rule.

// Both components animate these properties (0.12–0.15 s), so read only
// after the transition settles or a regression can hide mid-animation.
const SETTLE_MS = 400;

async function styleOf(el: Locator, prop: string): Promise<string> {
  return el.evaluate((node, p) => getComputedStyle(node).getPropertyValue(p), prop);
}

test("active History filter chip keeps its selected fill on hover", async ({ page }) => {
  // The chip strip only renders once there's history to filter.
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
  const active = page.locator(".filter-chip.active").first();
  await expect(active).toBeVisible();
  // Park the pointer elsewhere first so "before" is a genuine rest state.
  await page.mouse.move(0, 0);
  const before = await styleOf(active, "background-color");
  await active.hover();
  await page.waitForTimeout(SETTLE_MS);
  expect(await styleOf(active, "background-color")).toBe(before);
});

test("Download (kh-button) keeps its hard-shadow outline on hover", async ({ page }) => {
  await installMocks(page, {
    model_list: () => [
      {
        id: "whisper-base",
        displayName: "Whisper Base",
        filename: "ggml-base.bin",
        sizeMb: 142,
        speedRating: 9,
        accuracyRating: 6,
        description: "Fast and lightweight.",
        isDefault: false,
        downloadUrl: "https://example.test/ggml-base.bin",
        sha256: "a".repeat(64),
        isDownloaded: false,
        isSelected: false,
        expectedPath: "/tmp/models/ggml-base.bin",
      },
    ],
  });
  await page.goto("/");
  await page.getByRole("button", { name: /Settings/ }).first().click();
  await page.getByRole("button", { name: /^Model$/ }).first().click();
  const download = page.getByRole("button", { name: "Download", exact: true });
  await expect(download).toBeVisible();
  await page.mouse.move(0, 0);
  const before = await styleOf(download, "border-top-color");
  await download.hover();
  await page.waitForTimeout(SETTLE_MS);
  expect(await styleOf(download, "border-top-color")).toBe(before);
});
