// Unit tests for the pure reducer helpers in meeting-sessions.svelte.ts.
//
// These functions are side-effect-free — no Tauri, no $state, no IPC.
// They encode the exact semantics of the background-finalization state
// machine, making it trivial to cover every branch without a browser.
//
// meeting:finalizing  → reduceFinalizing
//   • clears activeId when it matches the ended session
//   • preserves a non-matching activeId
//   • sets finalizingId unconditionally
//
// meeting:session-ended → reduceEnded
//   • clears matching finalizingId (transcription done)
//   • does NOT clear a different session's finalizingId (session IDs
//     don't collide — the in-flight finalizing session must stay tracked)
//   • clears matching activeId (non-finalizing stop path)

import { describe, it, expect } from "vitest";
import { reduceFinalizing, reduceEnded } from "./meeting-sessions.svelte";

describe("reduceFinalizing", () => {
  it("clears activeId when it matches the finalizing sessionId", () => {
    const result = reduceFinalizing({ activeId: 7, finalizingId: null }, 7);
    expect(result.activeId).toBeNull();
  });

  it("sets finalizingId to the sessionId", () => {
    const result = reduceFinalizing({ activeId: 7, finalizingId: null }, 7);
    expect(result.finalizingId).toBe(7);
  });

  it("preserves a non-matching activeId", () => {
    // Another session is active (shouldn't happen in practice, but the
    // reducer must be conservative and not clobber it).
    const result = reduceFinalizing({ activeId: 99, finalizingId: null }, 7);
    expect(result.activeId).toBe(99);
    expect(result.finalizingId).toBe(7);
  });

  it("overwrites a pre-existing finalizingId with the new sessionId", () => {
    // Sequential finalization — new session starts before the previous
    // session-ended arrives (edge case; must not lose the new id).
    const result = reduceFinalizing({ activeId: null, finalizingId: 6 }, 7);
    expect(result.finalizingId).toBe(7);
  });
});

describe("reduceEnded", () => {
  it("clears finalizingId when it matches the ended sessionId", () => {
    const result = reduceEnded({ activeId: null, finalizingId: 7 }, 7);
    expect(result.finalizingId).toBeNull();
  });

  it("does NOT clear finalizingId when it belongs to a different session", () => {
    // Session 7 just ended, but session 8 is still finalizing.
    // The indicator for session 8 must NOT be cleared.
    const result = reduceEnded({ activeId: null, finalizingId: 8 }, 7);
    expect(result.finalizingId).toBe(8);
  });

  it("clears activeId when it matches the ended sessionId (non-finalizing stop)", () => {
    // meeting:session-ended arrives without a prior meeting:finalizing
    // (e.g. older backend or auto-stop path) — must still clear activeId.
    const result = reduceEnded({ activeId: 7, finalizingId: null }, 7);
    expect(result.activeId).toBeNull();
  });

  it("preserves activeId when it belongs to a different session", () => {
    const result = reduceEnded({ activeId: 99, finalizingId: 7 }, 7);
    expect(result.activeId).toBe(99);
    expect(result.finalizingId).toBeNull();
  });
});
