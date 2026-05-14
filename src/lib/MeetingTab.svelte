<!--
  Settings → Meeting tab (#332 phase 1, slice 5 — see also
  PermissionsTab #387, VocabularyTab #389, ReplacementsTab #390,
  GeneralTab #391). Coordinator: composes three self-contained
  child sections and owns only the app-classifier override state
  and IPCs (#693).

  Child sections carry their own mount/destroy lifecycle:
  - MeetingAutostartSection — auto-start mode IPC + select
  - DiarizerModelSection    — diarization toggle + model installer
  - AdvancedSection / MeetingAppOverridesPanel — override table

  App-override state lives here rather than in a dedicated section
  because the overrides panel is more complex (6 IPCs, per-row
  profile dropdowns, batch-add) and shares no lifecycle with the
  other two sections.
-->
<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onMount, tick } from "svelte";

  import AdvancedSection from "./AdvancedSection.svelte";
  import DiarizerModelSection from "./DiarizerModelSection.svelte";
  import MeetingAppOverridesPanel from "./MeetingAppOverridesPanel.svelte";
  import MeetingAutostartSection from "./MeetingAutostartSection.svelte";
  import { formatErrorDisplay } from "./errors";
  import "./settings-tab.css";
  import type {
    AudioSourceListing,
    BuiltinAppEntry,
    MeetingAppKind,
    MeetingAppOverride,
    ModelCard,
  } from "./types";

  // App-classifier overrides (#112).
  let appOverrides = $state<MeetingAppOverride[]>([]);
  let appOverridesLoaded = $state(false);
  let appOverridesError = $state<import("./errors").ErrorDisplay | null>(null);
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

  onMount(() => {
    void Promise.all([
      loadAppOverrides(),
      loadAppDefaults(),
      loadAvailableAudioSources(),
      loadAvailableModels(),
    ]);
  });
</script>

<h2 class="tab-title">Meeting</h2>

<MeetingAutostartSection />

<DiarizerModelSection />

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
  open={false}
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

