# Hush — Positioning & Marketing Review

Date: 2026-05-02
Reviewer: product-marketing pass
Scope: README, PRD, CHANGELOG, ARCHITECTURE, in-app surfaces, static assets

---

## 1. Current presentation

The README leads with the right line — *"Voice-to-text that stays on your machine"* — and the four bullets under "Why Hush" are tight: local, meeting mode, dictation, open. The privacy section spells out the two outbound surfaces (HF model downloads, manual GitHub update check) with hop-cap and SHA-256 verification, which is unusually honest for a privacy-claiming app and verifiable in code (`src-tauri/src/ipc/mod.rs`'s `is_huggingface_host`, `updater/mod.rs`'s single `api.github.com` call).

What the project is *telling people today*:

- "We're like VoiceInk, but cross-platform." That's the framing in the PRD §1 and the README acknowledgement. It's accurate but it sells Hush as a derivative — a port — when in practice the macOS implementation has now diverged into its own polished thing (three-window IA, native menu bar, tray, traffic-light permission health, meeting mode with diarization).
- "Honest about scope." The platform-support table is upfront that Linux/Windows are CI-only. Refreshing, but it currently reads as a disclaimer rather than a positioning choice.
- "It works." The README does not really sell *why someone would switch*. There is no screenshot, no GIF, no "here's the loop in 8 seconds" demo, no comparison, no "I built this because…" angle.

The PRD is a strong internal document but it is bleeding into external framing — the README links to it as the canonical "what it's meant to be" doc, which means a curious user lands on a 14-section spec written in milestones-and-risks language. That is a footgun for casual evaluation.

The only visual asset is `app-icon@2x.png`. There is no landing page, no screenshot directory, no demo video, no social card. The `docs/design/ui-redesign-*` mockups exist but aren't surfaced.

## 2. Differentiators that aren't being said loudly enough

