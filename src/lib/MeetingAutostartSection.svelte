<!--
  Settings → Meeting tab — Auto-start section (#693).
  Thin markup shell for the meeting auto-start selector. IPC
  state lives in `state/meeting-settings.svelte.ts`; this
  component only owns the load-on-mount lifecycle.
-->
<script lang="ts">
  import { onMount } from "svelte";

  import { meetingSettings } from "$lib/state/meeting-settings.svelte";
  import "./settings-tab.css";

  onMount(() => {
    void meetingSettings.loadMeetingAutostartMode();
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
      disabled={meetingSettings.meetingAutostartBusy}
      value={meetingSettings.meetingAutostartMode}
      onchange={meetingSettings.onMeetingAutostartChange}
    >
      <option value="off">Off — start manually</option>
      <option value="always">Always start a session</option>
    </select>
  </div>
  {#if meetingSettings.meetingAutostartError}
    <p class="settings-error">{meetingSettings.meetingAutostartError}</p>
  {/if}
</section>
