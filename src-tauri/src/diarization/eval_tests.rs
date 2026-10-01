//! Offline evaluation harness for the #1013 diarizer changes.
//!
//! `#[ignore]`d: needs the wespeaker model and a directory of labelled
//! speech. Not a regression test — it prints the numbers recorded in
//! learnings.md (same- vs cross-speaker distance distributions, the
//! duration study behind the gate constants, cross-session centroid
//! distances behind `AUTO_ACCEPT_THRESHOLD`, and synthetic-conversation
//! speaker-error / cluster-count tables behind the thresholds).
//!
//! ```text
//! HUSH_DIARIZATION_MODEL_PATH=…/voxceleb_resnet34_LM.onnx \
//! HUSH_EVAL_WAV_DIR=…/wav            # <speaker>-<chapter>-<utt>.wav, 16 kHz mono s16
//! HUSH_EVAL_WAV_DIR_DEGRADED=…/wav_opus   # optional, same names
//! cargo test --release --lib --no-default-features --features diarization-onnx \
//!   diarization::onnx::eval_tests -- --ignored --nocapture
//! ```
//!
//! Used during #1013 with Mini LibriSpeech dev-clean-2 (OpenSLR 31,
//! CC BY 4.0), optionally re-encoded through Opus 16 kb/s VoIP mode as
//! a stand-in for a conferencing stream.

use std::collections::HashMap;
use std::path::Path;

use super::*;
use crate::diarization::cluster::cosine_distance;
use crate::diarization::features::FbankConfig;
use crate::diarization::session::{ChunkMeta, ClusterPolicy, SessionClusterState};

/// Per (speaker, chapter) cap. LibriSpeech chapters are separate
/// recording sessions, so capping per chapter (not per speaker) keeps
/// the multi-chapter speakers' second sessions in the set for the
/// cross-session study.
const MAX_UTTS_PER_CHAPTER: usize = 12;
/// Crop durations (seconds) embedded per utterance; `f32::INFINITY` = full.
const CROPS: [f32; 6] = [0.8, 1.2, 2.0, 3.0, 5.0, f32::INFINITY];

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum FrontEnd {
    /// Pre-#1013: Povey, [-1,1], no CMN.
    Legacy,
    /// Pre-#1013 extractor + CMN only.
    LegacyCmn,
    /// Kaldi-compatible extractor + CMN (production).
    Wespeaker,
}

impl FrontEnd {
    fn config(self) -> (FbankConfig, bool) {
        match self {
            FrontEnd::Legacy => (FbankConfig::LEGACY, false),
            FrontEnd::LegacyCmn => (FbankConfig::LEGACY, true),
            FrontEnd::Wespeaker => (FbankConfig::WESPEAKER, true),
        }
    }
}

struct Utt {
    speaker: String,
    chapter: String,
    /// Per crop: (actual duration secs, embedding)
    crops: Vec<(f32, Vec<f32>)>,
}

fn load_wav(path: &Path) -> Vec<f32> {
    let mut reader = hound::WavReader::open(path).expect("open wav");
    assert_eq!(reader.spec().sample_rate, 16_000, "{}", path.display());
    reader
        .samples::<i16>()
        .map(|s| f32::from(s.expect("sample")) / 32768.0)
        .collect()
}

