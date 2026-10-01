//! Transcript-quality helpers (#1013): pure, whisper-agnostic functions
//! the streaming policy (`streaming.rs`), the whisper adapter
//! (`whisper.rs`) and the dictation IPC path call to filter
//! hallucinations and to measure confidence.
//!
//! Everything here is a pure function over text, token scores or VAD
//! frame probabilities, so it compiles without the `whisper` feature and
//! is unit-tested directly. None of it was copied from another project;
//! where an idea came from a permissively-licensed one, the item's doc
//! comment names the source and its licence.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Media-artefact blocklist
// ---------------------------------------------------------------------------

/// Whole-sentence phrases Whisper confabulates on silence or noise
/// because its training set is full of video outros and subtitle
/// credits. Idea from DSP-AGH's "Bag of Hallucinations" work (MIT,
/// ICASSP 2025) and Vexa's mixed-stream filter (Apache-2.0); the list
/// itself is ours and deliberately narrow.
///
/// **Only media-artefact phrases belong here** — things nobody says as
/// a complete turn in a meeting. Plausible meeting speech ("yeah",
/// "thank you", "I don't know", "bye") must never be added: the cost of
/// silently deleting a real turn is much higher than the cost of one
/// stray outro line. Entries are lowercase with no trailing punctuation.
const MEDIA_ARTEFACT_PHRASES: &[&str] = &[
    "thanks for watching",
    "thank you for watching",
    "thank you so much for watching",
    "thank you very much for watching",
    "thanks for watching and see you next time",
    "thank you for watching and see you next time",
    "please subscribe",
    "please subscribe to my channel",
    "subscribe to my channel",
    "don't forget to subscribe",
    "like and subscribe",
    "please like and subscribe",
    "please like comment and subscribe",
    "don't forget to like and subscribe",
    "see you in the next video",
    "i'll see you in the next video",
];

/// Sentence prefixes for subtitle-credit confabulations ("Subtitles by
/// the Amara.org community"). A prefix (not exact) match is safe here
/// because the prefix itself is already a credit line nobody speaks.
const MEDIA_ARTEFACT_PREFIXES: &[&str] = &[
    "subtitles by",
    "subtitled by",
    "captions by",
    "captioned by",
    "subtitles made by",
    "amara.org",
];

/// Lowercase, trim, and strip surrounding punctuation from one sentence
/// so "Thanks for watching!" and "thanks for watching." compare equal.
/// Inner punctuation is preserved except commas (so "please like,
/// comment, and subscribe" matches its comma-free entry) and the
/// typographic apostrophe is folded to ASCII.
fn normalize_sentence(s: &str) -> String {
    let folded: String = s
        .chars()
        .map(|c| if c == '\u{2019}' { '\'' } else { c })
        .filter(|c| *c != ',')
        .collect();
    let trimmed =
        folded.trim_matches(|c: char| c.is_whitespace() || (c.is_ascii_punctuation() && c != '\''));
    trimmed
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Split on sentence terminators (`.`, `!`, `?`, newline) that end a
/// sentence — i.e. are followed by whitespace or the end of the text —
/// so a dot inside "Amara.org" or "3.5" doesn't split.
fn split_sentences(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let ends = match c {
            '\n' => true,
            '.' | '!' | '?' => chars.peek().map_or(true, |(_, n)| n.is_whitespace()),
            _ => false,
        };
        if ends {
            out.push(&text[start..i]);
            start = i + c.len_utf8();
        }
    }
    out.push(&text[start..]);
    out
}

/// Whether a committed final consists **entirely** of media-artefact
/// sentences (#1013). The text is split on sentence terminators and
/// every non-empty sentence must be a blocklisted phrase (exact match
/// after [`normalize_sentence`]) or start with a credit prefix. Any real
/// sentence in the mix keeps the whole final.
pub fn is_media_artefact_final(text: &str) -> bool {
    let sentences: Vec<String> = split_sentences(text)
        .into_iter()
        .map(normalize_sentence)
        .filter(|s| !s.is_empty())
        .collect();
    if sentences.is_empty() {
        return false;
    }
    sentences.iter().all(|s| {
        MEDIA_ARTEFACT_PHRASES.contains(&s.as_str())
            || MEDIA_ARTEFACT_PREFIXES.iter().any(|p| s.starts_with(p))
    })
}

