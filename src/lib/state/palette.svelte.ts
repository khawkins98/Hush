// Command palette actions. Extracted from +page.svelte (#685) to keep
// the root route a pure layout file. All action targets are singleton
// state modules — no page-local state needed.
import type { CommandAction } from "$lib/CommandPalette.svelte";
import { dictation, TRAILING_SILENCE_MS } from "$lib/state/dictation.svelte";
import { meeting } from "$lib/state/meeting-sessions.svelte";
import { nav } from "$lib/state/nav.svelte";

const _actions = $derived<CommandAction[]>([
  {
    id: "dictation.start",
    label: "Start transcription",
    subtitle: dictation.noModelInstalled ? "Choose a model first" : undefined,
    group: "Transcribe",
    enabled:
      !dictation.recording &&
      !dictation.busy &&
      !dictation.noModelInstalled &&
      meeting.activeId === null,
    run: () => {
      void dictation.startRecord();
    },
  },
  {
    id: "dictation.stop",
    label: dictation.meetingOnlyActive ? "Stop meeting recording" : "Stop transcription",
    subtitle: dictation.meetingOnlyActive
      ? "Stop the current meeting recording"
      : "Stop the current recording and transcribe",
    group: "Transcribe",
    enabled: dictation.recording || dictation.meetingOnlyActive,
    run: () => {
      void dictation.stop(TRAILING_SILENCE_MS);
    },
  },
  {
    id: "navigate.history",
    label: "Show History",
    subtitle: "Switch to the History panel",
    group: "Navigate",
    run: () => {
      nav.activeSection = "history";
    },
  },
  {
    id: "navigate.dictation",
    label: "Show Transcribe",
    subtitle: "Switch back to the Transcribe panel",
    group: "Navigate",
    enabled: nav.activeSection !== "dictation",
    run: () => {
      nav.activeSection = "dictation";
    },
  },
  {
    id: "settings.general",
    label: "Open Settings: General",
    group: "Settings",
    run: () => {
      nav.openSettingsTab("general");
    },
  },
  {
    id: "settings.model",
    label: "Open Settings: Models",
    subtitle: dictation.activeModel?.displayName ?? "No model loaded",
    group: "Settings",
    run: () => {
      nav.openSettingsTab("model");
    },
  },
  {
    id: "settings.vocabulary",
    label: "Open Settings: Vocabulary",
    group: "Settings",
    run: () => {
      nav.openSettingsTab("vocabulary");
    },
  },
  {
    id: "settings.replacements",
    label: "Open Settings: Replacements",
    group: "Settings",
    run: () => {
      nav.openSettingsTab("replacements");
    },
  },
  {
    id: "settings.meeting",
    label: "Open Settings: Meeting",
    group: "Settings",
    run: () => {
      nav.openSettingsTab("meeting");
    },
  },
  {
    id: "settings.permissions",
    label: "Open Settings: Permissions",
    group: "Settings",
    run: () => {
      nav.openSettingsTab("permissions");
    },
  },
  {
    id: "settings.about",
    label: "Show About",
    group: "Settings",
    run: () => {
      nav.openSettingsTab("about");
    },
  },
]);

export const palette = {
  get actions(): CommandAction[] {
    return _actions;
  },
};
