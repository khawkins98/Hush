/**
 * Reactive state module for General tab runtime settings:
 * dictation stats, inference threads, mic gain, and first-run reset.
 *
 * IPC calls:
 *   get_dictation_stats
 *   get_inference_threads / set_inference_threads
 *   get_mic_gain_db / set_mic_gain_db
 *   reset_first_run
 *
 * Theme, status-line, and debug-console are localStorage-backed and
 * stay in GeneralTab since they use lib/theme.ts, lib/status-line.ts,
 * and lib/debug-console.ts directly — no IPC seam to own here.
 */
import { invoke } from "@tauri-apps/api/core";
import { formatErrorMessage } from "$lib/errors";
import type { DictationStats } from "$lib/types";

let _stats = $state<DictationStats | null>(null);

// Two-cell slider pattern (#348): `_inferenceThreads` is the
// persisted value; `_inferenceThreadsDisplay` tracks the slider
// thumb live during drag so the label updates in real time without
// firing one IPC per pixel of movement. The `change` event
// (release) is what persists.
let _inferenceThreads = $state(4);
let _inferenceThreadsDisplay = $state(4);
let _inferenceThreadsBusy = $state(false);
let _inferenceThreadsError = $state<string | null>(null);

// Same two-cell pattern for mic gain (#531).
let _micGainDb = $state(0);
let _micGainDbDisplay = $state(0);
let _micGainDbBusy = $state(false);
let _micGainDbError = $state<string | null>(null);

let _firstRunResetBusy = $state(false);
let _firstRunResetMessage = $state<string | null>(null);

async function loadStats(): Promise<void> {
  try {
    _stats = await invoke<DictationStats>("get_dictation_stats");
  } catch (e) {
    console.warn("[hush] get_dictation_stats failed", e);
  }
}

async function loadInferenceThreads(): Promise<void> {
  try {
    _inferenceThreads = await invoke<number>("get_inference_threads");
    _inferenceThreadsDisplay = _inferenceThreads;
    _inferenceThreadsError = null;
  } catch (e) {
    _inferenceThreadsError = "Couldn't read inference-threads setting.";
    console.warn("[hush] get_inference_threads failed", e);
  }
}

async function loadMicGainDb(): Promise<void> {
  try {
    _micGainDb = await invoke<number>("get_mic_gain_db");
    _micGainDbDisplay = _micGainDb;
    _micGainDbError = null;
  } catch (e) {
    _micGainDbError = "Couldn't read mic gain setting.";
    console.warn("[hush] get_mic_gain_db failed", e);
  }
}

export const generalRuntime = {
  get stats() {
    return _stats;
  },

  get inferenceThreads() {
    return _inferenceThreads;
  },
  get inferenceThreadsDisplay() {
    return _inferenceThreadsDisplay;
  },
  get inferenceThreadsBusy() {
    return _inferenceThreadsBusy;
  },
  get inferenceThreadsError() {
    return _inferenceThreadsError;
  },

  get micGainDb() {
    return _micGainDb;
  },
  get micGainDbDisplay() {
    return _micGainDbDisplay;
  },
  get micGainDbBusy() {
    return _micGainDbBusy;
  },
  get micGainDbError() {
    return _micGainDbError;
  },

  get firstRunResetBusy() {
    return _firstRunResetBusy;
  },
  get firstRunResetMessage() {
    return _firstRunResetMessage;
  },

  /** Load all runtime settings in parallel. Call from onMount. */
  async load(): Promise<void> {
    await Promise.all([loadInferenceThreads(), loadMicGainDb(), loadStats()]);
  },

  onInferenceThreadsInput(e: Event) {
    const next = Number((e.target as HTMLInputElement).value);
    if (Number.isFinite(next)) _inferenceThreadsDisplay = next;
  },

  async onInferenceThreadsChange(e: Event): Promise<void> {
    const next = Number((e.target as HTMLInputElement).value);
    if (!Number.isFinite(next)) return;
    _inferenceThreadsBusy = true;
    _inferenceThreadsError = null;
    try {
      await invoke("set_inference_threads", { threads: next });
      _inferenceThreads = next;
      _inferenceThreadsDisplay = next;
    } catch (err) {
      _inferenceThreadsError = formatErrorMessage(err);
      await loadInferenceThreads();
    } finally {
      _inferenceThreadsBusy = false;
    }
  },

  onMicGainDbInput(e: Event) {
    const next = Number((e.target as HTMLInputElement).value);
    if (Number.isFinite(next)) _micGainDbDisplay = next;
  },

  async onMicGainDbChange(e: Event): Promise<void> {
    const next = Number((e.target as HTMLInputElement).value);
    if (!Number.isFinite(next)) return;
    _micGainDbBusy = true;
    _micGainDbError = null;
    try {
      await invoke("set_mic_gain_db", { gainDb: next });
      _micGainDb = next;
      _micGainDbDisplay = next;
    } catch (err) {
      _micGainDbError = formatErrorMessage(err);
      await loadMicGainDb();
    } finally {
      _micGainDbBusy = false;
    }
  },

  async onResetFirstRun(): Promise<void> {
    _firstRunResetBusy = true;
    try {
      await invoke("reset_first_run");
      _firstRunResetMessage = "Welcome will show on next launch.";
      // Auto-clear after 3 s so the button returns to its actionable
      // label if the user changes their mind in the same session.
      setTimeout(() => {
        _firstRunResetMessage = null;
      }, 3000);
    } catch (e) {
      _firstRunResetMessage = formatErrorMessage(e);
    } finally {
      _firstRunResetBusy = false;
    }
  },
};
