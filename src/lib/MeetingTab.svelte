<!--
  Settings → Meeting tab (#332 phase 1, slice 5 — see also
  PermissionsTab #387, VocabularyTab #389, ReplacementsTab #390,
  GeneralTab #391). Owns its own state, IPC, and lifecycle for
  the second-largest tab in Settings: meeting auto-start mode,
  speaker-diarization toggle, the wespeaker model installer
  (download / cancel / remove), and the app-classifier overrides
  panel.

  Lifecycle: every IPC + the three diarizer-download event
  listeners (`model:download-progress`/`-done`/`-failed`,
  filtered by `wespeaker-resnet34-lm` id) load on mount and tear
  down on unmount. Pre-extraction the page kept all these
  listeners alive for the whole Settings session; now they only
  fire while the Meeting tab is active.

  Behavioural delta worth noting: if the user starts a diarizer
  download on the Meeting tab and then switches to a different
  tab, the download itself continues (Rust side is unaffected)
  but the live progress percentage stops updating. On returning
  to Meeting, `loadDiarizerModelStatus` re-reads the on-disk
  state, so a completed download immediately flips the UI to
  "ready" without the intermediate progress frames. This matches
  how the Whisper picker on the Model tab already behaves and
  was acceptable per #332's lazy-load goal.
-->
<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { onDestroy, onMount, tick } from "svelte";

  import AdvancedSection from "./AdvancedSection.svelte";
  import MeetingAppOverridesPanel from "./MeetingAppOverridesPanel.svelte";
  import { openExternal } from "./openExternal";
  import { Events } from "./events";
  import { formatErrorDisplay, formatErrorMessage, type ErrorDisplay } from "./errors";
  import "./settings-tab.css";
  import type {
    AudioSourceListing,
    BuiltinAppEntry,
    DiarizerModelStatus,
    MeetingAppKind,
    MeetingAppOverride,
    ModelCard,
  } from "./types";

  // Auto-start mode. Backend serde encoding is kebab-case
  // ("off" / "always") so values bind directly to <option>
  // strings.
  type MeetingAutostartMode = "off" | "always";
  let meetingAutostartMode = $state<MeetingAutostartMode>("off");
  let meetingAutostartBusy = $state(false);
  let meetingAutostartError = $state<string | null>(null);

  // Diarization toggle (#111). Off by default; runtime gates on
  // FlagGatedDiarizer reading the same atomic shared with
  // AppState.
  let diarizationEnabled = $state(false);
  let diarizationBusy = $state(false);
  let diarizationError = $state<string | null>(null);

  // Diarizer model status (#301). When the wespeaker .onnx is
  // missing, the toggle is informational only — runtime falls
  // back to source-only labels.
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

  // App-classifier overrides (#112).
  let appOverrides = $state<MeetingAppOverride[]>([]);
  let appOverridesLoaded = $state(false);
  let appOverridesError = $state<ErrorDisplay | null>(null);
  let newOverrideName = $state("");
  let newOverrideKind = $state<MeetingAppKind>("meeting");
  let overrideInputEl = $state<HTMLInputElement | null>(null);
  // Built-in classification table (#320). Read once on mount.
  let appDefaults = $state<BuiltinAppEntry[]>([]);

  // Per-app audio profile data (#427 Item 5). Loaded once on mount
  // alongside the override list — the panel renders a source +
  // model dropdown per row, so we need both lists available before
  // any row's profile is editable. Failures are non-fatal: the
  // panel hides the dropdowns when the lists are empty, so a
  // missing IPC just degrades gracefully to the kind-only UX.
  let availableAudioSources = $state<AudioSourceListing[]>([]);
  let availableModels = $state<ModelCard[]>([]);

  type DownloadProgressEvent = {
    id: string;
    bytesReceived: number;
    bytesTotal: number | null;
  };

  async function loadMeetingAutostartMode(): Promise<void> {
    try {
      meetingAutostartMode = await invoke<MeetingAutostartMode>(
        "get_meeting_autostart_mode",
      );
      meetingAutostartError = null;
    } catch (e) {
      meetingAutostartError = "Couldn't read auto-start mode.";
      console.warn("[hush] get_meeting_autostart_mode failed", e);
    }
  }

  async function onMeetingAutostartChange(e: Event) {
    const next = (e.target as HTMLSelectElement).value as MeetingAutostartMode;
    meetingAutostartBusy = true;
    meetingAutostartError = null;
    try {
      await invoke("set_meeting_autostart_mode", { mode: next });
      meetingAutostartMode = next;
    } catch (err) {
      meetingAutostartError = formatErrorMessage(err);
      await loadMeetingAutostartMode();
    } finally {
      meetingAutostartBusy = false;
    }
  }

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

  async function loadAppOverrides(): Promise<void> {
    try {
      appOverrides = await invoke<MeetingAppOverride[]>(
        "meeting_app_override_list",
      );
      appOverridesError = null;
    } catch (e) {
      appOverridesError = formatErrorDisplay(e);
    } finally {
      appOverridesLoaded = true;
    }
  }

  async function loadAppDefaults(): Promise<void> {
    // Built-in table is stable per build; read once + cache.
    // Failure is non-fatal — disclosure stays empty.
    try {
      appDefaults = await invoke<BuiltinAppEntry[]>(
        "meeting_app_classifier_defaults",
      );
    } catch (e) {
      console.warn("[hush] meeting_app_classifier_defaults failed", e);
    }
  }

  // Per-app profile dropdown data (#427 Item 5). Both lists are
  // best-effort: failures leave the array empty, the panel hides
  // the dropdowns, and the user is back to the kind-only UX.
  // Refresh on every mount rather than caching across mounts —
  // sources can change between sessions (mic plugged/unplugged).
  async function loadAvailableAudioSources(): Promise<void> {
    try {
      availableAudioSources = await invoke<AudioSourceListing[]>(
        "audio_list_sources",
      );
    } catch (e) {
      console.warn("[hush] audio_list_sources failed", e);
    }
  }

  async function loadAvailableModels(): Promise<void> {
    try {
      availableModels = await invoke<ModelCard[]>("model_list");
    } catch (e) {
      console.warn("[hush] model_list failed", e);
    }
  }

  // Wired through to MeetingAppOverridesPanel's `onSetProfile`.
  // The panel sends the FULL intended state on every change; we
  // forward both fields verbatim and patch the local
  // `appOverrides` list with the returned row so the UI reflects
  // the persisted state without a follow-up `list` round-trip.
  async function setAppOverrideProfile(
    override: MeetingAppOverride,
    preferredAudioSource: string | null,
    preferredModelId: string | null,
  ) {
    try {
      const updated = await invoke<MeetingAppOverride>(
        "meeting_app_override_set_profile",
        {
          appName: override.appName,
          preferredAudioSource,
          preferredModelId,
        },
      );
      appOverrides = appOverrides.map((o) =>
        o.appName === updated.appName ? updated : o,
      );
      appOverridesError = null;
    } catch (e) {
      appOverridesError = formatErrorDisplay(e);
    }
  }

  async function addAppOverride(e: Event) {
    e.preventDefault();
    const name = newOverrideName.trim();
    if (!name) return;
    try {
      const created = await invoke<MeetingAppOverride>(
        "meeting_app_override_upsert",
        { appName: name, kind: newOverrideKind },
      );
      appOverrides = [
        ...appOverrides.filter((o) => o.appName !== created.appName),
        created,
      ].sort((a, b) => a.appName.localeCompare(b.appName));
      newOverrideName = "";
      newOverrideKind = "meeting";
      appOverridesError = null;
      await tick();
      overrideInputEl?.focus();
    } catch (err) {
      appOverridesError = formatErrorDisplay(err);
    }
  }

  /// Batch add for the variant-suggestion box (#320 part 2).
  async function addAppOverrideVariants(
    appNames: string[],
    kind: MeetingAppKind,
  ) {
    if (appNames.length === 0) return;
    try {
      const created = await Promise.all(
        appNames.map((appName) =>
          invoke<MeetingAppOverride>("meeting_app_override_upsert", {
            appName,
            kind,
          }),
        ),
      );
      const createdNames = new Set(created.map((o) => o.appName));
      appOverrides = [
        ...appOverrides.filter((o) => !createdNames.has(o.appName)),
        ...created,
      ].sort((a, b) => a.appName.localeCompare(b.appName));
      newOverrideName = "";
      newOverrideKind = "meeting";
      appOverridesError = null;
      await tick();
      overrideInputEl?.focus();
    } catch (err) {
      // Partial-success recovery: reload to get a consistent view.
      await loadAppOverrides();
      appOverridesError = formatErrorDisplay(err);
    }
  }

  async function changeAppOverrideKind(
    override: MeetingAppOverride,
    kind: MeetingAppKind,
  ) {
    try {
      const updated = await invoke<MeetingAppOverride>(
        "meeting_app_override_upsert",
        { appName: override.appName, kind },
      );
      appOverrides = appOverrides.map((o) =>
        o.appName === updated.appName ? updated : o,
      );
      appOverridesError = null;
    } catch (e) {
      appOverridesError = formatErrorDisplay(e);
    }
  }

  async function deleteAppOverride(override: MeetingAppOverride) {
    try {
      await invoke("meeting_app_override_delete", {
        appName: override.appName,
      });
      appOverrides = appOverrides.filter(
        (o) => o.appName !== override.appName,
      );
      appOverridesError = null;
    } catch (e) {
      appOverridesError = formatErrorDisplay(e);
    }
  }

  onMount(async () => {
    void Promise.all([
      loadMeetingAutostartMode(),
      loadDiarizationEnabled(),
      loadDiarizerModelStatus(),
      loadAppOverrides(),
      loadAppDefaults(),
      loadAvailableAudioSources(),
      loadAvailableModels(),
    ]);

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

<h2 class="tab-title">Meeting</h2>

<section class="settings-group" aria-labelledby="settings-autostart-heading">
  <h2 id="settings-autostart-heading" class="group-heading">Auto-start</h2>
  <div class="select-row">
    <label class="select-label" for="settings-meeting-autostart">
      <span class="select-name">When a meeting app focuses</span>
      <span class="select-desc">
        Off keeps every meeting manual. Always opens a
        Meeting Mode session whenever a known meeting app
        (Zoom, Teams, Discord, …) comes to the foreground.
        Sessions stop manually either way.
      </span>
    </label>
    <select
      id="settings-meeting-autostart"
      data-testid="settings-meeting-autostart"
      disabled={meetingAutostartBusy}
      value={meetingAutostartMode}
      onchange={onMeetingAutostartChange}
    >
      <option value="off">Off — start manually</option>
      <option value="always">Always start a session</option>
    </select>
  </div>
  {#if meetingAutostartError}
    <p class="settings-error">{meetingAutostartError}</p>
  {/if}
</section>

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
          class="diarizer-download-button"
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

<!--
  App-classifier overrides — power-user surface (#427 Item 2).
  Most users never need to teach Hush "this app is a meeting app";
  the static defaults catch Zoom / Teams / Meet / etc. The
  override panel lets advanced users add custom mappings
  (auto-start a meeting session when a specific in-house tool
  focuses, or veto a default that misclassified a media app as a
  meeting). Hidden behind a disclosure so a first-time MeetingTab
  visit doesn't lead with a row of empty form fields.
-->
<AdvancedSection
  label="Advanced — app overrides"
  testId="settings-meeting-advanced-toggle"
>
  <MeetingAppOverridesPanel
    overrides={appOverrides}
    overridesLoaded={appOverridesLoaded}
    overridesError={appOverridesError}
    defaults={appDefaults}
    audioSources={availableAudioSources}
    models={availableModels}
    bind:newAppName={newOverrideName}
    bind:newKind={newOverrideKind}
    bind:inputEl={overrideInputEl}
    onSubmit={addAppOverride}
    onSubmitVariants={addAppOverrideVariants}
    onChangeKind={changeAppOverrideKind}
    onDelete={deleteAppOverride}
    onSetProfile={setAppOverrideProfile}
  />
</AdvancedSection>

<style>
  /* Card primitives (.tab-title, .settings-group, .toggle-row,
     .select-row, .settings-error, .settings-row-name/.desc,
     button.ghost, dark-mode variants) imported from
     `settings-tab.css` (#392). Only the diarizer-installed-
     model details below are tab-specific. */

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
    color: #555;
    font-weight: 500;
  }
  .diarizer-details dd {
    margin: 0;
    color: #1a1a1a;
    user-select: text;
    word-break: break-all;
  }
  .path-code {
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, monospace;
    font-size: 0.78rem;
    color: #2a2a2a;
    background-color: rgba(0, 0, 0, 0.04);
    padding: 0.1em 0.3em;
    border-radius: 4px;
  }
  button.link-like {
    background: none;
    border: none;
    padding: 0;
    color: #2c3e8f;
    text-decoration: underline;
    cursor: pointer;
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, monospace;
    font-size: 0.78rem;
    word-break: break-all;
    text-align: left;
  }
  button.link-like:hover {
    color: #1a2a6c;
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
</style>
