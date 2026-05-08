<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import {
    disable as disableAutostart,
    enable as enableAutostart,
    isEnabled as isAutostartEnabled,
  } from "@tauri-apps/plugin-autostart";
  import { onMount } from "svelte";

  import { formatErrorMessage } from "./errors";
  import "./settings-tab.css";

  let autostartEnabled = $state(false);
  let autostartBusy = $state(false);
  let autostartError = $state<string | null>(null);
  let autostartPathStale = $state(false);
  let autostartRetryBusy = $state(false);
  let autostartRetryFailed = $state(false);

  async function loadAutostartState(): Promise<void> {
    try {
      autostartEnabled = await isAutostartEnabled();
      autostartError = null;
    } catch (e) {
      autostartEnabled = false;
      if (!import.meta.env.DEV) {
        autostartError = "Couldn't read autostart state on this platform.";
      }
      console.warn("[hush] isAutostartEnabled failed", e);
    }
  }

  async function loadAutostartPathStatus(): Promise<void> {
    try {
      const status = await invoke<{ stale: boolean }>("get_autostart_path_status");
      autostartPathStale = status.stale;
    } catch (e) {
      console.warn("[hush] get_autostart_path_status failed", e);
      autostartPathStale = false;
    }
  }

  async function onRetryAutostartRegistration() {
    if (autostartRetryBusy) return;
    autostartRetryBusy = true;
    autostartRetryFailed = false;
    try {
      const ok = await invoke<boolean>("retry_autostart_registration");
      if (ok) {
        autostartPathStale = false;
      } else {
        autostartRetryFailed = true;
      }
    } catch (e) {
      autostartRetryFailed = true;
      console.warn("[hush] retry_autostart_registration failed", e);
    } finally {
      autostartRetryBusy = false;
    }
  }

  async function onAutostartToggle(e: Event) {
    const checked = (e.target as HTMLInputElement).checked;
    autostartBusy = true;
    autostartError = null;
    try {
      if (checked) await enableAutostart();
      else await disableAutostart();
      autostartEnabled = checked;
    } catch (err) {
      autostartError = formatErrorMessage(err);
      await loadAutostartState();
    } finally {
      autostartBusy = false;
    }
  }

  onMount(() => {
    void Promise.all([loadAutostartState(), loadAutostartPathStatus()]);
  });
</script>

<section class="settings-group" aria-labelledby="settings-startup-heading">
  <h2 id="settings-startup-heading" class="group-heading">Startup</h2>
  <label class="toggle-row">
    <input
      type="checkbox"
      data-testid="settings-autostart-toggle"
      disabled={autostartBusy}
      checked={autostartEnabled}
      onchange={onAutostartToggle}
    />
    <span class="toggle-label">
      <span class="toggle-name">Launch Hush at login</span>
      <span class="toggle-desc">
        Hush opens automatically when you sign in. The window
        stays in the background — your hotkey still works.
      </span>
    </span>
  </label>
  {#if autostartError}
    <p class="settings-error">{autostartError}</p>
  {/if}

  {#if autostartPathStale}
    <div
      class="settings-warning-row"
      data-testid="autostart-path-stale-warning"
      role="alert"
    >
      <p class="settings-row-name">⚠ Autostart path is out of date</p>
      <p class="settings-row-desc">
        Hush couldn't refresh the LaunchAgent at startup, so
        "Launch at Login" may not work after the next restart.
        Click below to retry — usually a one-click fix.
      </p>
      <button
        type="button"
        class="ghost"
        data-testid="autostart-retry-button"
        disabled={autostartRetryBusy}
        onclick={onRetryAutostartRegistration}
      >
        {autostartRetryBusy ? "Retrying…" : "Click to update"}
      </button>
      {#if autostartRetryFailed}
        <p
          class="settings-error"
          data-testid="autostart-retry-error"
        >
          Retry failed too. Check that <code
            >~/Library/LaunchAgents/</code
          > is writable, then try again.
        </p>
      {/if}
    </div>
  {/if}
</section>
