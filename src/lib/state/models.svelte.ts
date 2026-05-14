/// Model picker state module (#694). Extracted from SettingsPanel.svelte
/// to give model IPC a single owner and decouple the reactive state from
/// the settings-panel lifecycle.
///
/// The four model-management IPCs (`loadModels`, `downloadModel`,
/// `cancelDownload`, `removeModel`) live here because they only touch
/// model state. `selectModel` stays in SettingsPanel because it calls
/// the `onModelLoaded` prop callback; the download event listeners also
/// stay in SettingsPanel's `onMount`/`onDestroy` to keep lifecycle code
/// co-located with the component that owns the unlisten refs.
import { invoke } from "@tauri-apps/api/core";
import { SvelteMap } from "svelte/reactivity";

import { formatErrorDisplay, formatErrorMessage, type ErrorDisplay } from "$lib/errors";
import type { DownloadProgress, ModelCard, ModelSelectNotice } from "$lib/types";

export type ModelFetch = {
  models: ModelCard[];
  loaded: boolean;
  error: ErrorDisplay | null;
  restartNotice: ModelSelectNotice;
  // SvelteMap rather than plain Map: per-card mutations
  // (`.set` / `.delete`) trigger reactivity. A plain Map inside
  // `$state(...)` looks reactive at type level but Svelte 5's
  // proxy doesn't intercept Map operations, so a `Cancel` /
  // `download-done` mutation only repainted on the next unrelated
  // re-render (e.g. tab switch). See docs.svelte.dev → reactive
  // built-ins.
  downloading: SvelteMap<string, DownloadProgress>;
  failed: SvelteMap<string, string>;
};

let modelFetchState = $state<ModelFetch>({
  models: [],
  loaded: false,
  error: null,
  restartNotice: null,
  downloading: new SvelteMap(),
  failed: new SvelteMap(),
});

export const models = {
  get models() {
    return modelFetchState.models;
  },
  set models(val: ModelCard[]) {
    modelFetchState.models = val;
  },
  get loaded() {
    return modelFetchState.loaded;
  },
  set loaded(val: boolean) {
    modelFetchState.loaded = val;
  },
  get error() {
    return modelFetchState.error;
  },
  set error(val: ErrorDisplay | null) {
    modelFetchState.error = val;
  },
  get restartNotice() {
    return modelFetchState.restartNotice;
  },
  set restartNotice(val: ModelSelectNotice) {
    modelFetchState.restartNotice = val;
  },
  get downloading() {
    return modelFetchState.downloading;
  },
  get failed() {
    return modelFetchState.failed;
  },

  async loadModels(): Promise<void> {
    try {
      modelFetchState.models = await invoke<ModelCard[]>("model_list");
      modelFetchState.error = null;
    } catch (e) {
      modelFetchState.error = formatErrorDisplay(e);
    } finally {
      modelFetchState.loaded = true;
    }
  },

  async downloadModel(card: ModelCard): Promise<void> {
    modelFetchState.failed.delete(card.id);
    modelFetchState.downloading.set(card.id, { received: 0, total: null });
    try {
      await invoke("model_download", { id: card.id });
    } catch (e) {
      modelFetchState.failed.set(card.id, formatErrorMessage(e));
      modelFetchState.downloading.delete(card.id);
    }
  },

  async cancelDownload(card: ModelCard): Promise<void> {
    try {
      await invoke("model_cancel_download", { id: card.id });
    } catch (e) {
      console.warn("[hush] cancel download failed", e);
    }
    modelFetchState.downloading.delete(card.id);
  },

  async removeModel(card: ModelCard): Promise<void> {
    try {
      await invoke("model_remove", { id: card.id });
      await models.loadModels();
    } catch (e) {
      modelFetchState.error = formatErrorDisplay(e);
    }
  },
};