// ---------------------------------------------------------------------------
// Intra-segment n-gram loop detector
// ---------------------------------------------------------------------------

/// Smallest n-gram the loop detector considers. Single-word repeats
/// ("yeah yeah yeah", "no no no") are ordinary speech and are never
/// touched.
const LOOP_MIN_NGRAM: usize = 2;
/// Largest n-gram considered. Longer loops are whole sentences, which
/// the cross-final repetition guard in `streaming.rs` already handles.
const LOOP_MAX_NGRAM: usize = 6;
/// Minimum consecutive repetitions of the same n-gram.
const LOOP_MIN_REPEATS: usize = 3;
/// Length guard: the repeated run must cover at least this many words.
/// Keeps short emphatic repeats ("you know, you know, you know" = 6
/// words) as spoken, while a real decoder loop — which runs on for 4+
/// iterations of a phrase — is caught.
const LOOP_MIN_RUN_WORDS: usize = 8;

/// Normalise one word for loop comparison: lowercase, strip surrounding
/// punctuation. "Go," and "go." compare equal.
fn loop_key(word: &str) -> String {
    word.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'')
        .to_lowercase()
}

/// Whether `gram` is a whole-number repetition of a shorter prefix.
fn is_periodic(gram: &[String]) -> bool {
    let n = gram.len();
    (1..n)
        .filter(|p| n % p == 0)
        .any(|p| (p..n).all(|j| gram[j] == gram[j % p]))
}

/// Detect and collapse in-segment n-gram loops (#1013).
///
/// With `temperature_inc = 0` (which Hush pins to stop high-temperature
/// confabulation), whisper.cpp's entropy check never gets a fallback
/// pass to retry, so a looped decode is emitted as-is: "we need to we
/// need to we need to we need to ship it". This finds any 2–6 word
/// n-gram repeated ≥ [`LOOP_MIN_REPEATS`] times back-to-back whose run
/// spans ≥ [`LOOP_MIN_RUN_WORDS`] words, and keeps a single copy.
/// Meetily, Vexa and Handy (MIT / Apache-2.0 / MIT) all ship some form
/// of repeated-n-gram guard; this one is our own implementation.
///
/// Returns `None` when no loop was found (the common case — callers
/// keep the original text untouched), or `Some(collapsed)` otherwise.
/// The kept copy uses the original casing/punctuation of the first
/// occurrence.
pub fn collapse_ngram_loops(text: &str) -> Option<String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() < LOOP_MIN_RUN_WORDS {
        return None;
    }
    let keys: Vec<String> = words.iter().map(|w| loop_key(w)).collect();
    let mut out: Vec<&str> = Vec::with_capacity(words.len());
    let mut changed = false;
    let mut i = 0;
    'outer: while i < words.len() {
        for n in LOOP_MIN_NGRAM..=LOOP_MAX_NGRAM {
            if i + n * LOOP_MIN_REPEATS > words.len() {
                break;
            }
            // An all-empty-key n-gram (pure punctuation tokens) is not a
            // phrase; skip it so "- - - -" style noise isn't "collapsed".
            if keys[i..i + n].iter().all(|k| k.is_empty()) {
                continue;
            }
            // An n-gram that is itself a repeat of a shorter unit ("no
            // no") is a single-word repeat in disguise — skip it so the
            // "single-word repeats are speech" rule can't be bypassed
            // via the bigram path.
            if is_periodic(&keys[i..i + n]) {
                continue;
            }
            let mut reps = 1;
            while i + (reps + 1) * n <= words.len()
                && keys[i + reps * n..i + (reps + 1) * n] == keys[i..i + n]
            {
                reps += 1;
            }
            if reps >= LOOP_MIN_REPEATS && reps * n >= LOOP_MIN_RUN_WORDS {
                out.extend_from_slice(&words[i..i + n]);
                i += reps * n;
                changed = true;
                continue 'outer;
            }
        }
        out.push(words[i]);
        i += 1;
    }
    if changed {
        Some(out.join(" "))
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Token confidence aggregation
// ---------------------------------------------------------------------------

/// One decoded word and the model's confidence in it, `p ∈ [0, 1]`.
/// Crosses the IPC boundary on live partials when confidence shading is
/// on (`HUSH_CONFIDENCE_SHADING=1`) so the panel can dim shaky words.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WordConfidence {
    /// The word as it appears in the segment text (no surrounding
    /// whitespace).
    pub word: String,
    /// Geometric mean of the word's token probabilities.
    pub p: f32,
}

