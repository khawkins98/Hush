<!--
  macOS permission-probe lifecycle: the focus-debounced
  `get_permission_health` poll, the one-shot
  `diagnose_macos_permissions` capability check, and the recovery
  `PermissionsDialog` overlay.

  All state (`permStatuses`, `permissionHealth`, `macosCapable`,
  `showDialog`, `dialogIntro`) is now centralised in the
  `permissions` state module (#722). This component owns the
  *lifecycle* (timers, focus listener, onMount/onDestroy) only.

  `onOpenPrivacyPane` remains a prop because `PermissionsRows` (used
  in both the main window and the settings window) shares the same
  prop interface, and the settings window has no access to the
  main-window `permissions` module.
-->
<script lang="ts">
  import { onDestroy, onMount } from "svelte";

  import PermissionsDialog from "./PermissionsDialog.svelte";
  import { permissions } from "$lib/state/permissions.svelte";

  type Props = {
    /// Open the matching pane in System Settings. Owned by the
    /// parent because the retry / error-toast story lives there.
    onOpenPrivacyPane: (
      which: "microphone" | "screen-recording" | "input-monitoring",
    ) => Promise<void>;
  };

  let { onOpenPrivacyPane }: Props = $props();

  // 250 ms focus-event debounce. Holds the outstanding setTimeout
  // id so onDestroy can clear it; without the cancel a leftover
  // firing after unmount would write to a stale `permissionHealth`
  // cell (Svelte tolerates this but the IPC call is wasted).
  let refreshTimer: ReturnType<typeof setTimeout> | null = null;

  function refreshDebounced() {
    if (refreshTimer !== null) clearTimeout(refreshTimer);
    refreshTimer = setTimeout(() => {
      refreshTimer = null;
      void permissions.refreshHealth();
    }, 250);
  }

  onMount(() => {
    void permissions.refreshHealth();
    void permissions.diagnose();
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
  show={permissions.showPermissionsDialog}
  onDismiss={() => permissions.closeDialog()}
  {onOpenPrivacyPane}
  intro={permissions.permissionsDialogIntro}
/>
