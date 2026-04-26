import { expect, test } from "@playwright/test";
import { installMocks } from "./_mock";

// Phase 1 of the meeting-mode UX roadmap (#122) brings the audio
// source picker into the meeting panel itself — same listing the
// dictation controls show, just rendered inline with the Start
// button so the user picks mic vs system-audio in the same place
// where they kick off the session. The active-session line also
// surfaces the live utterance count and the picked source.
//
// These specs pin the panel's idle and active shapes so a future
// change can't silently regress the empty-session-feels-hollow fix
// this phase set out to make.

test.describe("meeting panel — Phase 1 picker + active line", () => {
  test("idle panel renders its own source picker scoped to .panel-meetings", async ({
    page,
  }) => {
    await installMocks(page);
    await page.goto("/");

    const panel = page.locator("section.panel-meetings");
    await expect(panel).toBeVisible();

    // Picker mounted inside the panel, not just in dictation controls.
    const select = panel.locator("select");
    await expect(select).toBeVisible();
    // Same optgroup structure as the dictation picker — mics first,
    // then the system-audio entry (disabled in default-mock state).
    await expect(panel.locator('optgroup[label="Microphone"]')).toHaveCount(1);
    await expect(panel.locator('optgroup[label="System audio"]')).toHaveCount(
      1,
    );

    // Hint copy primes the user that dictation drives utterances.
    await expect(panel).toContainText(/Pick the source, click Start/i);
  });

  test("active session swaps in dictation prompt + utterance counter", async ({
    page,
  }) => {
    // Force the panel into the active-session branch so we can pin
    // the Phase 1 prompt copy and counter binding without needing to
    // round-trip Start through the backend.
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
    });
    await page.goto("/");

    const panel = page.locator("section.panel-meetings");

    // Live utterance counter reads from the active session row.
    await expect(panel).toContainText(/3 utterances so far/i);
    // Phase 1 prompt — the load-bearing copy that keeps users from
    // walking away from an empty session.
    await expect(panel).toContainText(/Press your dictation hotkey/i);
    // The picker is hidden once the session is active; the source is
    // shown as a static label inside a <code> tag instead.
    await expect(panel.locator("select")).toHaveCount(0);
    await expect(panel.locator("code")).toContainText(/Built-in Microphone/i);

    // Stop button replaces Start.
    await expect(
      panel.getByRole("button", { name: "Stop session" }),
    ).toBeVisible();
  });
});
