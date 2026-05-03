<!--
  macOS permission-probe lifecycle: the focus-debounced
  `get_permission_health` poll, the one-shot
  `diagnose_macos_permissions` capability check, and the recovery
  `PermissionsDialog` overlay.

  State (`permissionHealth`, `permStatuses`, `macosCapable`,
  `showDialog`, `dialogIntro`) is bindable so the orchestrator
  stays the single source of truth — the dictation flow's
  Record-mode branch and the welcome derivations both read it.
  This component holds the *lifecycle*; the parent holds the
  *state*.
-->
<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onDestroy, onMount } from "svelte";

  import PermissionsDialog from "./PermissionsDialog.svelte";
  import type {
    MacosPermissionDiagnostic,
    PermissionHealthResponse,
    PermissionStatuses,
    PermissionsHealth,
  } from "./types";

  type Props = {
    /// Three-state probe (`granted` / `not-granted` / `stale`)
    /// used by the dictation flow's mode decision and the mic-only
    /// badge. Bindable so the orchestrator reads the latest value.
    permissionHealth: PermissionsHealth | null;
    /// Diagnostic statuses (mic / screen recording / input
    /// monitoring → `granted` / `denied` / `not-determined`). Used
    /// by the orchestrator's `allPermsGranted` / `anyPermsDenied`
    /// derivations. Null until the first probe completes.
    permStatuses: PermissionStatuses | null;
    /// Whether the host supports the macOS perm-reset surface at
    /// all (false on Linux/Windows). Drives the
    /// `MacosPermsPill`'s render.
    macosCapable: boolean;
    /// Recovery-dialog visibility. Parent flips to `true` from any
    /// error path that needs the user to fix permissions; the
    /// section flips it back to `false` on dismiss.
    showDialog: boolean;
    /// Optional intro copy rendered above the dialog body. Cleared
    /// to `undefined` on dismiss so the next open starts clean.
    dialogIntro: string | undefined;
    /// Open the matching pane in System Settings. Owned by the
    /// parent because the retry / error-toast story lives there.
    onOpenPrivacyPane: (
      which: "microphone" | "screen-recording" | "input-monitoring",
    ) => Promise<void>;
  };

  let {
    permissionHealth = $bindable(),
    permStatuses = $bindable(),
    macosCapable = $bindable(),
    showDialog = $bindable(),
    dialogIntro = $bindable(),
    onOpenPrivacyPane,
  }: Props = $props();

  // 250 ms focus-event debounce. Holds the outstanding setTimeout
  // id so onDestroy can clear it; without the cancel a leftover
  // firing after unmount would write to a stale `permissionHealth`
  // cell (Svelte tolerates this but the IPC call is wasted).
  let refreshTimer: ReturnType<typeof setTimeout> | null = null;

  function refreshDebounced() {
    if (refreshTimer !== null) clearTimeout(refreshTimer);
    refreshTimer = setTimeout(() => {
      refreshTimer = null;
      void refreshHealth();
    }, 250);
  }

  async function refreshHealth() {
    try {
      const res =
        await invoke<PermissionHealthResponse>("get_permission_health");
      permissionHealth = res.health;
    } catch (e) {
      // Non-fatal: the dictation flow falls back to mic-only when
      // `permissionHealth` stays null/stale, and the Record button
      // still works.
      console.warn("[hush] get_permission_health failed", e);
    }
  }

  async function loadCapabilityFlag() {
    try {
      const res =
        await invoke<MacosPermissionDiagnostic>("diagnose_macos_permissions");
      macosCapable = res.canReset;
      permStatuses = res.statuses;
    } catch (e) {
      console.error("diagnose_macos_permissions failed:", e);
    }
  }

  function dismiss() {
    showDialog = false;
    dialogIntro = undefined;
  }

  onMount(() => {
    void refreshHealth();
    void loadCapabilityFlag();
    window.addEventListener("focus", refreshDebounced);
  });

  onDestroy(() => {
    window.removeEventListener("focus", refreshDebounced);
    if (refreshTimer !== null) {
      clearTimeout(refreshTimer);
      refreshTimer = null;
    }
  });
</script>

<PermissionsDialog
  show={showDialog}
  onDismiss={dismiss}
  {onOpenPrivacyPane}
  intro={dialogIntro}
/>