/// One decoded, non-special token: its text (with whisper's leading
/// space convention intact) and its log-probability.
#[derive(Debug, Clone, PartialEq)]
pub struct TokenScore {
    pub text: String,
    pub plog: f32,
}

/// Per-segment confidence summary.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SegmentConfidence {
    /// Mean log-probability over the segment's text tokens. `None` for
    /// a segment with no text tokens.
    pub avg_logprob: Option<f32>,
    /// Word-level confidences in reading order.
    pub words: Vec<WordConfidence>,
}

/// Aggregate per-token log-probabilities into a segment average and
/// word-level confidences (#1013).
///
/// Whisper tokens are sub-word pieces; a token whose text starts with a
/// space begins a new word (the BPE convention whisper uses). A word's
/// confidence is the geometric mean of its pieces' probabilities, i.e.
/// `exp(mean(plog))`, which doesn't punish long words for having more
/// pieces the way a product would. Callers must pass text tokens only —
/// special tokens (timestamps, `<|endoftext|>`, …) are filtered upstream
/// by id because their probabilities say nothing about the words.
pub fn aggregate_token_scores(tokens: &[TokenScore]) -> SegmentConfidence {
    if tokens.is_empty() {
        return SegmentConfidence::default();
    }
    let sum: f32 = tokens.iter().map(|t| t.plog).sum();
    let avg_logprob = Some(sum / tokens.len() as f32);

    let mut words: Vec<WordConfidence> = Vec::new();
    let mut cur_word = String::new();
    let mut cur_plogs: Vec<f32> = Vec::new();
    let flush = |word: &mut String, plogs: &mut Vec<f32>, out: &mut Vec<WordConfidence>| {
        let w = word.trim();
        if !w.is_empty() && !plogs.is_empty() {
            let mean = plogs.iter().sum::<f32>() / plogs.len() as f32;
            out.push(WordConfidence {
                word: w.to_owned(),
                p: mean.exp().clamp(0.0, 1.0),
            });
        }
        word.clear();
        plogs.clear();
    };
    for t in tokens {
        if t.text.starts_with(' ') && !cur_word.trim().is_empty() {
            flush(&mut cur_word, &mut cur_plogs, &mut words);
        }
        cur_word.push_str(&t.text);
        cur_plogs.push(t.plog);
    }
    flush(&mut cur_word, &mut cur_plogs, &mut words);

    SegmentConfidence { avg_logprob, words }
}

/// Whether `words` still describes `text` word-for-word (whitespace
/// split). Any text rewrite downstream (loop collapse, trimming) breaks
/// the alignment; callers drop the word list rather than ship one the
/// frontend would mis-shade.
pub fn words_match_text(words: &[WordConfidence], text: &str) -> bool {
    let split: Vec<&str> = text.split_whitespace().collect();
    split.len() == words.len() && split.iter().zip(words).all(|(a, b)| *a == b.word)
}

// ---------------------------------------------------------------------------
// Dictation VAD trim + pad
// ---------------------------------------------------------------------------

/// Outcome of [`plan_dictation_trim`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrimPlan {
    /// The VAD found no usable speech. For a sub-second press this is
    /// the "accidental tap" no-op; for longer clips the caller keeps its
    /// existing behaviour (transcribe as-is) because skipping inference
    /// on a VAD miss would silently drop quiet real speech.
    NoSpeech,
    /// Transcribe `samples[start..end]`, then zero-pad to at least
    /// `pad_to` samples total.
    Trim {
        start: usize,
        end: usize,
        pad_to: usize,
    },
}

/// Tunables for [`plan_dictation_trim`], all in samples at 16 kHz.
#[derive(Debug, Clone, Copy)]
pub struct TrimConfig {
    /// Samples per VAD frame (Silero: 512).
    pub frame_len: usize,
    /// Frames at or above this probability count as speech. Lower than
    /// the meeting gate's 0.5 on purpose: trimming real speech is worse
    /// than keeping a little silence.
    pub threshold: f32,
    /// Kept before the first speech frame so soft onsets ("h…") survive.
    pub lead_pad: usize,
    /// Kept after the last speech frame so trailing consonants survive.
    pub tail_pad: usize,
    /// Minimum total speech frames for the clip to count as speech at
    /// all — rejects a single key-click frame.
    pub min_speech_frames: usize,
    /// Clips shorter than this after trimming are zero-padded up to it.
    pub min_len: usize,
}

