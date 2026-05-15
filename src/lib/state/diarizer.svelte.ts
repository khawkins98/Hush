/// Diarizer settings state module (#710). Owns the diarization toggle,
/// wespeaker model lifecycle state, the speaker-identity toggle (#667),
/// and the SettingsPanel auto-bundle helper. Event listeners stay in
/// DiarizerModelSection.svelte.
import { invoke } from "@tauri-apps/api/core";

import { formatErrorMessage } from "$lib/errors";
import type { DiarizerModelStatus } from "$lib/types";

const DIARIZER_MODEL_ID = "wespeaker-resnet34-lm";

let diarizationEnabled = $state(false);
let diarizationBusy = $state(false);
let diarizationError = $state<string | null>(null);

let diarizerModelStatus = $state<DiarizerModelStatus | null>(null);
let diarizerDownloadBusy = $state(false);
let diarizerDownloadProgress = $state<{ received: number; total: number | null } | null>(null);
let diarizerDownloadError = $state<string | null>(null);

let diarizerRemoveConfirming = $state(false);
let diarizerRemoveBusy = $state(false);
let diarizerRemoveError = $state<string | null>(null);

// Speaker identity (#667).
let speakerIdentityEnabled = $state(false);
let speakerIdentityBusy = $state(false);
let speakerIdentityError = $state<string | null>(null);

export const diarizer = {
  get diarizationEnabled() {
    return diarizationEnabled;
  },
  get diarizationBusy() {
    return diarizationBusy;
  },
  get diarizationError() {
    return diarizationError;
  },
  get diarizerModelStatus() {
    return diarizerModelStatus;
  },
  get diarizerDownloadBusy() {
    return diarizerDownloadBusy;
  },
  set diarizerDownloadBusy(val: boolean) {
    diarizerDownloadBusy = val;
  },
  get diarizerDownloadProgress() {
    return diarizerDownloadProgress;
  },
  set diarizerDownloadProgress(val: { received: number; total: number | null } | null) {
    diarizerDownloadProgress = val;
  },
  get diarizerDownloadError() {
    return diarizerDownloadError;
  },
  set diarizerDownloadError(val: string | null) {
    diarizerDownloadError = val;
  },
  get diarizerRemoveConfirming() {
    return diarizerRemoveConfirming;
  },
  set diarizerRemoveConfirming(val: boolean) {
    diarizerRemoveConfirming = val;
  },
  get diarizerRemoveBusy() {
    return diarizerRemoveBusy;
  },
  get diarizerRemoveError() {
    return diarizerRemoveError;
  },

  // Speaker identity (#667).
  get speakerIdentityEnabled() {
    return speakerIdentityEnabled;
  },
  get speakerIdentityBusy() {
    return speakerIdentityBusy;
  },
  get speakerIdentityError() {
    return speakerIdentityError;
  },

  async loadSpeakerIdentityEnabled(): Promise<void> {
    try {
      speakerIdentityEnabled = await invoke<boolean>(
        "get_speaker_identity_enabled",
      );
    } catch (e) {
      speakerIdentityError = "Couldn't read speaker identity setting.";
      console.warn("[hush] get_speaker_identity_enabled failed", e);
    }
  },

  async onSpeakerIdentityToggle(e: Event): Promise<void> {
    const checked = (e.target as HTMLInputElement).checked;
    speakerIdentityBusy = true;
    speakerIdentityError = null;
    try {
      await invoke("set_speaker_identity_enabled", { enabled: checked });
      speakerIdentityEnabled = checked;
    } catch (err) {
      speakerIdentityError = formatErrorMessage(err);
      await diarizer.loadSpeakerIdentityEnabled();
    } finally {
      speakerIdentityBusy = false;
    }
  },

  async loadDiarizationEnabled(): Promise<void> {
    try {
      diarizationEnabled = await invoke<boolean>("get_diarization_enabled");
    } catch (e) {
      diarizationError = "Couldn't read diarization setting.";
      console.warn("[hush] get_diarization_enabled failed", e);
    }
  },

  async onDiarizationToggle(e: Event): Promise<void> {
    const checked = (e.target as HTMLInputElement).checked;
    diarizationBusy = true;
    diarizationError = null;
    try {
      await invoke("set_diarization_enabled", { enabled: checked });
      diarizationEnabled = checked;
    } catch (err) {
      diarizationError = formatErrorMessage(err);
      await diarizer.loadDiarizationEnabled();
    } finally {
      diarizationBusy = false;
    }
  },

  async loadDiarizerModelStatus(): Promise<void> {
    try {
      diarizerModelStatus = await invoke<DiarizerModelStatus>(
        "get_diarizer_model_status",
      );
    } catch (e) {
      console.warn("[hush] get_diarizer_model_status failed", e);
      diarizerModelStatus = null;
    }
  },

  async onDiarizerDownload(): Promise<void> {
    if (diarizerDownloadBusy) return;
    diarizerDownloadBusy = true;
    diarizerDownloadProgress = null;
    diarizerDownloadError = null;
    try {
      await invoke("download_diarizer_model");
    } catch (err) {
      diarizerDownloadBusy = false;
      diarizerDownloadError = formatErrorMessage(err);
    }
  },

  async onDiarizerCancel(): Promise<void> {
    try {
      await invoke("model_cancel_download", { id: DIARIZER_MODEL_ID });
    } catch (err) {
      console.warn("[hush] model_cancel_download failed", err);
    }
  },

  async onDiarizerRemoveConfirm(): Promise<void> {
    if (diarizerRemoveBusy) return;
    diarizerRemoveBusy = true;
    diarizerRemoveError = null;
    try {
      await invoke("remove_diarizer_model");
      diarizationEnabled = false;
      await diarizer.loadDiarizerModelStatus();
      diarizerRemoveConfirming = false;
    } catch (err) {
      diarizerRemoveError = formatErrorMessage(err);
    } finally {
      diarizerRemoveBusy = false;
    }
  },

  async maybeAutoDownload(triggerId: string): Promise<void> {
    if (triggerId === DIARIZER_MODEL_ID) return;
    try {
      const status = await invoke<DiarizerModelStatus>(
        "get_diarizer_model_status",
      );
      if (!status.downloaded) {
        await invoke("download_diarizer_model");
      }
    } catch (err) {
      console.warn(
        "[hush] auto-bundle wespeaker download failed; user can retry from Settings → Meeting → Speakers",
        err,
      );
    }
  },
};
