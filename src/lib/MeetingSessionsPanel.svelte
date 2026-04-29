<script lang="ts">
  // Meeting Mode panel.
  //
  // Renders the meeting-mode UX: a multi-source picker (mic +
  // optional system-audio toggle) with a Start/Stop button, a live
  // transcript that updates while a session is in flight, and an
  // expand-on-click affordance for historical sessions.
  //
  // Speaker badges show "Speaker A" / "Speaker B" from D1's
  // EnergyDiarizer (#191/#201) when the diarizer produced a
  // verdict, falling back to the source-derived "You" (mic) /
  // "Remote" (system) when it didn't. Streaming whisper (#108)
  // feeds partials in continuously; the panel polls
  // `meeting_session_get` to merge in-flight partials with the
  // settled finals list.

  import ErrorDisplay from "./ErrorDisplay.svelte";
  import type { ErrorDisplay as ErrorDisplayShape } from "./errors";
  import type {
    AudioSourceListing,
    MeetingSession,
    MeetingSessionDetail,
    PersistedUtterance,
  } from "./types";

  type Props = {
    sessions: MeetingSession[];
    sessionsLoaded: boolean;
    sessionsError: ErrorDisplayShape | null;
    /// Active session id from the backend's `meeting_active_session`
    /// command. `null` means no session is in flight; renders Start
    /// button. Non-null means a session is open; renders Stop button
    /// + a live status indicator.
    activeSessionId: number | null;
    /// Active session's full detail — utterances + metadata —
    /// polled by the parent every ~3 s while a session is in
    /// flight (#122 PR4 live transcript). `null` while no session
    /// is active OR before the first poll completes.
    activeDetail: MeetingSessionDetail | null;
    busy: boolean;
    /// Audio source listings (mic devices + system-audio). Surfaced
    /// here so the meeting panel can run an independent multi-source
    /// picker — Phase 3 of #122 promotes mic + system-audio in
    /// parallel as the meeting default. The dictation hot path's
    /// own (single-source) picker lives in `ControlsSection` and
    /// reads its own state, so changes in either don't move the
    /// other.
    sources: AudioSourceListing[];
    sourcesLoaded: boolean;
    /// Mic device id chosen for the next meeting session. Single-
    /// select — meetings record at most one mic at a time, the
    /// "multi-source" axis is mic-vs-system-audio rather than
    /// mic-vs-mic. Two-way bound so the parent owns the state.
    meetingMicId: string | null;
    /// Whether the next meeting session also captures system audio
    /// alongside the mic. Defaults to `true` when the backend
    /// reports `is_supported`, `false` otherwise. Surfaced as a
    /// checkbox.
    meetingIncludeSystemAudio: boolean;
    /// Source kinds (`"microphone"` / `"system-audio"`) that have
    /// failed mid-session. Populated by the parent's
    /// `meeting:source-failed` event listener; the panel renders
    /// each entry as a struck-through chip in the active-session
    /// source line so the user knows that side stopped capturing.
    droppedSources: Set<string>;
    onDelete: (session: MeetingSession) => void | Promise<void>;
    onStart: () => void | Promise<void>;
    onStop: () => void | Promise<void>;
    /// Lazy-load the detail for a historical session row. Returns
    /// the detail (utterances + metadata) so the panel can render
    /// the transcript inline. The parent caches results in a
    /// `Map<id, MeetingSessionDetail>` so re-expanding a row
    /// doesn't re-hit the IPC.
    onLoadDetail: (id: number) => Promise<MeetingSessionDetail>;
  };

  let {
    sessions,
    sessionsLoaded,
    sessionsError,
    activeSessionId,
    activeDetail,
    busy,
    sources,
    sourcesLoaded,
    droppedSources,
    meetingMicId = $bindable(),
    meetingIncludeSystemAudio = $bindable(),
    onDelete,
    onStart,
    onStop,
    onLoadDetail,
  }: Props = $props();

  /**
   * Per-row expand-state for historical sessions (#122 PR5). Keyed
   * by session id; presence of an entry means the row is currently
   * showing its transcript. The cached detail is stored alongside
   * so a toggle-close-then-toggle-open round-trip doesn't re-issue
   * the IPC. `null` value means "expand requested, fetch in flight"
   * (renders a Loading line until the promise resolves).
   */
  let expandedDetails = $state<Map<number, MeetingSessionDetail | null>>(
    new Map(),
  );

  // Local search filter over historical sessions. Matches against
  // the visible identifying fields — app name and the user's notes
  // — case-insensitively. Frontend-only (no FTS round-trip): the
  // session list is small and already in memory, so a substring
  // filter is fast and gives an instant empty-on-no-match state.
  // The History tab uses SQLite FTS5 because transcripts can grow
  // unbounded; meetings here are at most a few dozen and don't
  // warrant the same plumbing.
  let searchQuery = $state("");
  let filteredSessions = $derived.by(() => {
    const q = searchQuery.trim().toLowerCase();
    if (q.length === 0) return sessions;
    return sessions.filter((s) => {
      if (s.appName.toLowerCase().includes(q)) return true;
      if (s.notes && s.notes.toLowerCase().includes(q)) return true;
      return false;
    });
  });

  /**
   * Stop-button confirmation state (#131). The Stop button used to
   * fire `onStop` immediately on click, which the round-8 UX
   * reviewer flagged as a real foot-gun: a stray mid-meeting click
   * ended the session with no undo. Now the first click flips
   * `confirmingStop = true` and the panel renders a "Yes / Cancel"
   * pair; only the explicit "Yes" commits. Reset whenever the
   * active session ends so a new session starts with a clean
   * confirmation state.
   */
  let confirmingStop = $state(false);
  /**
   * Tracks the "stop in flight" window — between the user
   * confirming Stop and the backend finishing the pump's
   * final-chunk drain (which can take up to ~10 s while whisper
   * inference completes on the last 10-s window). Without this,
   * the panel snaps from "Stop session" to a long visual silence
   * with no indication anything is happening, and the user reads
   * it as a hang.
   *
   * Cleared by an effect (below) when `activeSessionId` flips to
   * `null` — i.e. when the backend reports the session has
   * actually ended. Belt-and-braces: also cleared after a 30 s
   * watchdog so a backend hang doesn't strand the UI in the
   * stopping state forever.
   */
  let stopping = $state(false);
  /**
   * Set by the watchdog when the backend doesn't clear the
   * active session within 30 s of `confirmStop`. The Stop pump
   * normally completes within 10–15 s; 30 s is a wedged-pump
   * symptom worth surfacing rather than a transient slow path.
   * Pre-this-fix the watchdog cleared `stopping` and dropped the
   * user back to the Stop button as if nothing happened — they
   * had no signal that the original Stop had stalled.
   */
  let stoppingTimedOut = $state(false);
  let stoppingWatchdog: number | undefined;
  function requestStop() {
    confirmingStop = true;
  }
  function cancelStop() {
    confirmingStop = false;
  }
  async function confirmStop() {
    confirmingStop = false;
    stopping = true;
    stoppingTimedOut = false;
    if (stoppingWatchdog !== undefined) {
      window.clearTimeout(stoppingWatchdog);
    }
    stoppingWatchdog = window.setTimeout(() => {
      // Backend hasn't reported the session as ended after 30 s —
      // surface the timeout rather than silently snapping back to
      // the Stop button. The banner stays visible (so the user
      // sees the warning) but switches to a help-text state with
      // a "Try again" affordance.
      stoppingTimedOut = true;
    }, 30000);
    await onStop();
  }
  /**
   * Last-ditch retry from the timeout banner. The original
   * `meeting_stop_manual` may still be in flight (the pump's
   * spawn_blocking inference can hold for a long time on a slow
   * model); a second invocation is idempotent on the backend
   * side (state machine rejects "stop while not active") so
   * worst case is a redundant call that returns immediately.
   */
  async function retryStop() {
    stoppingTimedOut = false;
    if (stoppingWatchdog !== undefined) {
      window.clearTimeout(stoppingWatchdog);
    }
    stoppingWatchdog = window.setTimeout(() => {
      stoppingTimedOut = true;
    }, 30000);
    await onStop();
  }
  $effect(() => {
    // Backend cleared the active session — drop the stopping
    // banner and the watchdog. Leaving stopping=true after the
    // session ends would block the next Start button from
    // appearing.
    if (activeSessionId === null && stopping) {
      stopping = false;
      stoppingTimedOut = false;
      if (stoppingWatchdog !== undefined) {
        window.clearTimeout(stoppingWatchdog);
        stoppingWatchdog = undefined;
      }
    }
  });

  // Per-row click-to-confirm for session Delete. Stop-session got
  // a confirm in #131; Delete-historical-session was still
  // one-click despite producing the same "lost data with no undo"
  // outcome. First click arms; second click within 5 s fires;
  // 5 s timer auto-resets so a stale armed state can't catch
  // the user.
  let confirmingDeleteSessionId = $state<number | null>(null);
  let confirmDeleteSessionTimer: number | undefined;

  function handleSessionDelete(session: MeetingSession) {
    if (confirmingDeleteSessionId === session.id) {
      window.clearTimeout(confirmDeleteSessionTimer);
      confirmingDeleteSessionId = null;
      void onDelete(session);
      return;
    }
    window.clearTimeout(confirmDeleteSessionTimer);
    confirmingDeleteSessionId = session.id;
    confirmDeleteSessionTimer = window.setTimeout(() => {
      confirmingDeleteSessionId = null;
    }, 5000);
  }

  /**
   * Auto-expand the row of a session that just transitioned from
   * Active to Ended. The user clicked Stop, the panel re-renders
   * with the just-recorded session in the historical list — they
   * almost always want to see the transcript they just produced
   * without an extra click.
   *
   * Tracks the previous active id and reacts when it goes
   * non-null → null. The active session's `id` is what we remember,
   * not its detail; auto-expand kicks the existing lazy-load path
   * (`toggleSessionDetail`) which fetches the closed-session
   * detail just like a manual click would.
   */
  let previouslyActiveId = $state<number | null>(null);
  $effect(() => {
    const current = activeSessionId;
    const prev = previouslyActiveId;
    if (prev !== null && current === null) {
      // Session just ended — auto-expand its row if it appears in
      // the sessions list. The list is populated by the parent's
      // `refreshMeetingSessions`, which fires right after stop, so
      // a tick of latency is fine.
      void toggleSessionDetail(prev);
      // And clear any in-flight stop confirmation so the next
      // session starts fresh — the prompt is meaningless against
      // the no-active-session state.
      confirmingStop = false;
    }
    previouslyActiveId = current;
  });

  /**
   * Toggle a historical session row's transcript view. First open
   * lazy-fetches via the `onLoadDetail` callback the parent owns;
   * subsequent toggles just flip the entry in/out of the map.
   */
  async function toggleSessionDetail(id: number) {
    if (expandedDetails.has(id)) {
      const next = new Map(expandedDetails);
      next.delete(id);
      expandedDetails = next;
      return;
    }
    // Optimistically mark as "loading" so the row immediately
    // shows feedback. Map swap-in for Svelte reactivity.
    const loading = new Map(expandedDetails);
    loading.set(id, null);
    expandedDetails = loading;
    try {
      const detail = await onLoadDetail(id);
      const done = new Map(expandedDetails);
      // Guard against the user collapsing the row mid-fetch — only
      // commit if they're still expecting it.
      if (done.has(id)) {
        done.set(id, detail);
        expandedDetails = done;
      }
    } catch (e) {
      // Drop the loading marker on error so the row falls back to
      // collapsed; the parent's error region is the right place to
      // surface the failure (already wired via `sessionsError`).
      const after = new Map(expandedDetails);
      after.delete(id);
      expandedDetails = after;
      console.error("toggleSessionDetail:", e);
    }
  }

  /**
   * Display label for an utterance's speaker. The pump runs every
   * batch of finals through the configured `Diarize` impl
   * (production: `EnergyDiarizer`, #191 D1) which produces
   * `"Speaker A"` / `"Speaker B"` based on silence-gap timing. When
   * the diarizer leaves the label `None`, the dispatch falls back
   * to the source-derived `"mic"` / `"system"` tag — a coarse but
   * useful split that maps to "You" vs "Remote" on a typical call.
   * Both `PersistedUtterance` and `StreamingUtterance` carry a
   * `speakerLabel`; the helper accepts either via a structural type
   * so the partials list (#108 PR4) and the finals list use the
   * same display logic.
   */
  function speakerLabel(u: { speakerLabel: string | null }): string {
    switch (u.speakerLabel) {
      case "mic":
        return "You";
      case "system":
        return "Remote";
      case null:
      case undefined:
        return "Speaker";
      default:
        // "Speaker A" / "Speaker B" from EnergyDiarizer (#191), or
        // any future model-derived id, passes through verbatim.
        return u.speakerLabel;
    }
  }

  /**
   * Format a chunk-relative timestamp (`started_at_ms` measured
   * from session-open) as `mm:ss`. Read by the live-transcript
   * timeline so the user can scrub through the conversation.
   */
  function formatOffset(ms: number): string {
    const totalSeconds = Math.floor(ms / 1000);
    const minutes = Math.floor(totalSeconds / 60);
    const seconds = totalSeconds % 60;
    return `${minutes}:${seconds.toString().padStart(2, "0")}`;
  }

  /**
   * Build a clipboard-friendly transcript string. One block per
   * utterance, with the (mapped) speaker label and `mm:ss`
   * offset on the first line and the text on the next, blank
   * line between blocks. Plain text — meeting transcripts get
   * pasted into Slack, email, Notion, etc., where rich
   * formatting tends to clash with the destination's styling.
   *
   * Partials are deliberately excluded — they're still
   * revising; a copy that includes "I think we should — wait,
   * let me — actually, no…" mid-sentence is more confusing than
   * useful. Only finals make it.
   */
  function buildTranscriptText(
    utterances: { speakerLabel: string | null; startedAtMs: number; text: string }[],
  ): string {
    return utterances
      .filter((u) => u.text.trim().length > 0)
      .map((u) => {
        const label = speakerLabel(u);
        const time = formatOffset(u.startedAtMs);
        return `${label} (${time})\n${u.text.trim()}`;
      })
      .join("\n\n");
  }

  /**
   * Copy state — `null` when idle, the session id (or 0 for the
   * active session) when a copy just landed. Used by the
   * "Copied!" affordance on each Copy button so the user gets a
   * confirmation that the clipboard write actually succeeded.
   * Auto-clears after 2 s so a stale "Copied!" doesn't linger.
   */
  let copiedFromSessionId = $state<number | null>(null);
  let copiedTimer: number | undefined;
  /**
   * Error sentinel for the most-recent failed clipboard write.
   * Same indexing as `copiedFromSessionId` (0 = active session,
   * positive = historical session id). Auto-clears after 4 s
   * (longer than the success flash so the user has time to read
   * it). Pre-this-fix the failure path was a silent
   * `console.warn` and the user got no acknowledgement that
   * Copy didn't land.
   */
  let copyErrorFromSessionId = $state<number | null>(null);
  let copyErrorTimer: number | undefined;
  function flashCopied(id: number) {
    copiedFromSessionId = id;
    if (copiedTimer !== undefined) {
      window.clearTimeout(copiedTimer);
    }
    copiedTimer = window.setTimeout(() => {
      copiedFromSessionId = null;
    }, 2000);
  }
  function flashCopyError(id: number) {
    copyErrorFromSessionId = id;
    if (copyErrorTimer !== undefined) {
      window.clearTimeout(copyErrorTimer);
    }
    copyErrorTimer = window.setTimeout(() => {
      copyErrorFromSessionId = null;
    }, 4000);
  }
  async function copyActiveTranscript() {
    if (!activeDetail) return;
    const text = buildTranscriptText(activeDetail.utterances);
    if (!text) return;
    try {
      await navigator.clipboard.writeText(text);
      // 0 is a sentinel for "the active session" since session
      // ids start at 1 — keeps the same `copiedFromSessionId`
      // state usable for both flows without a separate flag.
      flashCopied(0);
    } catch (e) {
      console.warn("[hush] copy active transcript failed", e);
      flashCopyError(0);
    }
  }
  async function copySessionTranscript(sessionId: number) {
    const detail = expandedDetails.get(sessionId);
    if (!detail) return;
    const text = buildTranscriptText(detail.utterances);
    if (!text) return;
    try {
      await navigator.clipboard.writeText(text);
      flashCopied(sessionId);
    } catch (e) {
      console.warn("[hush] copy session transcript failed", e);
      flashCopyError(sessionId);
    }
  }

  /**
   * Format a wall-clock time of day for an utterance, computed
   * from the session's `startedAt` plus the utterance's
   * `startedAtMs` offset. Used in the historical/expanded view
   * (#136) so a 47:23 offset on yesterday's 90-minute call also
   * shows "2:47 PM" — the offset alone is meaningless without
   * scrolling back to find the session start.
   */
  function formatClockTime(sessionStartIso: string, offsetMs: number): string {
    const startMs = Date.parse(sessionStartIso);
    if (isNaN(startMs)) return "";
    const utteranceMs = startMs + offsetMs;
    const d = new Date(utteranceMs);
    return d.toLocaleString(undefined, {
      hour: "numeric",
      minute: "2-digit",
    });
  }

  let mics = $derived(sources.filter((s) => s.kind === "microphone"));
  let systemAudio = $derived(sources.find((s) => s.kind === "system-audio"));
  let pickableCount = $derived(mics.length + (systemAudio ? 1 : 0));

  /**
   * Effective source list for the active-session line. Phase 3 made
   * this multi-source (typically "Microphone + System audio"); the
   * `dropped` flag (true when the parent's
   * `meeting:source-failed` listener has tagged that source kind)
   * drives a struck-through render so the user sees which side has
   * stopped capturing without needing to read the tracing logs.
   *
   * `kind` mirrors the `AudioSource.kind` discriminator
   * (`"microphone"` / `"system-audio"`), so the dropped-set
   * lookup is a direct match against the same string the backend
   * emits.
   */
  type ActiveSourceChip = {
    label: string;
    kind: "microphone" | "system-audio";
    dropped: boolean;
  };
  let activeSources = $derived.by<ActiveSourceChip[]>(() => {
    const out: ActiveSourceChip[] = [];
    if (meetingMicId !== null) {
      const mic = mics.find((m) => m.id === meetingMicId);
      out.push({
        label: mic?.name ?? meetingMicId,
        kind: "microphone",
        dropped: droppedSources.has("microphone"),
      });
    }
    if (meetingIncludeSystemAudio && systemAudio?.isSupported) {
      out.push({
        label: systemAudio.name,
        kind: "system-audio",
        dropped: droppedSources.has("system-audio"),
      });
    }
    return out;
  });

  // Active session row, used for non-counter metadata (the row
  // exists to stay parallel to historical session rendering — no
  // counter reads here).
  let activeSession = $derived(
    activeSessionId === null
      ? null
      : sessions.find((s) => s.id === activeSessionId) ?? null,
  );

  // The existing `liveUtteranceCount` $derived (further down,
  // alongside the auto-scroll effect) is the load-bearing counter
  // for the in-session UI: reads `activeDetail.utterances.length`
  // every poll tick. The user-facing counter render gates on
  // `activeDetail !== null` to avoid flashing "0 utterances so
  // far" before the first poll lands. Pre-this-fix the counter
  // read `activeSession.utteranceCount` which only refreshed on
  // session start/stop; the pump's mid-session writes were
  // visible in the live transcript but invisible to the counter.

  // Validation for the Start button: at least one source must
  // resolve to something the backend can capture. Mic-with-no-mic
  // (a host with zero mic devices) AND no system audio = nothing
  // to record, so disable Start with a clear hint.
  let canStart = $derived(
    (meetingMicId !== null && mics.length > 0) ||
      (meetingIncludeSystemAudio && (systemAudio?.isSupported ?? false)),
  );

  /**
   * Live transcript auto-scroll (#135). The Slack-style behaviour:
   * keep new utterances in view by auto-scrolling to the bottom on
   * each append, *unless* the user has manually scrolled up — in
   * which case freeze auto-scroll until they jump back. The "jump
   * back" affordance is a "↓ N new" pill rendered when frozen;
   * clicking it re-engages auto-scroll.
   */
  let liveTranscriptEl: HTMLOListElement | null = $state(null);
  /**
   * `true` when the user is following the live tail (default).
   * `false` once they manually scroll up; back to `true` when
   * they click the jump-to-latest pill. Drives both the
   * auto-scroll behaviour and whether the pill renders.
   */
  let liveTranscriptFollowing = $state(true);
  /**
   * Count of utterances at the time the user scrolled up. The
   * pill shows `total - frozenAt` new since they paused —
   * matches Slack's "N new messages" framing.
   */
  let liveTranscriptFrozenAt = $state(0);

  /**
   * Detect manual user scroll-up to freeze auto-scroll. Browsers
   * fire `scroll` events for both programmatic and user-driven
   * scrolls, so we have to discriminate: if the bottom is in view
   * (within a small tolerance), follow; otherwise freeze.
   */
  function onLiveTranscriptScroll() {
    const el = liveTranscriptEl;
    if (!el) return;
    // 32 px tolerance: a user nudging the scrollbar a tiny bit
    // shouldn't lose the auto-tail. Anything beyond that is an
    // intentional scroll-up.
    const distanceFromBottom =
      el.scrollHeight - el.scrollTop - el.clientHeight;
    if (distanceFromBottom <= 32) {
      liveTranscriptFollowing = true;
    } else if (liveTranscriptFollowing) {
      liveTranscriptFollowing = false;
      // Snapshot finals + partials at freeze time so the "↓ N new"
      // pill counts both. Pre-#108 polish: the snapshot was finals-
      // only, which let the pill report "0 new" while whisper kept
      // revising the in-flight tail mid-monologue. The user's mental
      // model of "new" is "anything that landed since I scrolled up,"
      // partials included.
      liveTranscriptFrozenAt =
        (activeDetail?.utterances.length ?? 0) +
        (activeDetail?.currentPartials?.length ?? 0);
    }
  }

  function jumpToLiveTranscriptBottom() {
    const el = liveTranscriptEl;
    if (!el) return;
    el.scrollTop = el.scrollHeight;
    liveTranscriptFollowing = true;
  }

  // Auto-scroll on each utterance-count or partial-text-change
  // increment, but only while the user is following the tail. Reads
  // both via $derived so this re-runs exactly when new utterances
  // land OR when an in-flight partial revises (text content changed
  // — the partial array's reference might be the same instance, but
  // the inner string changes drive the effect through a join).
  let liveUtteranceCount = $derived(activeDetail?.utterances.length ?? 0);
  let livePartialFingerprint = $derived(
    (activeDetail?.currentPartials ?? [])
      .map((p) => `${p.speakerLabel ?? ""}:${p.text}`)
      .join("|"),
  );

  /**
   * Wall-clock-ms snapshot of the last time the live transcript
   * grew (a final landed, or a partial revised). Drives the
   * "listening • Ns since last update" indicator so the user can
   * tell the difference between "Hush is alive but the room is
   * quiet" and "Hush has hung." Initialised to session-start time
   * (mounted via $effect when activeSessionId flips from null) so
   * a fresh session shows "listening • 3 s" rather than a misleading
   * "0 s" or "—".
   */
  let lastUpdateMs = $state<number>(0);
  let nowMs = $state<number>(Date.now());
  let listeningTickHandle: number | undefined;
  $effect(() => {
    // Reset the timer when a session opens; clear when it closes.
    if (activeSessionId !== null) {
      lastUpdateMs = Date.now();
      if (listeningTickHandle === undefined) {
        // 1 Hz tick is plenty — the indicator reads in seconds.
        // We avoid sub-second updates so the text doesn't flicker
        // and so reduced-motion users get a stable reading.
        listeningTickHandle = window.setInterval(() => {
          nowMs = Date.now();
        }, 1000);
      }
    } else if (listeningTickHandle !== undefined) {
      window.clearInterval(listeningTickHandle);
      listeningTickHandle = undefined;
    }
  });
  $effect(() => {
    // Bump `lastUpdateMs` whenever the count grows or a partial
    // text revises. Reads both deps so $effect tracks them.
    void liveUtteranceCount;
    void livePartialFingerprint;
    lastUpdateMs = Date.now();
  });
  let secondsSinceUpdate = $derived(
    activeSessionId === null
      ? 0
      : Math.max(0, Math.floor((nowMs - lastUpdateMs) / 1000)),
  );
  $effect(() => {
    // Touch both deps so $effect tracks them.
    void liveUtteranceCount;
    void livePartialFingerprint;
    if (!liveTranscriptFollowing) return;
    const el = liveTranscriptEl;
    if (!el) return;
    // requestAnimationFrame so the new <li> is in the DOM before
    // we measure scrollHeight. Without it, the scroll lands at
    // the OLD bottom on the first new utterance / partial revision.
    requestAnimationFrame(() => {
      if (liveTranscriptEl) {
        liveTranscriptEl.scrollTop = liveTranscriptEl.scrollHeight;
      }
    });
  });

  // "N new since you scrolled up" — only meaningful while frozen.
  // Includes both finals AND in-flight partials so the pill counts
  // anything the user might want to scroll back to (a revising
  // partial mid-monologue still counts as "new since I last looked").
  let liveTranscriptNewCount = $derived(
    liveTranscriptFollowing
      ? 0
      : Math.max(
          0,
          liveUtteranceCount +
            (activeDetail?.currentPartials?.length ?? 0) -
            liveTranscriptFrozenAt,
        ),
  );

  function formatDuration(start: string, end: string | null): string {
    if (!end) return "in progress";
    const startMs = Date.parse(start);
    const endMs = Date.parse(end);
    if (isNaN(startMs) || isNaN(endMs)) return "?";
    const seconds = Math.round((endMs - startMs) / 1000);
    if (seconds < 60) return `${seconds}s`;
    const minutes = Math.round(seconds / 60);
    if (minutes < 60) return `${minutes} min`;
    const hours = Math.floor(minutes / 60);
    const remMin = minutes - hours * 60;
    return `${hours}h ${remMin}m`;
  }

  function formatStarted(iso: string): string {
    const d = new Date(iso);
    if (isNaN(d.getTime())) return iso;
    // Compact local-time format: "Apr 26, 14:32"
    return d.toLocaleString(undefined, {
      month: "short",
      day: "numeric",
      hour: "numeric",
      minute: "2-digit",
    });
  }

  function appKindLabel(kind: MeetingSession["appKind"]): string {
    switch (kind) {
      case "meeting":
        return "Meeting";
      case "media":
        return "Media";
      default:
        return "Other";
    }
  }

  /**
   * Render the persisted source-kind labels (`"mic"`, `"system"`)
   * as a single human-readable chip. The backend writes the kind
   * tags verbatim; the panel maps them to friendlier names so a
   * session reads as "Mic + System audio" rather than the
   * opaque "mic,system" CSV that lives in the database.
   *
   * Unknown values pass through unchanged so a future source kind
   * still surfaces something rather than getting silently dropped.
   */
  function sourceLabel(kind: string): string {
    switch (kind) {
      case "mic":
        return "Mic";
      case "system":
        return "System audio";
      default:
        return kind;
    }
  }
  function sourceListLabel(kinds: string[]): string {
    return kinds.map(sourceLabel).join(" + ");
  }
