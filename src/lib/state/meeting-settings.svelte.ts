/// Meeting settings state module (#710). Owns the auto-start selector
/// state plus the Meeting tab's app-override IPCs. Form-field bindings
/// stay in MeetingTab.svelte so the component can manage DOM focus.
import { invoke } from "@tauri-apps/api/core";

import {
  formatErrorDisplay,
  formatErrorMessage,
  type ErrorDisplay,
} from "$lib/errors";
import type {
  AudioSourceListing,
  BuiltinAppEntry,
  MeetingAppKind,
  MeetingAppOverride,
  ModelCard,
} from "$lib/types";

type MeetingAutostartMode = "off" | "always";

let meetingAutostartMode = $state<MeetingAutostartMode>("always");
let meetingAutostartBusy = $state(false);
let meetingAutostartError = $state<string | null>(null);

let appOverrides = $state<MeetingAppOverride[]>([]);
let appOverridesLoaded = $state(false);
let appOverridesError = $state<ErrorDisplay | null>(null);
let appDefaults = $state<BuiltinAppEntry[]>([]);
let availableAudioSources = $state<AudioSourceListing[]>([]);
let availableModels = $state<ModelCard[]>([]);

export const meetingSettings = {
  get meetingAutostartMode() {
    return meetingAutostartMode;
  },
  get meetingAutostartBusy() {
    return meetingAutostartBusy;
  },
  get meetingAutostartError() {
    return meetingAutostartError;
  },
  get appOverrides() {
    return appOverrides;
  },
  get appOverridesLoaded() {
    return appOverridesLoaded;
  },
  get appOverridesError() {
    return appOverridesError;
  },
  get appDefaults() {
    return appDefaults;
  },
  get availableAudioSources() {
    return availableAudioSources;
  },
  get availableModels() {
    return availableModels;
  },

  async load(): Promise<void> {
    await Promise.all([
      meetingSettings.loadAppOverrides(),
      meetingSettings.loadAppDefaults(),
      meetingSettings.loadAvailableAudioSources(),
      meetingSettings.loadAvailableModels(),
    ]);
  },

  async loadMeetingAutostartMode(): Promise<void> {
    try {
      meetingAutostartMode = await invoke<MeetingAutostartMode>(
        "get_meeting_autostart_mode",
      );
      meetingAutostartError = null;
    } catch (e) {
      meetingAutostartError = "Couldn't read auto-start mode.";
      console.warn("[hush] get_meeting_autostart_mode failed", e);
    }
  },

  async onMeetingAutostartChange(e: Event): Promise<void> {
    const next = (e.target as HTMLSelectElement).value as MeetingAutostartMode;
    meetingAutostartBusy = true;
    meetingAutostartError = null;
    try {
      await invoke("set_meeting_autostart_mode", { mode: next });
      meetingAutostartMode = next;
    } catch (err) {
      meetingAutostartError = formatErrorMessage(err);
      await meetingSettings.loadMeetingAutostartMode();
    } finally {
      meetingAutostartBusy = false;
    }
  },

  async loadAppOverrides(): Promise<void> {
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
  },

  async loadAppDefaults(): Promise<void> {
    try {
      appDefaults = await invoke<BuiltinAppEntry[]>(
        "meeting_app_classifier_defaults",
      );
    } catch (e) {
      console.warn("[hush] meeting_app_classifier_defaults failed", e);
    }
  },

  async loadAvailableAudioSources(): Promise<void> {
    try {
      availableAudioSources = await invoke<AudioSourceListing[]>(
        "audio_list_sources",
      );
    } catch (e) {
      console.warn("[hush] audio_list_sources failed", e);
    }
  },

  async loadAvailableModels(): Promise<void> {
    try {
      availableModels = await invoke<ModelCard[]>("model_list");
    } catch (e) {
      console.warn("[hush] model_list failed", e);
    }
  },

  async addAppOverride(
    appName: string,
    kind: MeetingAppKind,
  ): Promise<boolean> {
    const name = appName.trim();
    if (!name) return false;
    try {
      const created = await invoke<MeetingAppOverride>(
        "meeting_app_override_upsert",
        { appName: name, kind },
      );
      appOverrides = [
        ...appOverrides.filter((o) => o.appName !== created.appName),
        created,
      ].sort((a, b) => a.appName.localeCompare(b.appName));
      appOverridesError = null;
      return true;
    } catch (err) {
      appOverridesError = formatErrorDisplay(err);
      return false;
    }
  },

  async addAppOverrideVariants(
    appNames: string[],
    kind: MeetingAppKind,
  ): Promise<boolean> {
    if (appNames.length === 0) return false;
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
      appOverridesError = null;
      return true;
    } catch (err) {
      await meetingSettings.loadAppOverrides();
      appOverridesError = formatErrorDisplay(err);
      return false;
    }
  },

  async changeAppOverrideKind(
    override: MeetingAppOverride,
    kind: MeetingAppKind,
  ): Promise<void> {
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
  },

  async deleteAppOverride(override: MeetingAppOverride): Promise<void> {
    try {
      await invoke("meeting_app_override_delete", {
        appName: override.appName,
      });
      appOverrides = appOverrides.filter((o) => o.appName !== override.appName);
      appOverridesError = null;
    } catch (e) {
      appOverridesError = formatErrorDisplay(e);
    }
  },

  async setAppOverrideProfile(
    override: MeetingAppOverride,
    preferredAudioSource: string | null,
    preferredModelId: string | null,
  ): Promise<void> {
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
  },
};
