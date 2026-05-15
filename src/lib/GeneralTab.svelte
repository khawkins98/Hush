<!--
  Settings → General tab (#332 phase 1, slice 4 — see also
  PermissionsTab #387, VocabularyTab #389, ReplacementsTab #390).
  Thin coordinator: IPC-backed state lives in state/general-runtime.svelte.ts
  (#709); localStorage-backed state (status-line, debug-console) stays
  here since it uses lib/status-line.ts and lib/debug-console.ts
  directly with no IPC seam.

  `isMacOS` is read from `@tauri-apps/plugin-os` on mount and
  passed to `PttHotkeyEditor` so the modifier-glyph copy ("Right
  ⌘" vs "Right Ctrl") matches the host.
-->
<script lang="ts">
  import { onMount } from "svelte";
  import { platform } from "@tauri-apps/plugin-os";

  import AdvancedSection from "./AdvancedSection.svelte";
  import DictationStatsBar from "./DictationStatsBar.svelte";
  import GeneralInterfaceSection from "./GeneralInterfaceSection.svelte";
  import GeneralPerformanceSection from "./GeneralPerformanceSection.svelte";
  import GeneralStartupSection from "./GeneralStartupSection.svelte";
  import PttHotkeyEditor from "./PttHotkeyEditor.svelte";
  import { generalRuntime as gr } from "./state/general-runtime.svelte";
  import {
    readStatusLineEnabled,
    setStatusLineEnabled,
  } from "./status-line";
  import {
    readDebugConsoleEnabled,
    setDebugConsoleEnabled,
  } from "./debug-console";
  import "./settings-tab.css";
  import { invoke } from "@tauri-apps/api/core";
  import type { ToggleHotkeyStatus } from "./types";

  type Props = {
    /// Callback invoked when the developer console toggle changes.
    /// `SettingsPanel` listens to conditionally show the Debug tab.
    onDebugConsoleChange?: (enabled: boolean) => void;
  };

  let { onDebugConsoleChange }: Props = $props();

  let isMacOS = $state(false);
  let toggleHotkeyError = $state<ToggleHotkeyStatus>(null);

  // F5 technical status line — opt-in display under the main
  // window's waveform that surfaces "🎤 device · model". No IPC;
  // localStorage-backed via `lib/status-line.ts`.
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

  // Developer console toggle (#532). localStorage-backed; no IPC.
  // Calls `onDebugConsoleChange` so SettingsPanel can show/hide
  // the Debug tab without a Tauri event broadcast.
  let debugConsoleEnabled = $state(false);

  function onDebugConsoleToggle(event: Event) {
    const checked = (event.currentTarget as HTMLInputElement).checked;
    setDebugConsoleEnabled(checked);
    debugConsoleEnabled = checked;
    onDebugConsoleChange?.(checked);
  }

  onMount(async () => {
    void gr.load();
    try {
      isMacOS = (await platform()) === "macos";
    } catch (e) {
      console.warn("[hush] platform() failed in GeneralTab", e);
    }
    statusLineEnabled = readStatusLineEnabled();
    debugConsoleEnabled = readDebugConsoleEnabled();
    try {
      toggleHotkeyError = await invoke<ToggleHotkeyStatus>(
        "get_toggle_hotkey_status"
      );
    } catch (e) {
      console.warn("[hush] get_toggle_hotkey_status failed", e);
    }
  });
</script>

<h2 class="tab-title">General</h2>

{#if gr.stats}
  <section class="settings-group" aria-label="Transcription activity">
    <DictationStatsBar stats={gr.stats} />
  </section>
{/if}

<GeneralStartupSection />

<GeneralInterfaceSection />

<section class="settings-group" aria-labelledby="settings-hotkeys-heading">
  <h2 id="settings-hotkeys-heading" class="group-heading">Hotkeys</h2>
  <p class="settings-row">
    <span class="row-label">Toggle recording</span>
    <span class="row-value">
      <span class="chord"><kbd>Ctrl</kbd> + <kbd>⌥/Alt</kbd> + <kbd>H</kbd></span>
      <span class="row-note">Not currently editable — the push-to-talk combo below is.</span>
    </span>
  </p>
  {#if toggleHotkeyError}
    <p class="settings-error" data-testid="toggle-hotkey-error">
      ⚠️ Toggle hotkey could not be registered: {toggleHotkeyError}. Check that
      Hush has Input Monitoring access in System Settings → Privacy &amp; Security.
    </p>
  {/if}
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
  <GeneralPerformanceSection />

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
        disabled={gr.firstRunResetBusy}
        onclick={gr.onResetFirstRun}
      >
        {gr.firstRunResetMessage ?? "Show welcome on next launch"}
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
