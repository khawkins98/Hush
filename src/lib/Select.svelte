<!--
  Cross-platform styled dropdown replacing the native <select>.
  Renders a button trigger + absolute-positioned listbox panel so
  the closed and open states both use the app's own design tokens
  rather than the OS native widget.

  Supports grouped options (equivalent to <optgroup>) via the
  `groups` prop. Each group has a label and an array of options;
  options can be disabled. The component is fully keyboard-accessible:
  Arrow keys navigate, Enter/Space select, Escape closes.

  The native <select> in ControlsSection used to be targeted by
  `section.controls select` in audio-source-picker.spec.ts — those
  tests now use `[data-testid="source-picker-trigger"]` and ARIA
  role/attribute selectors. See tests/e2e/audio-source-picker.spec.ts.
-->
<script lang="ts">
  import { onMount, onDestroy } from "svelte";

  export type SelectOption = {
    value: string;
    label: string;
    disabled?: boolean;
  };

  export type SelectGroup = {
    label: string;
    options: SelectOption[];
  };

  type Props = {
    groups: SelectGroup[];
    value: string | null;
    onchange: (value: string) => void;
    disabled?: boolean;
    id?: string;
  };

  let { groups, value, onchange, disabled = false, id }: Props = $props();

  let open = $state(false);
  // Value highlighted by keyboard nav. Null when closed or before
  // any arrow-key press (the currently selected item is styled via
  // `aria-selected`, not `focused`).
  let focusedValue = $state<string | null>(null);
  let rootEl: HTMLDivElement | undefined = $state();
  let listboxEl: HTMLUListElement | undefined = $state();

  // All non-disabled options in document order — used by keyboard nav
  // to move to next / previous item without having to reason about
  // group boundaries.
  let flatEnabled = $derived(
    groups.flatMap((g) => g.options.filter((o) => !o.disabled)),
  );

  // The label to show in the closed trigger button.
  let selectedLabel = $derived(
    groups
      .flatMap((g) => g.options)
      .find((o) => o.value === value)?.label ?? "—",
  );

  function openPicker() {
    if (disabled) return;
    open = true;
    focusedValue = value;
    // On next tick, focus the listbox so keyboard events land there.
    setTimeout(() => listboxEl?.focus(), 0);
  }

  function closePicker() {
    open = false;
    focusedValue = null;
  }

  function select(optValue: string) {
    onchange(optValue);
    closePicker();
  }

  function handleTriggerKeydown(e: KeyboardEvent) {
    if (e.key === "Enter" || e.key === " " || e.key === "ArrowDown") {
      e.preventDefault();
      openPicker();
    }
  }

  function handleListboxKeydown(e: KeyboardEvent) {
    if (e.key === "Escape" || e.key === "Tab") {
      e.preventDefault();
      closePicker();
      // Return focus to the trigger.
      rootEl?.querySelector("button")?.focus();
      return;
    }
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      if (focusedValue !== null) select(focusedValue);
      return;
    }
    const idx = flatEnabled.findIndex((o) => o.value === focusedValue);
    if (e.key === "ArrowDown") {
      e.preventDefault();
      focusedValue = flatEnabled[(idx + 1) % flatEnabled.length]?.value ?? null;
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      const prev = idx <= 0 ? flatEnabled.length - 1 : idx - 1;
      focusedValue = flatEnabled[prev]?.value ?? null;
    }
  }

  // Close when clicking outside the component.
  function handleDocClick(e: MouseEvent) {
    if (open && rootEl && !rootEl.contains(e.target as Node)) {
      closePicker();
    }
  }

  onMount(() => document.addEventListener("click", handleDocClick, true));
  onDestroy(() => document.removeEventListener("click", handleDocClick, true));
</script>

