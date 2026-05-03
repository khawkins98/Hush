<!--
  Permission-health lifecycle wrapper (#432 main-page decomp).

  Owns the macOS permission probe lifecycle that previously sat on
  the main page: the focus-debounced `get_permission_health` poll,
  the one-shot `diagnose_macos_permissions` capability check, and
  the recovery `PermissionsDialog` overlay.

  The pre-#432 main page directly owned `permissionHealth`,
  `permStatuses`, `macosCapable`, `showPermissionsDialog`, and
  `permissionsDialogIntro`, plus the focus-event listener and the
  250 ms debounce timer. After the split this component holds the
  lifecycle code; the orchestrator still owns the *state* via
  bindable props because both the dictation section (Record-mode
  branch reads `permissionHealth`) and the welcome modal
  (`allPermsGranted`) need to read it without re-deriving.

  Visually this component renders only the recovery dialog overlay.
  The `MacosPermsPill` banner stays in the dictation section's
  layout slot so the visual structure of the page is unchanged —
  the orchestrator passes the bound `macosCapable` /
  `allPermsGranted` / `anyPermsDenied` to the pill.

  ## Cross-section boundary

  - **Bindable inputs**: parent sets `showDialog = true` /
    `dialogIntro = "..."` from any code path that wants the
    recovery dialog to open (TCC-shaped errors, FirstRunModal
    callback, …).
  - **Bindable outputs**: parent reads `permissionHealth`,
    `permStatuses`, `macosCapable` reactively. Re-deriving
    `allPermsGranted` etc. in the orchestrator keeps cross-section
    consumers (welcome path, MacosPermsPill, ControlsSection's
    screen-recording badge) on a single source of truth without
    forcing the section to re-export every derived shape.
  - **Callback**: `onOpenPrivacyPane` is supplied by the parent
    because the open-in-System-Settings flow has retry/error
    handling that lives outside this section's scope.
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
