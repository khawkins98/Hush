<!--
  ⌘K command palette (#411 phase F3).

  Floating overlay with substring-search over every app action:
  start dictation, switch audio source, change model, jump to a
  Settings tab, scroll to History. (A fuzzy ranker is a future
  iteration once we see what queries users actually type.) Pure-frontend component — every entry is
  an `Action` whose `run` callback is supplied by the parent (the
  main page), so this leaf has no IPC reach of its own.

  ## Triggering

  The parent owns the keyboard binding. This component just
  renders when `open === true` and emits `onClose` on Esc / outside
  click / completed run. Default trigger from the main page is
  `⌘K` (`Cmd+K` on macOS); the binding lives next to the rest of
  the page-level keyboard handlers so a future re-binding doesn't
  fork the listener tree.

  ## Filtering

  v1 uses substring match (case-insensitive) over `label + subtitle
  + group` joined with a space. Lightweight and zero-dep — a real
  fuzzy ranker (e.g. fzf-style sub-string-with-bonuses) is a future
  iteration once we see what queries users actually type. Sort
  order: filtered matches keep the parent-supplied order so groups
  stay coherent.

  ## Keyboard

  - Up / Down: move highlight (wraps).
  - Enter: run the highlighted action and close.
  - Esc: close without running.
  - Tab / Shift+Tab: also move highlight (so muscle-memory from
    other palettes works).

  Mouse: click an action to run; click the backdrop to close.
-->
<script lang="ts">
  import { onDestroy, onMount, tick } from "svelte";
  import { backOut, cubicIn } from "svelte/easing";
  import { fade, fly } from "svelte/transition";
  import { motionDuration } from "./motion";

  export type CommandAction = {
    /// Stable id for keyed list rendering + e2e selectors.
    id: string;
    /// Primary label, e.g. "Start dictation".
    label: string;
    /// Optional secondary text, e.g. "mic only" / "uses Screen
    /// Recording for system audio". Renders muted under the label.
    subtitle?: string;
    /// Group header above the action. Adjacent actions sharing a
    /// group render under a single header; switching group drops a
    /// new header. Optional — actions without a group sit under a
    /// blank slot at the top.
    group?: string;
    /// Optional shortcut hint rendered on the right (e.g. "⌘K").
    /// Display only — the parent owns the actual binding.
    hotkey?: string;
    /// Side-effect callback. May be async; the palette closes once
    /// it resolves (or synchronously, for non-promise returns).
    /// Errors propagate — callers should swallow / surface their
    /// own messaging.
    run: () => void | Promise<void>;
    /// When false, the action renders dimmed and isn't selectable.
    /// Use for state-gated actions ("Stop dictation" while idle).
    enabled?: boolean;
  };

  type Props = {
    open: boolean;
    actions: CommandAction[];
    onClose: () => void;
  };

  let { open, actions, onClose }: Props = $props();

  let query = $state("");
  let highlight = $state(0);
  let inputEl: HTMLInputElement | null = $state(null);
  let listEl: HTMLDivElement | null = $state(null);

  let filtered = $derived.by(() => {
    if (!query.trim()) return actions;
    const q = query.toLowerCase();
    return actions.filter((a) => {
      const haystack = `${a.label} ${a.subtitle ?? ""} ${a.group ?? ""}`.toLowerCase();
      return haystack.includes(q);
    });
  });

  // Selectable subset (enabled-only) so Up/Down/Enter skip dimmed
  // rows. Filtered renders all matches (visual context); selectable
  // governs keyboard navigation.
  let selectable = $derived(
    filtered.map((a, i) => ({ a, i })).filter(({ a }) => a.enabled !== false),
  );

  // Pre-compute the group-header dedup so the template doesn't
  // mutate a Set during iteration. The original implementation
  // worked but read fragile — Svelte's keyed-list patches don't
  // guarantee left-to-right re-evaluation of `{@const}` blocks.
  // Computing the {action, showGroup, selectableIndex} tuples here
  // gives us a pure derived view that the each loop can render
  // straight. `selectableIndex` is -1 for disabled rows, otherwise
  // the index within the `selectable` array used for keyboard nav
  // (avoids a per-row O(n) `findIndex` inside the template).
  let rows = $derived.by(() => {
    const out: {
      action: CommandAction;
      showGroup: boolean;
      selectableIndex: number;
    }[] = [];
    let lastGroup: string | undefined;
    let selectableCount = 0;
    for (const action of filtered) {
      const group = action.group;
      const showGroup = group !== undefined && group !== lastGroup;
      const selectableIndex =
        action.enabled !== false ? selectableCount++ : -1;
      out.push({ action, showGroup, selectableIndex });
      if (group !== undefined) lastGroup = group;
    }
    return out;
  });

  // Reset highlight + query whenever the palette opens fresh so a
  // re-trigger doesn't surface the previous session's stale state.
  $effect(() => {
    if (open) {
      query = "";
      highlight = 0;
      void tick().then(() => {
        inputEl?.focus();
      });
    }
  });

  // Keep highlight inside the selectable range when the filter
  // changes — typing past a match shouldn't leave Enter pointing
  // at empty space.
  $effect(() => {
    if (selectable.length === 0) {
      highlight = 0;
    } else if (highlight >= selectable.length) {
      highlight = selectable.length - 1;
    }
  });

  function handleInputKey(event: KeyboardEvent) {
    if (event.key === "Escape") {
      event.preventDefault();
      onClose();
      return;
    }
    if (event.key === "ArrowDown" || (event.key === "Tab" && !event.shiftKey)) {
      event.preventDefault();
      if (selectable.length === 0) return;
      highlight = (highlight + 1) % selectable.length;
      scrollHighlightIntoView();
      return;
    }
    if (event.key === "ArrowUp" || (event.key === "Tab" && event.shiftKey)) {
      event.preventDefault();
      if (selectable.length === 0) return;
      highlight = (highlight - 1 + selectable.length) % selectable.length;
      scrollHighlightIntoView();
      return;
    }
    if (event.key === "Enter") {
      event.preventDefault();
      void runHighlighted();
    }
  }

  function scrollHighlightIntoView() {
    void tick().then(() => {
      const node = listEl?.querySelector<HTMLElement>(
        ".cmdpal-row.highlighted",
      );
      node?.scrollIntoView({ block: "nearest" });
    });
  }

  // Run an action and close. We close BEFORE awaiting so the
  // palette dismisses immediately on Enter / click without waiting
  // for an async IPC. The trade-off is that a rejection from
  // `run()` happens after the palette is gone — we log instead of
  // surfacing in-UI because every caller in the page already
  // routes its own errors (start/stop write to `error` state,
  // openSettingsTab swallows non-fatal failures). Pre-#post-review
  // the rejections were unhandled because `runHighlighted` was
  // dispatched as `void runHighlighted()`.
  async function safelyRun(run: () => void | Promise<void>) {
    try {
      await Promise.resolve(run());
    } catch (err) {
      console.error("[hush] command palette action failed", err);
    }
  }

  async function runHighlighted() {
    const target = selectable[highlight]?.a;
    if (!target) return;
    onClose();
    await safelyRun(target.run);
  }

  async function runAction(action: CommandAction) {
    if (action.enabled === false) return;
    onClose();
    await safelyRun(action.run);
  }

  function handleBackdrop(event: MouseEvent) {
    // Backdrop click — bail out only when the click landed on the
    // backdrop itself, not on the dialog body bubbling up.
    if (event.target === event.currentTarget) onClose();
  }

  // Lock body scroll while open so the palette doesn't visually
  // sit over a scrolling page. Restored on close / unmount.
  let restoreOverflow: string | null = null;
  $effect(() => {
    if (typeof document === "undefined") return;
    if (open) {
      restoreOverflow = document.body.style.overflow;
      document.body.style.overflow = "hidden";
    } else if (restoreOverflow !== null) {
      document.body.style.overflow = restoreOverflow;
      restoreOverflow = null;
    }
  });

  onDestroy(() => {
    if (typeof document !== "undefined" && restoreOverflow !== null) {
      document.body.style.overflow = restoreOverflow;
      restoreOverflow = null;
    }
  });
</script>

{#if open}
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <div
    class="cmdpal-backdrop"
    role="presentation"
    onclick={handleBackdrop}
    transition:fade={{ duration: motionDuration(150) }}
    data-testid="command-palette-backdrop"
  >
    <div
      class="cmdpal-dialog"
      role="dialog"
      aria-modal="true"
      aria-label="Command palette"
      data-testid="command-palette"
      in:fly={{ y: -8, duration: motionDuration(180), easing: backOut }}
      out:fade={{ duration: motionDuration(120), easing: cubicIn }}
    >
      <input
        bind:this={inputEl}
        bind:value={query}
        type="text"
        class="cmdpal-input"
        placeholder="Type a command…"
        aria-label="Search commands"
        autocomplete="off"
        spellcheck="false"
        data-testid="command-palette-input"
        onkeydown={handleInputKey}
      />
      <div class="cmdpal-list" bind:this={listEl} role="listbox">
        {#if filtered.length === 0}
          <p class="cmdpal-empty" data-testid="command-palette-empty">
            No matching commands.
          </p>
        {:else}
          {#each rows as { action, showGroup, selectableIndex }, i (action.id)}
            {#if showGroup && action.group !== undefined}
              <p class="cmdpal-group">{action.group}</p>
            {/if}
            {@const isHighlighted =
              selectableIndex !== -1 && selectableIndex === highlight}
            <button
              type="button"
              class="cmdpal-row"
              class:highlighted={isHighlighted}
              class:disabled={action.enabled === false}
              role="option"
              aria-selected={isHighlighted}
              aria-disabled={action.enabled === false}
              data-testid="command-palette-row"
              data-action-id={action.id}
              onmouseenter={() => {
                if (selectableIndex !== -1) highlight = selectableIndex;
              }}
              onclick={() => runAction(action)}
            >
              <span class="cmdpal-row-text">
                <span class="cmdpal-row-label">{action.label}</span>
                {#if action.subtitle}
                  <span class="cmdpal-row-subtitle">{action.subtitle}</span>
                {/if}
              </span>
              {#if action.hotkey}
                <span class="cmdpal-row-hotkey" aria-hidden="true">
                  {action.hotkey}
                </span>
              {/if}
            </button>
          {/each}
        {/if}
      </div>
    </div>
  </div>
{/if}

<style>
  .cmdpal-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.4);
    backdrop-filter: blur(2px);
    -webkit-backdrop-filter: blur(2px);
    display: flex;
    justify-content: center;
    align-items: flex-start;
    padding-top: 12vh;
    z-index: 1000;
  }

  .cmdpal-dialog {
    width: min(560px, 90vw);
    max-height: 60vh;
    display: flex;
    flex-direction: column;
    background: var(--bg-elevated, #fff);
    color: var(--text-primary, #111);
    border: 1px solid var(--border, rgba(0, 0, 0, 0.1));
    border-radius: 10px;
    box-shadow:
      0 14px 40px rgba(0, 0, 0, 0.18),
      0 2px 6px rgba(0, 0, 0, 0.08);
    overflow: hidden;
  }

  .cmdpal-input {
    appearance: none;
    width: 100%;
    border: none;
    border-bottom: 1px solid var(--border, rgba(0, 0, 0, 0.08));
    padding: 0.85rem 1rem;
    font-family: inherit;
    font-size: 1rem;
    color: inherit;
    background: transparent;
    outline: none;
  }
  .cmdpal-input::placeholder {
    color: var(--text-muted, #888);
  }

  .cmdpal-list {
    overflow-y: auto;
    padding: 0.4rem 0;
  }

  .cmdpal-empty {
    margin: 0;
    padding: 1rem;
    color: var(--text-muted, #888);
    font-size: 0.9rem;
    text-align: center;
  }

  .cmdpal-group {
    margin: 0.6rem 0 0.2rem;
    padding: 0 1rem;
    font-size: 0.7rem;
    font-weight: 600;
    color: var(--text-muted, #888);
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }
  .cmdpal-group:first-child {
    margin-top: 0.2rem;
  }

  .cmdpal-row {
    appearance: none;
    border: none;
    background: transparent;
    width: 100%;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.6rem;
    padding: 0.55rem 1rem;
    font-family: inherit;
    color: inherit;
    text-align: left;
    cursor: pointer;
  }
  .cmdpal-row.highlighted {
    background: var(--accent-subtle, rgba(124, 111, 247, 0.14));
  }
  .cmdpal-row.disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }
  .cmdpal-row:focus-visible {
    outline: 2px solid var(--accent, #7c6ff7);
    outline-offset: -2px;
  }

  .cmdpal-row-text {
    display: flex;
    flex-direction: column;
    gap: 0.1rem;
    min-width: 0;
  }
  .cmdpal-row-label {
    font-size: 0.92rem;
    font-weight: 500;
    line-height: 1.2;
  }
  .cmdpal-row-subtitle {
    font-size: 0.78rem;
    color: var(--text-muted, #888);
    line-height: 1.2;
  }

  .cmdpal-row-hotkey {
    font-size: 0.75rem;
    color: var(--text-muted, #888);
    font-family: ui-monospace, "SF Mono", Menlo, monospace;
    padding: 0.1rem 0.4rem;
    border: 1px solid var(--border, rgba(0, 0, 0, 0.1));
    border-radius: 4px;
    flex-shrink: 0;
  }

  @media (prefers-reduced-motion: reduce) {
    .cmdpal-backdrop {
      backdrop-filter: none;
      -webkit-backdrop-filter: none;
    }
  }
</style>
