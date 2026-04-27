<!--
  PTT hotkey editor. Lives in the Settings → General → Hotkeys
  group. Reads/writes the backend `ptt_get_config` / `ptt_set_config`
  IPC commands and exposes:

  - An Enabled checkbox that toggles the listener gate.
  - A combo display rendered as `<kbd>` chips, plus a "Click and
    press your combo" capture surface that records held keys until
    they're released, then saves the new combo.
  - A Reset-to-default action.

  Capture happens on the Settings window itself: while in capture
  mode we install document-level keydown / keyup listeners and map
  KeyboardEvent.code values to the backend's PttKey names. The user
  must press at least one curated key (modifiers, F-keys, CapsLock);
  letter / digit / arrow events are ignored to prevent foot-gun
  bindings that would type into focused apps.
-->
<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onDestroy, onMount } from "svelte";
  import type { PttConfig } from "./types";

  type Props = {
    isMacOS: boolean;
  };

  let { isMacOS }: Props = $props();

  let combo = $state<string[]>([]);
  let enabled = $state(false);
  let listenerRunning = $state(false);
  let loaded = $state(false);
  let error = $state<string | null>(null);
  let saving = $state(false);

  // Capture mode: when true, we're listening for the user's next
  // combo. We accumulate held keys in `captured` (set, not array, to
  // dedup) and commit on the first all-released event.
  let capturing = $state(false);
  let captured = $state<Set<string>>(new Set());
  // Snapshot of `captured` at the moment of commit, used by the
  // "Save" button and for the "Press X to clear" affordance.
  let captureBuffer = $state<string[]>([]);

  // Reference `isMacOS` lazily — Svelte 5 props are reactive, and a
  // top-level `const` capture would freeze the initial value. The
  // function-form is also a tiny one-liner so this is just stylistic.
  const platformDefault = () => (isMacOS ? "RightMeta" : "RightControl");

  // Map `KeyboardEvent.code` → backend PttKey enum name. Returns
  // null for keys outside the curated PTT set so the capture
  // ignores them instead of binding (e.g.) Letter A to PTT.
  function ptKeyForCode(code: string): string | null {
    switch (code) {
      case "ControlRight": return "RightControl";
      case "ControlLeft": return "LeftControl";
      case "AltRight": return "RightAlt";
      case "AltLeft": return "LeftAlt";
      case "ShiftRight": return "RightShift";
      case "ShiftLeft": return "LeftShift";
      case "MetaRight": return "RightMeta";
      case "MetaLeft": return "LeftMeta";
      case "F1": case "F2": case "F3": case "F4": case "F5": case "F6":
      case "F7": case "F8": case "F9": case "F10": case "F11": case "F12":
        return code;
      case "CapsLock": return "CapsLock";
      default: return null;
    }
  }

  function pretty(name: string): string {
    if (!isMacOS) return name.replace(/^Right/, "Right ").replace(/^Left/, "Left ");
    // macOS prefers symbol-style modifier rendering.
    switch (name) {
      case "RightMeta": return "Right ⌘";
      case "LeftMeta": return "Left ⌘";
      case "RightAlt": return "Right ⌥";
      case "LeftAlt": return "Left ⌥";
      case "RightShift": return "Right ⇧";
      case "LeftShift": return "Left ⇧";
      case "RightControl": return "Right ⌃";
      case "LeftControl": return "Left ⌃";
      case "CapsLock": return "Caps Lock";
      default: return name;
    }
  }

  async function load() {
    try {
      const cfg = await invoke<PttConfig>("ptt_get_config");
      combo = cfg.combo;
      enabled = cfg.enabled;
      listenerRunning = cfg.listenerRunning;
      error = null;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loaded = true;
    }
  }

  async function persist(nextCombo: string[], nextEnabled: boolean) {
    saving = true;
    error = null;
    try {
      await invoke("ptt_set_config", {
        combo: nextCombo,
        enabled: nextEnabled,
      });
      combo = nextCombo;
      enabled = nextEnabled;
      // Re-read after persist so `listenerRunning` reflects the
      // outcome of the on-demand spawn. On macOS, the OS prompt
      // for Input Monitoring may still be visible; the listener
      // will start delivering events the moment it's granted, but
      // listenerRunning flips to true now (the thread is up; the
      // permission grant just gates whether events flow).
      await load();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
      await load();
    } finally {
      saving = false;
    }
  }

  function onEnabledChange(e: Event) {
    const next = (e.target as HTMLInputElement).checked;
    void persist(combo, next);
  }

  // ---- Capture mode --------------------------------------------------

  function startCapture() {
    captured = new Set();
    captureBuffer = [];
    capturing = true;
  }

  function cancelCapture() {
    capturing = false;
    captured = new Set();
    captureBuffer = [];
  }

  async function commitCapture() {
    const next = Array.from(captured);
    if (next.length === 0) return;
    capturing = false;
    captured = new Set();
    captureBuffer = [];
    await persist(next, enabled);
  }

  function onCaptureKeyDown(e: KeyboardEvent) {
    if (!capturing) return;
    // Escape always cancels — gives the user a way out if they
    // accidentally entered capture mode.
    if (e.key === "Escape") {
      e.preventDefault();
      cancelCapture();
      return;
    }
    // Enter commits the current buffer as the new combo (so the
    // user can release before saving).
    if (e.key === "Enter" && captureBuffer.length > 0) {
      e.preventDefault();
      void commitCapture();
      return;
    }
    const ptt = ptKeyForCode(e.code);
    if (ptt === null) {
      // Letter / digit / arrow / etc — ignore. Don't preventDefault;
      // we don't want to swallow normal typing (the user might be
      // about to Tab away to cancel).
      return;
    }
    e.preventDefault();
    if (!captured.has(ptt)) {
      captured = new Set([...captured, ptt]);
    }
    // Mirror into a stable array so the live preview is sorted.
    captureBuffer = Array.from(captured).sort();
  }

  function onCaptureKeyUp(_e: KeyboardEvent) {
    if (!capturing) return;
    // Auto-commit when ALL captured keys are released. This is the
    // natural "I'm done holding the combo" signal. We track
    // released state via the OS keyup, not by checking `captured`
    // (which we never decrement during capture — held keys we want
    // to remember).
    // Use a small timeout so multi-key chords with a tiny stagger
    // don't auto-commit prematurely.
    setTimeout(() => {
      if (!capturing) return;
      const stillHeld = anyCapturedKeyHeld();
      if (!stillHeld && captureBuffer.length > 0) {
        void commitCapture();
      }
    }, 80);
  }

  // Lightweight tracking of which keys are *physically* held during
  // capture (separate from `captured`, which accumulates the combo
  // we'll save). Filled by keydown, drained by keyup.
  let physicallyHeld = $state<Set<string>>(new Set());
  function anyCapturedKeyHeld(): boolean {
    for (const k of captureBuffer) {
      if (physicallyHeld.has(k)) return true;
    }
    return false;
  }
  // Update physicallyHeld in the same listeners.
  function trackPhysicalDown(e: KeyboardEvent) {
    const ptt = ptKeyForCode(e.code);
    if (ptt && !physicallyHeld.has(ptt)) {
      physicallyHeld = new Set([...physicallyHeld, ptt]);
    }
  }
  function trackPhysicalUp(e: KeyboardEvent) {
    const ptt = ptKeyForCode(e.code);
    if (ptt && physicallyHeld.has(ptt)) {
      const next = new Set(physicallyHeld);
      next.delete(ptt);
      physicallyHeld = next;
    }
  }

  function combinedDown(e: KeyboardEvent) {
    trackPhysicalDown(e);
    onCaptureKeyDown(e);
  }
  function combinedUp(e: KeyboardEvent) {
    trackPhysicalUp(e);
    onCaptureKeyUp(e);
  }

  $effect(() => {
    if (capturing) {
      window.addEventListener("keydown", combinedDown);
      window.addEventListener("keyup", combinedUp);
      return () => {
        window.removeEventListener("keydown", combinedDown);
        window.removeEventListener("keyup", combinedUp);
      };
    }
    return () => {};
  });

  async function resetToDefault() {
    await persist([platformDefault()], enabled);
  }

  onMount(() => {
    void load();
  });

  onDestroy(() => {
    window.removeEventListener("keydown", combinedDown);
    window.removeEventListener("keyup", combinedUp);
  });
