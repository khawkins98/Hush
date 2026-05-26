<!--
  Modal-ish dialog that asks the user how the bulk "Export
  filtered" should produce its files (#357 phase 3c). Sits above
  the History panel; closes on Cancel / Export / Escape.

  Scope choices for this first cut:
  - Kind: which streams (Auto-from-filter / Dictation / Meetings).
  - Meeting format: TXT / CSV / JSON (the same three the per-row
    popover offers — re-using the type from `types.ts` keeps the
    backend serde shape in lockstep).
  - Bundle: always one file per session for now. The "combined
    file" option from the issue is a follow-up — it interacts with
    the format (combined CSV makes sense; combined JSON is
    awkward; combined TXT works but needs separators).
  - Anonymize speakers: deferred to the same follow-up.
  - "Include metadata": deferred — metadata always on for now.

  The dialog returns the chosen options via `onConfirm`; the
  parent fires the OS folder picker + the IPC. Keeping the picker
  out of the dialog means the dialog doesn't need to talk to
  Tauri — easier to test, easier to embed in a future "preview"
  flow.
-->
<script lang="ts">
  import { onMount } from "svelte";
  import type {
    BundleKind,
    BundleSelection,
    MeetingExportFormat,
  } from "./types";

  type Props = {
    /// Default kind chip, computed from the panel's current filter.
    /// "auto" keeps the chip's scope; explicit kinds override.
    initialKind: BundleKind;
    /// Default meeting format; the per-row popover defaults to
    /// plain text and we match that here.
    initialMeetingFormat?: MeetingExportFormat;
    /// User confirmed — fire the OS picker + the IPC.
    onConfirm: (selection: BundleSelection) => void;
    /// User dismissed — no-op.
    onCancel: () => void;
  };

  let { initialKind, initialMeetingFormat = "text", onConfirm, onCancel }: Props =
    $props();

  // The state cells initialise from the props once. The dialog
  // is mounted fresh each open, so capturing the initial prop is
  // the desired behaviour — Svelte's warning is a heads-up for
  // long-lived components that need to react to prop changes,
  // which doesn't apply here.
  // svelte-ignore state_referenced_locally
  let kind = $state<BundleKind>(initialKind ?? "auto");
  // svelte-ignore state_referenced_locally
  let meetingFormat = $state<MeetingExportFormat>(
    initialMeetingFormat ?? "text",
  );

  // Escape closes the dialog (matches every native modal). Wired
  // on mount, removed on destroy via the returned cleanup.
  onMount(() => {
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") {
        onCancel();
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  function confirm() {
    onConfirm({ kind, meetingFormat });
  }

  // Meeting format only matters when meetings are in scope.
  // Disable the radio group when the user picked "Dictation" to
  // avoid suggesting a knob that has no effect.
  let meetingFormatDisabled = $derived(kind === "dictation");
</script>

<!-- Backdrop catches clicks outside the dialog. Positioned
     fixed so it covers the panel regardless of scroll. The
     dialog's own click handler stops propagation so a click
     inside the body doesn't trip the cancel. -->
<div
  class="dialog-backdrop"
  role="presentation"
  onclick={onCancel}
  onkeydown={(e) => e.key === "Enter" && onCancel()}
  data-testid="export-options-dialog-backdrop"
>
  <div
    class="dialog-body"
    role="dialog"
    aria-modal="true"
    aria-labelledby="export-options-title"
    tabindex="-1"
    onclick={(e) => e.stopPropagation()}
    onkeydown={(e) => e.stopPropagation()}
  >
    <h2 id="export-options-title">Export filtered</h2>
    <p class="dialog-desc">
      Hush writes one file per row into the folder you pick next.
      Use the search + filter chips to narrow what gets exported
      — only rows currently visible in the panel will be written.
    </p>

    <fieldset class="dialog-field">
      <legend>What to export</legend>
      <label class="radio-row">
        <input
          type="radio"
          name="kind"
          value="auto"
          checked={kind === "auto"}
          onchange={() => (kind = "auto")}
          data-testid="export-kind-auto"
        />
        <span>Match the current filter chip</span>
      </label>
      <label class="radio-row">
        <input
          type="radio"
          name="kind"
          value="dictation"
          checked={kind === "dictation"}
          onchange={() => (kind = "dictation")}
          data-testid="export-kind-dictation"
        />
        <span>Dictation only</span>
      </label>
      <label class="radio-row">
        <input
          type="radio"
          name="kind"
          value="meetings"
          checked={kind === "meetings"}
          onchange={() => (kind = "meetings")}
          data-testid="export-kind-meetings"
        />
        <span>Meetings only</span>
      </label>
      <label class="radio-row">
        <input
          type="radio"
          name="kind"
          value="both"
          checked={kind === "both"}
          onchange={() => (kind = "both")}
          data-testid="export-kind-both"
        />
        <span>Both kinds</span>
      </label>
    </fieldset>

    <fieldset class="dialog-field" disabled={meetingFormatDisabled}>
      <legend>Meeting format</legend>
      <label class="radio-row">
        <input
          type="radio"
          name="meeting-format"
          value="text"
          checked={meetingFormat === "text"}
          onchange={() => (meetingFormat = "text")}
          data-testid="export-fmt-text"
        />
        <span>Plain text (.txt)</span>
      </label>
      <label class="radio-row">
        <input
          type="radio"
          name="meeting-format"
          value="csv"
          checked={meetingFormat === "csv"}
          onchange={() => (meetingFormat = "csv")}
          data-testid="export-fmt-csv"
        />
        <span>CSV (.csv)</span>
      </label>
      <label class="radio-row">
        <input
          type="radio"
          name="meeting-format"
          value="json"
          checked={meetingFormat === "json"}
          onchange={() => (meetingFormat = "json")}
          data-testid="export-fmt-json"
        />
        <span>JSON (.json)</span>
      </label>
    </fieldset>

    <div class="dialog-actions">
      <button
        type="button"
        class="ghost"
        onclick={onCancel}
        data-testid="export-cancel"
      >
        Cancel
      </button>
      <button
        type="button"
        class="kh-button"
        onclick={confirm}
        data-testid="export-confirm"
      >
        Choose folder…
      </button>
    </div>
  </div>
</div>

<style>
  .dialog-backdrop {
    position: fixed;
    inset: 0;
    background-color: rgba(0, 0, 0, 0.4);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 100;
  }
  .dialog-body {
    width: min(28rem, 92vw);
    background-color: var(--bg-surface);
    color: var(--text-primary);
    border-radius: 10px;
    padding: 1.4rem 1.5rem 1.2rem;
    box-shadow: 0 12px 40px rgba(0, 0, 0, 0.25);
    display: flex;
    flex-direction: column;
    gap: 1rem;
  }
  h2 {
    margin: 0;
    font-size: 1.15rem;
    color: var(--text-primary);
  }
  .dialog-desc {
    margin: 0;
    font-size: 0.85rem;
    color: var(--text-secondary);
    line-height: 1.45;
  }
  .dialog-field {
    border: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
  }
  .dialog-field legend {
    font-weight: 600;
    font-size: 0.82rem;
    color: var(--text-secondary);
    margin-bottom: 0.25rem;
  }
  .dialog-field[disabled] {
    opacity: 0.55;
  }
  .radio-row {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    font-size: 0.88rem;
    color: var(--text-primary);
    cursor: pointer;
  }
  .dialog-actions {
    display: flex;
    justify-content: flex-end;
    gap: 0.5rem;
    padding-top: 0.5rem;
  }
  button.ghost {
    padding: 0.4em 0.95em;
    font-size: 0.86rem;
    font-weight: 500;
    border-radius: 8px;
    cursor: pointer;
    font-family: inherit;
    transition: background-color 0.12s, border-color 0.12s;
  }
  button.ghost {
    color: var(--text-primary);
    background-color: transparent;
    border: 1px solid var(--border-input);
  }
  button.ghost:hover {
    background-color: var(--bg-app);
  }

</style>
