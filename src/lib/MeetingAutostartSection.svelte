<!--
  Settings → Meeting tab — Auto-start section (#693).
  Extracted from MeetingTab.svelte to give the autostart IPC
  and markup a single owner. Manages its own load-on-mount
  lifecycle so the state initialises when the Meeting tab
  becomes visible and tears down cleanly when it unmounts.
-->
<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onMount } from "svelte";

  import { formatErrorMessage } from "./errors";
  import "./settings-tab.css";

  // Backend serde encoding is kebab-case ("off" / "always")
  // so values bind directly to <option> strings.
  type MeetingAutostartMode = "off" | "always";
  let meetingAutostartMode = $state<MeetingAutostartMode>("always");
  let meetingAutostartBusy = $state(false);
  let meetingAutostartError = $state<string | null>(null);

  async function loadMeetingAutostartMode(): Promise<void> {
    try {
      meetingAutostartMode = await invoke<MeetingAutostartMode>(
        "get_meeting_autostart_mode",
      );
      meetingAutostartError = null;
    } catch (e) {
      meetingAutostartError = "Couldn't read auto-start mode.";
      console.warn("[hush] get_meeting_autostart_mode failed", e);
    }
  }

  async function onMeetingAutostartChange(e: Event) {
    const next = (e.target as HTMLSelectElement).value as MeetingAutostartMode;
    meetingAutostartBusy = true;
    meetingAutostartError = null;
    try {
      await invoke("set_meeting_autostart_mode", { mode: next });
      meetingAutostartMode = next;
    } catch (err) {
      meetingAutostartError = formatErrorMessage(err);
      await loadMeetingAutostartMode();
    } finally {
      meetingAutostartBusy = false;
    }
  }

  onMount(() => {
    void loadMeetingAutostartMode();
  });
</script>

<section class="settings-group" aria-labelledby="settings-autostart-heading">
  <h2 id="settings-autostart-heading" class="group-heading">Auto-start</h2>
  <div class="select-row">
    <label class="select-label" for="settings-meeting-autostart">
      <span class="select-name">When mic activates in a meeting app</span>
      <span class="select-desc">
        Off keeps every meeting manual. Always opens a
        Meeting Mode session whenever your microphone
        activates while a known meeting app (Zoom, Teams,
        Discord, …) is running. Auto-started sessions stop
        when the meeting app releases the mic; manually
        started sessions stop when you click Stop.
      </span>
    </label>
    <select
      id="settings-meeting-autostart"
      data-testid="settings-meeting-autostart"
      disabled={meetingAutostartBusy}
      value={meetingAutostartMode}
      onchange={onMeetingAutostartChange}
    >
      <option value="off">Off — start manually</option>
      <option value="always">Always start a session</option>
    </select>
  </div>
  {#if meetingAutostartError}
    <p class="settings-error">{meetingAutostartError}</p>
  {/if}
</section>
