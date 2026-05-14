import { describe, it, expect } from "vitest";
import {
  shouldShowSpeakerLabels,
  joinUtterances,
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
