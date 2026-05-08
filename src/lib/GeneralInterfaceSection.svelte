<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";

  import { formatErrorMessage } from "./errors";
  import "./settings-tab.css";

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

  async function loadHudEnabled(): Promise<void> {
    try {
      hudEnabled = await invoke<boolean>("get_hud_enabled");
      hudError = null;
    } catch (e) {
      hudError = "Couldn't read HUD setting.";
      console.warn("[hush] get_hud_enabled failed", e);
    }
  }

  async function onHudToggle(e: Event) {
    const checked = (e.target as HTMLInputElement).checked;
    hudBusy = true;
    hudError = null;
    try {
      await invoke("set_hud_enabled", { enabled: checked });
      hudEnabled = checked;
    } catch (err) {
      hudError = formatErrorMessage(err);
      await loadHudEnabled();
    } finally {
      hudBusy = false;
    }
  }

  async function loadSoundCuesEnabled(): Promise<void> {
    try {
      soundCuesEnabled = await invoke<boolean>("get_sound_cues_enabled");
      soundCuesError = null;
    } catch (e) {
      soundCuesError = "Couldn't read audio-cues setting.";
      console.warn("[hush] get_sound_cues_enabled failed", e);
    }
  }

  async function onSoundCuesToggle(e: Event) {
    const checked = (e.target as HTMLInputElement).checked;
    soundCuesBusy = true;
    soundCuesError = null;
    try {
      await invoke("set_sound_cues_enabled", { enabled: checked });
      soundCuesEnabled = checked;
    } catch (err) {
      soundCuesError = formatErrorMessage(err);
      await loadSoundCuesEnabled();
    } finally {
      soundCuesBusy = false;
    }
  }

  async function loadSoundCueSubEnabled(): Promise<void> {
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
  }

  async function onSoundCueStartToggle(e: Event) {
    const checked = (e.target as HTMLInputElement).checked;
    soundCueStartBusy = true;
    soundCueSubError = null;
    try {
      await invoke("set_sound_cue_start_enabled", { enabled: checked });
      soundCueStartEnabled = checked;
    } catch (err) {
      soundCueSubError = formatErrorMessage(err);
      await loadSoundCueSubEnabled();
    } finally {
      soundCueStartBusy = false;
    }
  }

  async function onPreviewCue(kind: "start" | "done") {
    try {
      await invoke("preview_sound_cue", { kind });
    } catch (e) {
      console.warn("[hush] preview_sound_cue failed", e);
    }
  }

  async function onSoundCueCompleteToggle(e: Event) {
    const checked = (e.target as HTMLInputElement).checked;
    soundCueCompleteBusy = true;
    soundCueSubError = null;
    try {
      await invoke("set_sound_cue_complete_enabled", { enabled: checked });
      soundCueCompleteEnabled = checked;
    } catch (err) {
      soundCueSubError = formatErrorMessage(err);
      await loadSoundCueSubEnabled();
    } finally {
      soundCueCompleteBusy = false;
    }
  }

  onMount(() => {
    void Promise.all([
      loadHudEnabled(),
      loadSoundCuesEnabled(),
      loadSoundCueSubEnabled(),
    ]);
  });
</script>

<section class="settings-group" aria-labelledby="settings-interface-heading">
  <h2 id="settings-interface-heading" class="group-heading">Interface</h2>
  <label class="toggle-row">
    <input
      type="checkbox"
      data-testid="settings-hud-toggle"
      disabled={hudBusy}
      checked={hudEnabled}
      onchange={onHudToggle}
    />
    <span class="toggle-label">
      <span class="toggle-name">Show recording HUD</span>
      <span class="toggle-desc">
        The floating pill that appears in the top-right corner
        while Hush is capturing audio. Off hides it for both
        dictation and meeting mode; recording itself is
        unaffected.
      </span>
    </span>
  </label>
  {#if hudError}
    <p class="settings-error">{hudError}</p>
  {/if}

  <label class="toggle-row">
    <input
      type="checkbox"
      data-testid="settings-sound-cues-toggle"
      disabled={soundCuesBusy}
      checked={soundCuesEnabled}
      onchange={onSoundCuesToggle}
    />
    <span class="toggle-label">
      <span class="toggle-name">Audio cues</span>
      <span class="toggle-desc">
        Plays a short chime when recording starts and a
        second chime when the transcript is on the
        clipboard. Honours your system volume and Do Not
        Disturb. Off keeps Hush silent.
      </span>
    </span>
  </label>
  {#if soundCuesError}
    <p class="settings-error">{soundCuesError}</p>
  {/if}

  <div
    class="sound-cue-subtoggles"
    class:is-disabled={!soundCuesEnabled}
    aria-label="Per-event audio cues"
  >
    <div class="toggle-row toggle-row-sub">
      <label class="toggle-row-inner">
        <input
          type="checkbox"
          data-testid="settings-sound-cue-start-toggle"
          disabled={!soundCuesEnabled || soundCueStartBusy}
          checked={soundCueStartEnabled}
          onchange={onSoundCueStartToggle}
        />
        <span class="toggle-label">
          <span class="toggle-name">Recording-start cue</span>
          <span class="toggle-desc">
            Plays a chime the moment the mic goes hot.
          </span>
        </span>
      </label>
      <button
        type="button"
        class="cue-preview-btn"
        data-testid="settings-cue-preview-start"
        onclick={() => onPreviewCue("start")}
        aria-label="Preview the recording-start cue"
        title="Preview the recording-start cue"
      >▶</button>
    </div>
    <div class="toggle-row toggle-row-sub">
      <label class="toggle-row-inner">
        <input
          type="checkbox"
          data-testid="settings-sound-cue-complete-toggle"
          disabled={!soundCuesEnabled || soundCueCompleteBusy}
          checked={soundCueCompleteEnabled}
          onchange={onSoundCueCompleteToggle}
        />
        <span class="toggle-label">
          <span class="toggle-name">Transcription-complete cue</span>
          <span class="toggle-desc">
            Plays a chime once the transcript is on the clipboard
            — the "safe to paste" signal.
          </span>
        </span>
      </label>
      <button
        type="button"
        class="cue-preview-btn"
        data-testid="settings-cue-preview-done"
        onclick={() => onPreviewCue("done")}
        aria-label="Preview the transcription-complete cue"
        title="Preview the transcription-complete cue"
      >▶</button>
    </div>
  </div>
  {#if soundCueSubError}
    <p class="settings-error">{soundCueSubError}</p>
  {/if}
</section>
