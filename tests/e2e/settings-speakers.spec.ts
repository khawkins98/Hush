import { expect, test, type Page } from "@playwright/test";

import type { SpeakerIdentity } from "../../src/lib/types";

import { installMocks } from "./_mock";

// Settings → Meeting → Speakers → "Saved speakers" (#1013 follow-up).
// After the CMN fix, pre-fix voiceprints ("Older voice profile") never
// match new meetings, so a returning person appears as a new speaker.
// Merging that new speaker *into* the older named profile re-enrols it.
// The backend list lives Node-side so merge/rename/delete visibly update
// it, like the real store (mocks are serialised and can't hold state).

function identity(over: Partial<SpeakerIdentity> & { id: number }): SpeakerIdentity {
  return {
    displayName: null,
    utteranceCount: 6,
    confidenceState: "confirmed",
    createdAt: "2026-10-01T10:00:00Z",
    updatedAt: "2026-10-01T10:00:00Z",
    legacyVoiceprint: false,
    ...over,
  };
}

async function setup(page: Page, initial: SpeakerIdentity[]) {
  let speakers = [...initial];
  const calls: Array<{ cmd: string; args: Record<string, unknown> }> = [];
  await page.exposeFunction("__hushSpeakers", () => speakers);
  await page.exposeFunction("__hushSpeakerCall", (cmd: string, args: Record<string, unknown>) => {
    calls.push({ cmd, args });
    if (cmd === "speaker_merge") {
      const keep = speakers.find((s) => s.id === args.keepId)!;
      const absorb = speakers.find((s) => s.id === args.absorbId)!;
      keep.legacyVoiceprint = keep.legacyVoiceprint && absorb.legacyVoiceprint;
      keep.utteranceCount = absorb.utteranceCount;
      speakers = speakers.filter((s) => s.id !== args.absorbId);
    } else if (cmd === "speaker_rename") {
      const s = speakers.find((x) => x.id === args.id)!;
      s.displayName = (args.displayName as string | null) ?? null;
    } else if (cmd === "speaker_delete") {
      speakers = speakers.filter((s) => s.id !== args.id);
    }
  });
  type W = {
    __hushSpeakers: () => Promise<SpeakerIdentity[]>;
    __hushSpeakerCall: (cmd: string, args: unknown) => Promise<void>;
  };
  await installMocks(page, {
    get_diarization_enabled: () => true,
    get_speaker_identity_enabled: () => true,
    speaker_list: async () => (window as unknown as W).__hushSpeakers(),
    speaker_merge: async (args: unknown) =>
      (window as unknown as W).__hushSpeakerCall("speaker_merge", args),
    speaker_rename: async (args: unknown) =>
      (window as unknown as W).__hushSpeakerCall("speaker_rename", args),
    speaker_delete: async (args: unknown) =>
      (window as unknown as W).__hushSpeakerCall("speaker_delete", args),
  });
  await page.goto("/");
  await page.locator('[data-testid="sidebar-nav-settings"]').click();
  await page.locator('[data-testid="settings-tab-meeting"]').click();
  await expect(page.getByTestId("saved-speakers")).toBeVisible();
  return calls;
}

test.describe("Settings → Saved speakers", () => {
  test("merging a new speaker into an older named profile re-enrols it", async ({ page }) => {
    const calls = await setup(page, [
      identity({ id: 1, displayName: "Ken", utteranceCount: 40, legacyVoiceprint: true }),
      identity({ id: 7, utteranceCount: 6 }),
    ]);

    await expect(page.getByTestId("saved-speakers-legacy-hint")).toBeVisible();
    await expect(page.getByTestId("saved-speaker-1").getByTestId("legacy-badge")).toBeVisible();

    const fresh = page.getByTestId("saved-speaker-7");
    await fresh.getByRole("button", { name: "Merge into…" }).click();
    const merge = fresh.getByTestId("speaker-merge-panel");
    await merge.getByRole("combobox").selectOption({ label: "Ken (older profile)" });
    await expect(merge).toContainText("Ken keeps its name");
    await expect(merge).toContainText("recognised again in new meetings");
    await merge.getByRole("button", { name: "Merge" }).click();

    await expect.poll(() => calls.length).toBe(1);
    // The chosen target is kept (its name survives); the row merged away is absorbed.
    expect(calls[0]).toEqual({ cmd: "speaker_merge", args: { keepId: 1, absorbId: 7 } });
    await expect(page.getByTestId("saved-speaker-7")).toHaveCount(0);
    await expect(page.getByTestId("saved-speaker-1").getByTestId("legacy-badge")).toHaveCount(0);
    await expect(page.getByTestId("saved-speakers-legacy-hint")).toHaveCount(0);
  });

  test("rename and two-step delete", async ({ page }) => {
    const calls = await setup(page, [identity({ id: 3 })]);
    const row = page.getByTestId("saved-speaker-3");
    await expect(row).toContainText("Unnamed speaker 3");

    await row.getByRole("button", { name: "Rename" }).click();
    await row.getByRole("textbox").fill("Priya");
    await row.getByRole("button", { name: "Save" }).click();
    await expect(row).toContainText("Priya");
    expect(calls[0]).toEqual({ cmd: "speaker_rename", args: { id: 3, displayName: "Priya" } });

    await row.getByRole("button", { name: "Delete Priya" }).click();
    expect(calls).toHaveLength(1);
    await row.getByRole("button", { name: "Click again to delete Priya" }).click();
    await expect(page.getByTestId("saved-speaker-3")).toHaveCount(0);
    expect(calls[1]).toEqual({ cmd: "speaker_delete", args: { id: 3 } });
  });
});