<div class="select-root" bind:this={rootEl} class:open class:disabled>
  <button
    type="button"
    class="select-trigger"
    {id}
    {disabled}
    aria-haspopup="listbox"
    aria-expanded={open}
    data-testid="source-picker-trigger"
    onclick={openPicker}
    onkeydown={handleTriggerKeydown}
  >
    <span class="select-value">{selectedLabel}</span>
    <svg
      class="select-chevron"
      width="12"
      height="12"
      viewBox="0 0 12 12"
      aria-hidden="true"
      fill="none"
    >
      <path
        d="M2 4l4 4 4-4"
        stroke="currentColor"
        stroke-width="1.5"
        stroke-linecap="round"
        stroke-linejoin="round"
      />
    </svg>
  </button>

  {#if open}
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <ul
      class="select-listbox"
      role="listbox"
      tabindex="-1"
      bind:this={listboxEl}
      onkeydown={handleListboxKeydown}
      data-testid="source-picker-listbox"
    >
      {#each groups as group (group.label)}
        <li
          class="select-group"
          role="group"
          aria-label={group.label}
          data-group-label={group.label}
        >
          <span class="select-group-label">{group.label}</span>
          <ul class="select-group-options">
            {#each group.options as opt (opt.value)}
              <!-- svelte-ignore a11y_click_events_have_key_events -->
              <li
                class="select-option"
                role="option"
                aria-selected={value === opt.value}
                aria-disabled={opt.disabled ?? false}
                data-option-value={opt.value}
                data-focused={focusedValue === opt.value || undefined}
                onclick={!opt.disabled ? () => select(opt.value) : undefined}
              >
                {opt.label}
              </li>
            {/each}
          </ul>
        </li>
      {/each}
    </ul>
  {/if}
</div>

<style>
  .select-root {
    position: relative;
    width: 100%;
  }

  /* ── Trigger button ─────────────────────────────────────────── */
  .select-trigger {
    width: 100%;
    height: var(--control-height);
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.5rem;
    padding: 0 var(--control-padding-x);
    background: var(--bg-input);
    border: 1px solid var(--border-input);
    border-radius: var(--radius-md);
    color: var(--text-primary);
    font-family: inherit;
    font-size: 0.95rem;
    font-weight: 400;
    text-align: left;
    cursor: pointer;
    transition:
      border-color 0.12s,
      box-shadow 0.12s;
  }

  .select-trigger:hover:not(:disabled) {
    border-color: var(--accent-hover);
  }

  .select-trigger:focus-visible {
    outline: none;
    border-color: var(--border-focus);
    box-shadow: 0 0 0 3px var(--accent-subtle);
  }

  .select-root.open .select-trigger {
    border-color: var(--border-focus);
    box-shadow: 0 0 0 3px var(--accent-subtle);
  }

  .select-trigger:disabled,
  .select-root.disabled .select-trigger {
    opacity: 0.55;
    cursor: not-allowed;
  }

  .select-value {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .select-chevron {
    flex-shrink: 0;
    color: var(--text-muted);
    transition: transform 0.15s;
  }
  .select-root.open .select-chevron {
    transform: rotate(180deg);
  }

  /* ── Listbox panel ──────────────────────────────────────────── */
  .select-listbox {
    position: absolute;
    top: calc(100% + 4px);
    left: 0;
    right: 0;
    z-index: 100;
    list-style: none;
    margin: 0;
    padding: 0.3rem 0;
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    box-shadow:
      0 4px 12px rgba(0, 0, 0, 0.1),
      0 1px 3px rgba(0, 0, 0, 0.06);
    max-height: 14rem;
    overflow-y: auto;
    outline: none;
  }

  /* ── Groups ─────────────────────────────────────────────────── */
  .select-group {
    list-style: none;
  }

  /* Separate groups visually when more than one is shown. */
  .select-group + .select-group {
    border-top: 1px solid var(--border-subtle);
    margin-top: 0.25rem;
    padding-top: 0.25rem;
  }

  .select-group-label {
    display: block;
    padding: 0.2rem 0.75rem 0.15rem;
    font-size: 0.7rem;
    font-weight: 600;
    letter-spacing: 0.05em;
    text-transform: uppercase;
    color: var(--text-muted);
    pointer-events: none;
    user-select: none;
  }

  .select-group-options {
    list-style: none;
    margin: 0;
    padding: 0;
  }

  /* ── Options ────────────────────────────────────────────────── */
  .select-option {
    padding: 0.45rem 0.75rem;
    font-size: 0.9rem;
    color: var(--text-primary);
    cursor: pointer;
    border-radius: var(--radius-sm);
    margin: 0 0.3rem;
    transition: background-color 0.08s;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .select-option:hover,
  .select-option[data-focused] {
    background-color: var(--accent-subtle);
    color: var(--accent);
  }

  .select-option[aria-selected="true"] {
    font-weight: 600;
    color: var(--accent);
  }

  .select-option[aria-selected="true"]::before {
    content: "✓ ";
    font-size: 0.8em;
  }

  .select-option[aria-disabled="true"] {
    color: var(--text-muted);
    cursor: not-allowed;
    opacity: 0.7;
  }

  .select-option[aria-disabled="true"]:hover {
    background-color: transparent;
    color: var(--text-muted);
  }
</style>