impl TrimConfig {
    /// Defaults: Silero frames, 0.35 threshold, 300 ms lead / 400 ms
    /// tail padding, ≥ 6 speech frames (~190 ms — a deliberate "yes" is
    /// 250–400 ms; a key click or breath is shorter), pad to 1.25 s.
    ///
    /// Padding a short clip with silence rather than rejecting it is an
    /// idea from Handy (MIT), which pads sub-second clips to 1.25 s;
    /// whisper decodes very short buffers poorly, and trailing silence
    /// is the cheapest way to give it a full-sized input.
    pub const fn dictation_defaults() -> Self {
        Self {
            frame_len: 512,
            threshold: 0.35,
            lead_pad: 4_800,
            tail_pad: 6_400,
            min_speech_frames: 6,
            min_len: 20_000,
        }
    }
}

/// Decide how to trim + pad a mono 16 kHz dictation clip given one VAD
/// probability per `cfg.frame_len` frame (#1013). Pure: the caller runs
/// the VAD and hands in the scores, so the policy is testable without a
/// model. `frame_probs[i]` covers samples `[i*frame_len, (i+1)*frame_len)`.
pub fn plan_dictation_trim(
    total_samples: usize,
    frame_probs: &[f32],
    cfg: &TrimConfig,
) -> TrimPlan {
    let speech: Vec<usize> = frame_probs
        .iter()
        .enumerate()
        .filter(|(_, p)| **p >= cfg.threshold)
        .map(|(i, _)| i)
        .collect();
    if speech.len() < cfg.min_speech_frames {
        return TrimPlan::NoSpeech;
    }
    let first = speech[0];
    let last = *speech
        .last()
        .expect("non-empty: len >= min_speech_frames >= 1");
    let start = (first * cfg.frame_len).saturating_sub(cfg.lead_pad);
    let end = ((last + 1) * cfg.frame_len + cfg.tail_pad).min(total_samples);
    if start >= end {
        return TrimPlan::NoSpeech;
    }
    TrimPlan::Trim {
        start,
        end,
        pad_to: cfg.min_len,
    }
}

/// Frames scored at each end of a long dictation clip: 5 s of Silero
/// frames. Leading/trailing silence on a push-to-talk press is rarely
/// longer than a couple of seconds.
pub const EDGE_SCAN_FRAMES: usize = 156;

/// Which frame ranges to run the VAD over for a clip of `n_frames`:
/// the whole clip when it's at most `2 * edge` frames, otherwise just
/// the first and last `edge` frames.
pub fn edge_frame_ranges(n_frames: usize, edge: usize) -> Vec<std::ops::Range<usize>> {
    if n_frames <= edge * 2 {
        std::iter::once(0..n_frames).collect()
    } else {
        vec![0..edge, n_frames - edge..n_frames]
    }
}

/// Stitch per-range VAD scores back into one probability per frame,
/// treating the unscored middle as speech (p = 1.0). That is the safe
/// assumption: it can only make [`plan_dictation_trim`] keep *more*
/// audio, and it means a long clip is never judged speech-free.
pub fn assemble_edge_probs(
    n_frames: usize,
    ranges: &[std::ops::Range<usize>],
    scored: &[Vec<f32>],
) -> Vec<f32> {
    let mut probs = vec![1.0f32; n_frames];
    for (range, s) in ranges.iter().zip(scored) {
        for (f, p) in range.clone().zip(s) {
            probs[f] = *p;
        }
    }
    probs
}

