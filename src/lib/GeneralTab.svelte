<!--
  Settings → General tab (#332 phase 1, slice 4 — see also
  PermissionsTab #387, VocabularyTab #389, ReplacementsTab #390).
  Owns its own state, IPC, and lifecycle for the largest tab in
  the Settings window: autostart toggle (with stale-LaunchAgent
  warning), HUD-overlay toggle, audio-cues toggle, transcription-
  threads slider, and the first-run-reset button. Hotkey editing
  happens in `PttHotkeyEditor.svelte`, which is rendered inline.

  Lifecycle: every value here loads on mount via its own IPC.
  Pre-extraction the page eagerly loaded all of them on every
  Settings open regardless of which tab was active; now the IPCs
  fire only when General actually mounts. Same data, smaller
  cold-boot when the user opens Settings to a different tab.

  `isMacOS` is read from `@tauri-apps/plugin-os` on mount and
  passed to `PttHotkeyEditor` so the modifier-glyph copy ("Right
  ⌘" vs "Right Ctrl") matches the host. A one-frame
  default-then-correct flicker is imperceptible — same shape the
  page used pre-extraction.
-->
<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";
  import {
    enable as enableAutostart,
    disable as disableAutostart,
    isEnabled as isAutostartEnabled,
  } from "@tauri-apps/plugin-autostart";
  import { platform } from "@tauri-apps/plugin-os";

  import PttHotkeyEditor from "./PttHotkeyEditor.svelte";
  import { formatErrorMessage } from "./errors";

  let autostartEnabled = $state(false);
  let autostartBusy = $state(false);
  let autostartError = $state<string | null>(null);
  // Stale-LaunchAgent recovery (#317): set when boot-time re-
  // register failed; cleared by a successful retry.
  let autostartPathStale = $state(false);
  let autostartRetryBusy = $state(false);
  let autostartRetryFailed = $state(false);

  let firstRunResetBusy = $state(false);
  let firstRunResetMessage = $state<string | null>(null);

  let hudEnabled = $state(true);
  let hudBusy = $state(false);
  let hudError = $state<string | null>(null);

  // Audio cues (#292) — opt-in default off; cues are intrusive
  // in shared spaces / focus modes.
  let soundCuesEnabled = $state(false);
  let soundCuesBusy = $state(false);
  let soundCuesError = $state<string | null>(null);

  // Two-cell slider (#348): `inferenceThreads` is the persisted
  // value; `inferenceThreadsDisplay` tracks the slider thumb live
  // during drag so the inline label updates without firing one
  // IPC per pixel — the `change` event (release) persists.
  let inferenceThreads = $state(4);
  let inferenceThreadsDisplay = $state(4);
  let inferenceThreadsBusy = $state(false);
  let inferenceThreadsError = $state<string | null>(null);

  let isMacOS = $state(false);

  async function loadAutostartState(): Promise<void> {
    try {
      autostartEnabled = await isAutostartEnabled();
      autostartError = null;
    } catch (e) {
      autostartEnabled = false;
      autostartError = "Couldn't read autostart state on this platform.";
      console.warn("[hush] isAutostartEnabled failed", e);
    }
  }

  async function loadAutostartPathStatus(): Promise<void> {
    try {
      const status = await invoke<{ stale: boolean }>(
        "get_autostart_path_status",
      );
      autostartPathStale = status.stale;
    } catch (e) {
      // Non-fatal — the warning just doesn't render.
      console.warn("[hush] get_autostart_path_status failed", e);
      autostartPathStale = false;
    }
  }

  async function onRetryAutostartRegistration() {
    if (autostartRetryBusy) return;
    autostartRetryBusy = true;
    autostartRetryFailed = false;
    try {
      const ok = await invoke<boolean>("retry_autostart_registration");
      if (ok) {
        autostartPathStale = false;
      } else {
        autostartRetryFailed = true;
      }
    } catch (e) {
      autostartRetryFailed = true;
      console.warn("[hush] retry_autostart_registration failed", e);
    } finally {
      autostartRetryBusy = false;
    }
  }

  async function onAutostartToggle(e: Event) {
    const checked = (e.target as HTMLInputElement).checked;
    autostartBusy = true;
    autostartError = null;
    try {
      if (checked) await enableAutostart();
      else await disableAutostart();
      autostartEnabled = checked;
    } catch (err) {
      autostartError = formatErrorMessage(err);
      // Re-read so the checkbox reverts to truth rather than the
      // optimistic state that didn't persist.
      await loadAutostartState();
    } finally {
      autostartBusy = false;
    }
  }

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

  async function loadInferenceThreads(): Promise<void> {
    try {
      inferenceThreads = await invoke<number>("get_inference_threads");
      inferenceThreadsDisplay = inferenceThreads;
      inferenceThreadsError = null;
    } catch (e) {
      inferenceThreadsError = "Couldn't read inference-threads setting.";
      console.warn("[hush] get_inference_threads failed", e);
    }
  }

  /// Live drag handler. Only updates the visible label so the
  /// user sees the slider thumb's position in real time without
  /// firing one IPC per pixel of movement. The `change` event
  /// below fires on release and is what actually persists.
  function onInferenceThreadsInput(e: Event) {
    const next = Number((e.target as HTMLInputElement).value);
    if (Number.isFinite(next)) {
      inferenceThreadsDisplay = next;
    }
  }

  async function onInferenceThreadsChange(e: Event) {
    const next = Number((e.target as HTMLInputElement).value);
    if (!Number.isFinite(next)) {
      return;
    }
    inferenceThreadsBusy = true;
    inferenceThreadsError = null;
    try {
      await invoke("set_inference_threads", { threads: next });
      inferenceThreads = next;
      inferenceThreadsDisplay = next;
    } catch (err) {
      inferenceThreadsError = formatErrorMessage(err);
      // Snap the display back to the persisted value.
      await loadInferenceThreads();
    } finally {
      inferenceThreadsBusy = false;
    }
  }

  async function onResetFirstRun() {
    firstRunResetBusy = true;
    try {
      await invoke("reset_first_run");
      firstRunResetMessage = "Welcome will show on next launch.";
      // Clear after a moment so the button returns to its
      // actionable label in case the user changes their mind in
      // the same session.
      setTimeout(() => {
        firstRunResetMessage = null;
      }, 3000);
    } catch (e) {
      firstRunResetMessage = formatErrorMessage(e);
    } finally {
      firstRunResetBusy = false;
    }
  }

  onMount(async () => {
    // Run loads in parallel — they're independent and small, no
    // ordering concerns. `platform()` is the OS plugin call for
    // the PTT-editor glyph copy; failure is non-fatal (the
    // editor falls back to its own default).
    void Promise.all([
      loadAutostartState(),
      loadAutostartPathStatus(),
      loadHudEnabled(),
      loadSoundCuesEnabled(),
      loadInferenceThreads(),
    ]);
    try {
      isMacOS = (await platform()) === "macos";
    } catch (e) {
      console.warn("[hush] platform() failed in GeneralTab", e);
    }
  });
</script>

<h2 class="tab-title">General</h2>

<section class="settings-group" aria-labelledby="settings-startup-heading">
  <h2 id="settings-startup-heading" class="group-heading">Startup</h2>
  <label class="toggle-row">
    <input
      type="checkbox"
      data-testid="settings-autostart-toggle"
      disabled={autostartBusy}
      checked={autostartEnabled}
      onchange={onAutostartToggle}
    />
    <span class="toggle-label">
      <span class="toggle-name">Launch Hush at login</span>
      <span class="toggle-desc">
        Hush opens automatically when you sign in. The window
        stays in the background — your hotkey still works.
      </span>
    </span>
  </label>
  {#if autostartError}
    <p class="settings-error">{autostartError}</p>
  {/if}

  {#if autostartPathStale}
    <!--
      Stale-LaunchAgent warning (#317). The setup hook re-
      registers the plist on every launch; if that re-register
      failed (read-only home, fs permission), the LaunchAgent
      still points at whatever path it had before. Surface the
      failure with a retry button so the user isn't left with
      a silent broken autostart.
    -->
    <div
      class="settings-warning-row"
      data-testid="autostart-path-stale-warning"
      role="alert"
    >
      <p class="settings-row-name">⚠ Autostart path is out of date</p>
      <p class="settings-row-desc">
        Hush couldn't refresh the LaunchAgent at startup, so
        "Launch at Login" may not work after the next restart.
        Click below to retry — usually a one-click fix.
      </p>
      <button
        type="button"
        class="ghost"
        data-testid="autostart-retry-button"
        disabled={autostartRetryBusy}
        onclick={onRetryAutostartRegistration}
      >
        {autostartRetryBusy ? "Retrying…" : "Click to update"}
      </button>
      {#if autostartRetryFailed}
        <p
          class="settings-error"
          data-testid="autostart-retry-error"
        >
          Retry failed too. Check that <code
            >~/Library/LaunchAgents/</code
          > is writable, then try again.
        </p>
      {/if}
    </div>
  {/if}
</section>

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

  <!--
    Audio cues toggle (#292). Sits in the Interface group
    alongside the HUD toggle since both are sensory-feedback
    settings the user calibrates to their environment. Off by
    default — opt-in deliberately because cues are intrusive
    in shared spaces / meeting rooms / focus modes.
  -->
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
        Plays a short macOS system sound when recording
        starts (Tink) and when transcription completes
        (Glass — "safe to paste"). Honours your system
        volume and Do Not Disturb. Off keeps Hush silent.
      </span>
    </span>
  </label>
  {#if soundCuesError}
    <p class="settings-error">{soundCuesError}</p>
  {/if}
</section>

<section class="settings-group" aria-labelledby="settings-hotkeys-heading">
  <h2 id="settings-hotkeys-heading" class="group-heading">Hotkeys</h2>
  <p class="settings-row">
    <span class="row-label">Toggle recording</span>
    <span class="row-value">
      <span class="chord"><kbd>Ctrl</kbd> + <kbd>⌥/Alt</kbd> + <kbd>H</kbd></span>
      <span class="row-note">Not currently editable — the push-to-talk combo below is.</span>
    </span>
  </p>
  <h3 class="subgroup-heading">Push-to-talk</h3>
  <PttHotkeyEditor {isMacOS} />
</section>

<section class="settings-group" aria-labelledby="settings-performance-heading">
  <h2 id="settings-performance-heading" class="group-heading">Performance</h2>
  <label class="slider-row">
    <span class="toggle-label">
      <span class="toggle-name">
        Transcription threads:
        <span
          data-testid="settings-inference-threads-value"
          aria-live="polite"
        >{inferenceThreadsDisplay}</span>
        {#if inferenceThreadsBusy}
          <span class="row-note" aria-live="polite">Saving…</span>
        {/if}
      </span>
      <span id="settings-inference-threads-desc" class="toggle-desc">
        How many CPU threads whisper.cpp uses per chunk. More
        threads finish each chunk faster on a multi-core CPU but
        compete with other apps for cores. The default (4) suits
        most laptops; bump it up if transcription lags on a
        larger model, drop it if you want Hush to run quietly
        alongside heavy workloads.
      </span>
    </span>
    <input
      type="range"
      min="1"
      max="16"
      step="1"
      data-testid="settings-inference-threads-slider"
      aria-label="Transcription threads"
      aria-describedby="settings-inference-threads-desc"
      aria-valuetext={`${inferenceThreadsDisplay} threads`}
      disabled={inferenceThreadsBusy}
      value={inferenceThreadsDisplay}
      oninput={onInferenceThreadsInput}
      onchange={onInferenceThreadsChange}
    />
  </label>
  {#if inferenceThreadsError}
    <p class="settings-error">{inferenceThreadsError}</p>
  {/if}
</section>

<section class="settings-group" aria-labelledby="settings-firstrun-heading">
  <h2 id="settings-firstrun-heading" class="group-heading">First-run welcome</h2>
  <p class="settings-row settings-row-stack">
    <button
      type="button"
      class="ghost"
      data-testid="settings-reset-first-run"
      disabled={firstRunResetBusy}
      onclick={onResetFirstRun}
    >
      {firstRunResetMessage ?? "Show welcome on next launch"}
    </button>
    <span class="row-note">
      Re-shows the permissions explainer the next time you open
      Hush. Doesn't affect any other state.
    </span>
  </p>
</section>

<style>
  /* Per-tab style block (#332 phase 1). Settings-page CSS is
     scoped at the page level, so child-component markup needs
     its own copies. Shared classes (`.settings-group`,
     `.toggle-row`, etc.) hoist to a CSS module once all tabs
     are extracted — tracked in TODO(#392). */
  .tab-title {
    margin: 0 0 0.75rem;
    font-size: 1.4rem;
    letter-spacing: -0.01em;
  }
  .settings-group {
    margin: 0 0 1.75rem;
    max-width: 44rem;
  }
  .group-heading {
    margin: 0 0 0.6rem;
    font-size: 0.78rem;
    font-weight: 600;
    color: #666;
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }
  .subgroup-heading {
    margin: 1rem 0 0.5rem;
    font-size: 0.85rem;
    font-weight: 600;
    color: #444;
  }
  .toggle-row {
    display: flex;
    align-items: flex-start;
    gap: 0.75rem;
    padding: 0.65rem 0.85rem;
    background-color: white;
    border: 1px solid #e1e1e6;
    border-radius: 8px;
    cursor: pointer;
  }
  .toggle-row input[type="checkbox"] {
    margin-top: 0.2rem;
    flex-shrink: 0;
  }
  .toggle-label {
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
  }
  .toggle-name {
    font-weight: 600;
    color: #222;
  }
  .toggle-desc {
    font-size: 0.82rem;
    color: #666;
    line-height: 1.4;
  }
  .slider-row {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    padding: 0.65rem 0.85rem;
    background-color: white;
    border: 1px solid #e1e1e6;
    border-radius: 8px;
    cursor: pointer;
  }
  .slider-row input[type="range"] {
    width: 100%;
    color-scheme: light dark;
  }
  .settings-row {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: 1rem;
    margin: 0 0 0.5rem;
    padding: 0.55rem 0.85rem;
    background-color: white;
    border: 1px solid #e1e1e6;
    border-radius: 8px;
  }
  .settings-row-stack {
    flex-direction: column;
    align-items: flex-start;
    gap: 0.5rem;
  }
  .row-label {
    font-weight: 500;
    color: #333;
  }
  .row-value {
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 0.2rem;
    color: #555;
  }
  .chord {
    display: inline-flex;
    align-items: center;
    gap: 0.25rem;
    flex-wrap: wrap;
    justify-content: flex-end;
  }
  .row-note {
    display: block;
    font-size: 0.75rem;
    color: #888;
    text-align: right;
  }
  .settings-row-stack .row-note {
    text-align: left;
  }
  .settings-error {
    margin: 0.4rem 0 0;
    color: #8a1f1f;
    font-size: 0.85rem;
  }
  .settings-warning-row {
    margin-top: 0.6rem;
    padding: 0.65rem 0.85rem;
    background-color: #fdf6e3;
    border: 1px solid #e7c887;
    border-radius: 8px;
    color: #7a4e00;
  }
  .settings-row-name {
    margin: 0;
    font-weight: 600;
    color: #5a3700;
  }
  .settings-row-desc {
    margin: 0.25rem 0 0.55rem;
    font-size: 0.85rem;
    color: #6a4500;
    line-height: 1.4;
  }
  button.ghost {
    padding: 0.4em 0.85em;
    font-size: 0.85rem;
    font-weight: 500;
    background-color: white;
    border: 1px solid #d1d1d8;
    border-radius: 6px;
    cursor: pointer;
    color: #2c3e8f;
  }
  button.ghost:hover:not(:disabled) {
    background-color: #f4f5fa;
    border-color: #b8c1d8;
  }
  button.ghost:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }
  kbd {
    display: inline-block;
    padding: 0.05em 0.35em;
    border: 1px solid #d1d1d8;
    border-radius: 4px;
    background-color: #fafafa;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, monospace;
    font-size: 0.85em;
  }
  @media (prefers-color-scheme: dark) {
    .subgroup-heading { color: #d0d0d0; }
    .group-heading { color: #888; }
    .toggle-row,
    .slider-row,
    .settings-row {
      background-color: #2a2a2d;
      border-color: #38383b;
    }
    .toggle-name { color: #e8e8e8; }
    .toggle-desc { color: #a8a8a8; }
    .row-label { color: #d8d8d8; }
    .row-value { color: #b0b0b0; }
    .row-note { color: #888; }
    .settings-warning-row {
      background-color: #3d2f12;
      border-color: #6c4e1a;
      color: #f0c878;
    }
    .settings-row-name {
      color: #ffd790;
    }
    .settings-row-desc {
      color: #d8b46a;
    }
    button.ghost {
      background-color: #2a2a2d;
      border-color: #38383b;
      color: #b8c8ff;
    }
    button.ghost:hover:not(:disabled) {
      background-color: #38383b;
      border-color: #4a4a4d;
    }
    kbd {
      background-color: #2a2a2d;
      border-color: #4a4a4d;
      color: #d8d8d8;
    }
  }
</style>
