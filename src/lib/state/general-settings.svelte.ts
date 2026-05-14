/// General interface settings state module (#694). Extracted from
/// GeneralInterfaceSection.svelte so HUD and sound-cue IPC calls
/// have a single owner and the component can import rather than
/// inline each get/set pair.
///
/// What lives here:
///   - hudEnabled, soundCuesEnabled, soundCueStartEnabled,
///     soundCueCompleteEnabled reactive state + per-field busy/error
///   - load() (parallel-fetches all settings on mount)
///   - setHudEnabled(), setSoundCuesEnabled(), setSoundCueStartEnabled(),
///     setSoundCueCompleteEnabled(), previewSoundCue()
///
/// What stays in GeneralInterfaceSection:
///   - the template (checkbox rows, preview buttons, error messages)
///   - onMount wiring
import { invoke } from "@tauri-apps/api/core";

import { formatErrorMessage } from "$lib/errors";

let hudEnabled = $state(true);
let hudBusy = $state(false);
let hudError = $state<string | null>(null);

let soundCuesEnabled = $state(false);
let soundCuesBusy = $state(false);
let soundCuesError = $state<string | null>(null);

let soundCueStartEnabled = $state(true);
let soundCueCompleteEnabled = $state(true);
let soundCueStartBusy = $state(false);
let soundCueCompleteBusy = $state(false);
let soundCueSubError = $state<string | null>(null);

export const generalSettings = {
  get hudEnabled() {
    return hudEnabled;
  },
  get hudBusy() {
    return hudBusy;
  },
  get hudError() {
    return hudError;
  },
  get soundCuesEnabled() {
    return soundCuesEnabled;
  },
  get soundCuesBusy() {
    return soundCuesBusy;
  },
  get soundCuesError() {
    return soundCuesError;
  },
  get soundCueStartEnabled() {
    return soundCueStartEnabled;
  },
  get soundCueCompleteEnabled() {
    return soundCueCompleteEnabled;
  },
  get soundCueStartBusy() {
    return soundCueStartBusy;
  },
  get soundCueCompleteBusy() {
    return soundCueCompleteBusy;
  },
  get soundCueSubError() {
    return soundCueSubError;
  },

  /// Load all interface settings in parallel on mount.
  async load(): Promise<void> {
    await Promise.all([
      generalSettings.loadHudEnabled(),
      generalSettings.loadSoundCuesEnabled(),
      generalSettings.loadSoundCueSubEnabled(),
    ]);
  },

  async loadHudEnabled(): Promise<void> {
    try {
      hudEnabled = await invoke<boolean>("get_hud_enabled");
      hudError = null;
    } catch (e) {
      hudError = "Couldn't read HUD setting.";
      console.warn("[hush] get_hud_enabled failed", e);
    }
  },

  async loadSoundCuesEnabled(): Promise<void> {
    try {
      soundCuesEnabled = await invoke<boolean>("get_sound_cues_enabled");
      soundCuesError = null;
    } catch (e) {
      soundCuesError = "Couldn't read audio-cues setting.";
      console.warn("[hush] get_sound_cues_enabled failed", e);
    }
  },

  async loadSoundCueSubEnabled(): Promise<void> {
    try {
      const [start, complete] = await Promise.all([
        invoke<boolean>("get_sound_cue_start_enabled"),
        invoke<boolean>("get_sound_cue_complete_enabled"),
      ]);
      soundCueStartEnabled = start;
      soundCueCompleteEnabled = complete;
      soundCueSubError = null;
    } catch (e) {
      soundCueSubError = "Couldn't read per-event audio-cue settings.";
      console.warn("[hush] get_sound_cue_*_enabled failed", e);
    }
  },

  async setHudEnabled(enabled: boolean): Promise<void> {
    hudBusy = true;
    hudError = null;
    try {
      await invoke("set_hud_enabled", { enabled });
      hudEnabled = enabled;
    } catch (e) {
      hudError = formatErrorMessage(e);
      await generalSettings.loadHudEnabled();
    } finally {
      hudBusy = false;
    }
  },

  async setSoundCuesEnabled(enabled: boolean): Promise<void> {
    soundCuesBusy = true;
    soundCuesError = null;
    try {
      await invoke("set_sound_cues_enabled", { enabled });
      soundCuesEnabled = enabled;
    } catch (e) {
      soundCuesError = formatErrorMessage(e);
      await generalSettings.loadSoundCuesEnabled();
    } finally {
      soundCuesBusy = false;
    }
  },

  async setSoundCueStartEnabled(enabled: boolean): Promise<void> {
    soundCueStartBusy = true;
    soundCueSubError = null;
    try {
      await invoke("set_sound_cue_start_enabled", { enabled });
      soundCueStartEnabled = enabled;
    } catch (e) {
      soundCueSubError = formatErrorMessage(e);
      await generalSettings.loadSoundCueSubEnabled();
    } finally {
      soundCueStartBusy = false;
    }
  },

  async setSoundCueCompleteEnabled(enabled: boolean): Promise<void> {
    soundCueCompleteBusy = true;
    soundCueSubError = null;
    try {
      await invoke("set_sound_cue_complete_enabled", { enabled });
      soundCueCompleteEnabled = enabled;
    } catch (e) {
      soundCueSubError = formatErrorMessage(e);
      await generalSettings.loadSoundCueSubEnabled();
    } finally {
      soundCueCompleteBusy = false;
    }
  },

  /// Play a preview of the given sound cue. Best-effort: failures
  /// are logged to console but not surfaced in UI — a missing
  /// preview chime is not a functional error.
  async previewSoundCue(kind: "start" | "done"): Promise<void> {
    try {
      await invoke("preview_sound_cue", { kind });
    } catch (e) {
      console.warn("[hush] preview_sound_cue failed", e);
    }
  },
};
