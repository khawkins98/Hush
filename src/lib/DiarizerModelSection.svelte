<!--
  Settings → Meeting tab — Speakers / diarizer-model section (#693).
  Extracted from MeetingTab.svelte to give diarization toggle +
  wespeaker model lifecycle (download / cancel / remove) a single
  owner. Registers the three model-download event listeners on mount
  (filtered to the "wespeaker-resnet34-lm" id) and tears them down
  on unmount — matching the pre-#693 behaviour where they lived for
  the lifetime of the Meeting tab being visible.

  The diarization-enabled read happens here too. The set-profile
  path (remove_diarizer_model) flips diarization_enabled to false
  on the backend; we mirror that locally so the toggle stays in
  sync without a follow-up IPC round-trip.
-->
<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { onDestroy, onMount } from "svelte";

  import { openExternal } from "./openExternal";
  import { Events } from "./events";
  import { formatErrorMessage } from "./errors";
  import "./settings-tab.css";
  import type { DiarizerModelStatus } from "./types";

  // Diarization toggle (#111).
  let diarizationEnabled = $state(false);
  let diarizationBusy = $state(false);
  let diarizationError = $state<string | null>(null);

  // Diarizer model status (#301). When the wespeaker .onnx is
  // missing, the toggle is informational only — runtime falls back
  // to source-only labels until the download lands.
  let diarizerModelStatus = $state<DiarizerModelStatus | null>(null);
  let diarizerDownloadBusy = $state(false);
  let diarizerDownloadProgress = $state<{ received: number; total: number | null } | null>(null);
  let diarizerDownloadError = $state<string | null>(null);
  let unlistenDiarizerProgress: (() => void) | null = null;
  let unlistenDiarizerDone: (() => void) | null = null;
  let unlistenDiarizerFailed: (() => void) | null = null;

  // Remove-model affordance (#351). Two-state click-to-confirm.
  let diarizerRemoveConfirming = $state(false);
  let diarizerRemoveBusy = $state(false);
  let diarizerRemoveError = $state<string | null>(null);

  type DownloadProgressEvent = {
    id: string;
    bytesReceived: number;
    bytesTotal: number | null;
  };

  async function loadDiarizationEnabled(): Promise<void> {
    // Refresh-only path: re-read the persisted value, but don't
    // touch `diarizationError` if it's already non-null. The
    // setter-failure path needs the error to survive the post-
    // failure refresh; clobbering it on a successful read hid
    // the error from users (caught by #302 e2e).
    try {
      diarizationEnabled = await invoke<boolean>("get_diarization_enabled");
    } catch (e) {
      diarizationError = "Couldn't read diarization setting.";
      console.warn("[hush] get_diarization_enabled failed", e);
    }
  }

  async function onDiarizationToggle(e: Event) {
    const checked = (e.target as HTMLInputElement).checked;
    diarizationBusy = true;
    diarizationError = null;
    try {
      await invoke("set_diarization_enabled", { enabled: checked });
      diarizationEnabled = checked;
    } catch (err) {
      diarizationError = formatErrorMessage(err);
      await loadDiarizationEnabled();
    } finally {
      diarizationBusy = false;
    }
  }

  async function loadDiarizerModelStatus(): Promise<void> {
    try {
      diarizerModelStatus = await invoke<DiarizerModelStatus>(
        "get_diarizer_model_status",
      );
    } catch (e) {
      console.warn("[hush] get_diarizer_model_status failed", e);
      diarizerModelStatus = null;
    }
  }

  async function onDiarizerDownload() {
    if (diarizerDownloadBusy) return;
    diarizerDownloadBusy = true;
    diarizerDownloadProgress = null;
    diarizerDownloadError = null;
    try {
      await invoke("download_diarizer_model");
      // Completion lands via `model:download-done` listener.
    } catch (err) {
      diarizerDownloadBusy = false;
      diarizerDownloadError = formatErrorMessage(err);
    }
  }

  async function onDiarizerCancel() {
    // Reuses model_cancel_download (Whisper picker shares the
    // downloads slot via id keying).
    try {
      await invoke("model_cancel_download", { id: "wespeaker-resnet34-lm" });
    } catch (err) {
      console.warn("[hush] model_cancel_download failed", err);
    }
  }

  async function onDiarizerRemoveConfirm() {
    if (diarizerRemoveBusy) return;
    diarizerRemoveBusy = true;
    diarizerRemoveError = null;
    try {
      await invoke("remove_diarizer_model");
      // Backend flips diarization_enabled to false; mirror locally.
      diarizationEnabled = false;
      await loadDiarizerModelStatus();
      diarizerRemoveConfirming = false;
    } catch (err) {
      diarizerRemoveError = formatErrorMessage(err);
    } finally {
      diarizerRemoveBusy = false;
    }
  }

  onMount(async () => {
    void Promise.all([loadDiarizationEnabled(), loadDiarizerModelStatus()]);

    // Diarizer download lifecycle listeners (#301). Backend reuses
    // the existing `model:` events the Whisper download path
    // emits, but we filter by id so the diarizer download doesn't
    // get confused with a Whisper download in flight.
    const isDiarizerEvent = (id: string) => id === "wespeaker-resnet34-lm";
    unlistenDiarizerProgress = await listen<DownloadProgressEvent>(
      Events.ModelDownloadProgress,
      (event) => {
        if (!isDiarizerEvent(event.payload.id)) return;
        diarizerDownloadProgress = {
          received: event.payload.bytesReceived,
          total: event.payload.bytesTotal,
        };
      },
    );
    unlistenDiarizerDone = await listen<{ id: string }>(
      Events.ModelDownloadDone,
      async (event) => {
        if (!isDiarizerEvent(event.payload.id)) return;
        diarizerDownloadBusy = false;
        diarizerDownloadProgress = null;
        diarizerDownloadError = null;
        await loadDiarizerModelStatus();
      },
    );
    unlistenDiarizerFailed = await listen<{ id: string; message: string | null }>(
      Events.ModelDownloadFailed,
      async (event) => {
        if (!isDiarizerEvent(event.payload.id)) return;
        diarizerDownloadBusy = false;
        diarizerDownloadProgress = null;
        diarizerDownloadError = event.payload.message ?? "Download failed.";
        await loadDiarizerModelStatus();
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

  {#if diarizerModelStatus && !diarizerModelStatus.downloaded}
    <div class="diarizer-model-status" data-testid="diarizer-model-not-installed">
      <p class="settings-row-name">Speaker model not installed</p>
      <p class="settings-row-desc">
        Per-speaker labels need a {diarizerModelStatus.sizeMb} MB ONNX
        model. Hush downloads it once and verifies the
        SHA-256; the toggle below has no effect until this
        completes.
      </p>
      <div class="diarizer-download-row">
        <button
          type="button"
          class="ghost diarizer-download-button"
          data-testid="diarizer-download-button"
          disabled={diarizerDownloadBusy}
          onclick={onDiarizerDownload}
        >
          {#if diarizerDownloadBusy}
            {#if diarizerDownloadProgress?.total}
              Downloading… {Math.round(
                (100 * diarizerDownloadProgress.received) /
                  diarizerDownloadProgress.total,
              )}%
            {:else}
              Downloading…
            {/if}
          {:else}
            Download speaker model ({diarizerModelStatus.sizeMb} MB)
          {/if}
        </button>
        {#if diarizerDownloadBusy}
          <button
            type="button"
            class="ghost danger"
            data-testid="diarizer-cancel-button"
            onclick={onDiarizerCancel}
          >
            Cancel
          </button>
        {/if}
      </div>
      {#if diarizerDownloadError}
        <p class="settings-error" data-testid="diarizer-download-error">
          {diarizerDownloadError}
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
          Drop <code>{diarizerModelStatus.expectedPath}</code> with
          SHA-256 <code>{diarizerModelStatus.sha256}</code>. Restart
          Hush to load it.
        </p>
      </details>
    </div>
  {:else if diarizerModelStatus?.downloaded}
    <!--
      Installed-model details (#351). Replaces the old
      single-line "Speaker model installed." with the
      catalog metadata + a one-line description of how the
      labelling works + a Remove affordance.
    -->
    <div class="diarizer-model-status" data-testid="diarizer-model-ready">
      <p class="settings-row-name">
        {diarizerModelStatus.displayName} — installed
      </p>
      <details class="diarizer-installed-details">
        <summary>Model details</summary>
        <dl class="diarizer-details">
          <dt>Size</dt>
          <dd>{diarizerModelStatus.sizeMb} MB</dd>
          <dt>Path</dt>
          <dd><code class="path-code">{diarizerModelStatus.expectedPath}</code></dd>
          <dt>SHA-256</dt>
          <dd><code class="path-code">{diarizerModelStatus.sha256}</code></dd>
          <dt>Source</dt>
          <dd>
            <button
              type="button"
              class="link-like"
              onclick={() =>
                diarizerModelStatus &&
                openExternal(diarizerModelStatus.sourceUrl)}
              data-testid="diarizer-source-link"
            >
              {diarizerModelStatus.sourceUrl}
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
        {#if diarizerRemoveConfirming}
          <span class="settings-row-desc">
            Delete the speaker model? You can re-download anytime.
          </span>
          <button
            type="button"
            class="ghost danger"
            data-testid="diarizer-remove-confirm"
            disabled={diarizerRemoveBusy}
            onclick={onDiarizerRemoveConfirm}
          >
            {diarizerRemoveBusy ? "Removing…" : "Yes, remove"}
          </button>
          <button
            type="button"
            class="ghost"
            data-testid="diarizer-remove-cancel"
            disabled={diarizerRemoveBusy}
            onclick={() => (diarizerRemoveConfirming = false)}
          >
            Cancel
          </button>
        {:else}
          <button
            type="button"
            class="ghost danger"
            data-testid="diarizer-remove-button"
            onclick={() => (diarizerRemoveConfirming = true)}
          >
            Remove model
          </button>
        {/if}
      </div>
      {#if diarizerRemoveError}
        <p class="settings-error">{diarizerRemoveError}</p>
      {/if}
    </div>
  {/if}

  <label class="toggle-row">
    <input
      type="checkbox"
      data-testid="settings-diarization-toggle"
      disabled={diarizationBusy ||
        (diarizerModelStatus !== null && !diarizerModelStatus.downloaded)}
      checked={diarizationEnabled}
      onchange={onDiarizationToggle}
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
  {#if diarizationError}
    <p class="settings-error">{diarizationError}</p>
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
    color: #2c3e8f;
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

  @media (prefers-color-scheme: dark) {
    :root:not([data-theme="light"]) .diarizer-model-status {
      background-color: #2a2a2d;
      border-color: #38383b;
    }
    :root:not([data-theme="light"]) .diarizer-manual-install > summary {
      color: #aaa;
    }
    :root:not([data-theme="light"]) .diarizer-manual-install > summary:hover {
      color: #d8d8d8;
    }
    :root:not([data-theme="light"]) .path-code {
      background-color: rgba(255, 255, 255, 0.08);
    }
  }
  :root[data-theme="dark"] .diarizer-model-status {
    background-color: #2a2a2d;
    border-color: #38383b;
  }
  :root[data-theme="dark"] .diarizer-manual-install > summary {
    color: #aaa;
  }
  :root[data-theme="dark"] .diarizer-manual-install > summary:hover {
    color: #d8d8d8;
  }
  :root[data-theme="dark"] .path-code {
    background-color: rgba(255, 255, 255, 0.08);
  }
</style>
