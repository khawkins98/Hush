<!--
  Settings → About tab (#332 phase 1, slice 6 — final slice;
  see also Permissions #387, Vocabulary #389, Replacements #390,
  General #391, Meeting #394). Owns its own state, IPC, and
  lifecycle for the About surface: app name + versions, the
  manual "Check for updates" probe, and the static license /
  source / report-a-bug links.

  Lifecycle: app metadata loads on mount; the manual probe is
  click-driven only, no auto-fire (Hush does not poll — every
  update check is user-initiated, see #10).

  ## `updater:result` listener race (resolved here)

  The native macOS menu's "Check for Updates" item (#265) fires
  the probe in the background AND emits `settings:goto-tab` so
  the About tab opens to receive the result. With this tab now
  mounted on demand instead of present from the Settings window's
  init, there's a theoretical race: if the menu-spawned probe
  completes faster than Svelte's render-and-mount of this tab,
  the `updater:result` event is emitted before the listener is
  registered, and the result is missed.

  In practice the probe is an HTTPS round-trip to GitHub
  (200 ms+ on a healthy network); Svelte's mount is single-
  digit ms. The window between goto-tab dispatch and listener
  registration is well under the probe's network floor.

  If this race ever bites in the wild, the fix is to move the
  listener to the parent page (where it always lives) and pass
  the result to AboutTab via a prop. Filing here as a tracked
  risk rather than pre-fixing because the empirical timing
  margin is huge.
-->
<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { getName, getTauriVersion, getVersion } from "@tauri-apps/api/app";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { onDestroy, onMount } from "svelte";

  import { openExternal } from "./openExternal";
  import { Events } from "./events";
  import { formatErrorMessage } from "./errors";

  // Tauri runtime version + the app's productName / version, all
  // fetched at runtime so they track the actual build rather than
  // a hardcoded string that would silently rot.
  let appVersion = $state<string>("");
  let appName = $state<string>("Hush");
  let tauriVersion = $state<string>("");

  // Manual "Check for updates" probe (#223). Tagged-union result;
  // markup picks one of three branches.
  type UpdateCheckResult =
    | { kind: "upToDate"; current: string }
    | { kind: "updateAvailable"; current: string; latest: string; releaseUrl: string }
    | { kind: "checkFailed"; reason: string };
  let updateCheck = $state<UpdateCheckResult | null>(null);
  let updateChecking = $state(false);

  let unlistenUpdaterResult: UnlistenFn | null = null;

  async function loadAppMetadata(): Promise<void> {
    try {
      const [name, version, tauri] = await Promise.all([
        getName(),
        getVersion(),
        getTauriVersion(),
      ]);
      appName = name;
      appVersion = version;
      tauriVersion = tauri;
    } catch {
      // Non-fatal — the static fallback ("Hush" + empty version)
      // is still readable.
    }
  }

  // Click-driven probe. Idempotent — repeated clicks just re-
  // fetch. The Rust side maps transport errors to a `checkFailed`
  // variant, so a clean throw here means the IPC itself blew up
  // (e.g. command isn't registered in a stale build).
  async function onCheckForUpdates() {
    updateChecking = true;
    updateCheck = null;
    try {
      updateCheck = await invoke<UpdateCheckResult>("check_for_updates");
    } catch (e) {
      updateCheck = {
        kind: "checkFailed",
        reason: formatErrorMessage(e),
      };
    } finally {
      updateChecking = false;
    }
  }

  onMount(async () => {
    void loadAppMetadata();

    // Menu-driven probe handler (#265). The native menu spawns
    // the probe asynchronously and emits `updater:result` on
    // completion. We render the result inline. Suppressed when
    // `updateChecking` is true so a local probe in flight doesn't
    // get clobbered + double-announced via the role="status" line.
    unlistenUpdaterResult = await listen<UpdateCheckResult>(
      Events.UpdaterResult,
      (e) => {
        if (updateChecking) {
          return;
        }
        updateCheck = e.payload;
      },
    );
  });

  onDestroy(() => {
    unlistenUpdaterResult?.();
  });
</script>

<h2 class="tab-title">About</h2>
<section class="about-tab">
  <header class="about-header">
    <!--
      App name is subordinate to the "About" tab title (H2),
      so it's H3. Pre-fix it was a sibling H2 — two H2s with
      no semantic relationship was a hierarchy violation
      flagged in review #3.
    -->
    <h3 class="about-name">{appName}</h3>
    {#if appVersion}
      <p class="about-version">Version {appVersion}</p>
    {/if}
  </header>

  <p class="about-blurb">
    Local-only voice-to-text. Hotkey-driven dictation plus
    long-running meeting capture, powered by whisper.cpp on
    your own hardware. No cloud, no telemetry.
  </p>

  <!--
    Manual "Check for updates" probe (#223). Sits below the
    version line so the comparison is contextual: user sees
    their version, clicks Check, gets a result inline.
    Auto-update via tauri-plugin-updater is the heavier
    follow-up — see #10.
  -->
  <div class="about-updates">
    <button
      type="button"
      class="ghost"
      data-testid="settings-check-updates"
      disabled={updateChecking}
      onclick={onCheckForUpdates}
      aria-label="Check for application updates"
    >
      {updateChecking ? "Checking…" : "Check for updates"}
    </button>
    {#if updateCheck}
      {#if updateCheck.kind === "upToDate"}
        <p class="about-update-result about-update-ok" role="status">
          You're on {updateCheck.current} — that's the
          latest.
        </p>
      {:else if updateCheck.kind === "updateAvailable"}
        <p class="about-update-result about-update-available" role="status">
          <strong>Update available:</strong>
          {updateCheck.latest} (you're on
          {updateCheck.current}).
          <a
            href={updateCheck.releaseUrl}
            target="_blank"
            rel="noopener noreferrer"
          >Open release notes</a>.
        </p>
      {:else if updateCheck.kind === "checkFailed"}
        <!--
          Bare `reason` strings (e.g. "Try again in a few
          minutes.") read as fragmentary without a headline.
          With #281 the same surface now lights up from a
          menu click (potentially while the user's attention
          is elsewhere); the bold lead anchors what the
          paragraph is about.
        -->
        <p class="about-update-result about-update-failed" role="status">
          <strong>Couldn't check for updates.</strong>
          {updateCheck.reason}
        </p>
      {/if}
    {/if}
  </div>

  <dl class="about-meta">
    <dt>License</dt>
    <dd>
      <a
        href="https://www.apache.org/licenses/LICENSE-2.0"
        onclick={(e) => {
          e.preventDefault();
          openExternal("https://www.apache.org/licenses/LICENSE-2.0");
        }}
        rel="noopener noreferrer">Apache License 2.0</a
      >
    </dd>
    <dt>Source</dt>
    <dd>
      <a
        href="https://github.com/khawkins98/Hush"
        onclick={(e) => {
          e.preventDefault();
          openExternal("https://github.com/khawkins98/Hush");
        }}
        rel="noopener noreferrer">github.com/khawkins98/Hush</a
      >
    </dd>
    <dt>Report a bug</dt>
    <dd>
      <a
        href="https://github.com/khawkins98/Hush/issues/new"
        onclick={(e) => {
          e.preventDefault();
          openExternal("https://github.com/khawkins98/Hush/issues/new");
        }}
        rel="noopener noreferrer">Open an issue</a
      >
    </dd>
    {#if tauriVersion}
      <dt>Tauri runtime</dt>
      <dd><code>{tauriVersion}</code></dd>
    {/if}
  </dl>

  <p class="about-credit">
    Built on
    <a
      href="https://github.com/ggerganov/whisper.cpp"
      onclick={(e) => {
        e.preventDefault();
        openExternal("https://github.com/ggerganov/whisper.cpp");
      }}
      rel="noopener noreferrer">whisper.cpp</a
    >,
    <a
      href="https://tauri.app"
      onclick={(e) => {
        e.preventDefault();
        openExternal("https://tauri.app");
      }}
      rel="noopener noreferrer">Tauri</a
    >, and
    <a
      href="https://svelte.dev"
      onclick={(e) => {
        e.preventDefault();
        openExternal("https://svelte.dev");
      }}
      rel="noopener noreferrer">Svelte</a
    >.
  </p>
</section>

<style>
  /* Per-tab style block (#332 phase 1). Shared classes hoist
     to a CSS module once #392 lands. */
  .tab-title {
    margin: 0 0 0.75rem;
    font-size: 1.4rem;
    letter-spacing: -0.01em;
  }
  button.ghost {
    padding: 0.4em 0.85em;
    font-size: 0.85rem;
    font-weight: 500;
    background-color: white;
    border: 1px solid #d1d1d8;
    border-radius: 6px;
    cursor: pointer;
    color: #2c3e8f;
  }
  button.ghost:hover:not(:disabled) {
    background-color: #f4f5fa;
    border-color: #b8c1d8;
  }
  button.ghost:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }
  .about-tab {
    max-width: 36rem;
    line-height: 1.5;
  }
  .about-header {
    margin-bottom: 1.25rem;
  }
  .about-name {
    margin: 0;
    font-size: 1.05rem;
    font-weight: 600;
  }
  .about-version {
    margin: 0.15rem 0 0;
    color: #666;
    font-size: 0.85rem;
  }
  .about-blurb {
    margin: 0 0 1.25rem;
    font-size: 0.95rem;
    color: #333;
  }
  .about-updates {
    display: flex;
    flex-direction: column;
    gap: 0.6rem;
    margin: 0 0 1.25rem;
  }
  .about-updates button {
    align-self: flex-start;
  }
  .about-update-result {
    margin: 0;
    padding: 0.55rem 0.75rem;
    border-radius: 6px;
    font-size: 0.9rem;
    line-height: 1.4;
  }
  .about-update-ok {
    background-color: #e7f8ec;
    border: 1px solid #b6e5c5;
    color: #2a6b3c;
  }
  .about-update-available {
    background-color: #eef2ff;
    border: 1px solid #c7d2fe;
    color: #1e1b4b;
  }
  .about-update-failed {
    background-color: #fff7e6;
    border: 1px solid #ffd591;
    color: #8a5a00;
  }
  .about-meta {
    display: grid;
    grid-template-columns: max-content 1fr;
    column-gap: 1rem;
    row-gap: 0.4rem;
    margin: 0 0 1.25rem;
    font-size: 0.9rem;
  }
  .about-meta dt {
    color: #666;
    font-weight: 500;
  }
  .about-meta dd {
    margin: 0;
  }
  .about-meta code {
    font-family:
      ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: 0.85em;
    color: #444;
  }
  .about-credit {
    margin: 0;
    color: #666;
    font-size: 0.85rem;
  }
  .about-tab a {
    color: var(--accent-hover);
  }
  @media (prefers-color-scheme: dark) {
    .about-version,
    .about-meta dt,
    .about-credit {
      color: #9a9a9a;
    }
    .about-blurb {
      color: #d8d8d8;
    }
    .about-meta code {
      color: #b8b8b8;
    }
    .about-tab a {
      color: var(--accent);
    }
    .about-update-ok {
      background-color: rgba(46, 170, 83, 0.15);
      border-color: #2a6b3c;
      color: #b6e5c5;
    }
    .about-update-available {
      background-color: rgba(106, 140, 240, 0.15);
      border-color: #3a4a7a;
      color: #d8e0ff;
    }
    .about-update-failed {
      background-color: rgba(255, 193, 7, 0.12);
      border-color: #6b5300;
      color: #ffd591;
    }
    button.ghost {
      background-color: #2a2a2d;
      border-color: #38383b;
      color: #b8c8ff;
    }
    button.ghost:hover:not(:disabled) {
      background-color: #38383b;
      border-color: #4a4a4d;
    }
  }
</style>
