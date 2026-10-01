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
      <span class="select-desc" id="settings-meeting-autostart-desc">
        Record automatically when a meeting app turns on your mic.
      </span>
    </label>
    <select
      id="settings-meeting-autostart"
      data-testid="settings-meeting-autostart"
      aria-describedby="settings-meeting-autostart-desc"
      disabled={meetingSettings.meetingAutostartBusy}
      value={meetingSettings.meetingAutostartMode}
      onchange={meetingSettings.onMeetingAutostartChange}
    >
      <option value="off">Off — start manually</option>
      <option value="always">Always start a session</option>
    </select>
    <!--
      The full behaviour (start + two stop rules) ran to ~9 lines at
      800×600 and pushed the rest of the tab below the fold; it lives
      behind a disclosure so the card scans as one sentence + a select.
      Kept outside the <label> so clicking the summary doesn't focus
      the select.
    -->
    <details class="autostart-how" data-testid="settings-meeting-autostart-how">
      <summary>How it works</summary>
      <p>
        With <strong>Always</strong>, Hush opens a meeting session when your
        microphone turns on while a known meeting app (Zoom, Teams, Discord,
        …) is running. Auto-started sessions stop when the app releases the
        mic; sessions you start yourself stop when you click Stop.
        <strong>Off</strong> keeps every meeting manual.
      </p>
    </details>
  </div>
  {#if meetingSettings.meetingAutostartError}
    <p class="settings-error" role="alert">{meetingSettings.meetingAutostartError}</p>
  {/if}
</section>

<style>
  /* Full-width row under the label + select; summary styled like the
     other in-card disclosures (DiarizerModelSection) rather than a bare
     browser <details>. */
  .autostart-how {
    flex-basis: 100%;
    margin-top: -0.25rem;
  }
  .autostart-how > summary {
    cursor: pointer;
    font-size: 0.78rem;
    font-weight: 600;
    color: var(--text-secondary);
    padding: 0.15rem 0;
    user-select: none;
    width: fit-content;
  }
  .autostart-how > summary:hover {
    color: var(--text-primary);
  }
  .autostart-how > p {
    margin: 0.35rem 0 0;
    font-size: 0.82rem;
    color: var(--text-secondary);
    line-height: 1.45;
  }
</style>
