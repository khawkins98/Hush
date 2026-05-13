<!--
  First-run setup wizard (#511 — was a single-card permission
  briefing pre-rewrite). Two-step shell:

  - Step 1 (Welcome): privacy framing + audio pipeline diagram.
  - Step 2 (Permissions): compact card rows for Microphone /
    Input Monitoring / Screen Recording. Each row has a direct
    Allow button that fires the OS prompt inline (mic + input
    monitoring) or opens System Settings (screen recording, since
    SCK can't be requested programmatically). Rows reflect live
    grant status and dim with a ✓ once granted.

  Continue is never hard-blocked — Hush is usable without every
  permission (no-mic = can't record but everything else still
  works; no-IM = lose PTT but the toggle hotkey is fine; no-SCK
  = system-audio-meeting-mode unavailable but mic-only meetings
  + dictation still work). A soft warning surfaces under
  Continue when the mic is ungranted because that's the one
  permission the dictation hot path actually needs.

  A11y plumbing (preserved from #48):
  - `role="dialog"` + `aria-modal="true"` on the backdrop.
  - Window-level Escape dismisses.
  - Tab cycles within the card; auto-focus the first focusable
    element on open and on each step transition.
  - Focus restores to whatever was focused before the modal
    opened on dismiss.

  Polling cadence (#511): on open + every 1500 ms while step 2
  is visible, refresh the permission diagnostic so the OS prompt
  the user just clicked Allow on flips the row's UI without a
  page refresh. Stops when the modal closes.
-->
<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { onDestroy } from "svelte";

  import AudioPipelineDiagram from "./AudioPipelineDiagram.svelte";
  import type {
    MacosPermissionDiagnostic,
    PermissionStatus,
  } from "./types";

  type Props = {
    show: boolean;
    onDismiss: () => void | Promise<void>;
    /// Settings-deep-link fallback for users who can't grant
    /// inline (e.g. they denied an earlier prompt and need to
    /// flip the System Settings toggle manually). The backend
    /// `open_macos_privacy_pane` call is a no-op on non-macOS.
    onOpenPrivacyPane: (
      target: "microphone" | "input-monitoring",
    ) => void | Promise<void>;
  };

  let { show, onDismiss, onOpenPrivacyPane }: Props = $props();

  // Step order is permissions → welcome (#609). The previous order
  // was welcome → permissions, but the post-welcome flow then opened
  // a third PermissionsDialog modal redundantly. Reversing puts the
  // mandatory grant ask first (Hush is unusable without Microphone)
  // and the explainer second (now the user understands what they
  // just enabled), and lets `dismissFirstRun` skip the auto-open
  // of PermissionsDialog entirely.
  type Step = "permissions" | "welcome";
  let step = $state<Step>("permissions");

  let cardEl: HTMLElement | undefined = $state();
  let previousFocus: HTMLElement | null = null;

  // Live permission state. `null` while the first poll is in
  // flight or the call failed (which `diagnose_macos_permissions`
  // signals via NotApplicable on non-macOS, so a real `null` is
  // genuinely "haven't checked yet" rather than "denied").
  let diagnostic = $state<MacosPermissionDiagnostic | null>(null);
  let micReady = $derived(diagnostic?.statuses.microphone === "granted");
  // Whether IM was granted when the permissions step first polled in
  // this session. `null` until the first poll completes. Used to
  // detect a within-session grant that requires a restart.
  let imGrantedAtOpen = $state<boolean | null>(null);
  // Optimistic signal: user clicked the IM grant button this session.
  let imGrantAttempted = $state(false);
  let imNeedsRestart = $derived(
    imGrantedAtOpen === false &&
      (diagnostic?.statuses.inputMonitoring === "granted" ||
        diagnostic?.statuses.inputMonitoring === "not-applicable" ||
        imGrantAttempted),
  );
  let pollHandle: ReturnType<typeof setInterval> | null = null;

  // Per-row "Allow click in flight" guards so a user mashing the
  // button doesn't fire two OS prompts back-to-back. Cleared once
  // the resulting status is observed via the next poll tick.
  let micRequesting = $state(false);
  let imRequesting = $state(false);

  const FOCUSABLE_SELECTOR =
    'button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

  async function pollDiagnostic() {
    try {
      const next = await invoke<MacosPermissionDiagnostic>(
        "diagnose_macos_permissions",
      );
      // Record IM status at the very first poll so we can detect
      // a within-session grant (which requires a restart before
      // rdev's CGEventTap can pick it up).
      if (imGrantedAtOpen === null) {
        imGrantedAtOpen =
          next.statuses.inputMonitoring === "granted" ||
          next.statuses.inputMonitoring === "not-applicable";
      }
      diagnostic = next;
    } catch (e) {
      // Non-fatal — show whatever we last had. The next poll will
      // try again. On non-macOS the IPC returns NotApplicable for
      // every permission, which renders as already-granted-style
      // ✓ states (Linux + Windows don't have per-app TCC for
      // these, so "not applicable" reads as "doesn't apply, you're
      // fine").
      console.warn("[hush] diagnose_macos_permissions failed", e);
    }
  }

  function statusFor(
    key: "microphone" | "screenRecording" | "inputMonitoring",
  ): PermissionStatus | null {
    return diagnostic?.statuses[key] ?? null;
  }

  function isGranted(
    key: "microphone" | "screenRecording" | "inputMonitoring",
  ): boolean {
    const s = statusFor(key);
    // `not-applicable` (Linux / Windows) reads as granted in the
    // wizard so the row shows the ✓ and the user moves on. The
    // permissions don't apply on those platforms; nothing to grant.
    return s === "granted" || s === "not-applicable";
  }

  async function requestMicrophone() {
    if (micRequesting) return;
    micRequesting = true;
    try {
      await invoke("request_microphone_permission");
    } catch (e) {
      console.warn("[hush] request_microphone_permission failed", e);
    }
    // The OS dialog is async — the user's response surfaces via
    // the next poll tick; release the in-flight guard after a
    // short window so a mistaken second-click doesn't re-fire.
    setTimeout(() => {
      micRequesting = false;
      void pollDiagnostic();
    }, 400);
  }

  async function requestInputMonitoring() {
    if (imRequesting) return;
    imRequesting = true;
    imGrantAttempted = true;
    try {
      // Synchronous prompt: the IPC awaits the user's choice and
      // returns the resulting bool. We don't actually need the
      // return value — the diagnostic poll picks up the new
      // state — but awaiting the IPC means the UI's spinner /
      // disabled state stays visible during the prompt rather
      // than flickering off after a tick.
      await invoke<boolean>("request_input_monitoring_permission");
    } catch (e) {
      console.warn("[hush] request_input_monitoring_permission failed", e);
    }
    imRequesting = false;
    void pollDiagnostic();
  }

  function handleKeydown(event: KeyboardEvent) {
    if (!show) return;
    if (event.key === "Escape") {
      event.preventDefault();
      void dismiss();
      return;
    }
    if (event.key !== "Tab" || !cardEl) return;
    const focusable = cardEl.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR);
    if (focusable.length === 0) return;
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    const active = document.activeElement;
    if (event.shiftKey && active === first) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && active === last) {
      event.preventDefault();
      first.focus();
    }
  }

  async function dismiss() {
    previousFocus?.focus();
    previousFocus = null;
    await onDismiss();
  }

  // Restart polling whenever the modal opens or the step
  // changes. Welcome step doesn't need the poll (no permission
  // rows on that step), so only run the interval on the
  // permissions step. Stop the interval on close to avoid
  // leaking a tick that fires after dismiss.
  $effect(() => {
    // Read `step` so this effect re-runs on step transitions
    // (Svelte tracks reads inside the effect's scope). Without this
    // the autofocus only fires on initial open — clicking Continue
    // to advance Permissions → Welcome leaves focus on the now-
    // removed button, which keyboard-only users have to Tab back
    // out of. Caught by the post-merge UX review of #613.
    void step;
    if (show && cardEl) {
      previousFocus =
        document.activeElement instanceof HTMLElement
          ? document.activeElement
          : null;
      // Auto-focus the first focusable element on open / each
      // step transition.
      const first = cardEl.querySelector<HTMLElement>(FOCUSABLE_SELECTOR);
      first?.focus();
    }
  });

  $effect(() => {
    // Tear down any prior interval on every reactivity tick — we
    // recreate it below if the conditions still warrant polling.
    if (pollHandle !== null) {
      clearInterval(pollHandle);
      pollHandle = null;
    }
    if (show && step === "permissions") {
      void pollDiagnostic();
      pollHandle = setInterval(() => void pollDiagnostic(), 1500);
    }
  });

  onDestroy(() => {
    if (pollHandle !== null) {
      clearInterval(pollHandle);
      pollHandle = null;
    }
  });
</script>

<svelte:window onkeydown={handleKeydown} />

{#if show}
  <div
    class="first-run-backdrop"
    role="dialog"
    aria-modal="true"
    aria-labelledby="first-run-heading"
  >
    <article class="first-run-card" bind:this={cardEl} tabindex="-1">
      <!-- Step indicator. Two dots; the active one fills, the
           inactive one stays outlined. Read aloud as "Step 1 of 2"
           via aria-label so screen readers get the position too. -->
      <!--
        Step indicator. Permissions is step 1, welcome is step 2 (#609).
        The dots are rendered in display order — dot 1 highlights on
        permissions, dot 2 on welcome — so the visual progression
        matches the new flow.
      -->
      <div
        class="wizard-steps"
        role="progressbar"
        aria-label={`Step ${step === "permissions" ? 1 : 2} of 2`}
        aria-valuemin="1"
        aria-valuemax="2"
        aria-valuenow={step === "permissions" ? 1 : 2}
      >
        <span class="wizard-step-dot" class:active={step === "permissions"}></span>
        <span class="wizard-step-dot" class:active={step === "welcome"}></span>
      </div>

      {#if step === "welcome"}
        <header>
          <h2 id="first-run-heading">Welcome to Hush</h2>
          <p class="first-run-tagline">
            Local, private voice-to-text. Audio stays on your machine
            end-to-end.
          </p>
          <AudioPipelineDiagram />
        </header>

        <p class="welcome-body">
          Hush captures your microphone for dictation and (on macOS)
          your call's system audio for meeting transcription. Both
          stay on your device — no upload, no account, no telemetry.
          {#if micReady}
            You're all set — press your hotkey to start dictating.
          {:else}
            Head to Settings → Permissions any time to grant Microphone
            access, or try the hotkey and Hush will prompt you.
          {/if}
        </p>

        <footer class="first-run-footer">
          <p class="first-run-meta">
            Hush makes no other network requests except when you click
            Download on a model card. After downloading a speech
            model, the companion speaker-labelling model (~26 MB)
            also downloads automatically.
          </p>
          <div class="footer-actions">
            <button
              class="ghost"
              onclick={() => (step = "permissions")}
            >
              Back
            </button>
            <button
              class="primary"
              data-testid="wizard-finish"
              onclick={dismiss}
            >
              Start using Hush
            </button>
          </div>
        </footer>
      {:else}
        <header>
          <h2 id="first-run-heading">Permissions</h2>
          <p class="first-run-tagline">
            Hush needs a couple of OS permissions to work. You can
            skip any of them — Hush stays usable, just with the
            matching feature disabled.
          </p>
        </header>

        <ul class="wizard-perm-list" aria-label="Permissions">
          <li
            class="wizard-perm-row"
            class:granted={isGranted("microphone")}
            data-testid="wizard-perm-microphone"
          >
            <div class="wizard-perm-icon" aria-hidden="true">🎙</div>
            <div class="wizard-perm-text">
              <span class="wizard-perm-title">Microphone</span>
              <span class="wizard-perm-why">
                Required to record your voice for dictation and
                meetings.
              </span>
            </div>
            {#if isGranted("microphone")}
              <span class="wizard-perm-badge" aria-label="Granted">✓</span>
            {:else}
              <button
                class="primary wizard-allow-btn"
                disabled={micRequesting}
                data-testid="wizard-allow-microphone"
                onclick={requestMicrophone}
              >
                {micRequesting ? "Asking…" : "Allow"}
              </button>
            {/if}
          </li>

          <li
            class="wizard-perm-row"
            class:granted={isGranted("inputMonitoring") && !imNeedsRestart}
            class:dimmed={!isGranted("microphone")}
            data-testid="wizard-perm-input-monitoring"
          >
            <div class="wizard-perm-icon" aria-hidden="true">⌨️</div>
            <div class="wizard-perm-text">
              <span class="wizard-perm-title">Input Monitoring</span>
              <span class="wizard-perm-why">
                Required for push-to-talk to detect the hotkey while
                you're in another app.
              </span>
            </div>
            {#if isGranted("inputMonitoring")}
              <span class="wizard-perm-badge" aria-label="Granted">✓</span>
            {:else}
              <button
                class="primary wizard-allow-btn"
                disabled={imRequesting || !isGranted("microphone")}
                data-testid="wizard-allow-input-monitoring"
                title={!isGranted("microphone")
                  ? "Grant Microphone first"
                  : undefined}
                aria-describedby={!isGranted("microphone")
                  ? "wizard-allow-im-requirement"
                  : undefined}
                onclick={requestInputMonitoring}
              >
                {imRequesting ? "Asking…" : "Allow"}
              </button>
              {#if !isGranted("microphone")}
                <!-- Visually-hidden requirement copy for screen
                     readers. The `title` attr alone is unreliable
                     under VoiceOver; aria-describedby with a real
                     element makes the disabled-button reason
                     announce-able. #617. -->
                <span id="wizard-allow-im-requirement" class="visually-hidden">
                  Allow is disabled until Microphone permission is granted.
                </span>
              {/if}
            {/if}
            {#if imNeedsRestart}
              <!-- rdev's CGEventTap is established at startup; it
                   needs a full relaunch to see the new IM grant. -->
              <span class="wizard-perm-restart-hint" role="status">
                Push-to-talk is ready — restart Hush to activate it.
                <button
                  type="button"
                  class="wizard-restart-btn"
                  onclick={() => void invoke("relaunch_app")}
                >Restart Now</button>
              </span>
            {/if}
          </li>

        </ul>

        <footer class="first-run-footer">
          <p class="first-run-meta">
            {#if !isGranted("microphone")}
              <strong>Heads up:</strong> dictation needs Microphone
              access — without it, the Record button stays disabled.
              You can grant it now or later from Settings →
              Permissions.
            {:else}
              No telemetry, no cloud transcription, no analytics.
              Settings → Permissions has detailed status if you ever
              need to revisit.
            {/if}
          </p>
          <div class="footer-actions">
            <button class="ghost" onclick={dismiss}>Skip setup</button>
            <button
              class="primary"
              data-testid="wizard-continue-permissions"
              onclick={() => (step = "welcome")}
            >
              Continue
            </button>
          </div>
        </footer>
      {/if}
    </article>
  </div>
{/if}

<style>
.first-run-backdrop {
  position: fixed;
  inset: 0;
  background-color: rgba(15, 15, 15, 0.55);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 100;
  padding: 1.5rem;
}

.first-run-card {
  background-color: var(--bg-surface);
  color: var(--text-primary);
  border-radius: 12px;
  padding: 1.5rem 1.75rem;
  max-width: 32rem;
  width: 100%;
  max-height: calc(100vh - 3rem);
  overflow-y: auto;
  box-shadow: 0 8px 32px rgba(0, 0, 0, 0.18);
  text-align: left;
}

.first-run-card h2 {
  margin: 0 0 0.35rem;
  font-size: 1.5rem;
  letter-spacing: -0.01em;
}

.first-run-tagline {
  margin: 0 0 1rem;
  color: #555;
  font-size: 0.95rem;
}

/* Step indicator. Two dots; the active one fills with the
   accent colour, the inactive one stays outlined. Quiet visual
   weight so the focus stays on the step content below. */
.wizard-steps {
  display: flex;
  gap: 0.5rem;
  justify-content: center;
  margin: 0 0 1rem;
}
.wizard-step-dot {
  width: 0.55rem;
  height: 0.55rem;
  border-radius: 50%;
  background-color: transparent;
  border: 1.5px solid var(--accent, #7c6ff7);
  transition: background-color 0.15s;
}
.wizard-step-dot.active {
  background-color: var(--accent, #7c6ff7);
}

.welcome-body {
  margin: 0 0 1rem;
  color: #444;
  font-size: 0.92rem;
  line-height: 1.5;
}

/* Compact permission rows (#511). Three columns: icon, text
   block, action. Granted rows dim slightly + show a ✓ badge
   instead of the Allow button. The dimmed class on Input
   Monitoring drives the "ungrantable until Mic is granted"
   sequencing — the row is still visible, the button is just
   disabled. */
.wizard-perm-list {
  list-style: none;
  margin: 0 0 1rem;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
}
.wizard-perm-row {
  display: grid;
  grid-template-columns: 2.5rem 1fr auto;
  align-items: center;
  gap: 0.85rem;
  padding: 0.7rem 0.9rem;
  background-color: #f7f7f8;
  border: 1px solid #e1e1e6;
  border-radius: 10px;
  transition: opacity 0.15s, background-color 0.15s;
}
.wizard-perm-row.granted {
  opacity: 0.7;
  background-color: #f0f7f1;
  border-color: #cfe5d3;
}
.wizard-perm-row.dimmed {
  opacity: 0.55;
}
.wizard-perm-icon {
  font-size: 1.4rem;
  text-align: center;
  user-select: none;
}
.wizard-perm-text {
  display: flex;
  flex-direction: column;
  gap: 0.15rem;
  min-width: 0;
}
.wizard-perm-title {
  font-size: 0.95rem;
  font-weight: 600;
  color: #1a1a1a;
}
.wizard-perm-why {
  font-size: 0.82rem;
  color: #5a5a5a;
  line-height: 1.4;
}
.wizard-perm-badge {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 1.75rem;
  height: 1.75rem;
  border-radius: 50%;
  background-color: #2a6b3c;
  color: white;
  font-size: 0.95rem;
  font-weight: 600;
}
.wizard-allow-btn {
  white-space: nowrap;
}
/* Restart notice shown after a within-session IM grant.
   Spans all three grid columns so the icon + text columns
   aren't crammed. */
.wizard-perm-restart-hint {
  grid-column: 1 / -1;
  display: flex;
  align-items: center;
  gap: 0.55rem;
  flex-wrap: wrap;
  margin-top: 0.25rem;
  font-size: 0.78rem;
  color: #5a3e00;
  background-color: #fffae8;
  border-left: 3px solid #e0a020;
  padding: 0.4rem 0.6rem;
  border-radius: 4px;
}
.wizard-restart-btn {
  font-size: 0.75rem;
  font-weight: 600;
  padding: 0.2rem 0.55rem;
  border-radius: 5px;
  background-color: #c88a10;
  color: white;
  border: none;
  cursor: pointer;
  white-space: nowrap;
  line-height: 1.4;
}
.wizard-restart-btn:hover {
  background-color: #a87010;
}

.first-run-footer {
  margin-top: 0.75rem;
  display: flex;
  align-items: flex-end;
  gap: 1rem;
  justify-content: space-between;
  flex-wrap: wrap;
}
.first-run-meta {
  flex: 1;
  margin: 0;
  font-size: 0.8rem;
  color: #6a6a6a;
  line-height: 1.45;
}
.footer-actions {
  display: flex;
  gap: 0.5rem;
}

button {
  border-radius: 8px;
  border: 1px solid #d1d1d1;
  padding: 0.6em 1.1em;
  font-size: 0.9rem;
  font-family: inherit;
  color: #0f0f0f;
  background-color: #ffffff;
  cursor: pointer;
  font-weight: 600;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 0.5rem;
  transition: border-color 0.15s, background-color 0.15s, opacity 0.15s;
}
button:disabled {
  cursor: not-allowed;
  opacity: 0.55;
}
button:hover:not(:disabled) {
  border-color: var(--accent-hover);
}
button.ghost {
  padding: 0.45em 0.85em;
  font-size: 0.85rem;
  font-weight: 500;
  background-color: transparent;
  border: 1px solid #d1d1d1;
}
button.ghost:hover:not(:disabled) {
  background-color: #f0f0f0;
}
button.primary {
  background-color: var(--accent);
  color: white;
  border-color: var(--accent);
}
button.primary:hover:not(:disabled) {
  background-color: var(--accent-hover, #5c4fd4);
  border-color: var(--accent-hover, #5c4fd4);
}

/* Standard a11y idiom for screen-reader-only text. Used for the
 * disabled-Allow requirement copy on the Input Monitoring row
 * (#617). */
.visually-hidden {
  position: absolute;
  width: 1px;
  height: 1px;
  padding: 0;
  margin: -1px;
  overflow: hidden;
  clip: rect(0, 0, 0, 0);
  white-space: nowrap;
  border: 0;
}

/* Dark mode: mirror the two-context pattern used elsewhere in the
 * app (see `+page.svelte` banners) so the user's app-level theme
 * override (light / dark / auto) wins over the OS preference.
 *
 * - The `@media (prefers-color-scheme: dark)` block fires when the
 *   OS is dark AND the user hasn't forced light.
 * - The `:root[data-theme="dark"]` block fires when the user has
 *   forced dark regardless of OS.
 *
 * Rules duplicated rather than composed because one selector needs
 * a media query and the other doesn't — same trade-off as the
 * existing banner styles.  #617. */
@media (prefers-color-scheme: dark) {
  :root:not([data-theme="light"]) .first-run-backdrop {
    background-color: rgba(0, 0, 0, 0.65);
  }
  :root:not([data-theme="light"]) .first-run-card {
    background-color: #1f1f1f;
    color: #f0f0f0;
    box-shadow: 0 8px 32px rgba(0, 0, 0, 0.5);
  }
  :root:not([data-theme="light"]) .first-run-tagline,
  :root:not([data-theme="light"]) .welcome-body,
  :root:not([data-theme="light"]) .first-run-meta {
    color: #c0c0c0;
  }
  :root:not([data-theme="light"]) .wizard-perm-row {
    background-color: #2a2a2a;
    border-color: #3a3a3a;
  }
  :root:not([data-theme="light"]) .wizard-perm-row.granted {
    background-color: #1f3a25;
    border-color: #2c4a35;
  }
  :root:not([data-theme="light"]) .wizard-perm-title {
    color: #f0f0f0;
  }
  :root:not([data-theme="light"]) .wizard-perm-why {
    color: #b0b0b0;
  }
  :root:not([data-theme="light"]) button {
    color: #f0f0f0;
    background-color: #2a2a2a;
    border-color: #3a3a3a;
  }
  :root:not([data-theme="light"]) button.ghost {
    background-color: transparent;
  }
  :root:not([data-theme="light"]) button.ghost:hover:not(:disabled) {
    background-color: #353535;
  }
}

:root[data-theme="dark"] .first-run-backdrop {
  background-color: rgba(0, 0, 0, 0.65);
}
:root[data-theme="dark"] .first-run-card {
  background-color: #1f1f1f;
  color: #f0f0f0;
  box-shadow: 0 8px 32px rgba(0, 0, 0, 0.5);
}
:root[data-theme="dark"] .first-run-tagline,
:root[data-theme="dark"] .welcome-body,
:root[data-theme="dark"] .first-run-meta {
  color: #c0c0c0;
}
:root[data-theme="dark"] .wizard-perm-row {
  background-color: #2a2a2a;
  border-color: #3a3a3a;
}
:root[data-theme="dark"] .wizard-perm-row.granted {
  background-color: #1f3a25;
  border-color: #2c4a35;
}
:root[data-theme="dark"] .wizard-perm-title {
  color: #f0f0f0;
}
:root[data-theme="dark"] .wizard-perm-why {
  color: #b0b0b0;
}
:root[data-theme="dark"] button {
  color: #f0f0f0;
  background-color: #2a2a2a;
  border-color: #3a3a3a;
}
:root[data-theme="dark"] button.ghost {
  background-color: transparent;
}
:root[data-theme="dark"] button.ghost:hover:not(:disabled) {
  background-color: #353535;
}
</style>
