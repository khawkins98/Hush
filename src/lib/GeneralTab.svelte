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
  import { platform } from "@tauri-apps/plugin-os";

  import AdvancedSection from "./AdvancedSection.svelte";
  import DictationStatsBar from "./DictationStatsBar.svelte";
  import GeneralInterfaceSection from "./GeneralInterfaceSection.svelte";
  import GeneralStartupSection from "./GeneralStartupSection.svelte";
  import PttHotkeyEditor from "./PttHotkeyEditor.svelte";
  import { formatErrorMessage } from "./errors";
  import {
    readStatusLineEnabled,
    setStatusLineEnabled,
  } from "./status-line";
  import {
    readDebugConsoleEnabled,
    setDebugConsoleEnabled,
  } from "./debug-console";
  import { readStoredTheme, setTheme, type ThemePref } from "./theme";
  import type { DictationStats } from "./types";
  import "./settings-tab.css";

  type Props = {
    /// Callback invoked when the developer console toggle changes.
    /// `SettingsPanel` listens to conditionally show the Debug tab.
    onDebugConsoleChange?: (enabled: boolean) => void;
  };

  let { onDebugConsoleChange }: Props = $props();

  let firstRunResetBusy = $state(false);
  let firstRunResetMessage = $state<string | null>(null);

  // Two-cell slider (#348): `inferenceThreads` is the persisted
  // value; `inferenceThreadsDisplay` tracks the slider thumb live
  // during drag so the inline label updates without firing one
  // IPC per pixel — the `change` event (release) persists.
  let inferenceThreads = $state(4);
  let inferenceThreadsDisplay = $state(4);
  let inferenceThreadsBusy = $state(false);
  let inferenceThreadsError = $state<string | null>(null);

  // Mic gain slider (#531): same two-cell pattern as inferenceThreads.
  let micGainDb = $state(0);
  let micGainDbDisplay = $state(0);
  let micGainDbBusy = $state(false);
  let micGainDbError = $state<string | null>(null);

  let isMacOS = $state(false);

  // Dictation stats for the at-a-glance summary at the top of the
  // tab. Pre-r3 this rendered above the History panel on the main
  // window, but stats are reflective info that doesn't compete with
  // active-session controls — moved here so the main page reads as
  // dictation-now and the Settings General tab reads as dictation-
  // overall.
  let dictationStats = $state<DictationStats | null>(null);

  async function loadDictationStats() {
    try {
      dictationStats = await invoke<DictationStats>("get_dictation_stats");
    } catch (e) {
      console.warn("[hush] get_dictation_stats failed", e);
    }
  }

  // F5 technical status line — opt-in display under the main
  // window's waveform that surfaces "🎤 device · model". No IPC;
  // localStorage-backed via `lib/status-line.ts`. Sits inside the
  // Advanced section because casual users don't need to think
  // about which model is loaded — they just want it to work.
  let statusLineEnabled = $state(false);
  let statusLineBusy = $state(false);

  async function onStatusLineToggle(event: Event) {
    if (statusLineBusy) return;
    const checked = (event.currentTarget as HTMLInputElement).checked;
    statusLineBusy = true;
    try {
      await setStatusLineEnabled(checked);
      statusLineEnabled = checked;
    } finally {
      statusLineBusy = false;
    }
  }

  // Developer console toggle (#532). Enables the Debug tab in
  // Settings, which shows a live view of the Rust tracing log.
  // localStorage-backed (same pattern as statusLine); no IPC.
  // Calls `onDebugConsoleChange` so SettingsPanel can show/hide
  // the Debug tab without a Tauri event broadcast.
  let debugConsoleEnabled = $state(false);

  function onDebugConsoleToggle(event: Event) {
    const checked = (event.currentTarget as HTMLInputElement).checked;
    setDebugConsoleEnabled(checked);
    debugConsoleEnabled = checked;
    onDebugConsoleChange?.(checked);
  }

  // Appearance / theme override (#411 phase A). Default "system"
  // means follow `prefers-color-scheme`; explicit values force
  // light or dark regardless of OS preference. Persistence is
  // localStorage; the layout listens for a Tauri event to re-
  // apply when the setting changes from another window. Read at
  // mount rather than at script-evaluation time so the picker
  // reflects whatever the layout already applied.
  let themePref = $state<ThemePref>("system");
  let themeBusy = $state(false);

  async function onThemeChange(next: ThemePref) {
    if (themeBusy || next === themePref) return;
    themeBusy = true;
    try {
      await setTheme(next);
      themePref = next;
    } finally {
      themeBusy = false;
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

  async function loadMicGainDb(): Promise<void> {
    try {
      micGainDb = await invoke<number>("get_mic_gain_db");
      micGainDbDisplay = micGainDb;
      micGainDbError = null;
    } catch (e) {
      micGainDbError = "Couldn't read mic gain setting.";
      console.warn("[hush] get_mic_gain_db failed", e);
    }
  }

  function onMicGainDbInput(e: Event) {
    const next = Number((e.target as HTMLInputElement).value);
    if (Number.isFinite(next)) {
      micGainDbDisplay = next;
    }
  }

  async function onMicGainDbChange(e: Event) {
    const next = Number((e.target as HTMLInputElement).value);
    if (!Number.isFinite(next)) {
      return;
    }
    micGainDbBusy = true;
    micGainDbError = null;
    try {
      await invoke("set_mic_gain_db", { gainDb: next });
      micGainDb = next;
      micGainDbDisplay = next;
    } catch (err) {
      micGainDbError = formatErrorMessage(err);
      await loadMicGainDb();
    } finally {
      micGainDbBusy = false;
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
      loadInferenceThreads(),
      loadMicGainDb(),
      loadDictationStats(),
    ]);
    try {
      isMacOS = (await platform()) === "macos";
    } catch (e) {
      console.warn("[hush] platform() failed in GeneralTab", e);
    }
    themePref = readStoredTheme();
    statusLineEnabled = readStatusLineEnabled();
    debugConsoleEnabled = readDebugConsoleEnabled();
  });