</script>

<div class="ptt-editor">
  {#if !loaded}
    <p class="muted">Loading PTT configuration…</p>
  {:else}
    <label class="toggle-row" data-testid="ptt-enabled-toggle">
      <input
        type="checkbox"
        checked={enabled}
        disabled={saving}
        onchange={onEnabledChange}
      />
      <span class="toggle-label">
        <span class="toggle-name">Enable push-to-talk</span>
        <span class="toggle-desc">
          {#if isMacOS}
            macOS will prompt for Input Monitoring the first time
            this is on. The toggle hotkey works without it.
          {:else}
            Hold the combo below to record without focusing Hush.
          {/if}
        </span>
      </span>
    </label>

    {#if enabled && !listenerRunning}
      <p class="settings-hint warn">
        Couldn't start the keyboard listener. Try toggling off and
        back on; if that doesn't help, restart Hush.
      </p>
    {/if}

    <div class="combo-row">
      <span class="row-label">Combo</span>
      <span class="combo-display" data-testid="ptt-combo-display">
        {#if capturing}
          {#if captureBuffer.length === 0}
            <em class="muted">Press your combo…</em>
          {:else}
            {#each captureBuffer as key (key)}
              <kbd>{pretty(key)}</kbd>
            {/each}
          {/if}
        {:else}
          {#each combo as key (key)}
            <kbd>{pretty(key)}</kbd>
          {/each}
        {/if}
      </span>
    </div>

    <div class="combo-actions">
      {#if capturing}
        <button
          type="button"
          class="ghost"
          disabled={captureBuffer.length === 0}
          onclick={() => void commitCapture()}
        >
          Save combo
        </button>
        <button type="button" class="ghost" onclick={cancelCapture}>
          Cancel
        </button>
      {:else}
        <button
          type="button"
          class="ghost"
          disabled={saving}
          data-testid="ptt-record-button"
          onclick={startCapture}
        >
          Record new combo…
        </button>
        <button
          type="button"
          class="ghost ghost-subtle"
          disabled={saving}
          onclick={() => void resetToDefault()}
        >
          Reset to default
        </button>
      {/if}
    </div>

    {#if capturing}
      <p class="settings-hint">
        Hold the keys you want to use, then release them. Esc cancels.
        Letters / digits / arrows are ignored — combos must be made
        of modifiers, function keys, or Caps Lock.
      </p>
    {/if}

    {#if error}
      <p class="settings-error">{error}</p>
    {/if}
  {/if}
</div>

<style>
  .ptt-editor {
    display: flex;
    flex-direction: column;
    gap: 0.65rem;
    max-width: 44rem;
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

  .combo-row {
    display: flex;
    align-items: center;
    gap: 1rem;
    padding: 0.55rem 0.85rem;
    background-color: white;
    border: 1px solid #e1e1e6;
    border-radius: 8px;
  }
  .row-label {
    font-weight: 500;
    color: #333;
    min-width: 4rem;
  }
  .combo-display {
    display: inline-flex;
    flex-wrap: wrap;
    gap: 0.35rem;
  }

  .combo-actions {
    display: flex;
    gap: 0.5rem;
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
  button.ghost-subtle {
    color: #666;
  }

  .settings-hint {
    margin: 0;
    font-size: 0.8rem;
    color: #666;
    line-height: 1.4;
  }
  .settings-hint.warn {
    color: #8a5a00;
    background-color: #fff7e6;
    border: 1px solid #ffd591;
    border-radius: 6px;
    padding: 0.55rem 0.75rem;
  }
  .settings-error {
    margin: 0.4rem 0 0;
    color: #8a1f1f;
    font-size: 0.85rem;
  }
  .muted {
    color: #888;
    font-size: 0.85rem;
  }

  kbd {
    display: inline-block;
    padding: 0.05em 0.45em;
    border: 1px solid #d1d1d8;
    border-radius: 4px;
    background-color: #fafafa;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, monospace;
    font-size: 0.85em;
  }

  @media (prefers-color-scheme: dark) {
    .toggle-row,
    .combo-row {
      background-color: #2a2a2d;
      border-color: #38383b;
    }
    .toggle-name { color: #e8e8e8; }
    .toggle-desc { color: #a8a8a8; }
    .row-label { color: #d8d8d8; }
    .settings-hint { color: #a8a8a8; }
    .settings-hint.warn {
      color: #ffd591;
      background-color: #3a2c00;
      border-color: #6b5300;
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
    button.ghost-subtle { color: #888; }
    kbd {
      background-color: #2a2a2d;
      border-color: #4a4a4d;
      color: #d8d8d8;
    }
  }
</style>
