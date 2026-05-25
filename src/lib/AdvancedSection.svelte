<!--
  Reusable disclosure wrapper for "advanced" controls in Settings
  (#427 Item 2).

  Settings tabs render essential controls inline; sections hidden
  inside `<AdvancedSection>` collapse behind a chevron toggle so a
  first-time user sees one clear path through General / Meeting
  / Vocabulary without an intimidating spread of knobs. Power
  users click the toggle once and see everything.

  Per-session expansion only — `open` is a `$state` rune scoped
  to the component instance. No localStorage, no per-key
  persistence: the audit's read was that settings are
  rarely deep-dived, and a user who toggles every Advanced section
  open on every Settings open is still better served by the toggle
  being there than by the unstructured spread.

  When a deep-link from an error message lands a user in a tab
  with the relevant control inside an Advanced section, the
  parent can pass `open: true` (the default is closed) so the
  recovery surface is already visible.
-->
<script lang="ts">
  import type { Snippet } from "svelte";

  type Props = {
    /// Heading shown next to the chevron. Defaults to `"Advanced"`
    /// to keep the affordance recognisable across tabs; pass a
    /// more specific label (`"Advanced — performance"`) when a
    /// tab has multiple Advanced sections that need
    /// differentiation.
    label?: string;
    /// Initial expansion state. Default `false` (collapsed) —
    /// progressive disclosure means hidden by default. Pass
    /// `true` when navigating in from an error or deep link.
    open?: boolean;
    /// Optional `data-testid` for the disclosure button so e2e
    /// specs can drive expansion without depending on the label
    /// copy.
    testId?: string;
    /// The disclosed content. Rendered only when expanded so a
    /// closed Advanced section doesn't burn DOM nodes / mount
    /// listeners on tab open.
    children?: Snippet;
  };

  let {
    label = "Advanced",
    open: initialOpen = false,
    testId,
    children,
  }: Props = $props();

  // `initialOpen` is captured once to seed the local toggle state;
  // subsequent prop changes don't override the user's interaction.
  // svelte-check warns about local-state-from-prop referencing the
  // captured value — that's the intended pattern here.
  // svelte-ignore state_referenced_locally
  let open = $state(initialOpen);
</script>

<section class="advanced-section">
  <button
    type="button"
    class="advanced-toggle"
    aria-expanded={open}
    data-testid={testId}
    onclick={() => (open = !open)}
  >
    <span class="chevron" aria-hidden="true">{open ? "▾" : "▸"}</span>
    <span class="label">{label}</span>
  </button>
  {#if open}
    <div class="advanced-content">
      {@render children?.()}
    </div>
  {/if}
</section>

<style>
.advanced-section {
  /* Visual separator from the essential controls above; matches
     the `settings-group` margin used for top-level sections. */
  margin: 0 0 1.75rem;
  max-width: 44rem;
}
.advanced-toggle {
  appearance: none;
  display: inline-flex;
  align-items: center;
  gap: 0.4rem;
  padding: 0.35rem 0.55rem;
  margin-left: -0.55rem; /* align chevron with section text */
  background: transparent;
  border: none;
  font-family: inherit;
  font-size: 0.78rem;
  font-weight: 600;
  color: var(--text-muted);
  text-transform: uppercase;
  letter-spacing: 0.06em;
  cursor: pointer;
  border-radius: 4px;
}
.advanced-toggle:hover {
  background-color: rgba(0, 0, 0, 0.04);
  color: var(--text-primary);
}
.advanced-toggle:focus-visible {
  outline: 2px solid var(--accent, #ffb81c);
  outline-offset: 2px;
}
.chevron {
  font-size: 0.85rem;
  width: 0.9rem;
  text-align: center;
  color: var(--text-muted);
}
.advanced-content {
  margin-top: 0.6rem;
  padding-left: 0.1rem;
}

</style>
