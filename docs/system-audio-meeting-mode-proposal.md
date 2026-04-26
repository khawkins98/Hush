# Proposal: System Audio + Meeting Mode

**Status:** Draft for discussion. Not approved; not in the PRD yet.
**Authors:** Claude (drafted on Ken's behalf), 2026-04-26.
**Supersedes:** Original framing of [#33](https://github.com/khawkins98/Hush/issues/33) (system-audio capture only). This document widens the scope to include passive meeting transcription with session detection, streaming inference, and a privacy-first "audio never persists" stance.

---

## Why this exists

Hush today is a **push-to-paste dictation tool** — the user holds a hotkey, talks, and the transcript lands on the clipboard. The recording is short, deliberate, and one-speaker.

The user's stated next-step idea: extend Hush to also **passively transcribe meetings** — Zoom, Teams, Meet, anything playing on the system speakers and microphone. The transcript is the deliverable; the audio itself never lands on disk.

This is meaningfully different from the dictation flow:

|  | Dictation (today) | Meeting Mode (proposed) |
|---|---|---|
| Trigger | User-initiated, deliberate | App-detected, passive |
| Duration | Seconds | Minutes to hours |
| Speakers | One (the user) | Multiple |
| Source | Microphone | Mic + system audio (mixed or separated) |
| Storage | Single transcript row | Session timeline, per-utterance, per-speaker |
| Audio retention | Already none (zero buffer beyond transcription) | None — explicit guarantee, surfaced in UX |
| Output | Clipboard paste | Browse session, copy, summarize |

So this is a new product surface inside the same app, not an extension of the existing flow.

---

## Goals

1. **Capture system audio** alongside (or in lieu of) the microphone, on macOS / Linux / Windows.
2. **Transcribe streaming**, not in 30-second batch chunks — meeting users want utterances to appear within ~1–2 seconds.
3. **Detect sessions** automatically: when a known meeting app produces audio, prompt or auto-start a session; end when audio stops for N seconds.
4. **Diarize speakers** (segment "this part was speaker A, this part was speaker B"). Coarse is fine; per-utterance speaker labels suffice.
5. **Privacy-first**: audio enters a small ring buffer for inference and is **immediately discarded** after transcription. No WAV ever hits disk. Surface this guarantee in the UX so users understand it's a real architectural stance, not a marketing line.
6. **App-aware behavior**: distinguish "Teams call audio" from "YouTube audio" so we don't auto-record the user watching a video. The user can still opt in if they want.

## Non-goals

- **Cloud transcription.** Local-only inference is the project's identity; this proposal doesn't change that.
- **Recording mic + system separately for stereo channels.** Useful but complexity; v1 mixes them.
- **Real-time meeting summarization with an LLM.** A separate concern; out of scope here. Once transcripts exist, the user can pipe them through whatever they like.
- **Calendar integration / meeting metadata.** Out of scope.
- **Direct integration with meeting platform APIs** (Zoom SDK, Teams Graph). Out of scope; we work at the OS audio layer.

---

## Architecture overview

### Five new layers

```
┌──────────────────────────────────────────────┐
│ Session Manager                              │
│  - app-aware "should we record?" policy      │
│  - opens/closes sessions                     │
│  - emits session lifecycle events            │
└─────────────────┬────────────────────────────┘
                  │
┌─────────────────▼────────────────────────────┐
│ Streaming Transcriber                        │
│  - rolling buffer + sliding window           │
│  - per-chunk inference (Parakeet-fast,       │
│    Whisper-fallback)                         │
│  - emits utterances with timestamps          │
└─────────────────┬────────────────────────────┘
                  │
┌─────────────────▼────────────────────────────┐
│ Diarization                                  │
│  - per-utterance speaker label               │
│  - cluster across the session                │
└─────────────────┬────────────────────────────┘
                  │
┌─────────────────▼────────────────────────────┐
│ Audio Source Multiplexer                     │
│  - mic source (existing)                     │
│  - system audio source (NEW: per-OS)         │
│  - mixed stream out                          │
└─────────────────┬────────────────────────────┘
                  │
┌─────────────────▼────────────────────────────┐
│ Persistence                                  │
│  - new `sessions` + `utterances` tables      │
│  - existing `history` table for one-shot     │
│    dictation, untouched                      │
└──────────────────────────────────────────────┘
```

Audio flows up through the multiplexer into the streaming transcriber. Utterances come back down with timestamps + speaker labels and are written to `utterances`. The session manager opens/closes the parent `sessions` row.

**Importantly**: audio bytes **never** flow to persistence. The buffer is RAM-only and is overwritten in place every few hundred milliseconds.

### Five new components in the Rust crate

- `audio::source::SystemAudio` — a new `AudioCapture` impl per OS:
  - **macOS** — ScreenCaptureKit via a small Swift shim (most likely path; cpal upstream has an open issue but it's been stalled for over a year). Falls back to BlackHole/Loopback if the user has it installed.
  - **Windows** — WASAPI loopback. cpal already supports this via `Device::loopback()` on the default output. Mostly free.
  - **Linux** — PulseAudio/PipeWire monitor source. cpal exposes these as input devices already. Free.
- `transcription::streaming` — a new `StreamingTranscribe` trait that takes a chunk of audio and an "is final?" hint, returns partial-or-final utterances. The Whisper.cpp impl uses a sliding-window approach (the `whisper.cpp/examples/stream` pattern). The Parakeet-via-ONNX impl (#32) is naturally streaming.
- `diarization::Diarize` — a new trait. Initial impl: simple energy-based segmentation (no model, ~70% accurate). Better impl: `pyannote-onnx` or similar light diarization model. Both pluggable behind a feature flag.
- `meeting::SessionManager` — owns the policy ("is the user in a meeting right now?"). Reads from `active-win-pos-rs` (already a dep), maintains a list of "meeting apps" (Zoom, Teams, Meet, Discord, Slack-call, …), and emits `session:started` / `session:ended` events.
- `meeting::AppClassifier` — small lookup table: bundle id → "meeting" | "media" | "other". Distinguishes Zoom/Teams (record by default) from YouTube/Spotify (don't record). User can override per-app.

### Data model additions

```sql
CREATE TABLE meeting_sessions (
  id INTEGER PRIMARY KEY,
  app_name TEXT NOT NULL,        -- "zoom.us", "Microsoft Teams", etc.
  app_kind TEXT NOT NULL,        -- "meeting" | "media" | "other"
  started_at TEXT NOT NULL,      -- ISO-8601
  ended_at TEXT,                 -- NULL while in progress
  speaker_count INTEGER,         -- post-session diarization summary
  utterance_count INTEGER,       -- denormalised for list views
  notes TEXT                     -- user-editable summary; nullable
);

CREATE TABLE utterances (
  id INTEGER PRIMARY KEY,
  session_id INTEGER NOT NULL REFERENCES meeting_sessions(id) ON DELETE CASCADE,
  started_at_ms INTEGER NOT NULL,  -- offset from session start
  ended_at_ms INTEGER NOT NULL,
  speaker_label TEXT,              -- "Speaker A", "Speaker B", or user-renamed
  text TEXT NOT NULL,
  is_final INTEGER NOT NULL DEFAULT 1  -- 0 for partial chunks (rare in v1)
);

CREATE INDEX utterances_session_id ON utterances(session_id);
CREATE VIRTUAL TABLE utterances_fts USING fts5(text, content='utterances', content_rowid='id');
```

This sits alongside the existing `history` table (which keeps its current semantics for one-shot dictations). A possible future migration unifies them under a "transcript event" abstraction; not v1's problem.

### IPC surface (sketch)

New Tauri commands:
- `meeting_sessions_list(limit, offset)` — list completed sessions, newest first
- `meeting_session_get(id)` — full session with all utterances
- `meeting_session_delete(id)` — cascading delete (drops utterances)
- `meeting_start_manual(app_name?)` — user explicitly starts a session
- `meeting_stop_manual()` — user explicitly stops the active session
- `meeting_app_classify_set(app_name, kind)` — user override for the classifier

New events flowing backend → frontend:
- `meeting:session-started`
- `meeting:session-ended`
- `meeting:utterance` — partial or final utterance for the active session
- `meeting:prompt` — "audio detected in Zoom; start session?" prompt

---

## Privacy story

This is the architecturally load-bearing claim and deserves its own section.

**Audio never lands on disk.** Concretely:
- The audio source produces a stream of f32 samples into a bounded ring buffer (~30 s).
- The streaming transcriber pulls overlapping windows from the ring buffer for inference.
- Once a window is transcribed, the corresponding samples are no longer needed and the ring buffer overwrites them on the next push.
- No `fs::write`. No tmp file. No SQLite blob. The only persistence is text + timestamps.

**The UX surfaces this**:
- The Sessions panel header reads "Audio is transcribed live and never saved." — a permanent line, not a banner that disappears.
- A "what's recorded?" disclosure expands to spell out: "We capture system audio + microphone for transcription. The audio itself stays in memory for ~30 seconds during inference, then is discarded. We persist transcripts and timestamps."
- The first-run modal grows a section for Meeting Mode that explicitly contrasts with cloud meeting tools.

**The architecture is auditable**:
- The audio path is a single trait (`AudioCapture`) and the only persistence layer is `Repository<T>`. A reviewer can confirm there's no path from one to the other in less than five minutes by reading the code.
- A test asserts the invariant: a property test that runs a session through the streaming transcriber and asserts the disk delta is zero (no new files in any temp dir).

This is the real privacy story. We should not undersell it.

---

## Phased delivery

**Phase A — System audio capture only** (foundation). 1–2 weeks.
- `AudioCapture::start_with_source(Microphone(id) | SystemAudio)` (closes original #33 framing).
- macOS Swift shim for ScreenCaptureKit; Windows + Linux via cpal.
- Audio source picker in the UI.
- Permission UX: Screen Recording prompt on macOS first-run.
- No streaming, no sessions, no diarization yet. Mic + system audio still feed the existing dictation flow (user holds hotkey, talks-or-listens, gets transcript).

**Phase B — Streaming transcription**. 2–3 weeks.
- `StreamingTranscribe` trait, Whisper.cpp sliding-window impl.
- Replace the existing dictation flow's "stop → transcribe whole buffer" with the streaming path internally (same UX, smaller latency).
- This phase pays for itself even without meeting mode — it makes the dictation hot path feel snappier.

**Phase C — Sessions**. 2–3 weeks.
- `meeting::SessionManager` with the app-classifier policy.
- New tables, new IPC commands.
- Sessions panel in the UI: list, browse, copy, delete.
- Manual start/stop controls.
- "Detected audio in Zoom — start session?" prompt.

**Phase D — Diarization**. 1–2 weeks.
- Energy-based segmentation impl first (no model). Ship this; it's "good enough" for "two people" and identifies utterance boundaries cleanly.
- Add pyannote-onnx (or similar) behind a feature flag once we have a known-good model file we can auto-download.
- Speaker rename UX in the Sessions panel.

**Phase E — App-aware policy refinement**. Open-ended.
- User's per-app overrides.
- "Always record meetings, never record media" defaults that the user can flip.
- Per-app classifier tuning based on what bundle ids appear in real usage.

Each phase is independently shippable. After Phase A, Hush has system audio capture but no meeting mode. After Phase B, the existing dictation experience is faster. After Phase C, meeting mode exists. Phase D and E are quality bumps.

---

## Open questions

1. **Whisper streaming quality.** Sliding-window Whisper has known artifacts (word repetition at chunk boundaries). Is that acceptable for meeting transcripts, or do we hard-block on Parakeet (#32) before shipping streaming?
2. **macOS Swift shim build complexity.** Adding a Swift dependency means the build needs Xcode (already required for Tauri's macOS toolchain) but with a specific source-included `swift-bridge` setup. Worth it vs. waiting on cpal upstream?
3. **Permission proliferation.** The macOS permission story already has Microphone + Input Monitoring + Accessibility. Adding Screen Recording is a 4th prompt. Acceptable? Or do we make Meeting Mode opt-in so users who only dictate never see the prompt?
4. **Session "did I really mean to record this?" UX.** The risk: user puts on Zoom for a one-off cat-vet appointment, doesn't realize Hush is recording, transcript persists. Default-off-with-prompt is safer; default-on-for-known-meeting-apps is more useful. Pick one and let users flip.
5. **Diarization expectations.** Realistic accuracy for offline diarization is ~80% in good audio. Users will assume it's perfect. How do we frame it in the UI to set expectations correctly?
6. **Storage growth.** A 1-hour meeting at 1 utterance per 5s = ~720 rows. 10 meetings/day × 30 days × 720 = ~216K rows. SQLite handles this trivially, but UI list pagination needs to be there from day one.

---

## PRD revision proposal

Add a new top-level section to `hush-prd.md`, after the existing §5 (Engine choices) and before §6, titled **"Meeting Mode (v1.x)"**. Suggested text:

> ## §5b. Meeting Mode (v1.x)
>
> Hush v1's core flow is one-shot dictation: the user holds a hotkey, talks, gets a transcript. v1.x adds a passive-transcription surface ("Meeting Mode") that captures system audio + microphone during meetings, transcribes streaming, and persists per-utterance transcripts grouped into sessions. Audio never lands on disk — only transcripts and timestamps.
>
> Meeting Mode is **opt-in per app**. The first time the user runs a meeting (Zoom, Teams, Meet, etc.) Hush prompts: "Audio detected in Zoom — start a transcript session?" The user can answer once or set "always for this app." Media apps (YouTube, Spotify, Apple Music) default to "no" and the user can opt them in.
>
> Streaming transcription depends on either the Whisper.cpp sliding-window pattern or Parakeet (the streaming-friendly second engine — see §5). Whichever ships first is the v1.x default; the other becomes a settable preference.
>
> Diarization (per-speaker labels) starts as energy-based segmentation (no extra model). Ships better diarization later as a swap-in via the existing trait-seam pattern; the v1.x release does not gate on it.
>
> Privacy guarantee: audio is buffered in RAM for ~30 s during inference and discarded. No WAV files, no SQLite blobs, no temp files. The Sessions panel surfaces this guarantee permanently, not as a one-time banner.
>
> Out of scope for v1.x:
> - Cloud transcription (project identity stays local-only).
> - Direct meeting platform APIs (Zoom SDK, Teams Graph).
> - Calendar metadata.
> - Real-time LLM summarization (the user can pipe transcripts elsewhere).

Plus revisions to existing sections:

- **§3 Non-goals** — strike "transcription of meetings" if it's listed there (would need to check); replace with "real-time LLM summarization of meeting transcripts."
- **§5 Engine choices** — promote Parakeet (#32) from "approved second engine" to "required for streaming Meeting Mode." Note that Whisper sliding-window is the v1.x interim path.
- **§10 Permissions matrix** — add Screen Recording as a fourth macOS permission, gated on Meeting Mode being enabled.
- **§13.5 Test seam** — note that the streaming transcriber and the session manager add new trait seams; same mock-at-trait pattern applies.

---

## Why we should do this

Hush is well-positioned for this:
- **Trait-seamed architecture** means new audio sources, new transcribers, and new diarization models drop in without touching the core flow.
- **Local-first identity** — every cloud meeting tool has a privacy concern; "audio never leaves your machine, audio never lands on disk" is a real differentiator, not a marketing line.
- **Existing primitives** — foreground app detection, history persistence, settings, the IPC error model — all carry over.
- **Phased delivery** — each phase ships value independently; we can stop at any phase if priorities shift.

The risk is scope creep: Meeting Mode is a meaningfully larger product surface than dictation. The phased plan above is the mitigation — Phase A alone (system audio capture) is shippable on its own and was already approved as #33.

---

## Asks

If you agree with the direction:

1. **Approve the phased plan.** Confirm Phase A → B → C is the right order, or push back on the order.
2. **Approve the PRD revision text** (above) or propose changes.
3. **Pick a default for the "should we record media apps?" question** so I can encode it in the SessionManager policy.
4. **Confirm the privacy guarantee is the right framing.** "Audio never lands on disk" is strong; if you want stricter ("audio buffer ≤ 5 s" or similar), we set the bound here.

If you want changes before approving, reply with the specific section + suggested replacement text and I'll iterate.