</script>

<section class="meetings panel-meetings" aria-labelledby="meetings-heading">
  <header class="meetings-header">
    <h2 id="meetings-heading">
      <span class="panel-tag panel-tag-meetings" aria-hidden="true">M</span>
      Meeting transcripts
      <span class="panel-subtitle">live capture, never saved</span>
    </h2>
    {#if sessions.length > 0}
      <input
        type="search"
        class="meetings-search"
        placeholder="Filter by app or notes…"
        bind:value={searchQuery}
        aria-label="Filter meeting sessions"
      />
    {/if}
  </header>

  <!--
    Permanent privacy line. Round-7 UX reviewer noted the previous
    framing leaked implementation trivia ("30s ring buffer") into a
    user-facing line. Lead with the user benefit (text appears
    instantly), then the promise (nothing stored). The buffer
    detail moves into the "How it works" disclosure below for users
    who want it.
  -->
  <p class="privacy-line" role="note">
    Hush transcribes meeting audio live and never saves the audio
    itself — only the transcript and timestamps persist.
  </p>

  <p class="hint-prose">
    When a meeting app is in the foreground (Zoom, Teams, Meet,
    Discord, Slack-call) and you opt in, Hush opens a session and
    streams the transcript here. Sessions are searchable and editable
    after the meeting ends.
  </p>

  <!--
    Manual session lifecycle controls (#110 MVP). Source picker lives
    here too (Phase 1 of #122) so the user picks mic vs system-audio
    in the same place where they start the session. While a session
    is active the picker is hidden — switching sources mid-session
    isn't supported today and the source is shown as a static label
    so the user can see what the next dictation will capture from.

    Auto-detect from foreground app is a follow-up (#112); today the
    user clicks Start, dictates with the hotkey, each transcript lands
    as an utterance under the active session, then they click Stop.
  -->
  <div class="meeting-controls" role="group" aria-label="Meeting session controls">
    {#if activeSessionId !== null}
      <div class="meeting-active-stack">
        <!--
          Live region scoped to "session in progress" only — a
          screen reader announces it when the session starts and
          when it ends. The utterance counter sits in a sibling
          span with `aria-live="off"` so the polling-driven
          increments don't announce on every chunk; that would
          be intrusive over a 30-minute meeting.
        -->
        <!--
          aria-live toggles to "off" while `stopping` is true so
          the polite queue carries one message (the stopping
          banner below) rather than two — pre-this-fix a screen
          reader would read both "Session in progress" and
          "Stopping session…" back-to-back when the user
          confirmed Stop.
        -->
        <span
          class="meeting-active-indicator"
          role="status"
          aria-live={stopping ? "off" : "polite"}
        >
          <span class="meeting-active-dot" aria-hidden="true"></span>
          Session in progress
        </span>
        {#if activeDetail}
          <span class="meeting-utterance-count" aria-live="off">
            {liveUtteranceCount} utterance{liveUtteranceCount === 1
              ? ""
              : "s"} so far
          </span>
        {/if}
        <!--
          Listening pill — a low-key alive signal between
          utterances. Whisper inference on a 10-s chunk can take
          several seconds on a slow machine or larger model, so
          the gap between "you stopped speaking" and "the partial
          appears" is sometimes long enough that the user reads it
          as a hang. The pulsing gradient bar makes "alive but
          waiting" visible; the seconds-counter gives a concrete
          number to anchor the wait.

          aria-live="off" so the screen reader doesn't announce
          every 1-s tick over a 30-minute meeting; the
          session-progress indicator above already announces the
          alive state once.
        -->
        <span
          class="meeting-listening-pill"
          aria-live="off"
          data-testid="meeting-listening-pill"
        >
          <span class="meeting-listening-bar" aria-hidden="true"></span>
          {#if liveUtteranceCount === 0 && (activeDetail?.currentPartials?.length ?? 0) === 0}
            Listening — first utterance can take ~10 s while the
            chunk window fills.
          {:else}
            Listening — last update {secondsSinceUpdate} s ago.
          {/if}
        </span>
        <p class="meeting-dictate-prompt">
          <strong>Recording</strong> from
          {#each activeSources as src, i (src.kind)}
            {#if i > 0}<span aria-hidden="true"> + </span>{/if}
            <span
              class="active-source-chip"
              class:active-source-chip-dropped={src.dropped}
              aria-label={src.dropped
                ? `${src.label} — capture stopped`
                : src.label}
            >{src.label}{#if src.dropped}<span class="source-dropped-tag" aria-hidden="true">
                stopped capturing</span
              >{/if}</span>
          {/each}{#if activeSources.length === 0}<em>no source picked</em>{/if}.
          New text streams in as you speak — italicised lines are
          still firming up.
        </p>
      </div>
      <!--
        Stop confirmation (closes #131). The Stop button used the
        same blue / weight as Start, and a stray click mid-meeting
        ended the session with no undo. The first click now asks
        for confirmation inline (no modal) so the user has a
        deliberate moment to back out; the second click commits.
        Visual differentiation also lands here — `class="stop"`
        gives the same red treatment the dictation Stop button
        already uses, so the destructive action stops looking like
        a primary CTA.
      -->
      {#if stopping}
        <!--
          Wind-down state. `meeting_stop_manual` awaits the pump's
          final-chunk drain, which can take 10–15 s while whisper
          inference completes on the last 10-s window. Without a
          banner here, the panel reads as hung. We swap the Stop
          button for a disabled "Stopping…" affordance + an
          explanatory hint, and let the pulsing-bar style from the
          listening pill carry the "still alive, just waiting"
          signal.
        -->
        <div
          class="meeting-stopping"
          class:meeting-stopping-timeout={stoppingTimedOut}
          role="status"
          aria-live="polite"
        >
          {#if stoppingTimedOut}
            <!--
              Watchdog fired — the backend has been holding for
              30+ s without reporting the session as ended. Surface
              this as a soft error with a Try again affordance
              rather than silently snapping back to the Stop
              button (which was the pre-fix behaviour and read as
              "Stop just didn't do anything").
            -->
            <div class="meeting-stopping-text">
              <strong>Still stopping — this is taking longer than expected</strong>
              <span>
                The pump may be wedged on inference for the last
                chunk. You can try again, or quit and relaunch
                Hush if it's been more than a minute total.
              </span>
            </div>
            <button
              type="button"
              class="ghost"
              onclick={() => void retryStop()}
            >
              Try again
            </button>
          {:else}
            <span class="meeting-listening-bar" aria-hidden="true"></span>
            <div class="meeting-stopping-text">
              <strong>Stopping session…</strong>
              <span>
                Finishing the last 10 s chunk and writing the
                transcript. This can take up to a minute.
              </span>
            </div>
          {/if}
        </div>
      {:else if confirmingStop}
        <div class="meeting-stop-confirm" role="group" aria-label="Confirm stop session">
          <span class="meeting-stop-confirm-prompt">
            End session?
            {#if activeDetail}
              {liveUtteranceCount} utterance{liveUtteranceCount === 1
                ? ""
                : "s"} captured.
            {/if}
          </span>
          <!--
            Auto-focus the confirm action: the user just clicked
            Stop, so their focus was on a button that's now gone;
            landing focus on "Yes, end session" (the natural
            continuation) avoids stranding them mid-air. Suppressing
            the svelte-check a11y warning because the "don't move
            focus on mount" rule it enforces doesn't apply here —
            this *is* a focus continuation following an explicit
            user action.
          -->
          <!-- svelte-ignore a11y_autofocus -->
          <button
            type="button"
            class="stop"
            onclick={confirmStop}
            disabled={busy}
            autofocus
          >
            Yes, end session
          </button>
          <button
            type="button"
            class="ghost"
            onclick={cancelStop}
            disabled={busy}
          >
            Cancel
          </button>
        </div>
      {:else}
        <button type="button" class="stop" onclick={requestStop} disabled={busy}>
          Stop session
        </button>
      {/if}
    {:else}
      <div class="meeting-source-stack">
        <label class="meeting-source-label">
          Microphone
          {#if !sourcesLoaded}
            <span class="meeting-source-loading">Loading sources…</span>
          {:else if mics.length === 0}
            <span class="meeting-source-empty">
              No microphones detected.
            </span>
          {:else}
            <select bind:value={meetingMicId} disabled={busy}>
              {#each mics as mic (mic.id)}
                <option value={mic.id}>
                  {mic.name}{mic.isDefault ? " (default)" : ""}
                </option>
              {/each}
            </select>
          {/if}
        </label>
        {#if systemAudio}
          <label class="meeting-system-audio-toggle">
            <input
              type="checkbox"
              bind:checked={meetingIncludeSystemAudio}
              disabled={busy || !systemAudio.isSupported}
            />
            <span>
              Also record system audio
              {#if !systemAudio.isSupported}
                <span class="coming-soon-hint">
                  (macOS only today — Linux/Windows tracked in
                  <a
                    href="https://github.com/khawkins98/Hush/issues/106"
                    target="_blank"
                    rel="noopener noreferrer">#106</a
                  >/<a
                    href="https://github.com/khawkins98/Hush/issues/107"
                    target="_blank"
                    rel="noopener noreferrer">#107</a
                  >)
                </span>
              {:else}
                <span class="meeting-source-meta">
                  — captures the other side of Zoom / Meet / Teams calls
                </span>
              {/if}
            </span>
          </label>
        {/if}
      </div>
      <button
        type="button"
        class="primary"
        onclick={onStart}
        disabled={busy || !canStart}
        title={canStart ? undefined : "Pick at least one audio source"}
      >
        Start a session
      </button>
    {/if}
  </div>

  {#if activeSessionId !== null}
    <!--
      Live transcript view (#122 PR4). The parent polls the
      `meeting_session_get` IPC every ~3 s while a session is in
      flight; new utterances render here as they finalise. Each
      row carries a speaker badge derived from the source label:
      "You" for the microphone, "Remote" for system audio. D1
      diarization (#201) was wired briefly but collapsed
      cross-source utterances into a single "Speaker A"; reverted
      to source-only labels until D2 (#111, model-based ONNX
      embeddings) lands.
    -->
    {#if activeDetail && (activeDetail.utterances.length > 0 || (activeDetail.currentPartials?.length ?? 0) > 0)}
      <!--
        Copy-transcript affordance for the live session. Sits
        above the transcript so it's discoverable without
        scrolling, and stays out of the way (ghost button) so it
        doesn't compete with Stop session for attention. Disabled
        while the session has only partials — a partial-only
        clipboard write would be a confusing artefact.
      -->
      <div class="live-transcript-toolbar">
        <button
          type="button"
          class="ghost"
          class:copy-error={copyErrorFromSessionId === 0}
          onclick={() => void copyActiveTranscript()}
          disabled={activeDetail.utterances.length === 0}
          aria-label="Copy live transcript to clipboard"
          data-testid="meeting-copy-active-transcript"
        >
          {#if copiedFromSessionId === 0}
            Copied!
          {:else if copyErrorFromSessionId === 0}
            Copy failed — try again
          {:else}
            Copy transcript
          {/if}
        </button>
      </div>
      <!--
        No `aria-live` on the transcript list itself: a meeting can
        produce dozens of utterances, and a "polite" live region
        would re-announce every full text on each append while the
        user is still speaking — a brutal screen-reader experience.
        The `meeting-active-indicator` above is the only live
        region; it announces session-state transitions (start /
        stop / counter), which are the user-relevant signals.

        Slack-style auto-scroll: `bind:this` exposes the element to
        the auto-scroll effect; `onscroll` watches for user-driven
        scroll-up to freeze the tail. The "↓ N new" pill below
        (rendered only while frozen) lets them jump back.

        Partials (#108 PR4) render after finals with an italic +
        reduced-opacity treatment so the user can see the "still
        revising" tail in real time. They share a key with their
        speaker label so a revision swaps the text in place rather
        than re-mounting the row.
      -->
      <ol
        bind:this={liveTranscriptEl}
        onscroll={onLiveTranscriptScroll}
        class="live-transcript"
        aria-label="Live meeting transcript"
      >
        {#each activeDetail.utterances as utt (utt.id)}
          <li class="utterance speaker-row-{utt.speakerLabel ?? 'unknown'}">
            <div class="utterance-meta">
              <span
                class="speaker-badge speaker-{utt.speakerLabel ?? 'unknown'}"
              >
                {speakerLabel(utt)}
              </span>
              <span class="utterance-time">{formatOffset(utt.startedAtMs)}</span>
            </div>
            <p class="utterance-text">{utt.text}</p>
          </li>
        {/each}
        {#each activeDetail.currentPartials ?? [] as partial (partial.speakerLabel ?? 'unknown')}
          <!--
            `aria-live="off"` (inherited from the parent <ol> by
            default, but spelled out here so a future change to the
            list-level live setting doesn't accidentally start
            re-announcing partial revisions). The screen-reader cue
            "(in progress)" tacked onto the speaker badge below is
            announced once per row mount, not on every text revision —
            partial text content updates inside the same row don't
            re-trigger the announcement, since the row's accessible
            name comes from the badge label rather than the text.
          -->
          <li
            class="utterance utterance-partial speaker-row-{partial.speakerLabel ?? 'unknown'}"
            aria-live="off"
          >
            <div class="utterance-meta">
              <span
                class="speaker-badge speaker-{partial.speakerLabel ?? 'unknown'}"
              >
                {speakerLabel(partial)}
                <span class="sr-only"> (in progress)</span>
              </span>
              <span class="utterance-time"
                >{formatOffset(partial.startedAtMs)}</span
              >
              <span class="partial-indicator" aria-hidden="true">…</span>
            </div>
            <p class="utterance-text">{partial.text}</p>
          </li>
        {/each}
      </ol>
      {#if !liveTranscriptFollowing && liveTranscriptNewCount > 0}
        <button
          type="button"
          class="jump-to-latest"
          onclick={jumpToLiveTranscriptBottom}
          aria-label="Jump to latest transcript text"
        >
          ↓ {liveTranscriptNewCount} new
        </button>
      {/if}
    {:else}
      <p class="live-transcript-empty">
        Listening… new text will appear here within a few seconds.
      </p>
    {/if}
  {/if}

  <details class="how-it-works">
    <summary>How it works</summary>
    <p>
      Click Start to begin recording. New transcript text streams
      in as you speak — italicised lines are still firming up,
      solid lines are settled. Click Stop when you're done.
    </p>
    <p>
      Audio enters a small in-memory buffer (about 30 seconds at a
      time) where Hush's local Whisper model transcribes it. Once a
      window is transcribed, those audio samples are overwritten by
      the next window — the bytes never reach disk. The transcript
      and per-utterance timestamps are what gets persisted, plus
      the meeting-app name and an optional note you can add after
      the meeting ends.
    </p>
  </details>

  {#if sessionsError}
    <ErrorDisplay error={sessionsError} scope="Meeting sessions" />
  {/if}

  {#if !sessionsLoaded}
    <p class="empty-meetings">Loading sessions…</p>
  {:else if sessions.length === 0}
    <!--
      No-sessions placeholder. Round-7 UX reviewer noted the previous
      framing read as a GitHub-ticket summary, not product copy.
      Lead with the user-facing message ("coming soon"), bury the
      developer-facing tracking-issue list under a disclosure for
      readers who want to follow along.
    -->
    <div class="meetings-placeholder">
      <p class="placeholder-headline">
        No meeting transcripts yet.
      </p>
      <p>
        Click <strong>Start a session</strong> above when you're on a
        call to capture one. Audio stays in memory only — transcripts
        and timestamps are what land on disk.
      </p>
      <p class="placeholder-tail">
        Streaming partials and Speaker A / B labels both ship today.
        Model-based diarization (so the same person stays "Speaker
        A" across long meetings) is on the roadmap —
        <a
          href="https://github.com/khawkins98/Hush/issues/111"
          target="_blank"
          rel="noopener noreferrer">#111</a
        >.
      </p>
    </div>
  {:else if filteredSessions.length === 0}
    <p class="empty-meetings">
      No matches for "<em>{searchQuery}</em>". Filter is matched
      against app name and notes only.
    </p>
  {:else}
    <ul class="sessions-list">
      {#each filteredSessions as session (session.id)}
        <li class="session-row">
          <div class="session-meta">
            <span class="session-app">{session.appName}</span>
            <span class="session-kind session-kind-{session.appKind}">
              {appKindLabel(session.appKind)}
            </span>
            <span class="session-started">{formatStarted(session.startedAt)}</span>
            <span class="session-duration">
              {formatDuration(session.startedAt, session.endedAt)}
            </span>
            <span class="session-utterances">
              {session.utteranceCount} utterance{session.utteranceCount === 1 ? "" : "s"}
            </span>
            <!--
              Source-list metadata (#242). When the foreground app
              at session-open was a browser or generic productivity
              app, the classifier verdict ("Other") and app name
              ("manual") are uninformative — but the user usually
              cares more about *what was recorded* than which
              window had focus at the moment of clicking Start.
              Surface the captured sources so a session with mic +
              system audio reads as "Mic + System audio" even when
              app classification gave up.

              Only rendered when the row has at least one source —
              legacy rows from before migration 0004 lack the
              column, and we'd rather show nothing than a confusing
              blank chip.
            -->
            {#if session.sources && session.sources.length > 0}
              <span class="session-sources" aria-label="Audio sources">
                {sourceListLabel(session.sources)}
              </span>
            {/if}
          </div>
          {#if session.notes}
            <p class="session-notes">{session.notes}</p>
          {/if}
          {#if expandedDetails.has(session.id)}
            {@const detail = expandedDetails.get(session.id)}
            {#if detail === null}
              <p class="session-detail-loading">Loading transcript…</p>
            {:else if detail && detail.utterances.length === 0}
              <p class="session-detail-empty">
                This session didn't capture any speech — probably
                stopped before anything was said.
              </p>
            {:else if detail}
              <ol
                class="live-transcript session-detail-transcript"
                aria-label={`Transcript for ${session.appName} session`}
              >
                {#each detail.utterances as utt (utt.id)}
                  <li
                    class="utterance speaker-row-{utt.speakerLabel ?? 'unknown'}"
                  >
                    <div class="utterance-meta">
                      <span
                        class="speaker-badge speaker-{utt.speakerLabel ?? 'unknown'}"
                      >
                        {speakerLabel(utt)}
                      </span>
                      <span class="utterance-time">
                        {formatOffset(utt.startedAtMs)}
                      </span>
                      <!--
                        Wall-clock time alongside the offset for
                        historical sessions (#136) — `47:23` is
                        meaningless on yesterday's 90-minute
                        meeting without scrolling back to find the
                        start. The live view skips this since the
                        user knows when they hit Start a moment
                        ago.
                      -->
                      <span class="utterance-clock">
                        · {formatClockTime(session.startedAt, utt.startedAtMs)}
                      </span>
                    </div>
                    <p class="utterance-text">{utt.text}</p>
                  </li>
                {/each}
              </ol>
            {/if}
          {/if}
          <div class="session-actions">
            <button
              type="button"
              class="ghost"
              onclick={() => void toggleSessionDetail(session.id)}
              aria-expanded={expandedDetails.has(session.id)}
              aria-label={`${expandedDetails.has(session.id) ? "Hide" : "Show"} transcript for ${session.appName} session`}
            >
              {#if expandedDetails.has(session.id)}
                Hide transcript
              {:else if session.utteranceCount > 0}
                Show transcript ({session.utteranceCount})
              {:else}
                Show transcript
              {/if}
            </button>
            <!--
              Copy historical transcript. Only enabled when the
              detail is loaded (i.e. the user has already expanded
              the row at least once) — keeps the implementation
              simple by reusing the existing detail cache, and
              avoids a second IPC round-trip that could surprise
              the user with a delay on click.
            -->
            {#if expandedDetails.get(session.id)}
              <button
                type="button"
                class="ghost"
                class:copy-error={copyErrorFromSessionId === session.id}
                onclick={() => void copySessionTranscript(session.id)}
                disabled={(expandedDetails.get(session.id)?.utterances.length ?? 0) === 0}
                aria-label={`Copy transcript from ${session.appName} session to clipboard`}
                data-testid="meeting-copy-session-{session.id}"
              >
                {#if copiedFromSessionId === session.id}
                  Copied!
                {:else if copyErrorFromSessionId === session.id}
                  Copy failed
                {:else}
                  Copy
                {/if}
              </button>
            {/if}
            <button
              type="button"
              class="ghost danger"
              class:confirming={confirmingDeleteSessionId === session.id}
              onclick={() => handleSessionDelete(session)}
              aria-label={confirmingDeleteSessionId === session.id
                ? `Click again to confirm deleting session from ${session.appName}`
                : `Delete session from ${session.appName}`}
              data-testid="meeting-session-delete-{session.id}"
            >
              {confirmingDeleteSessionId === session.id
                ? "Click to confirm"
                : "Delete"}
            </button>
          </div>
        </li>
      {/each}
    </ul>
  {/if}
</section>

<style>
.meetings {
  margin-top: 2rem;
}

.meetings-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 1rem;
  flex-wrap: wrap;
  margin-bottom: 0.5rem;
}

.meetings-header h2 {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  font-size: 1.1rem;
  margin: 0;
}

.meetings-search {
  flex: 1;
  max-width: 18rem;
  padding: 0.5em 0.85em;
  font-size: 0.9rem;
  border-radius: 8px;
  border: 1px solid #d1d1d1;
  font-family: inherit;
  color: #0f0f0f;
  background-color: #ffffff;
}

.panel-tag {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 1.5rem;
  height: 1.5rem;
  border-radius: 4px;
  background-color: var(--accent);
  color: white;
  font-size: 0.85rem;
  font-weight: 600;
}

.panel-tag-meetings {
  /* Distinct hue from history (H), replacements (R), vocabulary (V),
     models (M-already), so the at-a-glance icon column reads
     uniformly. */
  background-color: #8a5cf0;
}

.panel-subtitle {
  font-size: 0.8rem;
  font-weight: 400;
  color: #777;
  margin-left: 0.25rem;
}

/*
  Privacy line. Always visible at the top of the panel — the
  load-bearing UX commitment that meeting-mode never persists raw
  audio. Visually a quiet, framed line so it doesn't compete with
  the session list, but obviously deliberate.
*/
.privacy-line {
  margin: 0.25rem 0 0.75rem;
  padding: 0.6rem 0.85rem;
  border-left: 3px solid var(--accent);
  background-color: rgba(106, 140, 240, 0.08);
  border-radius: 4px;
  font-size: 0.9rem;
  line-height: 1.45;
  color: #333;
}

.hint-prose {
  margin: 0 0 1rem;
  font-size: 0.9rem;
  line-height: 1.5;
  color: #555;
}

.meeting-controls {
  display: flex;
  flex-wrap: wrap;
  align-items: flex-end;
  gap: 0.6rem;
  margin: 0.5rem 0 1rem;
}

.meeting-source-stack {
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
  flex: 1 1 18rem;
}

.meeting-source-label {
  display: flex;
  flex-direction: column;
  gap: 0.3rem;
  font-size: 0.85rem;
  color: #555;
  min-width: 14rem;
}

.meeting-system-audio-toggle {
  display: flex;
  align-items: flex-start;
  gap: 0.45rem;
  font-size: 0.85rem;
  color: #333;
  line-height: 1.4;
  cursor: pointer;
  user-select: none;
}

.meeting-system-audio-toggle input[type="checkbox"] {
  margin: 0.2rem 0 0 0;
  flex-shrink: 0;
  cursor: pointer;
}

.meeting-system-audio-toggle input[type="checkbox"]:disabled {
  cursor: not-allowed;
}

.meeting-source-meta {
  color: #777;
  font-size: 0.8rem;
}

.coming-soon-hint {
  color: #aa6600;
  font-size: 0.8rem;
  font-style: italic;
}

.meeting-source-label select {
  padding: 0.45em 0.7em;
  font-size: 0.9rem;
  border-radius: 6px;
  border: 1px solid #d1d1d1;
  background-color: #ffffff;
  color: #0f0f0f;
  font-family: inherit;
}

.meeting-source-loading,
.meeting-source-empty {
  font-size: 0.85rem;
  color: #777;
  padding: 0.4rem 0;
}

.meeting-active-stack {
  display: flex;
  flex-direction: column;
  gap: 0.35rem;
  flex: 1 1 18rem;
}

.meeting-dictate-prompt {
  margin: 0;
  font-size: 0.88rem;
  line-height: 1.45;
  color: #333;
}

/*
  Active-session source chip. Renders in-line inside the "Recording
  from …" prompt, one per source the pump is capturing. Dropped
  sources (the `meeting:source-failed` event fired) get a strike-
  through + a small "stopped capturing" tag so the user notices
  rather than thinking both sides are still live.
*/
.active-source-chip {
  font-weight: 500;
}
.active-source-chip-dropped {
  text-decoration: line-through;
  text-decoration-color: rgba(216, 58, 58, 0.7);
  color: #777;
}
.source-dropped-tag {
  display: inline-block;
  margin-left: 0.35rem;
  padding: 0.05em 0.4em;
  font-size: 0.72rem;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.04em;
  background-color: rgba(216, 58, 58, 0.16);
  color: #8a0000;
  border-radius: 3px;
  text-decoration: none;
}

/* `.meeting-dictate-prompt code` removed: the active-session line
   no longer wraps the source summary in a <code> tag (a UX review
   flagged it as leaking dev aesthetic into the user-facing copy). */

/*
  Live transcript — appears under the active-session controls
  while a meeting is in flight. Granola-style coloured bubbles for
  the You / Remote split. Auto-scrolls naturally as new utterances
  push older ones up; an explicit max-height lets long meetings
  remain scannable without taking over the whole window.
*/
.live-transcript-toolbar {
  display: flex;
  justify-content: flex-end;
  margin: 0.5rem 0 -0.25rem;
}

.live-transcript {
  list-style: none;
  margin: 0.5rem 0 1rem;
  padding: 0.5rem 0.75rem;
  border: 1px solid #e0e0e0;
  border-radius: 8px;
  background-color: rgba(0, 0, 0, 0.01);
  max-height: 22rem;
  overflow-y: auto;
  display: flex;
  flex-direction: column;
  gap: 0.4rem;
}

.live-transcript-empty {
  margin: 0.5rem 0 1rem;
  padding: 0.6rem 0.85rem;
  border: 1px dashed #c7c7c7;
  border-radius: 8px;
  background-color: rgba(0, 0, 0, 0.02);
  color: #555;
  font-size: 0.88rem;
  font-style: italic;
}

.utterance {
  padding: 0.35rem 0.5rem;
  border-radius: 6px;
  display: flex;
  flex-direction: column;
  gap: 0.15rem;
}

.utterance.speaker-row-mic {
  background-color: rgba(106, 140, 240, 0.08);
  border-left: 3px solid var(--accent);
}

.utterance.speaker-row-system {
  background-color: rgba(216, 58, 58, 0.06);
  border-left: 3px solid #d83a3a;
}

.utterance.speaker-row-unknown {
  background-color: rgba(0, 0, 0, 0.03);
  border-left: 3px solid #aaa;
}

.utterance-meta {
  display: flex;
  align-items: center;
  gap: 0.45rem;
  font-size: 0.78rem;
}

.speaker-badge {
  display: inline-flex;
  align-items: center;
  font-size: 0.72rem;
  font-weight: 600;
  letter-spacing: 0.04em;
  text-transform: uppercase;
  padding: 0.1em 0.5em;
  border-radius: 3px;
}

.speaker-badge.speaker-mic {
  background-color: rgba(106, 140, 240, 0.18);
  color: #2a4cb0;
}

.speaker-badge.speaker-system {
  background-color: rgba(216, 58, 58, 0.16);
  color: #8a0000;
}

.speaker-badge.speaker-unknown {
  background-color: rgba(0, 0, 0, 0.08);
  color: #555;
}

.utterance-time {
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  color: #888;
  font-size: 0.74rem;
}

/*
  In-flight partial utterance treatment (#108 PR4). Italic + reduced
  opacity to distinguish "still being refined by whisper" from
  "settled and persisted". The dotted border replaces the solid
  speaker-row stripe so a quick glance separates partials from
  finals without re-reading the text. The "…" indicator next to the
  timestamp is a redundant visual cue for users who don't immediately
  parse the styling difference.
*/
.utterance-partial {
  opacity: 0.78;
  border-left-style: dashed !important;
}

.utterance-partial .utterance-text {
  font-style: italic;
}

/*
  Visually hidden but exposed to assistive tech. Used by the partial
  row's "(in progress)" suffix on the speaker badge so screen readers
  announce status without it consuming visual space. Standard
  WCAG-recommended pattern (clip-path + 1px box keeps the element
  reachable to AT but invisible / unfocusable for sighted users).
*/
.sr-only {
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

.partial-indicator {
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  color: #888;
  font-size: 0.78rem;
  letter-spacing: 0.1em;
  /*
    The ellipsis sits next to the timestamp; subtle pulse so it reads
    as "active" without being distracting. Static fallback when
    `prefers-reduced-motion` is set.
  */
  animation: partial-pulse 1.6s ease-in-out infinite;
}

@keyframes partial-pulse {
  0%, 100% { opacity: 0.6; }
  50% { opacity: 1; }
}

@media (prefers-reduced-motion: reduce) {
  .partial-indicator {
    animation: none;
    opacity: 1;
  }
}

.utterance-clock {
  font-size: 0.74rem;
  color: #888;
}

/*
  "↓ N new" pill rendered only while auto-scroll is frozen — the
  user manually scrolled up and we paused the tail. Click jumps
  to the bottom and re-engages auto-scroll. Anchored top-right of
  the transcript shell so it doesn't cover the visible utterances.
*/
.jump-to-latest {
  display: inline-block;
  margin: 0.4rem 0 0.6rem auto;
  padding: 0.3em 0.7em;
  font-size: 0.8rem;
  font-weight: 600;
  background-color: var(--accent);
  color: #ffffff;
  border: 1px solid var(--accent);
  border-radius: 999px;
  cursor: pointer;
  font-family: inherit;
}

.jump-to-latest:hover {
  background-color: #4a6cd0;
  border-color: #4a6cd0;
}

.utterance-text {
  margin: 0;
  font-size: 0.92rem;
  line-height: 1.5;
  color: #1a1a1a;
}

.meeting-active-indicator {
  display: inline-flex;
  align-items: center;
  gap: 0.45rem;
  font-size: 0.9rem;
  color: #4a6cd0;
  font-weight: 500;
}

.meeting-utterance-count {
  font-size: 0.85rem;
  color: #777;
  margin-left: 1.05rem;
}

.meeting-active-dot {
  width: 0.6rem;
  height: 0.6rem;
  border-radius: 50%;
  background-color: #d83a3a;
  animation: meeting-pulse 1.4s ease-in-out infinite;
}

@keyframes meeting-pulse {
  0%, 100% { opacity: 1; transform: scale(1); }
  50% { opacity: 0.5; transform: scale(0.85); }
}

@media (prefers-reduced-motion: reduce) {
  .meeting-active-dot {
    animation: none;
  }
}

/* Listening pill — sits below the utterance counter, before the
   recording-source line. Subtle by design: this is an "alive" hint,
   not a primary affordance. The pulsing gradient bar is the visual
   carrier; the text just labels it. */
.meeting-listening-pill {
  display: inline-flex;
  align-items: center;
  gap: 0.55rem;
  margin-top: 0.35rem;
  font-size: 0.82rem;
  color: #6a6a6a;
  line-height: 1.4;
  /* Wrap the text on narrow widths instead of pushing past the
     panel edge — the bar stays at fixed width so the pulse rhythm
     reads consistently across viewport sizes. */
  flex-wrap: wrap;
}

/* Pulsing gradient bar — the visual proxy for "Hush is alive,
   waiting for the next chunk". A short bar with a sliding
   highlight, similar to the indeterminate-progress idiom but
   muted (low contrast, slow tempo) so it doesn't compete with
   the red recording dot above it.

   Reused by the .meeting-stopping banner — same visual idiom for
   "still working, just hold on". */
.meeting-listening-bar {
  display: inline-block;
  width: 36px;
  height: 4px;
  border-radius: 2px;
  background: linear-gradient(
    90deg,
    rgba(74, 108, 208, 0.15) 0%,
    rgba(74, 108, 208, 0.6) 50%,
    rgba(74, 108, 208, 0.15) 100%
  );
  background-size: 200% 100%;
  background-position: 100% 0;
  animation: meeting-listen-shimmer 2.2s linear infinite;
  flex-shrink: 0;
}

@keyframes meeting-listen-shimmer {
  0% { background-position: 100% 0; }
  100% { background-position: -100% 0; }
}

@media (prefers-reduced-motion: reduce) {
  .meeting-listening-bar {
    /* Static gradient — same shape, no motion. The text label
       still conveys the "listening" state. */
    animation: none;
    background-position: 50% 0;
  }
}

/* Stopping banner — replaces the Stop button while
   meeting_stop_manual's pump drain is in flight. Same shimmer bar
   on the left, two-line text on the right (title + helper). Sized
   to roughly match the Stop button's footprint so the layout
   doesn't reflow dramatically when the user confirms. */
.meeting-stopping {
  display: inline-flex;
  align-items: center;
  gap: 0.75rem;
  padding: 0.55rem 0.85rem;
  background-color: #f5f7fc;
  border: 1px solid #d8e0f2;
  border-radius: 8px;
  max-width: 28rem;
}

.meeting-stopping-text {
  display: flex;
  flex-direction: column;
  gap: 0.1rem;
  font-size: 0.85rem;
  line-height: 1.4;
  color: #444;
}

.meeting-stopping-text strong {
  color: #2a4690;
  font-weight: 600;
}

/* Copy-failed transient state on the Copy buttons. Yellow-warn
   tint matches the stopping-timeout banner — both communicate
   "the action didn't quite land, here's a soft signal" without
   escalating to a red-error treatment. Auto-clears after 4 s
   via flashCopyError. */
button.copy-error {
  border-color: #ffd591;
  background-color: #fff7e6;
  color: #6a4400;
}
@media (prefers-color-scheme: dark) {
  button.copy-error {
    border-color: #6b5300;
    background-color: #3a2c00;
    color: #ffd591;
  }
}

/* Watchdog-fired variant — same shape as the in-progress
   stopping banner, but in a yellow/warn palette to signal "this
   is taking longer than expected" without escalating to red
   (the operation may still complete; we're just nudging). The
   inner button is the user's escape hatch. */
.meeting-stopping-timeout {
  background-color: #fff7e6;
  border-color: #ffd591;
  /* Wider since the timeout copy is longer. */
  max-width: 38rem;
}
.meeting-stopping-timeout .meeting-stopping-text strong {
  color: #6a4400;
}
.meeting-stopping-timeout .meeting-stopping-text {
  color: #5a4400;
}

@media (prefers-color-scheme: dark) {
  .meeting-listening-pill {
    color: #aaa;
  }
  .meeting-listening-bar {
    background: linear-gradient(
      90deg,
      rgba(140, 168, 240, 0.15) 0%,
      rgba(140, 168, 240, 0.6) 50%,
      rgba(140, 168, 240, 0.15) 100%
    );
    background-size: 200% 100%;
  }
  .meeting-stopping {
    background-color: #1f2540;
    border-color: #2e3a64;
  }
  .meeting-stopping-text {
    color: #cfd4e6;
  }
  .meeting-stopping-text strong {
    color: #b6c3f0;
  }
  .meeting-stopping-timeout {
    background-color: #3a2c00;
    border-color: #6b5300;
  }
  .meeting-stopping-timeout .meeting-stopping-text strong {
    color: #ffd591;
  }
  .meeting-stopping-timeout .meeting-stopping-text {
    color: #f0c87b;
  }
}

.how-it-works {
  margin: 0.5rem 0 0.75rem;
}

.how-it-works summary {
  cursor: pointer;
  font-size: 0.85rem;
  color: #666;
  user-select: none;
  padding: 0.25rem 0;
}

.how-it-works summary:hover {
  color: #1a1a1a;
}

.how-it-works[open] summary {
  margin-bottom: 0.5rem;
}

.how-it-works > p {
  margin: 0;
  padding: 0.5rem 0.75rem;
  background-color: rgba(0, 0, 0, 0.02);
  border-radius: 4px;
  font-size: 0.85rem;
  line-height: 1.55;
  color: #555;
}

.empty-meetings {
  margin: 0;
  padding: 0.65rem 0.85rem;
  background-color: #fff7e6;
  border: 1px solid #f0c87b;
  border-radius: 6px;
  color: #6a4a00;
  font-size: 0.9rem;
}

.meetings-placeholder {
  padding: 1rem 1.1rem;
  border: 1px dashed #c7c7c7;
  border-radius: 8px;
  background-color: rgba(0, 0, 0, 0.02);
  color: #444;
  font-size: 0.9rem;
  line-height: 1.55;
}

.placeholder-headline {
  margin: 0 0 0.5rem;
  font-weight: 600;
  color: #1a1a1a;
}

/* `.placeholder-list` styles removed: the developer-notes
   `<details>` block they styled was dropped from the empty-state
   copy in favour of two short inline issue links. */

.placeholder-tail {
  margin: 0.5rem 0 0;
  font-size: 0.85rem;
  color: #555;
}

.placeholder-tail a {
  color: #4a6cd0;
}

.sessions-list {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 0.6rem;
}

.session-row {
  border: 1px solid #e0e0e0;
  border-radius: 8px;
  padding: 0.75rem 1rem;
  background-color: rgba(0, 0, 0, 0.01);
}

.session-meta {
  display: flex;
  flex-wrap: wrap;
  gap: 0.5rem 0.85rem;
  align-items: center;
  font-size: 0.85rem;
  color: #555;
}

.session-app {
  font-weight: 600;
  color: #1a1a1a;
}

/* Quieter classification chip. Walkthrough round flagged the
   prior all-caps blue "MEETING" tag as overstated for what is
   ambient context — most rows in this list are Meeting-type by
   definition. Sentence case + lighter colour, smaller padding. */
.session-kind {
  padding: 0.05em 0.4em;
  border-radius: 3px;
  font-size: 0.72rem;
  font-weight: 500;
  color: #6a6a6a;
  background-color: transparent;
}

.session-kind-meeting {
  color: #5a6a9a;
}

.session-kind-media {
  color: #9a5a5a;
}

.session-kind-other {
  color: #777;
}

/* Source-list chip (#242). Filled background to read as
   concrete metadata ("this is what was actually captured") in
   contrast to the muted .session-kind chip ("this is the
   classifier's guess at what kind of app was running"). The
   classifier is uninformative for browsers / generic apps;
   sources never are. */
.session-sources {
  padding: 0.05em 0.5em;
  border-radius: 4px;
  font-size: 0.72rem;
  font-weight: 500;
  color: #2a4690;
  background-color: rgba(74, 108, 208, 0.12);
}

.session-notes {
  margin: 0.5rem 0 0;
  padding: 0.4rem 0.6rem;
  background-color: rgba(255, 235, 150, 0.3);
  border-radius: 4px;
  font-size: 0.9rem;
  color: #333;
}

.session-actions {
  margin-top: 0.5rem;
  display: flex;
  justify-content: flex-end;
  gap: 0.4rem;
}

.session-detail-transcript {
  /* Inherits the live-transcript shell. Override max-height since
     a closed session doesn't grow during display — show as much
     as fits naturally before clamping. */
  margin: 0.6rem 0 0.4rem;
  max-height: 28rem;
}

.session-detail-loading,
.session-detail-empty {
  margin: 0.5rem 0;
  padding: 0.5rem 0.75rem;
  border: 1px dashed #c7c7c7;
  border-radius: 6px;
  background-color: rgba(0, 0, 0, 0.02);
  color: #555;
  font-size: 0.85rem;
  font-style: italic;
}

button {
  border-radius: 8px;
  border: 1px solid #d1d1d1;
  padding: 0.4em 0.8em;
  font-size: 0.85rem;
  font-family: inherit;
  color: #0f0f0f;
  background-color: #ffffff;
  cursor: pointer;
}

button.primary {
  background-color: var(--accent);
  color: white;
  border-color: var(--accent);
  font-weight: 600;
  padding: 0.5em 1em;
  font-size: 0.9rem;
}

button.primary:hover:not(:disabled) {
  background-color: #4a6cd0;
  border-color: #4a6cd0;
}

/*
  Stop-session destructive button. Mirrors the dictation hot path's
  red Stop in `ControlsSection.svelte` — explicit visual signal that
  this action ends the session, distinct from the blue Start CTA.
*/
button.stop {
  background-color: #d83a3a;
  color: white;
  border-color: #d83a3a;
  font-weight: 600;
  padding: 0.5em 1em;
  font-size: 0.9rem;
}

button.stop:hover:not(:disabled) {
  background-color: #b22e2e;
  border-color: #b22e2e;
}

/*
  Inline stop-session confirmation (closes #131). Replaces the bare
  Stop button with a prompt + Yes/Cancel pair. Renders inline rather
  than as a modal so the user's eye stays on the running transcript;
  a modal would be heavier than the foot-gun warrants.
*/
.meeting-stop-confirm {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 0.5rem;
}

.meeting-stop-confirm-prompt {
  font-size: 0.9rem;
  color: #555;
  flex-basis: 100%;
}

button.ghost {
  background-color: transparent;
}

button.ghost.danger {
  color: #b03030;
  border-color: #e1b8b8;
}
button.ghost.danger:hover:not(:disabled) {
  background-color: #fbeaea;
  border-color: #d83a3a;
}
button.ghost.danger.confirming {
  background-color: #fbeaea;
  border-color: #d83a3a;
  color: #8a0000;
  font-weight: 600;
}

button:hover:not(:disabled) {
  border-color: var(--accent-hover);
}

button:disabled {
  opacity: 0.6;
  cursor: not-allowed;
}

.error {
  margin: 0 0 0.75rem;
  padding: 0.65rem 0.85rem;
  border: 1px solid #d83a3a;
  background-color: #fee;
  color: #8a0000;
  border-radius: 6px;
  font-size: 0.9rem;
}

@media (prefers-color-scheme: dark) {
  .panel-subtitle {
    color: #999;
  }
  .privacy-line {
    color: #ddd;
    background-color: rgba(106, 140, 240, 0.12);
  }
  .hint-prose {
    color: #aaa;
  }
  .meetings-search {
    color: #f0f0f0;
    background-color: #2a2a2a;
    border-color: #3a3a3a;
  }
  .meeting-source-label,
  .meeting-source-loading,
  .meeting-source-empty {
    color: #aaa;
  }
  .meeting-source-label select {
    color: #f0f0f0;
    background-color: #2a2a2a;
    border-color: #3a3a3a;
  }
  .meeting-dictate-prompt {
    color: #ddd;
  }
  .active-source-chip-dropped {
    color: #888;
    text-decoration-color: rgba(216, 58, 58, 0.85);
  }
  .source-dropped-tag {
    background-color: rgba(216, 58, 58, 0.22);
    color: #f8b8b8;
  }
  .meeting-stop-confirm-prompt {
    color: #aaa;
  }
  .meeting-system-audio-toggle {
    color: #ddd;
  }
  .meeting-source-meta {
    color: #aaa;
  }
  .coming-soon-hint {
    color: #d4a040;
  }
  .live-transcript {
    border-color: #3a3a3a;
    background-color: rgba(255, 255, 255, 0.02);
  }
  .live-transcript-empty {
    border-color: #444;
    background-color: rgba(255, 255, 255, 0.03);
    color: #aaa;
  }
  .utterance.speaker-row-mic {
    background-color: rgba(106, 140, 240, 0.14);
  }
  .utterance.speaker-row-system {
    background-color: rgba(216, 58, 58, 0.12);
  }
  .utterance.speaker-row-unknown {
    background-color: rgba(255, 255, 255, 0.04);
  }
  .speaker-badge.speaker-mic {
    background-color: rgba(106, 140, 240, 0.25);
    color: #c8d4f8;
  }
  .speaker-badge.speaker-system {
    background-color: rgba(216, 58, 58, 0.22);
    color: #f8b8b8;
  }
  .speaker-badge.speaker-unknown {
    background-color: rgba(255, 255, 255, 0.1);
    color: #aaa;
  }
  .utterance-time,
  .utterance-clock {
    color: #888;
  }
  .utterance-text {
    color: #f0f0f0;
  }
  .jump-to-latest {
    background-color: var(--accent);
    border-color: var(--accent);
    color: #ffffff;
  }
  .empty-meetings {
    background-color: #3a2e10;
    border-color: #7a5a20;
    color: #f0d090;
  }
  .meetings-placeholder {
    border-color: #444;
    background-color: rgba(255, 255, 255, 0.03);
    color: #bbb;
  }
  .placeholder-headline {
    color: #f0f0f0;
  }
  .placeholder-tail {
    color: #999;
  }
  .placeholder-tail a {
    color: var(--accent);
  }
  .session-row {
    border-color: #3a3a3a;
    background-color: rgba(255, 255, 255, 0.02);
  }
  .session-meta {
    color: #aaa;
  }
  .session-app {
    color: #f0f0f0;
  }
  .session-kind-meeting {
    color: #a8b8e0;
  }
  .session-kind-media {
    color: #d8a0a0;
  }
  .session-kind-other {
    color: #aaa;
  }
  .session-sources {
    color: #b6c3f0;
    background-color: rgba(140, 168, 240, 0.18);
  }
  .session-notes {
    background-color: rgba(255, 235, 150, 0.1);
    color: #ddd;
  }
  button {
    color: #f0f0f0;
    background-color: #2a2a2a;
    border-color: #3a3a3a;
  }
  button:hover:not(:disabled) {
    border-color: var(--accent);
  }
  .error {
    background-color: #4a1a1a;
    border-color: #d83a3a;
    color: #ffd0d0;
  }
}
</style>
