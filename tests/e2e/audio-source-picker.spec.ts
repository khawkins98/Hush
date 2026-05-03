import { expect, test } from "@playwright/test";
import { installMocks } from "./_mock";

// Phase A1 of the system-audio + meeting-mode pivot (#33) replaced
// the simple input-device dropdown with a grouped source picker:
// every mic device under a "Microphone" group, the system-audio
// entry under a "System audio" group. The system-audio option is
// rendered disabled with a "(coming soon — #33)" suffix until a
// per-platform backend ships.
//
// These specs pin the rendered shape so a future change that
// silently drops the system-audio entry, flips the disabled state,
// or changes the wire shape of `start_dictation`'s argument fails
// loud here rather than at hands-on smoke time.
//
// As of Phase 1 of the meeting-mode UX roadmap (#122), the meeting
// panel renders a *second* copy of the source picker so the user
// picks a meeting's source in the same place where they start the
// session. These specs scope to the dictation controls section
// (`section.controls`) so the assertions stay about the dictation
// hot path's picker; the meeting panel picker has its own coverage.
//
// UI design system (#364/#365): the native `<select>` was replaced
// with a custom listbox component (Select.svelte). Options now use
// ARIA roles (role="option", aria-disabled) and data-testid attrs
// instead of native HTML select/optgroup/option elements. The
// dropdown must be opened (trigger click) before options are visible.

test.describe("audio source picker", () => {
  test("renders both microphone and system-audio optgroups", async ({
    page,
  }) => {
    // Default mock from `_mock.ts` returns one mic + one system-audio
    // entry with `isSupported: false` (the current pre-platform-impl
    // state). That's the shape the user sees on first launch today.
    await installMocks(page);
    await page.goto("/");

    // Scope to the dictation controls section so we don't pick up the
    // meeting-panel picker added in #122 Phase 1.
    // After #468 r3 the audio picker is the left adjunct flanking
    // the Record button. Scope to `#dictation-section` to avoid
    // the meeting-panel picker.
    const controls = page.locator("#dictation-section");

    // Wait for the custom trigger to mount (loading placeholder is a <p>).
    const trigger = controls.locator('[data-testid="source-picker-trigger"]');
    await expect(trigger).toBeVisible();

    // Open the dropdown before checking options.
    await trigger.click();

    // The picker wraps options in groups with data-group-label.
    const micGroup = controls.locator('[data-group-label="Microphone"]');
    const sysGroup = controls.locator('[data-group-label="System audio"]');
    await expect(micGroup).toHaveCount(1);
    await expect(sysGroup).toHaveCount(1);

    // The mock surfaces "Built-in Microphone" as the only mic.
    const micOption = micGroup.locator('[role="option"]').first();
    await expect(micOption).toHaveText(/Built-in Microphone/);

    // The system-audio option is the disabled "coming soon" affordance.
    // aria-disabled="true" is the custom listbox's disabled signal.
    const sysOption = sysGroup.locator('[role="option"]').first();
    await expect(sysOption).toHaveAttribute("aria-disabled", "true");
    await expect(sysOption).toContainText(/coming soon/i);
    await expect(sysOption).not.toContainText(/#33/);
  });

  test("system-audio option becomes selectable when backend reports support", async ({
    page,
  }) => {
    // Override the default mock to simulate a platform whose backend
    // has shipped system-audio capture (e.g. a future PR landing
    // ScreenCaptureKit). The picker should drop the disabled state
    // AND the "coming soon" suffix.
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

    // After #468 r3 the audio picker is the left adjunct flanking
    // the Record button. Scope to `#dictation-section` to avoid
    // the meeting-panel picker.
    const controls = page.locator("#dictation-section");
    const trigger = controls.locator('[data-testid="source-picker-trigger"]');
    await expect(trigger).toBeVisible();
    await trigger.click();

    const sysOption = controls
      .locator('[data-group-label="System audio"]')
      .locator('[role="option"]')
      .first();
    // No `aria-disabled` = enabled.
    await expect(sysOption).not.toHaveAttribute("aria-disabled", "true");
    await expect(sysOption).not.toContainText(/coming soon/i);
  });

  test("Start invokes meeting_start_manual with a single mic AudioSource", async ({
    page,
  }) => {
    // Pin the wire shape of the source list passed to the meeting
    // pump. Click-record always goes through `meeting_start_manual`
    // post-#468 r3 — single-source mic when SCK isn't confirmed,
    // [mic, system-audio] when SCK is. PTT keeps using
    // `start_dictation` (hot-path), but click-record's contract
    // is the meeting pump.
    const seen: { sources: unknown }[] = [];
    await page.exposeFunction("__hush_record_start", (args: unknown) => {
      seen.push(args as { sources: unknown });
    });
    await installMocks(page, {
      meeting_start_manual: (args: unknown) => {
        (
          window as unknown as {
            __hush_record_start: (a: unknown) => void;
          }
        ).__hush_record_start(args);
        return { id: 1, startedAt: Date.now(), endedAt: null };
      },
    });
    await page.goto("/");

    await page.getByRole("button", { name: "Start recording" }).click();

    await expect.poll(() => seen.length).toBeGreaterThan(0);

    // Default mock has SCK NOT confirmed, so click-record stays
    // single-source mic. Multi-source path is exercised when the
    // permission_health mock returns confirmed.
    expect(seen[0]).toMatchObject({
      sources: [
        {
          kind: "microphone",
          deviceId: "Built-in Microphone",
        },
      ],
    });
  });

  test("Start emits ui:recording-state(true) so the tray label can sync", async ({
    page,
  }) => {
    // The tray's "Start / Stop Recording" menu item mirrors the
    // frontend's `recording` rune via the `ui:recording-state` event
    // (see src-tauri/src/tray/mod.rs::build). Without this emit the
    // tray label would freeze on "Start Recording" forever — a silent
    // regression CI couldn't catch otherwise. Pinning the contract
    // here keeps the four-place IPC sync rule honest for the cross-
    // window event channel.
    await installMocks(page);

    // Inject a wrapper around the e2e bus's fire() that records
    // every emitted ui:recording-state payload onto `window`.
    // Installed via addInitScript so it lands before the page's
    // first $effect runs.
    await page.addInitScript(() => {
      (window as unknown as { __hush_recording_events: unknown[] })
        .__hush_recording_events = [];
      // Wait for the bus singleton to come up, then wrap fire().
      const interval = window.setInterval(() => {
        const bus = (
          window as unknown as {
            __hush_e2e_event_bus?: {
              fire: (n: string, p: unknown) => void;
            };
          }
        ).__hush_e2e_event_bus;
        if (!bus) return;
        const original = bus.fire.bind(bus);
        bus.fire = (name: string, payload: unknown) => {
          if (name === "ui:recording-state") {
            (
              window as unknown as { __hush_recording_events: unknown[] }
            ).__hush_recording_events.push(payload);
          }
          original(name, payload);
        };
        window.clearInterval(interval);
      }, 5);
    });

    await page.goto("/");

    await page.getByRole("button", { name: "Start recording" }).click();

    // After the click, `start_dictation` resolves and the recording
    // rune flips; the $effect in +page.svelte fires
    // `emit("ui:recording-state", true)`.
    await expect
      .poll(() =>
        page.evaluate(() =>
          (window as unknown as { __hush_recording_events: unknown[] })
            .__hush_recording_events.includes(true),
        ),
      )
      .toBe(true);
  });
});