- **The privacy posture is genuinely audit-passing, not marketing.** I checked: there is no `analytics`, no `telemetry`, no startup beacon, no crash reporter. The two `reqwest` callers are the model downloader (HF host-pinned, hop-cap 4, SHA-256 verified) and the manual update check. That is a *much* stronger story than "we don't sell your data" — it's "you can grep for it." Lead with that.
- **Meeting Mode is the killer feature and is buried.** The CHANGELOG shows enormous investment in meeting capture: ScreenCaptureKit system audio, mic + remote in parallel, You/Remote source tagging, opt-in wespeaker ONNX diarization, per-app classifier (Zoom prompts but YouTube doesn't), audio-never-touches-disk RAM-only buffering. Almost nobody else in this category has all of that, certainly not free + local. The README mentions it in one bullet.
- **"No audio ever lands on disk" — not even temp files.** This is in the PRD §5b and is architecturally enforced (RAM ring buffers, no WAV staging). Granola says "we transcribe, we don't record"; Hush can say "we transcribe, and the audio is gone within 30 seconds — verifiable in `audio/sck.rs`." That's a stronger claim.
- **macOS-native polish.** Three windows with separate capabilities, native menu bar with ⌘1/⌘2/⌘3 section nav, tray with template icon that adapts to dark/light, autostart with Accessory activation policy (no Dock icon for background launches), traffic-light permission health with stale-vs-revoked detection. This is not Electron-with-a-mic-icon; it's a real Mac app.
- **Free. Forever. No account.** Otter, Granola, Superwhisper, Aiko-cloud all have either subscriptions, freemium gates, or sign-up. The README says "open source" but never says *"$0, no account, no minutes-per-month cap."* That sentence alone would convert.
- **Meeting + dictation in one app.** Most competitors pick one lane. Superwhisper / MacWhisper / Aiko are dictation. Granola / Otter / Fireflies are meetings. Hush is both, with the same model, sharing history. Worth saying.
- **Architectural credibility for the "show me the receipts" crowd.** The trait-seam pattern, hand-rolled mocks, four-place IPC sync rule, supply-chain pin policy, `learnings.md` decision log — this is the kind of project a security-conscious developer reads, decides "OK these people are serious," and recommends to their org. Don't bury it; link it from the README under a "How we keep ourselves honest" heading.

## 3. Audience clarity

The PRD is vague on this — it implicitly targets "people who liked VoiceInk." Real audiences, ranked by how strong the fit is:

1. **Privacy-leaning knowledge workers** — lawyers, therapists, doctors, journalists. They handle confidential audio and *cannot* upload to Otter for compliance reasons. Hush is one of the few options where they don't have to think about it.
2. **Developers and power users on macOS** — already running local LLMs, comfortable with whisper.cpp, want a polished app instead of a CLI. This is the launch audience and probably the audience that already finds it.
3. **Researchers with sensitive interview audio** — qualitative researchers, oral historians, ethnographers. Same logic as #1, plus they often work offline (fieldwork).
4. **Meeting-heavy IC employees at security-conscious orgs** — finance, defence, healthcare. "I need to take notes in this meeting but I can't put a bot in the call." Hush listens client-side, no bot, no cloud.
5. **Accessibility users** — voice-to-text as a daily input mechanism. Pricing-sensitive, privacy-incidental, but a real audience that should be acknowledged.

The README currently speaks to none of these explicitly; it speaks to "a person who knows what VoiceInk is."

## 4. Competitive landscape

| Tool | Local? | Meetings? | Free? | macOS-native? |
|---|---|---|---|---|
| **VoiceInk** | yes | no | yes (GPLv3) | yes |
| **MacWhisper** | yes | partial (file import) | freemium ($) | yes |
| **Superwhisper** | yes | no | freemium ($) | yes |
| **Aiko** | yes | file import only | yes | yes |
| **whisper.cpp + scripts** | yes | DIY | yes | n/a |
| **Otter.ai** | no | yes | freemium (acct + cap) | web/Electron |
| **Granola** | no (cloud LLM) | yes | freemium ($) | yes |
| **Fireflies / Fathom** | no | yes (bot in call) | freemium ($) | web |

Hush is the only row that is yes / yes / yes / yes. That is the corner to own: **"local, meetings, free, native — pick four."** No competitor currently checks all four boxes simultaneously on macOS.

The closest comparable is VoiceInk itself, which doesn't do meetings. The closest meeting comparable is Granola, which is cloud and paid. The intersection is empty space, and Hush is sitting in it.

## 5. Recommendations (ordered by leverage)

1. **Rewrite the README hero in three lines.** Headline: *"Local voice-to-text and meeting transcription for macOS. Free. No account. No cloud."* Subhead one sentence on the meeting use-case. Then a GIF of the dictation loop and a still of the meeting transcript with You/Remote tags. Move the VoiceInk acknowledgement to the bottom of the README — it is currently the second visual element and frames Hush as derivative.

2. **Ship a 15-second dictation GIF and a 30-second meeting demo.** No screenshots is the single biggest missed conversion lever. The mockups in `docs/design/` should at minimum be exposed; ideally replaced with real screenshots of the live app at 2× retina. This is half a day of work and probably doubles README dwell time.

3. **A comparison table near the top of the README.** Use the four-yes table above. It does the positioning argument in a glance, and it is honest — every row is verifiable. People share comparison tables on Hacker News; nobody shares feature bullets.

4. **A "Why I built this" paragraph from Ken.** Two sentences. Something like "I wanted VoiceInk's dictation loop on Linux too, and I wanted a meeting-transcription tool I could actually run during a confidential call. Neither existed, so I built one." The black-box-reimplementation discipline is a strength here — it tells a credible story about why this is a fresh codebase.

5. **Lead with the audit-able privacy claim, not the privacy promise.** Replace "no telemetry" bullets with: *"Two outbound network calls, both user-initiated. Grep the source for `reqwest::` if you don't believe it."* Privacy-conscious readers don't trust marketing claims; they trust ones they can verify. This is a small copy change with disproportionate trust-building effect.

6. **Pick a launch wedge audience and write to them.** Probably "macOS users in regulated industries who can't put their meetings in Otter." A short page (`docs/for-knowledge-workers.md` linked from README) walks through Zoom + Hush + clipboard-paste-into-Notion as a workflow. Concrete usage scaffolds adoption far better than a feature list.

7. **HN / Show HN launch motion when v1.0 tags.** Title pattern that works for this category: *"Hush — local voice-to-text and meeting transcription for macOS, no cloud."* The post body should be the "why I built this" + "how it stays local" + GIF. The black-box-from-VoiceInk angle is interesting backstory for HN specifically.

8. **A tagline that lands.** Current tagline is good but generic. Stronger options to A/B in copy: *"Dictation and meetings, on your laptop only."* / *"Whisper, but make it a Mac app."* / *"The transcription tool that doesn't upload your meeting."* The last one is the sharpest because it implicitly indicts the competition.

9. **Surface the architecture credibility for the developer audience.** A one-paragraph "Engineering" section in the README pointing at `ARCHITECTURE.md`, `learnings.md`, and the trait-seam table. Devs who care about this stuff become evangelists; everyone else skips the section harmlessly.

10. **Rename or de-emphasise STATUS.md as the second README link.** Right now the hero CTA chain is `Download · What's shipped · Privacy · Contribute`. "What's shipped" is an internal-feeling phrase. Replace with `Download · Screenshots · Privacy · Contribute` once screenshots exist, or `Download · Demo · Privacy · Contribute` if there is a video.

The pattern across all of these: Hush has built more than it's selling. The product is ahead of the marketing. Closing that gap costs days, not weeks.
