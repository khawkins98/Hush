<!--
  PTT hotkey editor. Lives in the Settings → General → Hotkeys
  group. Reads/writes the backend via the `ptt` state module
  (src/lib/state/ptt.svelte.ts, #720) and exposes:

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
  import { onDestroy, onMount } from "svelte";
  import "./settings-tab.css";
  import { ptt } from "./state/ptt.svelte";

  type Props = {
    isMacOS: boolean;
  };

  let { isMacOS }: Props = $props();

  // Capture mode: when true, we're listening for the user's next
  // combo. We accumulate held keys in `captured` (set, not array, to
  // dedup) and commit on the first all-released event.
  let capturing = $state(false);
  let captured = $state<Set<string>>(new Set());
  // Transient cue: flips true for ~1.8 s when the user presses
  // an ignored key (letter, digit, arrow) during capture mode.
  // Without it the capture surface feels broken on unsupported
  // input. The auto-clear timer prevents a stuck hint.
  let ignoredKeyHint = $state(false);
  let ignoredKeyTimer: ReturnType<typeof setTimeout> | undefined;
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

  function onEnabledChange(e: Event) {
    const next = (e.target as HTMLInputElement).checked;
    void ptt.persist(ptt.combo, next);
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
    await ptt.persist(next, ptt.enabled);
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
    const pttKey = ptKeyForCode(e.code);
    if (pttKey === null) {
      // Letter / digit / arrow / etc — ignored, but surface a
      // transient cue so the user knows the press registered and
      // *why* nothing happened. Without this, capture mode
      // feels broken when the user presses an unsupported key.
      // Not intercepting the keystroke (no preventDefault) so the
      // user can still Tab away to cancel if they want.
      ignoredKeyHint = true;
      if (ignoredKeyTimer !== undefined) clearTimeout(ignoredKeyTimer);
      ignoredKeyTimer = setTimeout(() => {
        ignoredKeyHint = false;
      }, 1800);
      return;
    }
    e.preventDefault();
    if (!captured.has(pttKey)) {
      captured = new Set([...captured, pttKey]);
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
    const pttKey = ptKeyForCode(e.code);
    if (pttKey && !physicallyHeld.has(pttKey)) {
      physicallyHeld = new Set([...physicallyHeld, pttKey]);
    }
  }
  function trackPhysicalUp(e: KeyboardEvent) {
    const pttKey = ptKeyForCode(e.code);
    if (pttKey && physicallyHeld.has(pttKey)) {
      const next = new Set(physicallyHeld);
      next.delete(pttKey);
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
    await ptt.persist([platformDefault()], ptt.enabled);
  }

  onMount(() => {
    void ptt.load();
  });

  onDestroy(() => {
    window.removeEventListener("keydown", combinedDown);
    window.removeEventListener("keyup", combinedUp);
  });
</script>

<div class="ptt-editor">
  {#if !ptt.loaded}
    <p class="muted">Loading PTT configuration…</p>
  {:else}
    <label class="toggle-row" data-testid="ptt-enabled-toggle">
      <input
        type="checkbox"
        checked={ptt.enabled}
        disabled={ptt.saving}
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

    {#if ptt.enabled && !ptt.listenerRunning}
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
          {#each ptt.combo as key (key)}
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
          disabled={ptt.saving}
          data-testid="ptt-record-button"
          onclick={startCapture}
        >
          Record new combo…
        </button>
        <button
          type="button"
          class="ghost ghost-subtle"
          disabled={ptt.saving}
          onclick={() => void resetToDefault()}
        >
          Reset to default
        </button>
      {/if}
    </div>

    {#if capturing}
      <p class="settings-hint" class:settings-hint-flash={ignoredKeyHint}>
        {#if ignoredKeyHint}
          That key isn't usable as a PTT trigger. Combos must be
          made of modifiers, function keys, or Caps Lock.
        {:else}
          Hold the keys you want to use, then release them. Esc
          cancels. Letters / digits / arrows are ignored — combos
          must be made of modifiers, function keys, or Caps Lock.
        {/if}
      </p>
    {/if}

    {#if ptt.error}
      <p class="settings-error">{ptt.error}</p>
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

  /* `.toggle-row`, `.toggle-label`, `.toggle-name`, `.toggle-desc`,
     `.row-label`, `button.ghost`, `kbd`,
     `.settings-error` imported from `settings-tab.css` (#392). */

  .combo-row {
    display: flex;
    align-items: center;
    gap: 1rem;
    padding: 0.55rem 0.85rem;
    background-color: white;
    border: 1px solid #e1e1e6;
    border-radius: 8px;
  }
  /* PttHotkeyEditor's `.row-label` adds a min-width to keep the
     "Hotkey:" label aligned with adjacent rows; the base
     properties come from settings-tab.css. */
  .row-label {
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
  /* `.ghost-subtle` is a PttHotkeyEditor-only quieter variant of
     `button.ghost` for the secondary "Reset" affordance. */
  button.ghost-subtle {
    color: var(--text-muted);
  }

  .settings-hint {
    margin: 0;
    font-size: 0.8rem;
    color: var(--text-muted);
    line-height: 1.4;
  }
  .settings-hint.warn {
    color: var(--warning-text);
    background-color: var(--warning-bg);
    border: 1px solid #ffd591;
    border-radius: 6px;
    padding: 0.55rem 0.75rem;
  }
  /* Brief flash when the user presses an ignored key — drops back
     to the neutral hint after the 1.8 s timer in
     `onCaptureKeyDown`. */
  .settings-hint-flash {
    color: var(--danger);
    transition: color 0.18s ease-out;
  }
  .muted {
    color: var(--text-muted);
    font-size: 0.85rem;
  }

</style>
