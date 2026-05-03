<!--
  Menu-bar quick-access popover (#427 Item 1).

  A compact always-on-top window summoned from the tray
  ("Show Quick popover" menu item). Surfaces the primary
  affordance — start / stop dictation — without forcing the
  user to bring the main window forward. Inspired by Panic's
  hotkey-driven apps and Rogue Amoeba's per-app menu-bar
  posture: the killer action is one click away from anywhere.

  ## What's here
  - State indicator dot (idle / recording).
  - Start / Stop button that emits `hotkey:toggle` — the same
    event the tray's "Start/Stop Recording" menu item uses.
    The main window's listener handles the actual start/stop
    so its `recording` rune (and the `ui:recording-state`
    broadcast feeding the dot) stays the single source of
    truth. Direct `start_dictation` / `stop_dictation` invokes
    from the popover would create a second start path that
    bypasses the main window's state machine — visible as the
    "HUD says Recording but main window doesn't" desync seen
    in the first-cut smoke test.
  - "Open Hush" link calling the `show_main_window` IPC,
    which surfaces the main window from Rust without needing
    the broader `core:window:allow-get-all-windows` permission
    on this capability.

  ## What's NOT here (deliberate)
  - Source picker. Most popover invocations are quick "I want
    to dictate now" moments; the user can configure a
    preferred source from the main window's `ControlsSection`
    or via Settings. Adding a source select would crowd the
    320×220 popover.
  - Recent transcript snippet. The original spec called for
    "last 80 chars of most-recent transcript"; left as a
    follow-up so this PR can ship the structural surface
    without also threading a new event/query for the
    transcript text. Tracked alongside the rest of the #427
    Item 1 polish.
  - Tray-anchored positioning. The popover currently shows in
    its default OS-picked location; anchoring near the tray
    icon (`tauri-plugin-positioner`-style) is a follow-up
    once the structure has been validated by hands-on macOS
    testing.

  ## Lifecycle
  Mounted whenever the menu-bar window is shown (which is
  controlled from `tray/mod.rs::handle_menu_event` →
  `tray:popover`). The window is created with `visible: false`
  in `tauri.conf.json` so it stays out of the way until the
  user invokes it. Listeners (`ui:recording-state`) tear down
  cleanly on `onDestroy` so a hide → show cycle doesn't leak
  handlers (per the #330 fix shape that the HUD uses).
-->
<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { emit, listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
  import { onDestroy, onMount } from "svelte";
  import { Events } from "$lib/events";

  // Mirrors the `recording` rune in the main window. Set by the
  // `ui:recording-state` Tauri event — same event the tray's
  // toggle-label listener reads. Defaults to false: a fresh
  // popover assumes idle until a state event lands.
  let recording = $state(false);

  // Disables the start/stop button during in-flight start /
  // stop IPCs so a stale double-click can't race against
  // itself. Cleared after the IPC resolves regardless of
  // outcome.
  let busy = $state(false);

  // Last invoke error surfaced inline so the user sees something
  // happened (and can retry from the main window if needed).
  // Cleared on the next interaction.
  let error = $state<string | null>(null);

  let unlistenRecording: UnlistenFn | null = null;

  onMount(async () => {
    unlistenRecording = await listen<boolean>(
      Events.UiRecordingState,
      (event) => {
        const next = event.payload;
        if (typeof next === "boolean") {
          recording = next;
        }
      },
    );
  });

  onDestroy(() => {
    unlistenRecording?.();
    unlistenRecording = null;
  });

  async function toggleRecording() {
    if (busy) return;
    busy = true;
    error = null;
    try {
      // Emit the same event the tray's "Start/Stop Recording"
      // menu item uses (`hotkey:toggle`). The main window's
      // listener owns the start/stop state machine + the
      // `ui:recording-state` broadcast that drives this
      // popover's indicator. Going through that single path
      // avoids the desync seen in the first-cut smoke test
      // where direct `start_dictation` from the popover left
      // the main window's `recording` rune at false while the
      // backend pump was already running.
      await emit(Events.HotkeyToggle);
    } catch (e) {
      error = formatError(e);
    } finally {
      busy = false;
    }
  }

  async function openMain() {
    error = null;
    try {
      // `show_main_window` is the Rust-side equivalent of
      // `WebviewWindow.getByLabel("main").show()` — same
      // best-effort show + unminimize + focus chain, but
      // doesn't require the popover's capability to grant
      // `core:window:allow-get-all-windows` (a broader JS
      // permission than this capability needs).
      await invoke("show_main_window");
      await getCurrentWebviewWindow().hide();
    } catch (e) {
      error = formatError(e);
    }
  }

  async function dismiss() {
    try {
      await getCurrentWebviewWindow().hide();
    } catch {
      // Hide failure is non-fatal — the window simply stays open.
    }
  }

  function formatError(e: unknown): string {
    if (e instanceof Error) return e.message;
    if (typeof e === "string") return e;
    if (typeof e === "object" && e !== null && "message" in e) {
      const m = (e as { message: unknown }).message;
      if (typeof m === "string") return m;
    }
    return "Something went wrong. Open the main window to retry.";
  }
</script>

<svelte:window
  onkeydown={(e) => {
    if (e.key === "Escape") void dismiss();
  }}
/>

<div
  class="popover-root"
  role="dialog"
  aria-label="Hush quick controls"
  data-tauri-drag-region
  data-testid="menu-bar-root"
>
  <header class="popover-header" data-tauri-drag-region>
    <span
      class="state-dot"
      class:recording
      aria-hidden="true"
    ></span>
    <span class="state-label" aria-live="polite">
      {recording ? "Recording" : "Ready"}
    </span>
  </header>

  <div class="popover-body">
    <button
      type="button"
      class="primary-action"
      data-testid="popover-toggle"
      disabled={busy}
      onclick={toggleRecording}
    >
      {#if busy}
        Working…
      {:else if recording}
        Stop dictation
      {:else}
        Start dictation
      {/if}
    </button>

    <p class="hint">
      {#if recording}
        Click stop, or use your hotkey, when you're done.
      {:else}
        Default microphone. Pick a different source from the
        main window.
      {/if}
    </p>

    {#if error}
      <p class="error-line" role="alert" data-testid="popover-error">
        {error}
      </p>
    {/if}
  </div>

  <footer class="popover-footer" data-tauri-drag-region>
    <button
      type="button"
      class="secondary-action"
      data-testid="popover-open-main"
      onclick={openMain}
    >
      Open Hush
    </button>
    <span class="hint-shortcut" aria-hidden="true">Esc to dismiss</span>
  </footer>
</div>

<style>
  /* Reset body so the transparent window genuinely shows the
     rounded card behind. Mirrors the HUD's body reset. */
  :global(html), :global(body) {
    margin: 0;
    padding: 0;
    background-color: transparent;
    overflow: hidden;
    color: white;
    font-family:
      -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Oxygen,
      Ubuntu, Cantarell, "Helvetica Neue", Arial, sans-serif;
    -webkit-font-smoothing: antialiased;
  }

  .popover-root {
    display: flex;
    flex-direction: column;
    width: 100vw;
    height: 100vh;
    background-color: rgba(28, 28, 32, 0.96);
    border-radius: 12px;
    border: 1px solid rgba(255, 255, 255, 0.08);
    color: rgba(255, 255, 255, 0.92);
    box-shadow: 0 10px 30px rgba(0, 0, 0, 0.4);
    overflow: hidden;
    user-select: none;
  }

  .popover-header {
    display: flex;
    align-items: center;
    gap: 0.55rem;
    padding: 0.7rem 1rem 0.45rem;
    cursor: grab;
  }
  .popover-header:active { cursor: grabbing; }

  .state-dot {
    width: 0.65rem;
    height: 0.65rem;
    border-radius: 50%;
    background-color: rgba(255, 255, 255, 0.35);
    flex-shrink: 0;
  }
  .state-dot.recording {
    background-color: #ff4040;
    box-shadow: 0 0 6px rgba(255, 64, 64, 0.65);
    animation: dot-pulse 1.2s ease-in-out infinite;
  }
  @keyframes dot-pulse {
    0%, 100% { opacity: 1; transform: scale(1); }
    50% { opacity: 0.55; transform: scale(0.85); }
  }
  @media (prefers-reduced-motion: reduce) {
    .state-dot.recording { animation: none; }
  }

  .state-label {
    font-size: 0.85rem;
    font-weight: 600;
    letter-spacing: 0.01em;
  }

  .popover-body {
    display: flex;
    flex-direction: column;
    align-items: stretch;
    gap: 0.5rem;
    padding: 0.5rem 1rem 0.85rem;
    flex: 1;
  }

  .primary-action {
    appearance: none;
    border: none;
    background-color: #6a8cf0;
    color: white;
    font-family: inherit;
    font-size: 0.95rem;
    font-weight: 600;
    padding: 0.6rem 1rem;
    border-radius: 8px;
    cursor: pointer;
    transition: background-color 0.12s, transform 0.1s;
  }
  .primary-action:hover:not(:disabled) {
    background-color: #5a7be0;
  }
  .primary-action:active:not(:disabled) {
    transform: translateY(1px);
  }
  .primary-action:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }
  .primary-action:focus-visible {
    outline: 2px solid rgba(255, 255, 255, 0.7);
    outline-offset: 2px;
  }

  .hint {
    margin: 0;
    font-size: 0.75rem;
    color: rgba(255, 255, 255, 0.6);
    line-height: 1.35;
  }

  .error-line {
    margin: 0.25rem 0 0;
    font-size: 0.78rem;
    color: #ff8a8a;
  }

  .popover-footer {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.5rem;
    padding: 0.55rem 1rem 0.7rem;
    border-top: 1px solid rgba(255, 255, 255, 0.08);
    cursor: grab;
  }
  .popover-footer:active { cursor: grabbing; }

  .secondary-action {
    appearance: none;
    border: 1px solid rgba(255, 255, 255, 0.18);
    background-color: transparent;
    color: rgba(255, 255, 255, 0.85);
    font-family: inherit;
    font-size: 0.78rem;
    font-weight: 500;
    padding: 0.32rem 0.7rem;
    border-radius: 6px;
    cursor: pointer;
    transition: background-color 0.12s, border-color 0.12s;
  }
  .secondary-action:hover {
    background-color: rgba(255, 255, 255, 0.08);
    border-color: rgba(255, 255, 255, 0.32);
    color: white;
  }
  .secondary-action:focus-visible {
    outline: 2px solid rgba(255, 255, 255, 0.7);
    outline-offset: 2px;
  }

  .hint-shortcut {
    font-size: 0.7rem;
    color: rgba(255, 255, 255, 0.4);
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  }
</style>
