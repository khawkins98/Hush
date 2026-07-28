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
 * Map a backend speaker label to user-facing copy.
 *
 * The backend writes source-derived tags (`"mic"` / `"system"`) when
 * the diarizer abstains, and model-derived ones (`"Speaker 1"`, or a
 * resolved identity name) when it doesn't. Only the source-derived
 * pair needs translating; everything else passes through.
 *
 * Shared rather than duplicated because #1003 made mixed vocabulary
 * reachable in a single session: channel-based separation leaves mic
 * utterances on the `"mic"` tag while remote ones still get
 * `"Speaker N"`. Before that change a mic+system meeting had every
 * source diarized, so the raw labels happened to be consistent and
 * the live pane could get away with printing them verbatim. It can't
 * now — without this the transcript and clipboard read
 * `"mic: …"` / `"Speaker 1: …"` side by side.
 */
export function speakerDisplayLabel(label: string | null): string | null {
  switch (label) {
    case "mic":
      return "You";
    case "system":
      return "Remote";
    default:
      return label;
  }
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
    .map((u) => {
      const label = showLabels ? speakerDisplayLabel(u.speakerLabel) : null;
      return label ? `${label}: ${u.text}` : u.text;
    })
    .join(separator);
}
