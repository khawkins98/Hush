import { describe, it, expect } from "vitest";
import {
  shouldShowSpeakerLabels,
  joinUtterances,
  resolvePartialLabels,
} from "$lib/transcript-format";
import type { UtteranceLike } from "$lib/transcript-format";

describe("shouldShowSpeakerLabels", () => {
  it("returns false for an empty list", () => {
    expect(shouldShowSpeakerLabels([])).toBe(false);
  });

  it("returns false when all utterances share the same label", () => {
    const utts: UtteranceLike[] = [
      { text: "Hello", speakerLabel: "Speaker A" },
      { text: "World", speakerLabel: "Speaker A" },
    ];
    expect(shouldShowSpeakerLabels(utts)).toBe(false);
  });

  it("returns false when all speaker labels are null", () => {
    const utts: UtteranceLike[] = [
      { text: "Hello", speakerLabel: null },
      { text: "World", speakerLabel: null },
    ];
    expect(shouldShowSpeakerLabels(utts)).toBe(false);
  });

  it("returns true when two or more distinct labels are present", () => {
    const utts: UtteranceLike[] = [
      { text: "Hello", speakerLabel: "Speaker A" },
      { text: "Hi there", speakerLabel: "Speaker B" },
    ];
    expect(shouldShowSpeakerLabels(utts)).toBe(true);
  });

  it("ignores null labels when counting distinct speakers", () => {
    // One real label + one null → still single distinct label → false
    const utts: UtteranceLike[] = [
      { text: "Hello", speakerLabel: "Speaker A" },
      { text: "...", speakerLabel: null },
    ];
    expect(shouldShowSpeakerLabels(utts)).toBe(false);
  });

  it("only requires two distinct labels, not two non-null labels", () => {
    const utts: UtteranceLike[] = [
      { text: "a", speakerLabel: "A" },
      { text: "b", speakerLabel: "B" },
      { text: "c", speakerLabel: null },
    ];
    expect(shouldShowSpeakerLabels(utts)).toBe(true);
  });
});

describe("joinUtterances", () => {
  it("returns empty string for an empty list", () => {
    expect(joinUtterances([], "\n\n")).toBe("");
  });

  it("joins with the given separator when labels are hidden", () => {
    const utts: UtteranceLike[] = [
      { text: "Hello", speakerLabel: "Speaker A" },
      { text: "World", speakerLabel: "Speaker A" },
    ];
    expect(joinUtterances(utts, "\n\n")).toBe("Hello\n\nWorld");
  });

  it("prefixes speaker labels when two distinct speakers are present", () => {
    const utts: UtteranceLike[] = [
      { text: "Hello", speakerLabel: "Alice" },
      { text: "Hi there", speakerLabel: "Bob" },
    ];
    expect(joinUtterances(utts, "\n")).toBe("Alice: Hello\nBob: Hi there");
  });

  it("omits label prefix for utterances with null label even when labels are shown", () => {
    const utts: UtteranceLike[] = [
      { text: "Hello", speakerLabel: "Alice" },
      { text: "Hi", speakerLabel: "Bob" },
      { text: "...", speakerLabel: null },
    ];
    expect(joinUtterances(utts, "\n")).toBe("Alice: Hello\nBob: Hi\n...");
  });

  it("respects the separator choice between clipboard and live view", () => {
    const utts: UtteranceLike[] = [
      { text: "A", speakerLabel: "X" },
      { text: "B", speakerLabel: "X" },
    ];
    expect(joinUtterances(utts, "\n\n")).toBe("A\n\nB");
    expect(joinUtterances(utts, "\n")).toBe("A\nB");
  });
});

describe("resolvePartialLabels (#1013 live label flip)", () => {
  const sys = (text: string): UtteranceLike => ({ text, speakerLabel: "system" });

  it("keeps 'system' partials when no final has been diarized", () => {
    const finals: UtteranceLike[] = [
      { text: "hi", speakerLabel: "mic" },
      { text: "hello", speakerLabel: "system" },
    ];
    expect(resolvePartialLabels(finals, [sys("so")])).toEqual([sys("so")]);
  });

  it("borrows the most recent diarized label for system partials", () => {
    const finals: UtteranceLike[] = [
      { text: "a", speakerLabel: "Speaker 1" },
      { text: "b", speakerLabel: "mic" },
      { text: "c", speakerLabel: "Speaker 2" },
      { text: "d", speakerLabel: "mic" },
    ];
    expect(resolvePartialLabels(finals, [sys("next")])).toEqual([
      { text: "next", speakerLabel: "Speaker 2" },
    ]);
  });

  it("never borrows an in-room voice's label for a remote partial", () => {
    // In-room separation (opt-in): a second voice in the room is labelled
    // "In-room 2". A later remote partial must keep the last *remote* label.
    const finals: UtteranceLike[] = [
      { text: "a", speakerLabel: "Speaker 1" },
      { text: "b", speakerLabel: "In-room 2" },
    ];
    expect(resolvePartialLabels(finals, [sys("next")])).toEqual([
      { text: "next", speakerLabel: "Speaker 1" },
    ]);
    // Only in-room voices diarized so far: stay "Remote".
    expect(resolvePartialLabels([{ text: "b", speakerLabel: "In-room 2" }], [sys("x")])).toEqual([
      sys("x"),
    ]);
  });

  it("leaves mic partials alone", () => {
    const finals: UtteranceLike[] = [{ text: "a", speakerLabel: "Speaker 1" }];
    const mic: UtteranceLike = { text: "me", speakerLabel: "mic" };
    expect(resolvePartialLabels(finals, [mic])).toEqual([mic]);
  });

  it("does not mutate its inputs", () => {
    const finals: UtteranceLike[] = [{ text: "a", speakerLabel: "Speaker 1" }];
    const partials = [sys("p")];
    resolvePartialLabels(finals, partials);
    expect(partials[0].speakerLabel).toBe("system");
  });

  it("stops the live pane flipping Remote → Speaker N", () => {
    // One remote talker so far: before the fix the in-flight partial
    // read "Remote:" (and its presence alone forced labels on).
    const finals: UtteranceLike[] = [
      { text: "Morning all.", speakerLabel: "Speaker 1" },
    ];
    const joined = joinUtterances(
      [...finals, ...resolvePartialLabels(finals, [sys("Shall we start")])],
      "\n",
    );
    expect(joined).toBe("Morning all.\nShall we start");
  });
});
