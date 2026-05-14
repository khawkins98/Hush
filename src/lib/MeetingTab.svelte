<!--
  Settings → Meeting tab (#332 phase 1, slice 5 — see also
  PermissionsTab #387, VocabularyTab #389, ReplacementsTab #390,
  GeneralTab #391). Thin coordinator: composes three self-contained
  child sections and keeps only the override form-field bindings
  (`newOverrideName`, `newOverrideKind`, `overrideInputEl`) local.

  Child sections carry their own mount/destroy lifecycle:
  - MeetingAutostartSection — auto-start mode IPC + select
  - DiarizerModelSection    — diarization toggle + model installer
  - AdvancedSection / MeetingAppOverridesPanel — override table

  The override IPC state now lives in `state/meeting-settings.svelte.ts`.
  This component only wraps those mutations so it can clear the bound
  form fields and restore focus after successful adds.
-->
<script lang="ts">
  import { onMount, tick } from "svelte";

  import { meetingSettings } from "$lib/state/meeting-settings.svelte";
  import AdvancedSection from "./AdvancedSection.svelte";
  import DiarizerModelSection from "./DiarizerModelSection.svelte";
  import MeetingAppOverridesPanel from "./MeetingAppOverridesPanel.svelte";
  import MeetingAutostartSection from "./MeetingAutostartSection.svelte";
  import "./settings-tab.css";
  import type {
    MeetingAppKind,
    MeetingAppOverride,
  } from "./types";

  let newOverrideName = $state("");
  let newOverrideKind = $state<MeetingAppKind>("meeting");
  let overrideInputEl = $state<HTMLInputElement | null>(null);

  async function addAppOverride(e: Event) {
    e.preventDefault();
    const added = await meetingSettings.addAppOverride(
      newOverrideName,
      newOverrideKind,
    );
    if (!added) return;
    newOverrideName = "";
    newOverrideKind = "meeting";
    await tick();
    overrideInputEl?.focus();
  }

  async function addAppOverrideVariants(
    appNames: string[],
    kind: MeetingAppKind,
  ) {
    const added = await meetingSettings.addAppOverrideVariants(appNames, kind);
    if (!added) return;
    newOverrideName = "";
    newOverrideKind = "meeting";
    await tick();
    overrideInputEl?.focus();
  }

  async function changeAppOverrideKind(
    override: MeetingAppOverride,
    kind: MeetingAppKind,
  ) {
    await meetingSettings.changeAppOverrideKind(override, kind);
  }

  async function deleteAppOverride(override: MeetingAppOverride) {
    await meetingSettings.deleteAppOverride(override);
  }

  async function setAppOverrideProfile(
    override: MeetingAppOverride,
    preferredAudioSource: string | null,
    preferredModelId: string | null,
  ) {
    await meetingSettings.setAppOverrideProfile(
      override,
      preferredAudioSource,
      preferredModelId,
    );
  }

  onMount(() => {
    void meetingSettings.load();
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
    overrides={meetingSettings.appOverrides}
    overridesLoaded={meetingSettings.appOverridesLoaded}
    overridesError={meetingSettings.appOverridesError}
    defaults={meetingSettings.appDefaults}
    audioSources={meetingSettings.availableAudioSources}
    models={meetingSettings.availableModels}
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

