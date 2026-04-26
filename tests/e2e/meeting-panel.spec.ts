import { expect, test } from "@playwright/test";
import { installMocks } from "./_mock";

// Meeting panel UX specs spanning Phase 1–3 of the meeting-mode
// roadmap (#122). The panel:
//
//  - Renders an inline mic dropdown + a "Also record system audio"
//    checkbox (Phase 3 multi-source). The dictation source picker
//    in `section.controls` is independent — these tests scope to
//    `section.panel-meetings`.
//  - Defaults the system-audio toggle to ON when the backend reports
//    the entry as `isSupported: true`, OFF otherwise.
//  - Active-session view replaces the picker with an "Auto-recording
//    from <sources>" line + live utterance counter.

test.describe("meeting panel — multi-source picker", () => {
  test("idle panel renders mic dropdown + system-audio checkbox", async ({
    page,
  }) => {
    await installMocks(page);
    await page.goto("/");

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
    await expect(panel).toContainText(/coming soon/i);

    // Phase 3 hint copy: explains auto-recording, not hotkey-driven.
    await expect(panel).toContainText(/Click Start to begin auto-recording/i);
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
    });
    await page.goto("/");

    const panel = page.locator("section.panel-meetings");

    // Live utterance counter reads from the active session row.
    await expect(panel).toContainText(/3 utterances so far/i);
    // Phase 3 active-session copy — auto-recording, not hotkey-driven.
    await expect(panel).toContainText(/Auto-recording/i);
    // Picker is hidden once the session is active.
    await expect(panel.locator("select")).toHaveCount(0);
    await expect(panel.locator('input[type="checkbox"]')).toHaveCount(0);

    // Stop button replaces Start.
    await expect(
      panel.getByRole("button", { name: "Stop session" }),
    ).toBeVisible();
  });
});
