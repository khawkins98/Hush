import { describe, expect, it } from "vitest";

import { shadeLiveTranscript, shadeText } from "./confidence-shading";
import type { StreamingUtterance } from "./types";

const partial = (text: string, ps?: number[], speakerLabel: string | null = null): StreamingUtterance => ({
  text,
  startedAtMs: 0,
  endedAtMs: 1000,
  isFinal: false,
  speakerLabel,
  words: ps ? text.split(/\s+/).map((word, i) => ({ word, p: ps[i] })) : undefined,
});

describe("shadeText", () => {
  it("marks words below the threshold and keeps whitespace", () => {
    const words = [
      { word: "hello", p: 0.9 },
      { word: "wrold", p: 0.2 },
    ];
    expect(shadeText("hello  wrold", words)).toEqual([
      { text: "hello", low: false },
      { text: "  ", low: false },
      { text: "wrold", low: true },
    ]);
  });

  it("returns null without confidences", () => {
    expect(shadeText("hello", undefined)).toBeNull();
    expect(shadeText("hello", [])).toBeNull();
  });

  it("returns null when words don't align with the text", () => {
    expect(shadeText("hello there", [{ word: "hello", p: 0.9 }])).toBeNull();
    expect(
      shadeText("hello there", [
        { word: "hello", p: 0.9 },
        { word: "where", p: 0.9 },
      ]),
    ).toBeNull();
  });
});

describe("shadeLiveTranscript", () => {
  it("splits plain finals from shaded partial lines, keeping label prefixes", () => {
    const partials = [partial("maybe this", [0.3, 0.9])];
    const full = "Speaker 1: final words\nSpeaker 2: maybe this";
    expect(shadeLiveTranscript(full, partials)).toEqual({
      head: "Speaker 1: final words",
      tail: [
        {
          label: "Speaker 2: ",
          tokens: [
            { text: "maybe", low: true },
            { text: " ", low: false },
            { text: "this", low: false },
          ],
        },
      ],
    });
  });

  it("is null when no partial carries confidences (shading off)", () => {
    expect(shadeLiveTranscript("a\nb", [partial("b")])).toBeNull();
  });

  it("renders a partial without confidences as one plain token", () => {
    const out = shadeLiveTranscript("x\ny\nz", [partial("y"), partial("z", [0.1])]);
    expect(out?.head).toBe("x");
    expect(out?.tail[0].tokens).toEqual([{ text: "y", low: false }]);
    expect(out?.tail[1].tokens).toEqual([{ text: "z", low: true }]);
  });

  it("falls back to plain when the line structure doesn't match", () => {
    expect(shadeLiveTranscript("something else", [partial("maybe", [0.2])])).toBeNull();
  });
});
