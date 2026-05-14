import { invoke } from "@tauri-apps/api/core";
import type {
  MacosPermissionDiagnostic,
  PermissionHealthResponse,
  PermissionsHealth,
  PermissionStatuses,
} from "$lib/types";

// Central store for macOS permission diagnostic state shared across
// the main window's onboarding, dictation, and health surfaces.
//
// Design: one `diagnose()` function replaces the duplicated
// `diagnose_macos_permissions` calls that previously lived
// independently in FirstRunModal.pollDiagnostic() and
// PermissionHealthSection.loadCapabilityFlag(). Both components
// now delegate to this module so the result lands in one place.
//
// The Settings panel renders PermissionsRows directly and reads
// this module's exported getters for shared state.

let diagnostic = $state<MacosPermissionDiagnostic | null>(null);
let permissionHealth = $state<PermissionsHealth | null>(null);
let showPermissionsDialog = $state(false);
let permissionsDialogIntro = $state<string | undefined>(undefined);
let staleBannerDismissed = $state(false);

// Latest-wins sequence guard: if a later diagnose() call resolves
// before an earlier one, the earlier result is discarded. Prevents
// a slow IPC response from overwriting a fresher one.
let diagnoseSeq = 0;
let healthSeq = 0;

// Convenient derivatives of `diagnostic`; derived so they update
// reactively whenever `diagnose()` resolves.
const permStatuses = $derived<PermissionStatuses | null>(
  diagnostic?.statuses ?? null,
);
const macosCapable = $derived(diagnostic?.canReset ?? false);

// Semantics preserved exactly from the previous +page.svelte
// derivations — no behaviour change is intended here.
const allPermsGranted = $derived(
  !!permStatuses
    && permStatuses.microphone === "granted"
    && permStatuses.inputMonitoring !== "denied",
);
const anyPermsDenied = $derived(
  !!permStatuses
    && (permStatuses.microphone === "denied"
      || permStatuses.inputMonitoring === "denied"),
);
const anyPermsStale = $derived(
  macosCapable
    && !!permissionHealth
    && (permissionHealth.microphone === "stale"
      || permissionHealth.inputMonitoring === "stale"),
);

export const permissions = {
  // ---- State getters ----

  get diagnostic() {
    return diagnostic;
  },

  get permStatuses() {
    return permStatuses;
  },

  get permissionHealth() {
    return permissionHealth;
  },

  get macosCapable() {
    return macosCapable;
  },

  get showPermissionsDialog() {
    return showPermissionsDialog;
  },

  get permissionsDialogIntro() {
    return permissionsDialogIntro;
  },

  get staleBannerDismissed() {
    return staleBannerDismissed;
  },

  set staleBannerDismissed(val: boolean) {
    staleBannerDismissed = val;
  },

  // ---- Derived getters ----

  get allPermsGranted() {
    return allPermsGranted;
  },

  get anyPermsDenied() {
    return anyPermsDenied;
  },

  get anyPermsStale() {
    return anyPermsStale;
  },

  // ---- Actions ----

  /** Refresh TCC status + bundle-id metadata. Updates permStatuses and
   *  macosCapable. Latest-wins: concurrent callers don't race. */
  async diagnose() {
    const seq = ++diagnoseSeq;
    try {
      const res = await invoke<MacosPermissionDiagnostic>(
        "diagnose_macos_permissions",
      );
      if (seq !== diagnoseSeq) return;
      diagnostic = res;
    } catch (e) {
      console.warn("[hush] diagnose_macos_permissions failed", e);
    }
  },

  /** Refresh the csreq-based health verdict (grants present but stale). */
  async refreshHealth() {
    const seq = ++healthSeq;
    try {
      const res =
        await invoke<PermissionHealthResponse>("get_permission_health");
      if (seq !== healthSeq) return;
      permissionHealth = res.health;
    } catch (e) {
      // Non-fatal — the dictation flow falls back gracefully when
      // health is null/stale.
      console.warn("[hush] get_permission_health failed", e);
    }
  },

  /** Show the recovery dialog, optionally with a targeted intro string. */
  openDialog(intro?: string) {
    permissionsDialogIntro = intro;
    showPermissionsDialog = true;
  },

  /** Dismiss the recovery dialog and clear the intro copy. */
  closeDialog() {
    showPermissionsDialog = false;
    permissionsDialogIntro = undefined;
  },

  /** Open the named pane in macOS System Settings. */
  async openPrivacyPane(
    target: "microphone" | "input-monitoring" | "screen-recording",
  ) {
    try {
      await invoke("open_macos_privacy_pane", { target });
    } catch (e) {
      console.error("open_macos_privacy_pane failed:", e);
    }
  },
};