/// Apply a [`TrimPlan::Trim`] to `samples`, returning the trimmed and
/// zero-padded buffer. Padding goes at the end: whisper handles
/// trailing silence well and it keeps timestamps anchored at speech.
pub fn apply_trim(samples: &[f32], start: usize, end: usize, pad_to: usize) -> Vec<f32> {
    let end = end.min(samples.len());
    let start = start.min(end);
    let mut out = Vec::with_capacity((end - start).max(pad_to));
    out.extend_from_slice(&samples[start..end]);
    if out.len() < pad_to {
        out.resize(pad_to, 0.0);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- media-artefact blocklist ----

    #[test]
    fn blocks_classic_outro_phrases() {
        for t in [
            "Thanks for watching!",
            "Thank you for watching.",
            "thank you so much for watching",
            "Please subscribe to my channel.",
            "Don’t forget to subscribe!",
            "Please like, comment, and subscribe.",
            "  Thanks for watching!  ",
        ] {
            assert!(is_media_artefact_final(t), "{t:?} should be blocked");
        }
    }

    #[test]
    fn blocks_subtitle_credit_prefixes() {
        assert!(is_media_artefact_final(
            "Subtitles by the Amara.org community"
        ));
        assert!(is_media_artefact_final("Captions by GetTranscribed"));
    }

    #[test]
    fn blocks_multi_sentence_finals_made_only_of_artefacts() {
        assert!(is_media_artefact_final(
            "Thanks for watching. Please subscribe."
        ));
    }

    #[test]
    fn keeps_plausible_meeting_speech() {
        for t in [
            "Yeah.",
            "Thank you.",
            "Thank you so much.",
            "I don't know.",
            "Bye!",
            "Thanks, everyone.",
            "Okay, see you next week.",
            "So",
            "",
            "Thanks for watching the build, Sam.",
            "Subscribe to the channel later if you want, but first let's look at Q3.",
        ] {
            assert!(!is_media_artefact_final(t), "{t:?} must not be blocked");
        }
    }

    #[test]
    fn keeps_final_mixing_artefact_with_real_sentence() {
        assert!(!is_media_artefact_final(
            "Thanks for watching. Now let's review the budget."
        ));
    }

    // ---- n-gram loop detector ----

    #[test]
    fn collapses_long_phrase_loop() {
        let t = "we need to we need to we need to we need to ship it";
        assert_eq!(
            collapse_ngram_loops(t).as_deref(),
            Some("we need to ship it")
        );
    }

    #[test]
    fn collapses_loop_ignoring_case_and_punctuation() {
        let t = "Thank you. Thank you. Thank you. Thank you. Thank you.";
        assert_eq!(collapse_ngram_loops(t).as_deref(), Some("Thank you."));
    }

    #[test]
    fn keeps_single_word_repeats() {
        assert_eq!(
            collapse_ngram_loops("yeah yeah yeah, so I think we're fine here"),
            None
        );
        assert_eq!(collapse_ngram_loops("no no no no no no no no no"), None);
    }

    #[test]
    fn keeps_short_emphatic_bigram_repeats_under_length_guard() {
        // 3 × 2 words = 6 < LOOP_MIN_RUN_WORDS.
        assert_eq!(
            collapse_ngram_loops("you know, you know, you know, it was fine"),
            None
        );
    }

    #[test]
    fn keeps_two_repeats() {
        assert_eq!(
            collapse_ngram_loops("one more time one more time and then we stop for lunch"),
            None
        );
    }

    #[test]
    fn keeps_text_without_loops() {
        let t = "And so my fellow Americans, ask not what your country can do for you";
        assert_eq!(collapse_ngram_loops(t), None);
    }

    #[test]
    fn collapses_loop_in_middle_preserving_context() {
        let t = "So I said go to the go to the go to the go to the store please";
        assert_eq!(
            collapse_ngram_loops(t).as_deref(),
            Some("So I said go to the store please")
        );
    }

    // ---- token aggregation ----

    fn tok(text: &str, p: f32) -> TokenScore {
        TokenScore {
            text: text.to_owned(),
            plog: p.ln(),
        }
    }

    #[test]
    fn aggregates_average_logprob() {
        let c = aggregate_token_scores(&[tok(" Hello", 0.5), tok(" world", 0.5)]);
        let avg = c.avg_logprob.unwrap();
        assert!((avg - 0.5f32.ln()).abs() < 1e-5);
    }

    #[test]
    fn groups_subword_tokens_into_words_with_geometric_mean() {
        let c = aggregate_token_scores(&[
            tok(" Hel", 0.9),
            tok("lo", 0.4),
            tok(" there", 0.8),
            tok(".", 0.9),
        ]);
        assert_eq!(c.words.len(), 2);
        assert_eq!(c.words[0].word, "Hello");
        assert!((c.words[0].p - (0.9f32 * 0.4).sqrt()).abs() < 1e-4);
        assert_eq!(c.words[1].word, "there.");
    }

    #[test]
    fn first_token_without_leading_space_starts_a_word() {
        let c = aggregate_token_scores(&[tok("Hi", 0.9), tok(" you", 0.9)]);
        assert_eq!(
            c.words.iter().map(|w| w.word.as_str()).collect::<Vec<_>>(),
            ["Hi", "you"]
        );
    }

    #[test]
    fn empty_tokens_have_no_confidence() {
        assert_eq!(aggregate_token_scores(&[]), SegmentConfidence::default());
    }

    #[test]
    fn words_match_text_detects_rewrites() {
        let c = aggregate_token_scores(&[tok(" Hello", 0.9), tok(" world", 0.9)]);
        assert!(words_match_text(&c.words, "Hello world"));
        assert!(!words_match_text(&c.words, "Hello"));
        assert!(!words_match_text(&c.words, "Hello there"));
    }

    // ---- dictation trim + pad ----

    fn cfg() -> TrimConfig {
        TrimConfig::dictation_defaults()
    }

    #[test]
    fn no_speech_frames_is_no_speech() {
        assert_eq!(
            plan_dictation_trim(16_000, &[0.0; 31], &cfg()),
            TrimPlan::NoSpeech
        );
    }

    #[test]
    fn single_click_frame_is_no_speech() {
        let mut p = vec![0.0; 31];
        p[10] = 0.9;
        p[11] = 0.9;
        assert_eq!(plan_dictation_trim(16_000, &p, &cfg()), TrimPlan::NoSpeech);
    }

    #[test]
    fn trims_leading_and_trailing_silence_with_padding() {
        // 5 s clip: speech in frames 50..=80 (1.6 s – 2.6 s).
        let total = 80_000;
        let mut p = vec![0.0; total / 512];
        for f in p.iter_mut().take(81).skip(50) {
            *f = 0.9;
        }
        let plan = plan_dictation_trim(total, &p, &cfg());
        assert_eq!(
            plan,
            TrimPlan::Trim {
                start: 50 * 512 - 4_800,
                end: 81 * 512 + 6_400,
                pad_to: 20_000,
            }
        );
    }

    #[test]
    fn trim_window_is_clamped_to_clip_bounds() {
        let total = 8_000; // 0.5 s
        let p = vec![0.9; total / 512];
        match plan_dictation_trim(total, &p, &cfg()) {
            TrimPlan::Trim { start, end, .. } => {
                assert_eq!(start, 0);
                assert_eq!(end, total);
            }
            other => panic!("expected trim, got {other:?}"),
        }
    }

    #[test]
    fn apply_trim_pads_short_clips_with_trailing_zeros() {
        let samples = vec![0.5f32; 8_000];
        let out = apply_trim(&samples, 1_000, 7_000, 20_000);
        assert_eq!(out.len(), 20_000);
        assert!(out[..6_000].iter().all(|s| *s == 0.5));
        assert!(out[6_000..].iter().all(|s| *s == 0.0));
    }

    #[test]
    fn apply_trim_does_not_pad_long_clips() {
        let samples = vec![0.5f32; 40_000];
        let out = apply_trim(&samples, 0, 30_000, 20_000);
        assert_eq!(out.len(), 30_000);
    }

    #[test]
    #[allow(clippy::single_range_in_vec_init)]
    fn short_clips_are_scored_whole() {
        assert_eq!(edge_frame_ranges(100, 156), [0..100]);
        assert_eq!(edge_frame_ranges(312, 156), [0..312]);
    }

    #[test]
    fn long_clips_score_only_their_edges() {
        assert_eq!(edge_frame_ranges(1_000, 156), vec![0..156, 844..1_000]);
    }

    #[test]
    fn unscored_middle_counts_as_speech_and_edges_still_trim() {
        let ranges = edge_frame_ranges(1_000, 156);
        let head = vec![0.0; 156]; // silent lead-in
        let tail = vec![0.0; 156]; // silent tail
        let probs = assemble_edge_probs(1_000, &ranges, &[head, tail]);
        assert!(probs[156..844].iter().all(|p| *p == 1.0));
        match plan_dictation_trim(1_000 * 512, &probs, &cfg()) {
            TrimPlan::Trim { start, end, .. } => {
                assert_eq!(start, 156 * 512 - cfg().lead_pad);
                assert_eq!(end, 844 * 512 + cfg().tail_pad);
            }
            other => panic!("expected trim, got {other:?}"),
        }
    }
}
