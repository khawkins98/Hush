/// PTT (push-to-talk) persisted config state module (#720).
/// Extracted from PttHotkeyEditor.svelte so the backend IPC
/// calls have a single owner and the component becomes a thin
/// presentation layer.
///
/// What lives here:
///   - combo, enabled, listenerRunning, loaded, error, saving state
///   - load() — reads ptt_get_config on component mount
///   - persist() — writes ptt_set_config and re-reads after save
///
/// What stays in PttHotkeyEditor:
///   - capture state machine (capturing, captured, captureBuffer,
///     physicallyHeld, ignoredKeyHint + timer)
///   - window keydown/keyup DOM listeners and $effect cleanup
///   - onMount / onDestroy wiring (tied to component DOM lifetime;
///     listeners must not live on a singleton module — see learnings.md)
import { invoke } from "@tauri-apps/api/core";

import type { PttConfig } from "$lib/types";

let combo = $state<string[]>([]);
let enabled = $state(false);
let listenerRunning = $state(false);
let loaded = $state(false);
let error = $state<string | null>(null);
let saving = $state(false);

export const ptt = {
  get combo() {
    return combo;
  },
  get enabled() {
    return enabled;
  },
  get listenerRunning() {
    return listenerRunning;
  },
  get loaded() {
    return loaded;
  },
  get error() {
    return error;
  },
  set error(val: string | null) {
    error = val;
  },
  get saving() {
    return saving;
  },

  /// Read current PTT config from the backend. Called on mount so
  /// the editor always reflects the persisted state.
  async load(): Promise<void> {
    try {
      const cfg = await invoke<PttConfig>("ptt_get_config");
      combo = cfg.combo;
      enabled = cfg.enabled;
      listenerRunning = cfg.listenerRunning;
      error = null;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loaded = true;
    }
  },

  /// Write a new combo + enabled state to the backend, then re-read so
  /// `listenerRunning` reflects the outcome of the on-demand spawn.
  /// On macOS, the Input Monitoring prompt may still be visible; the
  /// listener starts delivering events the moment the grant is made,
  /// but listenerRunning flips to true now (thread is up; permission
  /// grant just gates whether events flow).
  async persist(nextCombo: string[], nextEnabled: boolean): Promise<void> {
    saving = true;
    error = null;
    try {
      await invoke("ptt_set_config", {
        combo: nextCombo,
        enabled: nextEnabled,
      });
      combo = nextCombo;
      enabled = nextEnabled;
      await ptt.load();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
      await ptt.load();
    } finally {
      saving = false;
    }
  },
};

