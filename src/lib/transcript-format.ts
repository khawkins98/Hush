// Shared transcript-formatting helpers (#478).
//
// The "show speaker labels?" decision is the same in three places:
// the live transcript pane (RecordPanel.svelte), the meeting-mode
// auto-copy clipboard text (+page.svelte's stop_manual completion
// handler), and the History row's inline-transcript expansion
// (HistoryMeetingRow.svelte). Extracted here so the rule is
// expressed once.
//
// The rule: render labels iff ≥2 distinct labels appear across the
// utterance list. Single-speaker sessions (one person dictating,
// the diarizer labelling everything as "Speaker A" or just "mic")
// would otherwise repeat the same label on every line, which the
// eye reads as noise. Once a second speaker is detected the labels
// become useful turn-taking context for the prior lines too, so
// we apply the decision uniformly across the whole transcript.

export interface UtteranceLike {
  text: string;
  speakerLabel: string | null;
}

/**
 * Decide whether speaker labels should be rendered for a session.
 * Returns `true` when at least two distinct non-empty speaker
 * labels are present in the utterance list.
 */
export function shouldShowSpeakerLabels(utterances: UtteranceLike[]): boolean {
  const distinct = new Set(
    utterances.map((u) => u.speakerLabel).filter((l): l is string => !!l),
  );
  return distinct.size >= 2;
}

/**
 * Join an utterance list into the multi-line clipboard / live-
 * preview format. `separator` is `"\n\n"` for clipboard copy and
 * `"\n"` for the live transcript pane (denser, fits the side panel).
 *
 * When `shouldShowSpeakerLabels` decides labels are noise, the
 * output is the bare `text` lines; otherwise each line is prefixed
 * `"<label>: <text>"` (or just `<text>` when an individual
 * utterance has no label, e.g. a partial that hasn't been
 * diarized yet).
 */
export function joinUtterances(
  utterances: UtteranceLike[],
  separator: string,
): string {
  if (utterances.length === 0) return "";
  const showLabels = shouldShowSpeakerLabels(utterances);
  return utterances
    .map((u) =>
      showLabels && u.speakerLabel
        ? `${u.speakerLabel}: ${u.text}`
        : u.text,
    )
    .join(separator);
}