fn list_utts(dir: &Path) -> Vec<(String, String, std::path::PathBuf)> {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .expect("read wav dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "wav"))
        .collect();
    files.sort();
    let mut per_chapter: HashMap<(String, String), usize> = HashMap::new();
    let mut out = Vec::new();
    for p in files {
        let stem = p.file_stem().unwrap().to_string_lossy().to_string();
        let mut parts = stem.split('-');
        let spk = parts.next().unwrap().to_owned();
        let chap = parts.next().unwrap().to_owned();
        let n = per_chapter.entry((spk.clone(), chap.clone())).or_default();
        if *n >= MAX_UTTS_PER_CHAPTER {
            continue;
        }
        *n += 1;
        out.push((spk, chap, p));
    }
    out
}

/// [`embed_all`] with an optional on-disk cache (`HUSH_EVAL_CACHE=<dir>`)
/// so re-running the analysis doesn't re-embed thousands of crops.
fn embed_all_cached(model: &TypedRunnableModel<TypedModel>, dir: &Path, fe: FrontEnd) -> Vec<Utt> {
    let Ok(cache_dir) = std::env::var("HUSH_EVAL_CACHE") else {
        return embed_all(model, dir, fe);
    };
    let stem = format!(
        "{}-{fe:?}",
        dir.file_name()
            .map_or("set".into(), |n| n.to_string_lossy())
    );
    let meta_path = Path::new(&cache_dir).join(format!("{stem}.txt"));
    let bin_path = Path::new(&cache_dir).join(format!("{stem}.bin"));
    if let (Ok(meta), Ok(bin)) = (
        std::fs::read_to_string(&meta_path),
        std::fs::read(&bin_path),
    ) {
        let floats: Vec<f32> = bin
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        let per_crop = 1 + EMBEDDING_DIM;
        let mut out = Vec::new();
        for (i, line) in meta.lines().enumerate() {
            let mut parts = line.split(' ');
            let speaker = parts.next().unwrap().to_owned();
            let chapter = parts.next().unwrap().to_owned();
            let base = i * CROPS.len() * per_crop;
            let crops = (0..CROPS.len())
                .map(|c| {
                    let o = base + c * per_crop;
                    (floats[o], floats[o + 1..o + per_crop].to_vec())
                })
                .collect();
            out.push(Utt {
                speaker,
                chapter,
                crops,
            });
        }
        return out;
    }
    let utts = embed_all(model, dir, fe);
    let meta: String = utts
        .iter()
        .map(|u| format!("{} {}\n", u.speaker, u.chapter))
        .collect();
    let mut bin = Vec::new();
    for u in &utts {
        for (d, e) in &u.crops {
            bin.extend_from_slice(&d.to_le_bytes());
            for v in e {
                bin.extend_from_slice(&v.to_le_bytes());
            }
        }
    }
    std::fs::create_dir_all(&cache_dir).expect("cache dir");
    std::fs::write(&meta_path, meta).expect("write cache meta");
    std::fs::write(&bin_path, bin).expect("write cache bin");
    utts
}

fn embed_all(model: &TypedRunnableModel<TypedModel>, dir: &Path, fe: FrontEnd) -> Vec<Utt> {
    let (cfg, cmn) = fe.config();
    let list = list_utts(dir);
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    let chunk = list.len().div_ceil(threads);
    let mut out: Vec<Utt> = Vec::with_capacity(list.len());
    std::thread::scope(|scope| {
        let handles: Vec<_> = list
            .chunks(chunk)
            .map(|part| {
                scope.spawn(move || {
                    let mel = MelExtractor::with_config(cfg);
                    part.iter()
                        .map(|(spk, chap, path)| {
                            let pcm = load_wav(path);
                            let crops = CROPS
                                .iter()
                                .map(|&secs| {
                                    let want = if secs.is_finite() {
                                        ((secs * 16_000.0) as usize).min(pcm.len())
                                    } else {
                                        pcm.len()
                                    };
                                    // Take the crop from the middle, so it
                                    // is mostly speech rather than lead-in.
                                    let start = (pcm.len() - want) / 2;
                                    let slice = &pcm[start..start + want];
                                    let emb =
                                        embed_features(model, &mel, slice, cmn).expect("embed");
                                    (want as f32 / 16_000.0, emb)
                                })
                                .collect();
                            Utt {
                                speaker: spk.clone(),
                                chapter: chap.clone(),
                                crops,
                            }
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        for h in handles {
            out.extend(h.join().expect("embed thread"));
        }
    });
    out
}

fn pct(sorted: &[f32], p: f32) -> f32 {
    if sorted.is_empty() {
        return f32::NAN;
    }
    let idx = ((sorted.len() - 1) as f32 * p).round() as usize;
    sorted[idx]
}

/// Equal error rate and the threshold that achieves it.
fn eer(same: &[f32], diff: &[f32]) -> (f32, f32) {
    let mut best = (1.0_f32, 0.0_f32);
    let mut gap = f32::MAX;
    for i in 0..=200 {
        let t = i as f32 / 100.0;
        let frr = same.iter().filter(|&&d| d > t).count() as f32 / same.len() as f32;
        let far = diff.iter().filter(|&&d| d <= t).count() as f32 / diff.len() as f32;
        if (frr - far).abs() < gap {
            gap = (frr - far).abs();
            best = ((frr + far) / 2.0, t);
        }
    }
    best
}

fn summarise(name: &str, same: &mut [f32], diff: &mut [f32]) {
    same.sort_by(|a, b| a.partial_cmp(b).unwrap());
    diff.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let (e, t) = eer(same, diff);
    eprintln!(
        "  {name:<28} same p5/p50/p95 = {:.3}/{:.3}/{:.3}   cross p5/p50/p95 = {:.3}/{:.3}/{:.3}   EER {:.1}% @ {:.2}",
        pct(same, 0.05),
        pct(same, 0.5),
        pct(same, 0.95),
        pct(diff, 0.05),
        pct(diff, 0.5),
        pct(diff, 0.95),
        e * 100.0,
        t
    );
}

fn pair_distances(utts: &[Utt], crop: usize) -> (Vec<f32>, Vec<f32>) {
    let mut same = Vec::new();
    let mut diff = Vec::new();
    for i in 0..utts.len() {
        for j in (i + 1)..utts.len() {
            let d = cosine_distance(&utts[i].crops[crop].1, &utts[j].crops[crop].1);
            if utts[i].speaker == utts[j].speaker {
                same.push(d);
            } else {
                diff.push(d);
            }
        }
    }
    (same, diff)
}

fn unit_mean(vs: &[&Vec<f32>]) -> Vec<f32> {
    let mut sum = vec![0.0; EMBEDDING_DIM];
    for v in vs {
        let n = crate::diarization::session::normalised(v);
        for (s, x) in sum.iter_mut().zip(&n) {
            *s += x;
        }
    }
    crate::diarization::session::normalised(&sum)
}

/// Distance-to-own-centroid vs nearest-other-centroid per crop length.
fn duration_study(utts: &[Utt]) {
    let speakers: Vec<String> = {
        let mut s: Vec<String> = utts.iter().map(|u| u.speaker.clone()).collect();
        s.sort();
        s.dedup();
        s
    };
    for (ci, secs) in CROPS.iter().enumerate() {
        let mut own = Vec::new();
        let mut confused = 0usize;
        for (i, u) in utts.iter().enumerate() {
            // Leave-one-out centroids from full-length utterances.
            let mut best_other = f32::MAX;
            let mut own_d = f32::NAN;
            for spk in &speakers {
                let members: Vec<&Vec<f32>> = utts
                    .iter()
                    .enumerate()
                    .filter(|(j, v)| *j != i && &v.speaker == spk)
                    .map(|(_, v)| &v.crops[CROPS.len() - 1].1)
                    .collect();
                if members.is_empty() {
                    continue;
                }
                let d = cosine_distance(&u.crops[ci].1, &unit_mean(&members));
                if spk == &u.speaker {
                    own_d = d;
                } else {
                    best_other = best_other.min(d);
                }
            }
            own.push(own_d);
            if best_other < own_d {
                confused += 1;
            }
        }
        own.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let label = if secs.is_finite() {
            format!("{secs:.1}s")
        } else {
            "full".to_owned()
        };
        eprintln!(
            "  crop {label:>5}: own-centroid dist p50 {:.3} p90 {:.3}   nearest-centroid is wrong speaker: {:.1}%",
            pct(&own, 0.5),
            pct(&own, 0.9),
            100.0 * confused as f32 / utts.len() as f32
        );
    }
}

// AS-norm was evaluated for #1013 item 8 and NOT adopted: with CMN the raw
// cross-session distances already separate cleanly (0 false merges at the
// 0.25 auto-accept threshold, including clean<->Opus channel shift), and
// AS-norm over a small cohort turned that into 5-53 false merges at
// thresholds accepting every genuine pair. Kept here so the comparison is
// reproducible if a future model / real-call data changes the picture.
/// Cohort size below which [`as_norm_score`] declines to normalise: with
/// only a handful of impostor scores the mean/std estimates are noise.
const AS_NORM_MIN_COHORT: usize = 4;

/// Number of highest cohort scores AS-norm averages over ("adaptive"
/// means only the most similar impostors count).
const AS_NORM_TOP_N: usize = 10;

/// Adaptive symmetric score normalisation (AS-norm; Matejka et al.,
/// Interspeech 2017; also the wespeaker / 3D-Speaker scoring back ends,
/// Apache-2.0 — implemented here from the paper's description).
///
/// A raw cosine score between a session centroid and a stored voiceprint
/// shifts with the recording channel: the same person on a headset and
/// on a Zoom mixdown scores lower than either does against itself, while
/// two people on the same codec score higher. AS-norm re-expresses the
/// score relative to how each side scores against a cohort of *other*
/// speakers:
///
/// `½·[(s − μ_e)/σ_e + (s − μ_t)/σ_t]`
///
/// where `μ_e, σ_e` are the mean/std of the enrolment voiceprint's top-N
/// cohort similarities and `μ_t, σ_t` the same for the test centroid. A
/// channel shift that lowers every score of the test centroid lowers its
/// cohort statistics too, and largely cancels. Returns `None` when the
/// cohort is smaller than [`AS_NORM_MIN_COHORT`].
fn as_norm_score(test: &[f32], enrol: &[f32], cohort: &[&[f32]]) -> Option<f32> {
    if cohort.len() < AS_NORM_MIN_COHORT {
        return None;
    }
    let sim = |a: &[f32], b: &[f32]| 1.0 - cosine_distance(a, b);
    let stats = |x: &[f32]| {
        let mut scores: Vec<f32> = cohort.iter().map(|c| sim(x, c)).collect();
        scores.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
        scores.truncate(AS_NORM_TOP_N);
        let n = scores.len() as f32;
        let mean = scores.iter().sum::<f32>() / n;
        let var = scores.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / n;
        // Floor σ so a near-degenerate cohort can't blow the score up.
        (mean, var.sqrt().max(0.02))
    };
    let s = sim(test, enrol);
    let (me, se) = stats(enrol);
    let (mt, st) = stats(test);
    Some(0.5 * ((s - me) / se + (s - mt) / st))
}

/// Session-like centroids: disjoint groups of 5 full utterances per
/// chapter (5 = `MIN_UTTERANCE_COUNT_FOR_MATCH`), tagged with speaker,
/// chapter and recording condition.
struct SessionCentroid {
    speaker: String,
    chapter: String,
    condition: &'static str,
    centroid: Vec<f32>,
}

fn session_centroids(utts: &[Utt], condition: &'static str) -> Vec<SessionCentroid> {
    let mut by: HashMap<(String, String), Vec<&Vec<f32>>> = HashMap::new();
    for u in utts {
        by.entry((u.speaker.clone(), u.chapter.clone()))
            .or_default()
            .push(&u.crops[CROPS.len() - 1].1);
    }
    let mut keys: Vec<_> = by.keys().cloned().collect();
    keys.sort();
    let mut out = Vec::new();
    for k in keys {
        for group in by[&k].chunks_exact(5) {
            out.push(SessionCentroid {
                speaker: k.0.clone(),
                chapter: k.1.clone(),
                condition,
                centroid: unit_mean(group),
            });
        }
    }
    out
}

/// Cross-session identity matching: centroid-vs-centroid distances for
/// the same speaker in a *different* chapter (a later meeting) vs
/// different speakers, raw and AS-normalised. `pairs_filter` picks
/// which condition pairs to score (clean↔clean, clean↔opus).
fn cross_session_study(
    all: &[SessionCentroid],
    label: &str,
    pairs_filter: impl Fn(&str, &str) -> bool,
) {
    let mut same = Vec::new();
    let mut diff = Vec::new();
    let mut same_norm = Vec::new();
    let mut diff_norm = Vec::new();
    for (i, a) in all.iter().enumerate() {
        for (j, b) in all.iter().enumerate() {
            if j <= i || !pairs_filter(a.condition, b.condition) {
                continue;
            }
            let same_spk = a.speaker == b.speaker;
            if same_spk && a.chapter == b.chapter {
                continue; // same session: not a cross-session trial
            }
            let d = cosine_distance(&a.centroid, &b.centroid);
            // Cohort: every centroid of a third speaker (what the
            // identity store holds besides the candidate).
            let cohort: Vec<&[f32]> = all
                .iter()
                .filter(|c| c.speaker != a.speaker && c.speaker != b.speaker)
                .map(|c| c.centroid.as_slice())
                .collect();
            let n = as_norm_score(&a.centroid, &b.centroid, &cohort).unwrap_or(f32::NAN);
            if same_spk {
                same.push(d);
                same_norm.push(n);
            } else {
                diff.push(d);
                diff_norm.push(n);
            }
        }
    }
    for v in [&mut same, &mut diff, &mut same_norm, &mut diff_norm] {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    }
    eprintln!(
        "  {label}: {} same-speaker cross-session pairs (distance max {:.3}, p90 {:.3}, p50 {:.3}); {} cross-speaker pairs (min {:.3}, p1 {:.3}, p50 {:.3})",
        same.len(),
        same.last().copied().unwrap_or(f32::NAN),
        pct(&same, 0.9),
        pct(&same, 0.5),
        diff.len(),
        diff.first().copied().unwrap_or(f32::NAN),
        pct(&diff, 0.01),
        pct(&diff, 0.5)
    );
    for t in [0.2_f32, 0.25, 0.3, 0.35, 0.4, 0.45] {
        eprintln!(
            "    raw auto-accept @ {t:.2}: same-speaker accepted {:5.1}%  false merges {}/{}",
            100.0 * same.iter().filter(|&&d| d <= t).count() as f32 / same.len().max(1) as f32,
            diff.iter().filter(|&&d| d <= t).count(),
            diff.len()
        );
    }
    eprintln!(
        "    AS-norm: same p10/p50 {:.2}/{:.2}   cross p50/p99/max {:.2}/{:.2}/{:.2}",
        pct(&same_norm, 0.1),
        pct(&same_norm, 0.5),
        pct(&diff_norm, 0.5),
        pct(&diff_norm, 0.99),
        diff_norm.last().copied().unwrap_or(f32::NAN)
    );
    for t in [2.0_f32, 3.0, 4.0, 5.0] {
        eprintln!(
            "    AS-norm accept @ {t:.1}: same-speaker accepted {:5.1}%  false merges {}/{}",
            100.0 * same_norm.iter().filter(|&&n| n >= t).count() as f32
                / same_norm.len().max(1) as f32,
            diff_norm.iter().filter(|&&n| n >= t).count(),
            diff_norm.len()
        );
    }
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// One synthetic conversation: (reference speaker index, utt index, crop index).
fn make_conversation(rng: &mut Rng, by_spk: &[Vec<usize>], k: usize) -> Vec<(usize, usize, usize)> {
    let mut spks: Vec<usize> = (0..by_spk.len()).collect();
    for i in (1..spks.len()).rev() {
        spks.swap(i, rng.below(i + 1));
    }
    let chosen = &spks[..k];
    let turns = 40 + rng.below(41);
    let mut out = Vec::new();
    let mut cur = rng.below(k);
    for _ in 0..turns {
        if k > 1 && rng.below(100) < 70 {
            let mut nxt = rng.below(k - 1);
            if nxt >= cur {
                nxt += 1;
            }
            cur = nxt;
        }
        let spk = chosen[cur];
        let utt = by_spk[spk][rng.below(by_spk[spk].len())];
        // ~30% short turns (backchannels, "yeah", "right").
        let crop = if rng.below(100) < 30 {
            rng.below(2) // 0.8 s / 1.2 s
        } else {
            2 + rng.below(4) // 2 s .. full
        };
        out.push((cur, utt, crop));
    }
    out
}

#[derive(Default, Clone, Copy)]
struct Score {
    error_secs: f32,
    total_secs: f32,
    abstain_secs: f32,
    clusters: usize,
    exact_count: usize,
    convs: usize,
}

impl Score {
    fn add(&mut self, other: Score) {
        self.error_secs += other.error_secs;
        self.total_secs += other.total_secs;
        self.abstain_secs += other.abstain_secs;
        self.clusters += other.clusters;
        self.exact_count += other.exact_count;
        self.convs += other.convs;
    }
}

/// Duration-weighted speaker error with a greedy one-to-one mapping
/// from hypothesis clusters to reference speakers. Abstentions count
/// as errors (they render as the generic "Remote" label).
fn score(reference: &[usize], hyp: &[Option<usize>], durs: &[f32], k: usize) -> Score {
    let mut overlap: HashMap<(usize, usize), f32> = HashMap::new();
    let mut total = 0.0;
    let mut abstain = 0.0;
    for ((r, h), d) in reference.iter().zip(hyp).zip(durs) {
        total += d;
        match h {
            Some(h) => *overlap.entry((*h, *r)).or_default() += d,
            None => abstain += d,
        }
    }
    let mut pairs: Vec<((usize, usize), f32)> = overlap.into_iter().collect();
    pairs.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    let mut used_h = Vec::new();
    let mut used_r = Vec::new();
    let mut matched = 0.0;
    for ((h, r), d) in pairs {
        if !used_h.contains(&h) && !used_r.contains(&r) {
            used_h.push(h);
            used_r.push(r);
            matched += d;
        }
    }
    let mut hs: Vec<usize> = hyp.iter().flatten().copied().collect();
    hs.sort();
    hs.dedup();
    Score {
        error_secs: total - matched,
        total_secs: total,
        abstain_secs: abstain,
        clusters: hs.len(),
        exact_count: usize::from(hs.len() == k),
        convs: 1,
    }
}

#[derive(Clone, Copy)]
struct Pipeline {
    policy: ClusterPolicy,
    threshold: f32,
    /// `Some(offset)` runs the session-end re-cluster.
    recluster: Option<f32>,
}

/// Run one conversation through the online matcher (+ optional
/// re-cluster); returns (online score, final score).
fn run_conversation(
    conv: &[(usize, usize, usize)],
    utts: &[Utt],
    p: Pipeline,
    k: usize,
) -> (Score, Score) {
    let mut state = SessionClusterState::with_policy(p.threshold, p.policy);
    let mut hyp = Vec::new();
    let mut durs = Vec::new();
    let mut reference = Vec::new();
    let mut t = 0_u64;
    for &(r, u, c) in conv {
        let (dur, emb) = &utts[u].crops[c];
        let meta = ChunkMeta {
            duration_secs: *dur,
            hint: crate::diarization::ChunkHint::default(),
            started_at_ms: t,
            ended_at_ms: t + (*dur * 1000.0) as u64,
        };
        t += (*dur * 1000.0) as u64 + 300;
        hyp.push(state.assign(emb, meta));
        durs.push(*dur);
        reference.push(r);
    }
    let online = score(&reference, &hyp, &durs, k);
    let Some(offset) = p.recluster else {
        return (online, online);
    };
    let relabels = state.recluster((p.threshold + offset).clamp(0.0, 2.0));
    // Apply relabels by start time.
    let mut by_start: HashMap<u64, String> = HashMap::new();
    for r in relabels {
        by_start.insert(r.started_at_ms, r.new_label);
    }
    let mut t = 0_u64;
    let mut final_hyp = Vec::new();
    for (i, &(_, u, c)) in conv.iter().enumerate() {
        let dur = utts[u].crops[c].0;
        let id = match by_start.get(&t) {
            Some(label) => label
                .strip_prefix("Speaker ")
                .and_then(|n| n.parse::<usize>().ok())
                .map(|n| n - 1),
            None => hyp[i],
        };
        final_hyp.push(id);
        t += (dur * 1000.0) as u64 + 300;
    }
    (online, score(&reference, &final_hyp, &durs, k))
}

fn conversation_study(utts: &[Utt], label: &str, pipelines: &[(String, Pipeline)]) {
    let mut speakers: Vec<String> = utts.iter().map(|u| u.speaker.clone()).collect();
    speakers.sort();
    speakers.dedup();
    let by_spk: Vec<Vec<usize>> = speakers
        .iter()
        .map(|s| (0..utts.len()).filter(|&i| &utts[i].speaker == s).collect())
        .collect();
    eprintln!("\n== synthetic conversations ({label}): speaker error % (abstain %) / mean clusters / exact-count % ==");
    for k in [1_usize, 2, 3, 4, 5] {
        eprintln!("  K={k}");
        for (name, p) in pipelines {
            let mut rng = Rng(0x9E37_79B9_7F4A_7C15 ^ k as u64);
            let mut online = Score::default();
            let mut fin = Score::default();
            for _ in 0..60 {
                let conv = make_conversation(&mut rng, &by_spk, k);
                let (o, f) = run_conversation(&conv, utts, *p, k);
                online.add(o);
                fin.add(f);
            }
            let fmt = |s: &Score| {
                format!(
                    "{:5.1}% ({:4.1}%) / {:4.2} / {:3.0}%",
                    100.0 * s.error_secs / s.total_secs,
                    100.0 * s.abstain_secs / s.total_secs,
                    s.clusters as f32 / s.convs as f32,
                    100.0 * s.exact_count as f32 / s.convs as f32
                )
            };
            if p.recluster.is_some() {
                eprintln!(
                    "    {name:<34} online {}   re-clustered {}",
                    fmt(&online),
                    fmt(&fin)
                );
            } else {
                eprintln!("    {name:<34} online {}", fmt(&online));
            }
        }
    }
}

#[test]
#[ignore]
fn diarizer_evaluation_report() {
    let (Ok(model_path), Ok(wav_dir)) = (
        std::env::var("HUSH_DIARIZATION_MODEL_PATH"),
        std::env::var("HUSH_EVAL_WAV_DIR"),
    ) else {
        eprintln!("skipping: set HUSH_DIARIZATION_MODEL_PATH and HUSH_EVAL_WAV_DIR");
        return;
    };
    let model = build_tract_model(Path::new(&model_path)).expect("load model");

    let mut sets: Vec<(String, FrontEnd, Vec<Utt>)> = Vec::new();
    for fe in [FrontEnd::Legacy, FrontEnd::LegacyCmn, FrontEnd::Wespeaker] {
        let t0 = std::time::Instant::now();
        let utts = embed_all_cached(&model, Path::new(&wav_dir), fe);
        eprintln!(
            "embedded {} utts × {} crops with {fe:?} in {:?}",
            utts.len(),
            CROPS.len(),
            t0.elapsed()
        );
        sets.push(("clean".to_owned(), fe, utts));
    }
    if let Ok(deg) = std::env::var("HUSH_EVAL_WAV_DIR_DEGRADED") {
        for fe in [FrontEnd::Legacy, FrontEnd::Wespeaker] {
            let utts = embed_all_cached(&model, Path::new(&deg), fe);
            sets.push(("opus16k".to_owned(), fe, utts));
        }
    }

    eprintln!("\n== pairwise cosine distance, same vs cross speaker ==");
    for (cond, fe, utts) in &sets {
        for (ci, name) in [(CROPS.len() - 1, "full"), (2, "2.0s"), (1, "1.2s")] {
            let (mut s, mut d) = pair_distances(utts, ci);
            summarise(&format!("{cond}/{fe:?}/{name}"), &mut s, &mut d);
        }
    }

    eprintln!("\n== duration study (leave-one-out speaker centroids) ==");
    for (cond, fe, utts) in &sets {
        if *fe == FrontEnd::LegacyCmn {
            continue;
        }
        eprintln!(" {cond}/{fe:?}");
        duration_study(utts);
    }

    eprintln!("\n== cross-session identity (5-utterance centroids, different chapter) ==");
    for fe in [FrontEnd::Legacy, FrontEnd::Wespeaker] {
        let mut all: Vec<SessionCentroid> = Vec::new();
        for (cond, f, utts) in &sets {
            if *f == fe {
                let tag: &'static str = if cond == "clean" { "clean" } else { "opus" };
                all.extend(session_centroids(utts, tag));
            }
        }
        cross_session_study(&all, &format!("{fe:?} clean<->clean"), |a, b| {
            a == "clean" && b == "clean"
        });
        cross_session_study(&all, &format!("{fe:?} opus<->opus"), |a, b| {
            a == "opus" && b == "opus"
        });
        cross_session_study(
            &all,
            &format!("{fe:?} clean<->opus (channel shift)"),
            |a, b| a != b,
        );
    }

    let legacy = |t: f32| Pipeline {
        policy: ClusterPolicy::LEGACY,
        threshold: t,
        recluster: None,
    };
    let gated = |t: f32, rc: Option<f32>| Pipeline {
        policy: ClusterPolicy::default(),
        threshold: t,
        recluster: rc,
    };
    let only_gate = |t: f32| Pipeline {
        policy: ClusterPolicy {
            duration_gate: true,
            unit_sum_centroid: false,
        },
        threshold: t,
        recluster: None,
    };
    let only_unit = |t: f32| Pipeline {
        policy: ClusterPolicy {
            duration_gate: false,
            unit_sum_centroid: true,
        },
        threshold: t,
        recluster: None,
    };
    for (cond, fe, utts) in &sets {
        let mut pipes: Vec<(String, Pipeline)> = Vec::new();
        match fe {
            FrontEnd::Legacy => {
                for t in [0.4_f32, 0.6] {
                    pipes.push((format!("legacy matcher @{t:.2}"), legacy(t)));
                }
            }
            FrontEnd::LegacyCmn => continue,
            FrontEnd::Wespeaker => {
                for t in [0.4_f32, 0.5, 0.6, 0.7] {
                    pipes.push((format!("legacy matcher @{t:.2}"), legacy(t)));
                }
                for t in [0.5_f32, 0.6] {
                    pipes.push((format!("gate only @{t:.2}"), only_gate(t)));
                    pipes.push((format!("unit-sum only @{t:.2}"), only_unit(t)));
                }
                for t in [0.4_f32, 0.5, 0.6, 0.7] {
                    pipes.push((format!("gate+unit @{t:.2}"), gated(t, None)));
                }
                for t in [0.5_f32, 0.6] {
                    for off in [0.0_f32, 0.1, 0.2] {
                        pipes.push((
                            format!("gate+unit @{t:.2} + AHC +{off:.1}"),
                            gated(t, Some(off)),
                        ));
                    }
                }
            }
        }
        conversation_study(utts, &format!("{cond}/{fe:?}"), &pipes);
    }
}
