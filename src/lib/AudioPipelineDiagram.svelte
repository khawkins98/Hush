<!--
  Static audio-pipeline diagram (#427 Item 3).

  Renders a left-to-right view of the active capture path: source
  nodes (mic, optional system audio) → Whisper engine → transcript
  output. Inline SVG so there's no asset to ship and the colours
  pick up the existing CSS tokens (`--accent` / `--text-muted`)
  for free dark-mode parity.

  Two consumers today:
  - `FirstRunModal.svelte` — informational header so a first-time
    user immediately understands what Hush does with their audio
    (and that the chain ends in their clipboard, not a cloud).
  - `AboutTab.svelte` — the same diagram as a "how it works"
    explainer for users who land in Settings later.

  When `systemActive` is true the diagram renders both source
  nodes with a Y-join into the Whisper node; when false it's a
  single mic → engine → transcript chain. Either way the Whisper
  node carries the active model name so the user can see exactly
  which engine is processing.

  No interactivity. The component is `aria-hidden` because the
  same information is in surrounding text — the diagram is a
  visual aid, not the primary content. A screen-reader-only
  label could be added when a consumer specifically wants the
  diagram to carry the explanation, but neither current consumer
  does.
-->
<script lang="ts">
  type Props = {
    /// Whether the mic-source node should render in the active
    /// (accent) palette. Default true — both informational
    /// surfaces show the full pipeline as if it were operating.
    micActive?: boolean;
    /// Whether the system-audio node renders. When false, the
    /// diagram collapses to a single source-arrow-engine-arrow-
    /// transcript chain instead of the Y-join. Default true.
    systemActive?: boolean;
    /// Model name to render inside the Whisper node. Defaults to
    /// `"whisper.cpp"` for surfaces (like AboutTab) where the
    /// active model isn't loaded into the page state.
    modelName?: string;
  };

  let {
    micActive = true,
    systemActive = true,
    modelName = "whisper.cpp",
  }: Props = $props();

  let bothSources = $derived(micActive && systemActive);
</script>

<figure
  class="pipeline-diagram"
  aria-hidden="true"
  data-testid="audio-pipeline-diagram"
>
  <svg
    viewBox="0 0 360 96"
    role="presentation"
    width="100%"
    height="96"
    preserveAspectRatio="xMidYMid meet"
  >
    <!--
      Layout: left column = sources (one or two stacked nodes);
      middle = Whisper engine; right = transcript output. Arrows
      connect on the horizontal midline (y=48). When two sources
      are active, both nodes Y-join into the engine node via two
      diagonal arrows.
    -->

    <defs>
      <marker
        id="pipeline-arrowhead"
        viewBox="0 0 8 8"
        refX="7"
        refY="4"
        markerWidth="6"
        markerHeight="6"
        orient="auto-start-reverse"
      >
        <path d="M0,0 L8,4 L0,8 Z" fill="currentColor" />
      </marker>
    </defs>

    <!-- Microphone source -->
    {#if micActive}
      <g class="node source" transform="translate(0, {bothSources ? 8 : 32})">
        <rect width="92" height="32" rx="6" />
        <text x="46" y="20" text-anchor="middle">Microphone</text>
      </g>
    {/if}

    <!-- System audio source (only when active) -->
    {#if systemActive}
      <g class="node source" transform="translate(0, {bothSources ? 56 : 32})">
        <rect width="92" height="32" rx="6" />
        <text x="46" y="20" text-anchor="middle">System audio</text>
      </g>
    {/if}

    <!--
      Source → engine arrows. With one source: a single horizontal
      line at y=48. With two sources: two diagonals from the source
      midlines (y=24 for top, y=72 for bottom) into the engine's
      left edge (x=140, y=48).
    -->
    {#if bothSources}
      <line class="arrow" x1="92" y1="24" x2="140" y2="48" />
      <line class="arrow" x1="92" y1="72" x2="140" y2="48" />
    {:else}
      <line class="arrow" x1="92" y1="48" x2="140" y2="48" />
    {/if}

    <!-- Whisper engine -->
    <g class="node engine" transform="translate(140, 32)">
      <rect width="100" height="32" rx="6" />
      <text x="50" y="14" text-anchor="middle" class="engine-label">Whisper</text>
      <text x="50" y="26" text-anchor="middle" class="engine-model">{modelName}</text>
    </g>

    <!-- Engine → transcript arrow -->
    <line class="arrow" x1="240" y1="48" x2="268" y2="48" />

    <!-- Transcript output -->
    <g class="node output" transform="translate(268, 32)">
      <rect width="92" height="32" rx="6" />
      <text x="46" y="20" text-anchor="middle">Transcript</text>
    </g>
  </svg>
  <figcaption class="pipeline-caption">
    Audio stays on your device end-to-end.
  </figcaption>
</figure>

<style>
.pipeline-diagram {
  margin: 0;
  padding: 0;
  width: 100%;
  /* Bound the SVG width so very wide containers don't stretch the
     diagram beyond comfortable readability — caption still
     centres under it. */
  max-width: 28rem;
  margin-inline: auto;
}

.pipeline-diagram svg {
  display: block;
  width: 100%;
  height: auto;
  /* Source / engine / output text colour cascades from the SVG
     element so dark-mode and theme-overrides reach inline text
     elements without per-node overrides. */
  color: var(--text-primary, #111);
}

.pipeline-diagram .node rect {
  fill: var(--bg-surface, #ffffff);
  stroke: var(--border, #d8d8de);
  stroke-width: 1;
}

.pipeline-diagram .source rect {
  /* Source nodes get a subtle accent tint so they read as inputs
     rather than blending with the engine. */
  stroke: var(--accent, #6a8cf0);
}

.pipeline-diagram .engine rect {
  fill: var(--accent-subtle, rgba(106, 140, 240, 0.12));
  stroke: var(--accent, #6a8cf0);
  stroke-width: 1.5;
}

.pipeline-diagram .output rect {
  /* Output node uses the brand accent fill as a deliberate "this
     is where it ends up" emphasis — the user's clipboard. */
  fill: var(--accent, #6a8cf0);
  stroke: var(--accent-hover, #396cd8);
}

.pipeline-diagram .node text {
  font-family: inherit;
  font-size: 11px;
  font-weight: 500;
  fill: var(--text-primary, #111);
}

.pipeline-diagram .output text {
  fill: var(--text-on-accent, #ffffff);
}

.pipeline-diagram .engine .engine-label {
  font-weight: 600;
}
.pipeline-diagram .engine .engine-model {
  font-size: 9px;
  font-weight: 400;
  fill: var(--text-secondary, #444);
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
}

.pipeline-diagram .arrow {
  stroke: var(--text-muted, #888);
  stroke-width: 1.5;
  marker-end: url(#pipeline-arrowhead);
  fill: none;
  /* `currentColor` on the arrowhead path picks up this stroke
     colour so light + dark themes inherit without per-theme
     marker variants. */
  color: var(--text-muted, #888);
}

.pipeline-caption {
  margin: 0.5rem 0 0;
  font-size: 0.78rem;
  color: var(--text-muted, #888);
  text-align: center;
  font-style: italic;
}
</style>
