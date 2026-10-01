import { describe, expect, it } from "vitest";
import { lastUtteranceId, mergeDetailDelta } from "./meeting-detail";
import type { MeetingSessionDetail, PersistedUtterance, StreamingUtterance } from "./types";

function utt(id: number, startedAtMs: number, text = `u${id}`): PersistedUtterance {
  return {
    id,
    sessionId: 1,
    startedAtMs,
    endedAtMs: startedAtMs + 500,
    speakerLabel: "mic",
    text,
    isFinal: true,
    speakerIdentityId: null,
  };
}

function partial(text: string): StreamingUtterance {
  return { text, startedAtMs: 9_000, endedAtMs: 9_500, isFinal: false, speakerLabel: "mic" };
}

function detail(
  utterances: PersistedUtterance[],
  currentPartials: StreamingUtterance[] = [],
  utteranceCount = utterances.length,
): MeetingSessionDetail {
  return {
    session: {
      id: 1,
      appName: "Zoom",
      appKind: "meeting",
      startedAt: "2026-10-01T10:00:00Z",
      endedAt: null,
      speakerCount: null,
      utteranceCount,
      notes: null,
      sources: ["mic"],
      appTitle: null,
      name: null,
    },
    utterances,
    currentPartials,
  };
}

describe("lastUtteranceId", () => {
  it("is 0 for an empty list", () => {
    expect(lastUtteranceId([])).toBe(0);
  });
  it("is the max id regardless of order", () => {
    expect(lastUtteranceId([utt(5, 0), utt(9, 100), utt(7, 50)])).toBe(9);
  });
});

describe("mergeDetailDelta", () => {
  it("appends new finals and replaces partials + header", () => {
    const prev = detail([utt(1, 0), utt(2, 1_000)], [partial("hel")]);
    const delta = detail([utt(3, 2_000)], [partial("hello there")], 3);
    const merged = mergeDetailDelta(prev, delta);
    expect(merged.utterances.map((u) => u.id)).toEqual([1, 2, 3]);
    expect(merged.currentPartials).toEqual([partial("hello there")]);
    expect(merged.session.utteranceCount).toBe(3);
  });

  it("re-sorts by start time when a new final starts earlier", () => {
    const prev = detail([utt(1, 0), utt(2, 3_000)]);
    const merged = mergeDetailDelta(prev, detail([utt(3, 1_500)], [], 3));
    expect(merged.utterances.map((u) => u.id)).toEqual([1, 3, 2]);
  });

  it("ignores rows already held", () => {
    const prev = detail([utt(1, 0), utt(2, 1_000)]);
    const merged = mergeDetailDelta(prev, detail([utt(2, 1_000), utt(3, 2_000)], [], 3));
    expect(merged.utterances.map((u) => u.id)).toEqual([1, 2, 3]);
  });

  it("returns the previous object when nothing changed", () => {
    const prev = detail([utt(1, 0)], [partial("same")]);
    expect(mergeDetailDelta(prev, detail([], [partial("same")], 1))).toBe(prev);
  });

  it("keeps the utterance array when only partials changed", () => {
    const prev = detail([utt(1, 0)], [partial("a")]);
    const merged = mergeDetailDelta(prev, detail([], [partial("ab")], 1));
    expect(merged).not.toBe(prev);
    expect(merged.utterances).toBe(prev.utterances);
  });
});
