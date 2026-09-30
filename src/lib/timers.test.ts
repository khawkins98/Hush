import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { timerScope } from "./timers";

describe("timerScope", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("runs a timer and forgets it once fired", () => {
    const scope = timerScope();
    const fn = vi.fn();
    scope.set(fn, 100);
    expect(scope.size).toBe(1);
    vi.advanceTimersByTime(100);
    expect(fn).toHaveBeenCalledOnce();
    expect(scope.size).toBe(0);
  });

  it("clearAll cancels every pending timer", () => {
    const scope = timerScope();
    const a = vi.fn();
    const b = vi.fn();
    scope.set(a, 100);
    scope.set(b, 500);
    scope.clearAll();
    vi.advanceTimersByTime(1000);
    expect(a).not.toHaveBeenCalled();
    expect(b).not.toHaveBeenCalled();
    expect(scope.size).toBe(0);
  });
});
