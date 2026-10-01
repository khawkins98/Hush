// Confidence shading for the live meeting transcript (#1013).
//
// When the backend runs with `HUSH_CONFIDENCE_SHADING=1`, each
// in-flight partial carries word-level model confidence. These helpers
// turn that into renderable tokens so the live pane can dim words the
// model is unsure of — a cue that the text may still change, or that
// it's worth a re-listen. Finals are never shaded: they're persisted as
// plain text and arrive without confidences.
//
// Kept out of `transcript-format.ts` on purpose: that module owns the
// speaker-label rules, and this one only post-processes its output, so
// a label change there can't break shading (we fall back to plain text
// whenever the two disagree).

import type { StreamingUtterance, WordConfidence } from "./types";

/// Words below this probability render dimmed. Whisper's per-token p is
/// usually > 0.8 on clear speech; 0.5 flags genuinely shaky words
/// without dimming half of every sentence.
export const LOW_CONFIDENCE_P = 0.5;

export type ShadedToken = { text: string; low: boolean };

/// Split `text` into word + whitespace tokens, marking words whose
/// confidence is below `threshold`. Returns `null` when `words` doesn't
/// line up word-for-word with `text` (the backend rewrote the text, or
/// there are no confidences) so the caller renders plain text instead
/// of shading the wrong words.
export function shadeText(
  text: string,
  words: WordConfidence[] | undefined,
  threshold: number = LOW_CONFIDENCE_P,
): ShadedToken[] | null {
  if (!words || words.length === 0) return null;
  const parts = text.split(/(\s+)/).filter((p) => p.length > 0);
  const wordParts = parts.filter((p) => !/^\s+$/.test(p));
  if (wordParts.length !== words.length) return null;
  if (wordParts.some((w, i) => w !== words[i].word)) return null;
  let wi = 0;
  return parts.map((p) => {
    if (/^\s+$/.test(p)) return { text: p, low: false };
    const low = words[wi].p < threshold;
    wi += 1;
    return { text: p, low };
  });
}

export type ShadedLine = { label: string; tokens: ShadedToken[] };
export type ShadedTranscript = { head: string; tail: ShadedLine[] };

/// Split the already-joined live transcript (`joinUtterances(finals +
/// partials, "\n")`) into a plain head (the finals) and shaded tail
/// lines (the partials). Each partial owns the last lines of `full`, in
/// order, and each such line ends with that partial's text — whatever
/// label prefix `joinUtterances` chose stays as an opaque `label`.
///
/// Returns `null` (render plain) when no partial carries confidences or
/// the line structure doesn't match what we expect.
export function shadeLiveTranscript(
  full: string,
  partials: StreamingUtterance[],
  threshold: number = LOW_CONFIDENCE_P,
): ShadedTranscript | null {
  if (partials.length === 0) return null;
  if (!partials.some((p) => p.words && p.words.length > 0)) return null;
  if (partials.some((p) => p.text.includes("\n"))) return null;
  const lines = full.split("\n");
  if (lines.length < partials.length) return null;
  const headCount = lines.length - partials.length;
  const tail: ShadedLine[] = [];
  for (let i = 0; i < partials.length; i++) {
    const line = lines[headCount + i];
    const p = partials[i];
    if (!line.endsWith(p.text)) return null;
    const label = line.slice(0, line.length - p.text.length);
    const tokens = shadeText(p.text, p.words, threshold) ?? [{ text: p.text, low: false }];
    tail.push({ label, tokens });
  }
  return { head: lines.slice(0, headCount).join("\n"), tail };
}
