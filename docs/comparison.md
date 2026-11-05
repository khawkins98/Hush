# Hush comparison — 2026-07-28 snapshot

[Back to Hush](../README.md). This preserves the README's dated survey; it has not been refreshed for current pricing or product capabilities.


"Free?" and "Open source?" are split on purpose — they're often conflated, but a free tier on closed-source SaaS is the vendor's to revoke, while an OSS license is yours to fork. The split also catches the middle ground: *source-available* licences like BSL let you read and build the code without granting open-source rights.

**Snapshot, surveyed 2026-07-28.** This space has matured fast — local ASR got good enough that "private meeting notes" stopped being a research project, and the field went from a handful of dictation tools to a crowded mix of dictation apps, meeting recorders, and hybrids. Treat this table as a dated sample of the notable options, not a census. Corrections and additions by PR are welcome.

|  | Local? | Dictation? | Meetings? | Free? | Open source? | Platforms |
|---|---|---|---|---|---|---|
| **Hush** | ✅ | ✅ hotkey | ✅ mic + system audio in parallel (macOS) | ✅ | ✅ Apache 2.0 | macOS · Linux · Windows |
| [OpenWhispr](https://github.com/OpenWhispr/openwhispr) | ✅ | ✅ | ✅ auto-detect Zoom / Teams | ✅ local unlimited; cloud tier: 2k words/wk free, $8/mo Pro | ✅ MIT | macOS · Linux · Windows |
| [Whispering](https://github.com/EpicenterHQ/epicenter) | ✅ | ✅ | — | ✅ | ✅ AGPLv3 | macOS · Linux · Windows |
| [Buzz](https://github.com/chidiwilliams/buzz) | ✅ | partial (live mic, no PTT into other apps) | file import only | ✅ | ✅ MIT | macOS · Linux · Windows |
| [Meetily](https://meetily.ai) | ✅ | — | ✅ system audio, no bot; Ollama summaries | ✅ Community Edition; separate paid PRO tier | ✅ MIT | macOS · Linux · Windows |
| [VoiceInk](https://github.com/Beingpax/VoiceInk) | ✅ | ✅ | — | ✅ | ✅ GPLv3 | macOS only |
| [Detto](https://github.com/Gremble-io/Detto) | ✅ | ✅ hotkey | ✅ mic + system audio, channel-separated | ✅ personal + internal business use | — BSL 1.1 (converts to MIT 2030-05-12) | macOS only (Apple Silicon) |
| [MacWhisper](https://goodsnooze.gumroad.com/l/macwhisper) | ✅ | ✅ | partial (file import) | freemium | — | macOS only |
| [Superwhisper](https://superwhisper.com) | ✅ | ✅ | ✅ free tier ([Meeting Transcription](https://superwhisper.com/meeting-transcription)) | freemium | — | macOS · Windows · iOS |
| [Granola](https://www.granola.ai) | cloud LLM | — | ✅ | freemium | — | macOS · Windows |
| [Otter](https://otter.ai) / [Fireflies](https://fireflies.ai) / [Fathom](https://fathom.video) | — | — | ✅ (cloud bot or web) | freemium | — | web |

The cross-platform OSS rows are the real competition. [OpenWhispr](https://github.com/OpenWhispr/openwhispr) is the closest — local dictation + meetings on the same three OSes — though its builds bundle an optional cloud tier (local stays unlimited; cloud caps at 2k words/week free). What sets Hush apart: dictation and parallel-source meeting capture in one app, one whisper.cpp load, one searchable history, with a privacy posture you can verify (below).

The two nearest macOS-only apps are worth a look on their own merits, and Hush owes both a debt. [VoiceInk](https://github.com/Beingpax/VoiceInk) is what inspired Hush in the first place. [Detto](https://github.com/Gremble-io/Detto) takes the same local-first premise somewhere different — Obsidian-vault-native markdown output, pre-call client briefings, and Parakeet on the Neural Engine instead of Whisper — and a couple of its design choices directly improved Hush. See [Acknowledgements](acknowledgements.md) for what and how.
