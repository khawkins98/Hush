<!--
  Settings → Meeting tab — Speakers / diarizer-model section (#693).
  Thin markup shell for the diarization toggle + wespeaker model UI.
  IPC state lives in `state/diarizer.svelte.ts`; the component keeps
  only the three model-download event listeners so their lifetime stays
  tied to the visible Settings section.
-->
<script lang="ts">
  import { listen } from "@tauri-apps/api/event";
  import { onDestroy, onMount } from "svelte";

  import { diarizer } from "$lib/state/diarizer.svelte";
  import { openExternal } from "./openExternal";
  import { Events } from "./events";
  import "./settings-tab.css";

  let unlistenDiarizerProgress: (() => void) | null = null;
  let unlistenDiarizerDone: (() => void) | null = null;
  let unlistenDiarizerFailed: (() => void) | null = null;

  type DownloadProgressEvent = {
    id: string;
    bytesReceived: number;
    bytesTotal: number | null;
  };

  onMount(async () => {
    void Promise.all([
      diarizer.loadDiarizationEnabled(),
      diarizer.loadDiarizerModelStatus(),
      diarizer.loadSpeakerIdentityEnabled(),
    ]);

    // Diarizer download lifecycle listeners (#301). Backend reuses
    // the existing `model:` events the Whisper download path emits,
    // but we filter by id so the diarizer download doesn't get
    // confused with a Whisper download in flight.
    const isDiarizerEvent = (id: string) => id === "wespeaker-resnet34-lm";
    unlistenDiarizerProgress = await listen<DownloadProgressEvent>(
      Events.ModelDownloadProgress,
      (event) => {
        if (!isDiarizerEvent(event.payload.id)) return;
        diarizer.diarizerDownloadProgress = {
          received: event.payload.bytesReceived,
          total: event.payload.bytesTotal,
        };
      },
    );
    unlistenDiarizerDone = await listen<{ id: string }>(
      Events.ModelDownloadDone,
      async (event) => {
        if (!isDiarizerEvent(event.payload.id)) return;
        diarizer.diarizerDownloadBusy = false;
        diarizer.diarizerDownloadProgress = null;
        diarizer.diarizerDownloadError = null;
        await diarizer.loadDiarizerModelStatus();
      },
    );
    unlistenDiarizerFailed = await listen<{ id: string; message: string | null }>(
      Events.ModelDownloadFailed,
      async (event) => {
        if (!isDiarizerEvent(event.payload.id)) return;
        diarizer.diarizerDownloadBusy = false;
        diarizer.diarizerDownloadProgress = null;
        diarizer.diarizerDownloadError = event.payload.message ?? "Download failed.";
        await diarizer.loadDiarizerModelStatus();
      },
    );
  });

  onDestroy(() => {
    unlistenDiarizerProgress?.();
    unlistenDiarizerDone?.();
    unlistenDiarizerFailed?.();
  });
</script>

<!--
  Diarization toggle + model status (#111, #301). When the
  wespeaker model is present AND the toggle is on, the
  meeting pump routes utterances through OnnxDiarizer; if
  the model is missing the toggle is informational only
  (FlagGatedDiarizer's inner is NoopDiarizer until the
  download lands), so the download affordance appears
  before the toggle.
-->
<section class="settings-group" aria-labelledby="settings-diarization-heading">
  <h2 id="settings-diarization-heading" class="group-heading">Speakers</h2>

  {#if diarizer.diarizerModelStatus && !diarizer.diarizerModelStatus.downloaded}
    <div class="diarizer-model-status" data-testid="diarizer-model-not-installed">
      <p class="settings-row-name">Speaker model not installed</p>
      <p class="settings-row-desc">
        Per-speaker labels need a {diarizer.diarizerModelStatus.sizeMb} MB ONNX
        model. Hush downloads it once and verifies the
        SHA-256; the toggle below has no effect until this
        completes.
      </p>
      <div class="diarizer-download-row">
        <button
          type="button"
          class="ghost diarizer-download-button"
          data-testid="diarizer-download-button"
          disabled={diarizer.diarizerDownloadBusy}
          onclick={diarizer.onDiarizerDownload}
        >
          {#if diarizer.diarizerDownloadBusy}
            {#if diarizer.diarizerDownloadProgress?.total}
              Downloading… {Math.round(
                (100 * diarizer.diarizerDownloadProgress.received) /
                  diarizer.diarizerDownloadProgress.total,
              )}%
            {:else}
              Downloading…
            {/if}
          {:else}
            Download speaker model ({diarizer.diarizerModelStatus.sizeMb} MB)
          {/if}
        </button>
        {#if diarizer.diarizerDownloadBusy}
          <button
            type="button"
            class="ghost danger"
            data-testid="diarizer-cancel-button"
            onclick={diarizer.onDiarizerCancel}
          >
            Cancel
          </button>
        {/if}
      </div>
      {#if diarizer.diarizerDownloadError}
        <p class="settings-error" data-testid="diarizer-download-error">
          {diarizer.diarizerDownloadError}
        </p>
      {/if}
      <!--
        Manual-drop escape hatch (audit-2). Corp networks that
        block huggingface.co can't use the Download button;
        surface the expected path so the user can drop the
        file there manually.
      -->
      <details class="diarizer-manual-install">
        <summary>Or install manually</summary>
        <p class="settings-row-desc">
          Drop <code>{diarizer.diarizerModelStatus.expectedPath}</code> with
          SHA-256 <code>{diarizer.diarizerModelStatus.sha256}</code>. Restart
          Hush to load it.
        </p>
      </details>
    </div>
  {:else if diarizer.diarizerModelStatus?.downloaded}
    <!--
      Installed-model details (#351). Replaces the old
      single-line "Speaker model installed." with the
      catalog metadata + a one-line description of how the
      labelling works + a Remove affordance.
    -->
    <div class="diarizer-model-status" data-testid="diarizer-model-ready">
      <p class="settings-row-name">
        {diarizer.diarizerModelStatus.displayName} — installed
      </p>
      <details class="diarizer-installed-details">
        <summary>Model details</summary>
        <dl class="diarizer-details">
          <dt>Size</dt>
          <dd>{diarizer.diarizerModelStatus.sizeMb} MB</dd>
          <dt>Path</dt>
          <dd><code class="path-code">{diarizer.diarizerModelStatus.expectedPath}</code></dd>
          <dt>SHA-256</dt>
          <dd><code class="path-code">{diarizer.diarizerModelStatus.sha256}</code></dd>
          <dt>Source</dt>
          <dd>
            <button
              type="button"
              class="link-like"
              onclick={() =>
                diarizer.diarizerModelStatus &&
                openExternal(diarizer.diarizerModelStatus.sourceUrl)}
              data-testid="diarizer-source-link"
            >
              {diarizer.diarizerModelStatus.sourceUrl}
            </button>
          </dd>
        </dl>
        <p class="settings-row-desc diarizer-explainer">
          Each utterance gets a 256-dim speaker embedding;
          embeddings are clustered live (1-NN with threshold)
          so utterances from the same voice get the same
          Speaker N label across the session. Labels reset
          between sessions.
        </p>
      </details>
      <div class="diarizer-installed-actions">
        {#if diarizer.diarizerRemoveConfirming}
          <span class="settings-row-desc">
            Delete the speaker model? You can re-download anytime.
          </span>
          <button
            type="button"
            class="ghost danger"
            data-testid="diarizer-remove-confirm"
            disabled={diarizer.diarizerRemoveBusy}
            onclick={diarizer.onDiarizerRemoveConfirm}
          >
            {diarizer.diarizerRemoveBusy ? "Removing…" : "Yes, remove"}
          </button>
          <button
            type="button"
            class="ghost"
            data-testid="diarizer-remove-cancel"
            disabled={diarizer.diarizerRemoveBusy}
            onclick={() => (diarizer.diarizerRemoveConfirming = false)}
          >
            Cancel
          </button>
        {:else}
          <button
            type="button"
            class="ghost danger"
            data-testid="diarizer-remove-button"
            onclick={() => (diarizer.diarizerRemoveConfirming = true)}
          >
            Remove model
          </button>
        {/if}
      </div>
      {#if diarizer.diarizerRemoveError}
        <p class="settings-error">{diarizer.diarizerRemoveError}</p>
      {/if}
    </div>
  {/if}

  <label class="toggle-row">
    <input
      type="checkbox"
      data-testid="settings-diarization-toggle"
      disabled={diarizer.diarizationBusy ||
        (diarizer.diarizerModelStatus !== null && !diarizer.diarizerModelStatus.downloaded)}
      checked={diarizer.diarizationEnabled}
      onchange={diarizer.onDiarizationToggle}
    />
    <span class="toggle-label">
      <span class="toggle-name">Label speakers in meeting transcripts</span>
      <span class="toggle-desc">
        Groups utterances by who spoke (Speaker 1, Speaker 2, …)
        instead of just tagging mic vs. system audio. Off
        keeps the simpler mic / system labels.
      </span>
    </span>
  </label>
  {#if diarizer.diarizationError}
    <p class="settings-error">{diarizer.diarizationError}</p>
  {/if}

  <!--
    Speaker identity toggle (#667). Only meaningful when diarization
    is on — we need speaker labels before we can build cross-session
    profiles. Disabled (not hidden) when diarization is off so the
    user can see it exists and understand the dependency.
  -->
  <label class="toggle-row speaker-identity-toggle-row">
    <input
      type="checkbox"
      data-testid="settings-speaker-identity-toggle"
      disabled={diarizer.speakerIdentityBusy || !diarizer.diarizationEnabled}
      checked={diarizer.speakerIdentityEnabled}
      onchange={diarizer.onSpeakerIdentityToggle}
    />
    <span class="toggle-label">
      <span class="toggle-name">Remember speakers across meetings</span>
      <span class="toggle-desc">
        Stores voice fingerprints locally to recognise recurring speakers
        across sessions. You can name them and delete the data anytime.
        Requires speaker labelling to be on.
      </span>
    </span>
  </label>
  {#if diarizer.speakerIdentityError}
    <p class="settings-error">{diarizer.speakerIdentityError}</p>
  {/if}
</section>

<style>
  /* Card primitives imported from `settings-tab.css` (#392).
     Only the diarizer-specific styles live here. */

  /* Speakers panel — not-installed / installing card. Matches the
     same card treatment as .toggle-row so the section reads as a
     cohesive block before the toggle appears beneath it. */
  .diarizer-model-status {
    background-color: white;
    border: 1px solid #e1e1e6;
    border-radius: 8px;
    padding: 0.65rem 0.85rem;
    margin-bottom: 0.75rem;
  }

  /* Action row within the not-installed card — aligns the download
     button and optional cancel button side-by-side. */
  .diarizer-download-row {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    margin-top: 0.55rem;
  }

  /* "Or install manually" disclosure — styled to match the
     AdvancedSection toggle so it reads as part of the same
     design language rather than a bare browser <details>. */
  .diarizer-manual-install {
    margin-top: 0.65rem;
  }
  .diarizer-manual-install > summary {
    list-style: none;
    display: inline-flex;
    align-items: center;
    gap: 0.35rem;
    cursor: pointer;
    font-size: 0.78rem;
    font-weight: 600;
    color: #666;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    padding: 0.25rem 0;
    user-select: none;
    border-radius: 4px;
  }
  .diarizer-manual-install > summary::-webkit-details-marker {
    display: none;
  }
  .diarizer-manual-install > summary::before {
    content: "▸";
    font-size: 0.85rem;
    width: 0.9rem;
    text-align: center;
    color: #888;
  }
  .diarizer-manual-install[open] > summary::before {
    content: "▾";
  }
  .diarizer-manual-install > summary:hover {
    color: #333;
  }
  .diarizer-manual-install > .settings-row-desc {
    margin-top: 0.4rem;
    padding-left: 0.1rem;
  }

  /* Diarization toggle — dim the whole row when the model isn't
     installed so it's visually clear the feature is unavailable
     until the download completes. Scoped here (not in
     settings-tab.css) to avoid dimming unrelated disabled toggles
     on other tabs. */
  .toggle-row:has(input[type="checkbox"]:disabled) {
    opacity: 0.5;
    cursor: not-allowed;
  }

  /* Speakers panel — installed-model details (#351). */
  .diarizer-installed-details {
    margin-top: 0.5rem;
  }
  .diarizer-installed-details summary {
    cursor: pointer;
    font-size: 0.85rem;
    color: #1a4a63;
    user-select: none;
  }
  .diarizer-details {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 0.4rem 0.85rem;
    margin: 0.6rem 0 0.4rem;
    font-size: 0.85rem;
  }
  .diarizer-details dt {
    color: var(--text-muted);
    font-weight: 500;
  }
  .diarizer-details dd {
    margin: 0;
    color: var(--text-primary);
    user-select: text;
    word-break: break-all;
  }
  .path-code {
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, monospace;
    font-size: 0.78rem;
    color: var(--text-secondary);
    background-color: rgba(0, 0, 0, 0.05);
    padding: 0.1em 0.3em;
    border-radius: 4px;
  }
  button.link-like {
    background: none;
    border: none;
    padding: 0;
    color: var(--info-text);
    text-decoration: underline;
    cursor: pointer;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, monospace;
    font-size: 0.78rem;
    word-break: break-all;
    text-align: left;
  }
  button.link-like:hover {
    color: var(--accent);
  }
  .diarizer-explainer {
    margin: 0.5rem 0 0;
    line-height: 1.5;
  }
  .diarizer-installed-actions {
    margin-top: 0.65rem;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.5rem;
  }

  /* Speaker identity toggle — visually subordinate to the diarization
     toggle above it: slight left indent + smaller description text to
     read as "depends on the toggle above". Dimmed when diarization is
     off (the disabled state already applies via :has, but adding the
     indent makes the hierarchy legible at a glance). */
  .speaker-identity-toggle-row {
    margin-left: 1.25rem;
    border-left: 2px solid var(--border-subtle, #e1e1e6);
    padding-left: 0.75rem;
    margin-top: 0.1rem;
  }
  .speaker-identity-toggle-row .toggle-desc {
    font-size: 0.8rem;
  }

</style>