</script>

<h2 class="tab-title">General</h2>

{#if dictationStats}
  <section class="settings-group" aria-label="Transcription activity">
    <DictationStatsBar stats={dictationStats} />
  </section>
{/if}

<GeneralStartupSection />

<GeneralInterfaceSection />

<section class="settings-group" aria-labelledby="settings-appearance-heading">
  <h2 id="settings-appearance-heading" class="group-heading">Appearance</h2>
  <div
    class="settings-row settings-row-stack"
    data-testid="settings-theme-row"
  >
    <span class="row-label" id="settings-theme-label">Theme</span>
    <div
      class="segmented"
      role="radiogroup"
      aria-labelledby="settings-theme-label"
    >
      {#each [["system", "System"], ["light", "Light"], ["dark", "Dark"]] as [value, label] (value)}
        <button
          type="button"
          class="segmented-option"
          role="radio"
          aria-checked={themePref === value}
          data-testid={`settings-theme-${value}`}
          disabled={themeBusy}
          onclick={() => onThemeChange(value as ThemePref)}
        >
          {label}
        </button>
      {/each}
    </div>
    <span class="row-note">
      System follows your macOS appearance setting. Light and Dark
      override regardless of the OS preference.
    </span>
  </div>
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

<!--
  Performance + First-run-welcome live inside an Advanced
  disclosure (#427 Item 2) — neither is something a first-time
  user needs to think about. The slider's default (4 threads)
  works for most laptops; the welcome-reset is a diagnostic
  affordance for re-showing the permissions explainer. Power
  users click the toggle and see both.
-->
<AdvancedSection
  label="Advanced"
  testId="settings-general-advanced-toggle"
>
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
    <label class="slider-row">
      <span class="toggle-label">
        <span class="toggle-name">
          Microphone boost:
          <span
            data-testid="settings-mic-gain-db-value"
            aria-live="polite"
          >{micGainDbDisplay === 0 ? "Off (0 dB)" : `+${micGainDbDisplay} dB`}</span>
          {#if micGainDbBusy}
            <span class="row-note" aria-live="polite">Saving…</span>
          {/if}
        </span>
        <span id="settings-mic-gain-db-desc" class="toggle-desc">
          Amplify microphone input before transcription. Useful if
          your voice comes through quietly. 0 = no boost; 6 dB ≈
          double the perceived volume; 20 dB is the maximum safe
          boost. Has no effect on system-audio capture.
        </span>
      </span>
      <input
        type="range"
        min="0"
        max="20"
        step="1"
        data-testid="settings-mic-gain-db-slider"
        aria-label="Microphone boost"
        aria-describedby="settings-mic-gain-db-desc"
        aria-valuetext={micGainDbDisplay === 0 ? "No boost" : `+${micGainDbDisplay} dB`}
        disabled={micGainDbBusy}
        value={micGainDbDisplay}
        oninput={onMicGainDbInput}
        onchange={onMicGainDbChange}
      />
    </label>
    {#if micGainDbError}
      <p class="settings-error">{micGainDbError}</p>
    {/if}
  </section>

  <!--
    F5 technical status line — opt-in display of "🎤 device ·
    model" under the main-window waveform. Useful for power users
    who want to confirm device + model at a glance; off-putting
    for first-time users. No IPC needed; localStorage-backed via
    `lib/status-line.ts` with cross-window sync via Tauri event so
    the toggle takes effect on the open main window without a
    reload.
  -->
  <section class="settings-group" aria-labelledby="settings-statusline-heading">
    <h2 id="settings-statusline-heading" class="group-heading">Display</h2>
    <label class="toggle-row">
      <input
        type="checkbox"
        data-testid="settings-status-line-toggle"
        disabled={statusLineBusy}
        checked={statusLineEnabled}
        onchange={onStatusLineToggle}
      />
      <span class="toggle-label">
        <span class="toggle-name">Show device + model status line</span>
        <span class="toggle-desc">
          Adds a small line under the main-window waveform reading
          "🎤 Built-in Microphone · whisper-medium" so you can
          confirm the active device and Whisper model at a glance.
          Off by default.
        </span>
      </span>
    </label>
    <label class="toggle-row">
      <input
        type="checkbox"
        data-testid="settings-debug-console-toggle"
        checked={debugConsoleEnabled}
        onchange={onDebugConsoleToggle}
      />
      <span class="toggle-label">
        <span class="toggle-name">Developer console</span>
        <span class="toggle-desc">
          Adds a Debug tab with a live view of the Rust backend's
          log stream. Useful for diagnosing issues and generating
          bug reports. Off by default.
        </span>
      </span>
    </label>
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
</AdvancedSection>

<!--
  Card-primitive CSS imported from src/lib/settings-tab.css (#392).
  No tab-specific styles in this component — every class GeneralTab
  uses lives in the shared module.
-->
