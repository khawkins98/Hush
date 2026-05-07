<!--
  Splash window (#584 Angle 2).

  Shown during the ~1–2 s gap between process launch and
  `AppState::build_default` completing. The setup hook in
  `lib.rs::run` closes this window and reveals the main window
  once build_default returns.

  Static HTML / CSS only — no JS, no IPC, no event listeners. The
  splash window's capability file (`capabilities/splashscreen.json`)
  has no permissions, which means an unexpected script in this
  WebView has nothing to call. That keeps the blast radius zero
  for the splash surface and makes the page itself trivially
  reviewable.

  If we ever want phase progress here (Angle 1's per-phase trace
  feeds naturally into a "Loading model…" subtitle), that's a
  follow-up that needs an event-listener permission and a
  Tauri-side emit from inside build_default. v1 stays simple.
-->
<svelte:head>
  <title>Hush</title>
</svelte:head>

<div class="splash" role="status" aria-label="Hush is loading">
  <div class="splash-card">
    <div class="splash-wordmark">Hush</div>
    <div class="splash-spinner" aria-hidden="true"></div>
    <div class="splash-status">Loading…</div>
  </div>
</div>

<style>
  /*
    Reset the host page and the splash card. The splash window is
    `transparent: true` so the rounded card sits on a transparent
    backdrop — gives macOS its expected "floating panel" feel
    rather than a flat rectangle nailed to the desktop.
  */
  :global(html),
  :global(body) {
    margin: 0;
    padding: 0;
    background: transparent;
    font-family:
      -apple-system,
      BlinkMacSystemFont,
      "Segoe UI",
      sans-serif;
    overflow: hidden;
    -webkit-user-select: none;
    user-select: none;
  }

  .splash {
    width: 100vw;
    height: 100vh;
    display: flex;
    align-items: center;
    justify-content: center;
  }

  .splash-card {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 1rem;
    padding: 1.5rem 2.5rem;
    background: rgba(28, 28, 30, 0.92);
    color: #f5f5f7;
    border-radius: 12px;
    box-shadow: 0 12px 32px rgba(0, 0, 0, 0.35);
    backdrop-filter: blur(20px);
    -webkit-backdrop-filter: blur(20px);
  }

  .splash-wordmark {
    font-size: 1.6rem;
    font-weight: 600;
    letter-spacing: 0.02em;
  }

  /*
    CSS-only spinner. 28×28, 2.5px ring with one quarter-arc in a
    contrasting tint. Spins once per ~900 ms — fast enough to feel
    responsive on a cold-boot delay, slow enough not to cue
    "something is broken." No JS keeps the splash startable
    without the Tauri JS bridge, which means this window can paint
    before any IPC is wired up.
  */
  .splash-spinner {
    width: 28px;
    height: 28px;
    border: 2.5px solid rgba(255, 255, 255, 0.15);
    border-top-color: rgba(255, 255, 255, 0.85);
    border-radius: 50%;
    animation: splash-spin 0.9s linear infinite;
  }

  .splash-status {
    font-size: 0.85rem;
    color: rgba(245, 245, 247, 0.7);
  }

  @keyframes splash-spin {
    to {
      transform: rotate(360deg);
    }
  }

  /*
    Light-mode override mirrors the rest of the app's theming
    pattern. The transparent backdrop lets the macOS desktop or
    parent window context show through; the card is what carries
    the contrast.
  */
  @media (prefers-color-scheme: light) {
    .splash-card {
      background: rgba(245, 245, 247, 0.95);
      color: #1c1c1e;
      box-shadow: 0 8px 24px rgba(0, 0, 0, 0.18);
    }
    .splash-spinner {
      border-color: rgba(0, 0, 0, 0.12);
      border-top-color: rgba(0, 0, 0, 0.6);
    }
    .splash-status {
      color: rgba(28, 28, 30, 0.65);
    }
  }
</style>
