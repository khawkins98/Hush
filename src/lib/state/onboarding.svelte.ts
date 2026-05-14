import { invoke } from "@tauri-apps/api/core";
import { dictation } from "$lib/state/dictation.svelte";

// First-run wizard orchestration state. `showFirstRun` is set true
// on app start if the backend flag is unset, and cleared by
// completeFirstRun() once the user dismisses the wizard.
//
// `completeFirstRun()` reloads audio sources after marking the
// wizard complete because the user may have just granted Microphone
// permission inside the wizard — the source list must re-enumerate
// before the dictation flow tries to start.

let showFirstRun = $state(false);

export const onboarding = {
  get showFirstRun() {
    return showFirstRun;
  },

  /** Check the backend flag and show the wizard if first-run is not yet done. */
  async check() {
    try {
      const done = await invoke<boolean>("get_first_run_completed");
      if (!done) showFirstRun = true;
    } catch (e) {
      console.error("get_first_run_completed failed:", e);
    }
  },

  /** Mark first run done, hide the wizard, and reload audio sources so
   *  any mic permission just granted inside the wizard takes effect. */
  async completeFirstRun() {
    showFirstRun = false;
    try {
      await invoke("mark_first_run_completed");
    } catch (e) {
      console.error("mark_first_run_completed failed:", e);
    }
    void dictation.loadSources();
  },
};
