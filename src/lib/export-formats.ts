/**
 * Single-dictation export-format conversions (#427 Item 4).
 *
 * Pure transformations from a `DictationResult`'s text + duration
 * into the four formats the export picker offers. Lives separately
 * from `ResultBlock.svelte` so the conversions are unit-testable
 * and reusable if the picker later moves to the meeting-session
 * surface (which DOES have per-segment word-level timestamps —
 * the SRT/VTT helpers here treat the dictation as a single
 * timecoded block since that's all a `DictationResult` provides).
 *
 * The bulk-history exporter for meeting sessions
 * (`ExportOptionsDialog.svelte`) is a separate path with its own
 * format set (TXT/CSV/JSON); these helpers are deliberately scoped
 * to single-dictation output.
 */

export type ExportFormat = "plain" | "markdown" | "srt" | "vtt";

/**
 * Pretty label for each format. Used by the picker UI; not part of
 * the conversion itself, but co-located so a new format only needs
 * one place to edit.
 */
export const EXPORT_FORMAT_LABELS: Record<ExportFormat, string> = {
  plain: "Plain text",
  markdown: "Markdown",
  srt: "SRT",
  vtt: "WebVTT",
};

export interface SingleDictationExportInput {
  /** Final transcript text. Empty string is allowed — caller may want to copy "nothing" verbatim. */
  text: string;
  /** Wall-clock duration of the captured audio, in milliseconds. `null` only when the capture format was degenerate (we treat that as 0 ms). */
  durationMs: number | null;
  /** When the dictation completed, used by the markdown export's heading. Defaults to "now" at call time so callers don't have to thread `Date.now()`. */
  capturedAt?: Date;
}

export function exportAs(format: ExportFormat, input: SingleDictationExportInput): string {
  switch (format) {
    case "plain":
      return toPlain(input);
    case "markdown":
      return toMarkdown(input);
    case "srt":
      return toSrt(input);
    case "vtt":
      return toVtt(input);
  }
}

export function toPlain({ text }: SingleDictationExportInput): string {
  return text;
}

export function toMarkdown({ text, capturedAt }: SingleDictationExportInput): string {
  const stamp = (capturedAt ?? new Date()).toLocaleString();
  // Heading + blank line + body. Trailing newline so a paste into a
  // longer Markdown document doesn't run into the next paragraph.
  return `## ${stamp}\n\n${text}\n`;
}

export function toSrt(input: SingleDictationExportInput): string {
  const duration = Math.max(0, input.durationMs ?? 0);
  // SRT timecode is HH:MM:SS,mmm — one block covering [0, duration).
  const start = formatSrtTimestamp(0);
  const end = formatSrtTimestamp(duration);
  return `1\n${start} --> ${end}\n${input.text}\n`;
}

export function toVtt(input: SingleDictationExportInput): string {
  const duration = Math.max(0, input.durationMs ?? 0);
  // WebVTT is similar to SRT but uses `.` for the millisecond
  // separator, no cue index, and a `WEBVTT` magic header.
  const start = formatVttTimestamp(0);
  const end = formatVttTimestamp(duration);
  return `WEBVTT\n\n${start} --> ${end}\n${input.text}\n`;
}

/**
 * `HH:MM:SS,mmm` for SRT cues. Always padded to 8 chars on the
 * left of the comma + 3 ms digits — durations under an hour still
 * render as `00:00:42,500` so subtitle tools that strict-parse the
 * format don't choke.
 */
function formatSrtTimestamp(ms: number): string {
  return formatTimestamp(ms, ",");
}

function formatVttTimestamp(ms: number): string {
  return formatTimestamp(ms, ".");
}

function formatTimestamp(ms: number, msSep: "," | "."): string {
  const totalMs = Math.max(0, Math.round(ms));
  const hours = Math.floor(totalMs / 3_600_000);
  const minutes = Math.floor((totalMs % 3_600_000) / 60_000);
  const seconds = Math.floor((totalMs % 60_000) / 1000);
  const millis = totalMs % 1000;
  const pad = (n: number, w: number) => String(n).padStart(w, "0");
  return `${pad(hours, 2)}:${pad(minutes, 2)}:${pad(seconds, 2)}${msSep}${pad(millis, 3)}`;
}

const STORAGE_KEY = "hush.export.format";

/**
 * Read the user's last-picked export format from localStorage,
 * falling back to `plain` when absent or corrupted.
 */
export function readStoredFormat(): ExportFormat {
  if (typeof localStorage === "undefined") return "plain";
  const raw = localStorage.getItem(STORAGE_KEY);
  if (raw === "plain" || raw === "markdown" || raw === "srt" || raw === "vtt") {
    return raw;
  }
  return "plain";
}

/**
 * Persist the user's most recent export format. Best-effort —
 * localStorage failures (private mode, quota) are swallowed since
 * the UI's state is the source of truth in-session.
 */
export function rememberFormat(format: ExportFormat): void {
  if (typeof localStorage === "undefined") return;
  try {
    localStorage.setItem(STORAGE_KEY, format);
  } catch {
    // ignore
  }
}
