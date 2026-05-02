<!--
  First-run welcome modal. Static content (no fetches behind it),
  shown once per install on the very first launch and dismissed via
  the Got It button or Escape. The two permission sections deep-
  link into System Settings on macOS via `open_macos_privacy_pane`;
  on Linux / Windows that command is a no-op and the user can
  still proceed cleanly.

  A11y plumbing (closes #48):
  - Backdrop carries `role="dialog"` + `aria-modal="true"` so
    assistive tech treats it as a modal.
  - Escape dismisses (window-level keydown, gated on `show`).
  - Tab cycles within the modal — focus cannot escape to the page
    behind the backdrop.
  - Auto-focus lands on the first action button on open; on
    dismiss focus restores to whatever was focused before.

  Extracted from `+page.svelte` (#156 follow-up) so the welcome
  modal owns its own focus trap, keydown handler, and styles
  rather than living as ~120 LOC of inline markup + script in the
  parent page.
-->
<script lang="ts">
  import AudioPipelineDiagram from "./AudioPipelineDiagram.svelte";

  type Props = {
    show: boolean;
    onDismiss: () => void | Promise<void>;
    onOpenPrivacyPane: (
      target: "microphone" | "input-monitoring" | "screen-recording",
    ) => void | Promise<void>;
  };

  let { show, onDismiss, onOpenPrivacyPane }: Props = $props();

  // Modal element ref + the focused-element-before-modal stash. The
  // ref backs the focus trap (so Tab cycles within the modal instead
  // of escaping to the rest of the page); the stash lets us restore
  // focus to whatever the user was on before the welcome appeared
  // when they dismiss it.
  let cardEl: HTMLElement | undefined = $state();
  let previousFocus: HTMLElement | null = null;

  // Selector for the focusable elements we cycle between in the
  // modal. Excludes elements with `tabindex="-1"` so the dialog
  // wrapper itself (which is not focusable by users) does not enter
  // the rotation.
  const FOCUSABLE_SELECTOR =
    'button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

  // Trap Tab cycling inside the modal (closes #48 focus trap). Tab
  // from the last focusable wraps to the first; Shift+Tab from the
  // first wraps to the last. Escape dismisses (per WAI-ARIA guidance
  // for `role="dialog"` `aria-modal="true"`).
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
    // Restore focus to whatever the user was on before the modal
    // opened. Defensive: the previously-focused element may have
    // been removed from the DOM, in which case `.focus()` is a no-op
    // and the browser falls back to body, which is fine.
    previousFocus?.focus();
    previousFocus = null;
    await onDismiss();
  }

  // Auto-focus the first focusable element when the modal opens, and
  // remember what was focused before so we can restore it on
  // dismiss. Runs whenever `show` flips — including back to false —
  // but only acts on the open transition.
  $effect(() => {
    if (show && cardEl) {
      previousFocus =
        document.activeElement instanceof HTMLElement ? document.activeElement : null;
      // Focus the first action button so a keyboard-only user lands
      // on something useful (the "Open Microphone settings" button)
      // rather than the dialog wrapper.
      const first = cardEl.querySelector<HTMLElement>(FOCUSABLE_SELECTOR);
      first?.focus();
    }
  });
</script>

<!--
  Window-level keydown so Escape works regardless of whether the
  active element is inside the modal — the listener is gated on
  `show`, so it is a no-op when the modal isn't visible.
-->
<svelte:window onkeydown={handleKeydown} />

{#if show}
  <div class="first-run-backdrop" role="dialog" aria-modal="true" aria-labelledby="first-run-heading">
    <article class="first-run-card" bind:this={cardEl} tabindex="-1">
      <header>
        <h2 id="first-run-heading">Welcome to Hush</h2>
        <p class="first-run-tagline">
          Local, private voice-to-text. Here's what to know about
          permissions and privacy before you start.
        </p>
        <!--
          Audio pipeline diagram (#427 Item 3). Sits as a visual
          lead-in so a user immediately sees the chain — mic /
          system audio → Whisper → transcript — before reading
          the permissions sections below. The caption ("Audio
          stays on your device end-to-end") seeds the privacy
          framing the modal's footer reinforces.
        -->
        <AudioPipelineDiagram />
      </header>

      <section class="first-run-section">
        <h3>Microphone</h3>
        <p>
          Hush records audio only while you've explicitly started a
          dictation session. The first time you record, your OS will
          ask you to grant Hush microphone access. Without it, the
          dictation pipeline can't capture what you say.
        </p>
        <button class="ghost" onclick={() => onOpenPrivacyPane("microphone")}>
          Open Microphone settings
        </button>
      </section>

      <section class="first-run-section">
        <h3>Input Monitoring (macOS — push-to-talk)</h3>
        <p>
          Push-to-talk (hold <kbd>Right ⌘</kbd> while you speak) is
          <strong>on by default</strong>; macOS will prompt for
          Input Monitoring the first time the listener spawns. If
          you'd rather not, disable it in Settings → General →
          Hotkeys. The toggle hotkey
          (<kbd>Ctrl</kbd> + <kbd>⌥/Alt</kbd> + <kbd>H</kbd>) and the
          on-screen Start button work either way.
        </p>
        <button class="ghost" onclick={() => onOpenPrivacyPane("input-monitoring")}>
          Open Input Monitoring settings
        </button>
      </section>

      <!--
        Screen Recording — required for Meeting Mode (#269). Without
        this section, users hit an unexpected TCC prompt the first
        time they try Meeting Mode and many reflexively dismiss it,
        silently breaking system-audio capture with no clear error.
        The copy explicitly addresses the counterintuitive name —
        macOS bundles system-audio capture under "Screen Recording"
        even though Hush captures no pixels.
      -->
      <section class="first-run-section">
        <h3>Screen Recording (macOS — system audio for Meeting Mode)</h3>
        <p>
          Meeting Mode records the other side of a Zoom / Teams /
          Meet call alongside your microphone. macOS bundles
          system-audio capture under the <em>Screen Recording</em>
          permission category — despite the name, Hush
          <strong>never captures pixels</strong>; only audio. The
          prompt fires the first time you start a Meeting Mode
          session with system audio enabled, or when you click
          <em>Grant in Settings…</em> on the Permissions tab.
          Microphone-only Meeting Mode sessions (and dictation)
          don't need this — only the system-audio capture path
          does.
        </p>
        <button class="ghost" onclick={() => onOpenPrivacyPane("screen-recording")}>
          Open Screen Recording settings
        </button>
      </section>

      <footer class="first-run-footer">
        <p class="first-run-meta">
          Hush makes no other network requests except when you click
          Download on a model card. No telemetry, no cloud transcription,
          no analytics.
        </p>
        <button class="primary" onclick={dismiss}>Got it</button>
      </footer>
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
  background-color: #ffffff;
  border-radius: 12px;
  padding: 1.5rem 1.75rem;
  max-width: 30rem;
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
  margin: 0 0 1.25rem;
  color: #555;
  font-size: 0.95rem;
}

.first-run-section {
  margin-bottom: 1.25rem;
  padding-bottom: 1.25rem;
  /* Walkthrough round flagged the previous `#eee` divider as
     barely visible on the white card — easy to miss the section
     break. Slightly stronger grey reads as a deliberate boundary
     without turning into a hard rule. */
  border-bottom: 1px solid #d8d8d8;
}

.first-run-section:last-of-type {
  border-bottom: none;
}

.first-run-section h3 {
  margin: 0 0 0.35rem;
  font-size: 1rem;
  font-weight: 600;
}

.first-run-section p {
  margin: 0 0 0.6rem;
  font-size: 0.9rem;
  color: #444;
  line-height: 1.5;
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

/* Mirrors the parent page's base button + .ghost / .primary
   variants. Svelte's scoped styles don't inherit the page-level
   rules into this component, so we duplicate the visible
   attributes here. Keep in sync with `+page.svelte`. */
button {
  border-radius: 8px;
  border: 1px solid #d1d1d1;
  padding: 0.7em 1.2em;
  font-size: 1em;
  font-family: inherit;
  color: #0f0f0f;
  background-color: #ffffff;
  cursor: pointer;
  font-weight: 600;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 0.5rem;
  transition: border-color 0.15s, background-color 0.15s;
}

button:hover:not(:disabled) {
  border-color: var(--accent-hover);
}

button.ghost {
  padding: 0.3em 0.75em;
  font-size: 0.8rem;
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
  background-color: #4a6cd0;
  border-color: #4a6cd0;
}

@media (prefers-color-scheme: dark) {
  .first-run-backdrop {
    background-color: rgba(0, 0, 0, 0.65);
  }
  .first-run-card {
    background-color: #1f1f1f;
    color: #f0f0f0;
    box-shadow: 0 8px 32px rgba(0, 0, 0, 0.5);
  }
  .first-run-tagline,
  .first-run-section p,
  .first-run-meta {
    color: #c0c0c0;
  }
  .first-run-section {
    border-bottom-color: #2e2e2e;
  }
  button {
    color: #f0f0f0;
    background-color: #2a2a2a;
    border-color: #3a3a3a;
  }
  button.ghost {
    background-color: transparent;
  }
  button.ghost:hover:not(:disabled) {
    background-color: #353535;
  }
}
</style>
