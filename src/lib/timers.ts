/**
 * Component-scoped timeouts that are all cancelled together.
 *
 * Fire-and-forget `setTimeout(() => (flag = false), 2000)` calls outlive
 * the component that scheduled them and write `$state` after unmount.
 * Harmless today, but it's the class of bug that turns into "state from
 * a closed panel flips something in a reopened one". Create one scope per
 * component and call `clearAll()` from `onDestroy`.
 */
export function timerScope() {
  const pending = new Set<ReturnType<typeof setTimeout>>();
  return {
    set(fn: () => void, ms: number): ReturnType<typeof setTimeout> {
      const id = setTimeout(() => {
        pending.delete(id);
        fn();
      }, ms);
      pending.add(id);
      return id;
    },
    clearAll(): void {
      for (const id of pending) clearTimeout(id);
      pending.clear();
    },
    get size(): number {
      return pending.size;
    },
  };
}
