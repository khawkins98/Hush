/// Replacement-rule state module (#721).
/// Extracted from ReplacementsTab.svelte so all replacement-domain IPC
/// has a single owner and the tab component becomes a thin mediator.
///
/// What lives here:
///   - rules, loaded, error reactive state
///   - load() — fetches rules on tab mount
///   - add() — creates a rule and appends it locally
///   - remove() — deletes a rule and removes it locally
///
/// What stays in ReplacementsTab:
///   - newFind / newReplace / inputEl (form input state, tied to DOM)
///   - the tick() + inputEl.focus() reset after a successful add
///   - onMount wiring (the component owns the mount lifecycle)
import { invoke } from "@tauri-apps/api/core";

import { formatErrorDisplay, type ErrorDisplay } from "$lib/errors";
import type { ReplacementRule } from "$lib/types";

let rules = $state<ReplacementRule[]>([]);
let loaded = $state(false);
let error = $state<ErrorDisplay | null>(null);

export const replacements = {
  get rules() {
    return rules;
  },
  get loaded() {
    return loaded;
  },
  get error() {
    return error;
  },
  set error(val: ErrorDisplay | null) {
    error = val;
  },

  /// Load all replacement rules. Called once on ReplacementsTab mount.
  /// Re-fetches on every mount rather than caching — the user may have
  /// changed rules from another settings open.
  async load(): Promise<void> {
    try {
      rules = await invoke<ReplacementRule[]>("replacements_list");
      error = null;
    } catch (e) {
      error = formatErrorDisplay(e);
    } finally {
      loaded = true;
    }
  },

  /// Create a new rule appended at the end of the current list.
  /// Returns the created rule so the caller can reset form inputs and
  /// move focus. Throws on IPC error so the caller can surface it.
  async add(findText: string, replaceText: string): Promise<ReplacementRule> {
    // sortOrder = current length: append-at-end semantics.
    // The backend stores it on the row and uses it for canonical
    // ordering; a reordering UI would update sortOrder without
    // re-creating rows.
    const created = await invoke<ReplacementRule>("replacement_create", {
      findText,
      replaceText,
      sortOrder: rules.length,
    });
    rules = [...rules, created];
    error = null;
    return created;
  },

  /// Delete a rule by id, removing it from the local list on success.
  async remove(rule: ReplacementRule): Promise<void> {
    await invoke("replacement_delete", { id: rule.id });
    rules = rules.filter((r) => r.id !== rule.id);
    error = null;
  },
};
