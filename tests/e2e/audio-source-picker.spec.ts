import { expect, test } from "@playwright/test";
import { installMocks } from "./_mock";

// Phase A1 of the system-audio + meeting-mode pivot (#33) replaced
// the simple input-device dropdown with a grouped source picker:
// every mic device under a "Microphone" optgroup, the system-audio
// entry under a "System audio" optgroup. The system-audio option is
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
    const controls = page.locator("section.controls");

    // Wait for the picker to mount with a real `<select>` (the loading
    // placeholder is a `<p>`).
    await expect(controls.locator("select")).toBeVisible();

    // The picker wraps options in <optgroup> by source kind.
    const micGroup = controls.locator('optgroup[label="Microphone"]');
    const sysGroup = controls.locator('optgroup[label="System audio"]');
    await expect(micGroup).toHaveCount(1);
    await expect(sysGroup).toHaveCount(1);

    // The mock surfaces "Built-in Microphone" as the only mic.
    const micOption = micGroup.locator("option").first();
    await expect(micOption).toHaveText(/Built-in Microphone/);

    // The system-audio option is the disabled "coming soon" affordance.
    // Use the attribute check rather than `toBeDisabled` because
    // Playwright's interactivity-shaped helpers don't fully recognise
    // `<option>` as a disable-able element on every WebKit/Chromium
    // version we test against — the HTML attribute is the canonical
    // signal anyway.
    const sysOption = sysGroup.locator("option").first();
    await expect(sysOption).toHaveAttribute("disabled", "");
    await expect(sysOption).toContainText(/coming soon/i);
    await expect(sysOption).toContainText(/#33/);
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

    const sysOption = page
      .locator("section.controls")
      .locator('optgroup[label="System audio"]')
      .locator("option")
      .first();
    // No `disabled` attribute = enabled. See the parallel test for
    // why we use the attribute rather than `toBeEnabled`.
    await expect(sysOption).not.toHaveAttribute("disabled", "");
    await expect(sysOption).not.toContainText(/coming soon/i);
  });

  test("Start invokes start_dictation with a Microphone AudioSource", async ({
    page,
  }) => {
    // Capture the args passed to `start_dictation` so we can pin the
    // wire shape of the `AudioSource` argument. This is the load-
    // bearing contract the Rust side dispatches on — a future change
    // that drops the discriminator or renames `deviceId` would
    // silently start sending an undecodable shape.
    const seen: { source: unknown }[] = [];
    await page.exposeFunction("__hush_record_start", (args: unknown) => {
      seen.push(args as { source: unknown });
    });
    await installMocks(page, {
      start_dictation: (args: unknown) => {
        // Re-fire on the page side so the test side can collect.
        // Playwright's overrideEntries serialise functions via
        // `toString()`, which is why we need the indirection: a
        // direct closure capture on `seen` would not survive the
        // bridge.
        (
          window as unknown as {
            __hush_record_start: (a: unknown) => void;
          }
        ).__hush_record_start(args);
        return undefined;
      },
    });
    await page.goto("/");

    // Default mock has Whisper Base as isDownloaded — start is enabled.
    await page.getByRole("button", { name: "Start recording" }).click();

    // Wait for the invoke to land.
    await expect.poll(() => seen.length).toBeGreaterThan(0);

    // The discriminated AudioSource argument: kind="microphone",
    // deviceId is the picker's selected id. The default mock pre-
    // populates the picker with "Built-in Microphone" (the one mic
    // returned by `audio_list_sources`).
    expect(seen[0]).toMatchObject({
      source: {
        kind: "microphone",
        deviceId: "Built-in Microphone",
      },
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
