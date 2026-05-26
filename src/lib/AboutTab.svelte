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
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { platform } from "@tauri-apps/plugin-os";
  import { onDestroy, onMount } from "svelte";

  import AudioPipelineDiagram from "./AudioPipelineDiagram.svelte";
  import ErrorDisplay from "./ErrorDisplay.svelte";
  import { openExternal } from "./openExternal";
  import { Events } from "./events";
  import {
    formatErrorDisplay,
    formatErrorMessage,
    type ErrorDisplay as ErrorDisplayShape,
  } from "./errors";
  import type { UpdateCheckResult } from "./types";
  import { formatBuildTimestamp, type BuildInfo } from "./utils/format";
  import "./settings-tab.css";

  // App version + Tauri runtime version + build timestamp, all sourced
  // from `get_build_info` (a custom IPC command). This avoids needing
  // the `app:default` capability grant — the main window capability was
  // intentionally kept minimal (#238) and explicitly excludes app-metadata
  // reads. `get_build_info` bakes the same info at compile time via
  // `build.rs` env vars, so it's equally accurate without a capability.
  let appVersion = $state<string>("");
  let buildTimestamp = $state<number>(0);
  let tauriVersion = $state<string>("");

  // Manual "Check for updates" probe (#223). Tagged-union result;
  // markup picks one of three branches.
  let updateCheck = $state<UpdateCheckResult | null>(null);
  let updateChecking = $state(false);

  // Auto-update install flow (#10 Step 6). The Install button drives
  // `install_pending_update`; the IPC fires `updater:download-progress`
  // and `updater:install-pending` events that this tab listens on so
  // the user sees the download bar and the "installing…" handoff.
  //
  // Inert when the plugin isn't registered (Steps 1–4 of the spec are
  // maintainer-only; until those land the IPC surfaces
  // `installUnavailable = true` and the UI falls back to the manual
  // release-notes link).
  type InstallState = "idle" | "installing" | "pending" | "failed";
  let installState = $state<InstallState>("idle");
  let installProgress = $state<{ downloaded: number; total: number | null } | null>(null);
  // Routed through `formatErrorDisplay` so the failed-install
  // pane gets the same headline + hint + collapsible technical
  // details treatment as the rest of the app's error surfaces
  // (#199 / #497). Pre-#497 this was a bare string rendered into
  // a single paragraph.
  let installError = $state<ErrorDisplayShape | null>(null);
  let installUnavailable = $state(false);

  let unlistenUpdaterResult: UnlistenFn | null = null;
  let unlistenDownloadProgress: UnlistenFn | null = null;
  let unlistenInstallPending: UnlistenFn | null = null;

  // The Gatekeeper warning under the Install button is macOS-
  // only — the Linux / Windows updater path doesn't have an
  // equivalent quarantine prompt. Read once on mount so a
  // failed `platform()` call falls through silently to "treat
  // as non-macOS" rather than leaving the warning permanently
  // visible on Linux.
  let isMacOS = $state(false);

  async function loadAppMetadata(): Promise<void> {
    try {
      const info = await invoke<BuildInfo>("get_build_info");
      appVersion = info.version;
      tauriVersion = info.tauriVersion;
      buildTimestamp = info.buildTimestamp;
    } catch (e) {
      // Non-fatal — version line is hidden when appVersion is empty.
      console.warn("[hush] get_build_info failed — app metadata unavailable", e);
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

  // Click-driven install (#10). Wraps the IPC + manages the
  // download-progress / install-pending state transitions for the
  // UI. The IPC's `updater-unavailable` typed variant is the gate
  // for whether the install path is even active; we match on
  // `kind` so the UI can pick the right fallback copy without
  // substring-matching free-form messages (same pattern #386
  // established for permission errors).
  async function onInstallUpdate() {
    installState = "installing";
    installProgress = null;
    installError = null;
    installUnavailable = false;
    // Pin the version the user is consenting to install. The IPC
    // refuses if the plugin's check resolves to a different
    // version — closes the TOCTOU gap where a release rotation
    // between "Check for updates" and "Install" would otherwise
    // install a different version silently (#497).
    const expectedVersion =
      updateCheck?.kind === "updateAvailable" ? updateCheck.latest : null;
    try {
      await invoke<void>("install_pending_update", {
        expectedVersion,
      });
      // The plugin relaunches the app on success, so this branch
      // is rarely observed in production. Still — if we ever do
      // return cleanly without a relaunch (e.g. a race where the
      // update was withdrawn between check + install), reset the
      // install state so the UI doesn't sit in a stale
      // "installing…" forever.
      installState = "idle";
      // Re-fetch the check result so the UI reflects "up to date"
      // rather than the stale "Update available" message.
      void onCheckForUpdates();
    } catch (e) {
      const ipc = e as { kind?: string; message?: string };
      if (ipc.kind === "updater-unavailable") {
        installUnavailable = true;
      } else {
        installError = formatErrorDisplay(e);
      }
      installState = "failed";
    }
  }

  // Format `installProgress` as a percentage string when the
  // upstream archive declared a Content-Length, else as a raw
  // downloaded-byte count. The plugin reports per-chunk delta —
  // not running totals — so we accumulate locally in
  // `installProgress.downloaded`.
  function formatInstallProgress(p: { downloaded: number; total: number | null } | null): string {
    if (p === null) {
      return "Starting download…";
    }
    if (p.total !== null && p.total > 0) {
      const pct = Math.min(100, Math.floor((p.downloaded / p.total) * 100));
      return `Downloading… ${pct}%`;
    }
    const mb = (p.downloaded / 1024 / 1024).toFixed(1);
    return `Downloading… ${mb} MB`;
  }

  onMount(async () => {
    void loadAppMetadata();

    try {
      isMacOS = (await platform()) === "macos";
    } catch (e) {
      console.warn("[hush] platform() failed in AboutTab", e);
    }

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

    // Auto-update install events (#10). The plugin invokes our
    // progress callback once per chunk; we accumulate locally so
    // the UI's progress bar moves smoothly even though each event
    // carries only the chunk delta (`chunkLen`), not a running total.
    unlistenDownloadProgress = await listen<{
      chunkLen: number;
      total: number | null;
    }>("updater:download-progress", (e) => {
      const prev = installProgress?.downloaded ?? 0;
      installProgress = {
        downloaded: prev + e.payload.chunkLen,
        total: e.payload.total,
      };
    });

    unlistenInstallPending = await listen<{ version: string }>(
      "updater:install-pending",
      () => {
        // Bytes are on disk; the plugin is about to swap the
        // installed app and relaunch. Swap the UI from
        // "Downloading…" to "Installing — app will relaunch"
        // so the user knows the upcoming reload is expected.
        installState = "pending";
      },
    );
  });

  onDestroy(() => {
    unlistenUpdaterResult?.();
    unlistenDownloadProgress?.();
    unlistenInstallPending?.();
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
    <h3 class="about-name">Hush</h3>
    {#if appVersion}
      <p class="about-version">
        Version {appVersion}{#if buildTimestamp > 0} · <span data-testid="about-build-timestamp">Built {formatBuildTimestamp(buildTimestamp)}</span>{/if}
      </p>
    {/if}
  </header>

  <p class="about-blurb">
    Local-only voice-to-text. Hotkey-driven dictation plus
    long-running meeting capture, powered by whisper.cpp on
    your own hardware. No cloud, no telemetry.
  </p>

  <!--
    "How it works" diagram (#427 Item 3). A visual restatement of
    the blurb above — the audio chain is a single page on the
    user's device, ending in their clipboard. Embedded here as a
    later-encounter explainer for users who skipped or forgot the
    first-run welcome modal.
  -->
  <AudioPipelineDiagram />

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
      class="kh-button kh-button--sm"
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
        <!--
          Auto-update install surface (#10). Steps 1–4 of the
          spec in `src-tauri/src/updater/mod.rs` are
          maintainer-only — until those land the IPC returns
          "auto-update is not configured for this build" and the
          markup falls back to the manual release-notes link.
          Once activated, the Install button drives
          `install_pending_update` and the download / install
          progress lights up via the two listeners in onMount.
        -->
        <div class="about-update-available-block" role="status">
          <p class="about-update-result about-update-available">
            <strong>Update available:</strong>
            {updateCheck.latest} (you're on {updateCheck.current}).
          </p>

          {#if installState === "idle"}
            <div class="about-install-actions">
              <a
                href={updateCheck.releaseUrl}
                onclick={(e) => {
                  e.preventDefault();
                  openExternal(e.currentTarget.href);
                }}
                rel="noopener noreferrer"
                class="primary about-release-notes-btn"
              >Open release notes</a>
              <button
                type="button"
                class="ghost"
                data-testid="about-install-update"
                onclick={onInstallUpdate}
              >
                Install update
              </button>
            </div>
            {#if isMacOS}
              <!--
                Gatekeeper note (#491). Shown pre-click so the user
                isn't surprised by the system dialog after relaunch.
                Also shown in the failed-retry branch below so a
                user who clicks Try Again still has the forewarning.
              -->
              <p class="about-install-gatekeeper-note">
                After installing, macOS may ask you to confirm it's
                safe to open Hush. Click <strong>Open</strong> when
                prompted.
              </p>
            {/if}
          {:else if installState === "installing"}
            <p
              class="about-update-result about-update-installing"
              data-testid="about-install-progress"
              role="status"
              aria-live="polite"
            >
              {formatInstallProgress(installProgress)}
            </p>
          {:else if installState === "pending"}
            <p
              class="about-update-result about-update-installing"
              data-testid="about-install-pending"
              role="status"
              aria-live="polite"
            >
              <strong>Installing —</strong> Hush will relaunch in a
              moment.
            </p>
          {:else if installState === "failed"}
            {#if installUnavailable}
              <p class="about-install-unavailable" role="status">
                Auto-install isn't available yet. Use the link
                below to download the latest release manually.
              </p>
              <p class="about-install-actions">
                <a
                  href={updateCheck.releaseUrl}
                  onclick={(e) => {
                    e.preventDefault();
                    openExternal(e.currentTarget.href);
                  }}
                  rel="noopener noreferrer"
                >Open release notes</a>
              </p>
            {:else}
              {#if installError}
                <div data-testid="about-install-failed">
                  <ErrorDisplay error={installError} scope="Update" />
                </div>
              {/if}
              <div class="about-install-actions">
                <button
                  type="button"
                  class="primary"
                  onclick={onInstallUpdate}
                >
                  Try again
                </button>
                <a
                  href={updateCheck.releaseUrl}
                  onclick={(e) => {
                    e.preventDefault();
                    openExternal(e.currentTarget.href);
                  }}
                  rel="noopener noreferrer"
                  class="about-update-fallback"
                >Open release notes</a>
              </div>
              {#if isMacOS}
                <!--
                  Repeat the Gatekeeper note here so a user who
                  retries still sees the forewarning before the
                  successful relaunch surfaces the dialog (UX
                  review F6).
                -->
                <p class="about-install-gatekeeper-note">
                  After installing, macOS may ask you to confirm
                  it's safe to open Hush. Click
                  <strong>Open</strong> when prompted.
                </p>
              {/if}
            {/if}
          {/if}
        </div>
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
    <dt>Changelog</dt>
    <dd>
      <a
        href="https://github.com/khawkins98/Hush/releases"
        onclick={(e) => {
          e.preventDefault();
          openExternal("https://github.com/khawkins98/Hush/releases");
        }}
        rel="noopener noreferrer">Release notes</a
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
  /* `.tab-title` + `button.ghost` imported from
     `settings-tab.css` (#392). The .about-* classes below are
     genuinely About-tab-specific. */
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
    color: var(--text-muted);
    font-size: 0.85rem;
  }
  .about-blurb {
    margin: 0 0 1.25rem;
    font-size: 0.95rem;
    color: var(--text-primary);
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
    background-color: var(--success-bg);
    border: 1px solid var(--success-border);
    color: var(--success-text);
  }
  .about-update-available {
    background-color: var(--info-bg);
    border: 1px solid var(--info-border);
    color: var(--info-text);
  }
  .about-update-failed {
    background-color: var(--warning-bg);
    border: 1px solid var(--warning-border);
    color: var(--warning-text);
  }
  .about-update-installing {
    background-color: var(--info-bg);
    border: 1px solid var(--info-border);
    color: var(--info-text);
    font-variant-numeric: tabular-nums;
  }
  .about-update-available-block {
    display: flex;
    flex-direction: column;
    gap: 0.6rem;
  }
  .about-install-actions {
    display: flex;
    align-items: center;
    gap: 0.85rem;
    flex-wrap: wrap;
    margin: 0;
  }
  .about-install-gatekeeper-note {
    margin: 0;
    padding: 0.5rem 0.75rem;
    border-radius: 6px;
    background-color: var(--bg-surface, #f4f4f6);
    border: 1px solid var(--border, #e1e1e6);
    font-size: 0.82rem;
    line-height: 1.4;
    color: var(--text-secondary, #555);
  }
  .about-install-unavailable {
    margin: 0;
    padding: 0.55rem 0.75rem;
    border-radius: 6px;
    font-size: 0.88rem;
    color: var(--text-secondary, #555);
    background-color: var(--bg-surface, #f4f4f6);
    border: 1px solid var(--border, #e1e1e6);
  }
  .about-release-notes-btn {
    display: inline-block;
    padding: 0.45em 0.85em;
    border-radius: 6px;
    border: 1px solid var(--accent);
    background-color: var(--accent);
    color: white;
    font-size: 0.88rem;
    font-weight: 500;
    line-height: 1.2;
    text-decoration: none;
  }
  .about-release-notes-btn:hover {
    background-color: var(--accent-hover, #ba5733);
    border-color: var(--accent-hover, #ba5733);
    color: white;
  }
  .about-update-fallback {
    font-size: 0.88rem;
    color: var(--text-secondary, #555);
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
    color: var(--text-muted);
    font-weight: 500;
  }
  .about-meta dd {
    margin: 0;
  }
  .about-meta code {
    font-family:
      ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: 0.85em;
    color: var(--text-secondary);
  }
  .about-credit {
    margin: 0;
    color: var(--text-muted);
    font-size: 0.85rem;
  }
  .about-tab a {
    color: var(--accent-blue);
  }
</style>
