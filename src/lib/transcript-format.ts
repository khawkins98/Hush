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

/** Source tags the backend writes when the diarizer abstains. */
function isSourceTag(label: string | null): boolean {
  return label === "mic" || label === "system";
}

/**
 * In-room voices beyond the local user (opt-in in-room separation) are
 * labelled `"In-room N"` by the backend — a separate family precisely so
 * they can be told apart from remote `"Speaker N"` clusters here, since
 * utterances carry no source field.
 */
function isInRoomLabel(label: string | null): boolean {
  return label !== null && label.startsWith("In-room ");
}

/**
 * Give in-flight remote partials the label their final will most likely
 * land with (#1013).
 *
 * The diarizer only runs on finals, so a system-audio partial always
 * carries the raw `"system"` tag ("Remote") and then flips to
 * `"Speaker 2"` the moment it finalizes — every remote line visibly
 * changes speaker mid-sentence. Once the session has at least one
 * diarized remote final, a `"system"` partial borrows the most recent
 * diarized label instead: in conversation the current talker is usually
 * the last one, and when that guess is wrong the final corrects it in
 * place, which reads better than flipping every line.
 *
 * With no diarized finals yet (diarizer off, no model, or nobody has
 * spoken long enough) partials keep `"system"` → "Remote". Mic partials
 * are untouched: the channel already says they're the local user.
 * Returns new objects; the inputs are not mutated.
 */
export function resolvePartialLabels<T extends UtteranceLike>(
  finals: readonly T[],
  partials: readonly T[],
): T[] {
  let lastDiarized: string | null = null;
  for (const u of finals) {
    // Only remote clusters are candidates: a remote partial must never
    // borrow an in-room voice's label.
    if (u.speakerLabel && !isSourceTag(u.speakerLabel) && !isInRoomLabel(u.speakerLabel)) {
      lastDiarized = u.speakerLabel;
    }
  }
  if (lastDiarized === null) return [...partials];
  const label = lastDiarized;
  return partials.map((p) =>
    p.speakerLabel === "system" ? { ...p, speakerLabel: label } : p,
  );
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
