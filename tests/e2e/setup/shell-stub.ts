// Stub for `@tauri-apps/plugin-shell` used in Playwright e2e mode.
//
// In production, `open(url)` hands off to the OS browser via
// `tauri-plugin-shell`. In e2e mode the real plugin would try to
// reach `window.__TAURI_INTERNALS__` and throw. Tests that exercise
// link clicks just need to verify the *call shape* (was the right
// URL handed off?), not the real OS behaviour — so we route through
// the same `window.__hush_e2e.invoke` mock bus the other stubs use.
// A test can install a recording handler for the synthetic
// `plugin:shell|open` command to assert "user clicked the GitHub
// issues link with the expected URL"; the default mock is a no-op.

// `window.__hush_e2e` is declared by `core-stub.ts` (loaded first
// because every page that uses this stub also imports `invoke`).
// We avoid re-declaring it here — TypeScript flags conflicting
// `declare global` blocks even when the types are textually
// identical. Cast at the read site instead.

export async function open(path: string): Promise<void> {
  const bus = (window as unknown as {
    __hush_e2e?: {
      invoke?: Record<string, (args: unknown) => unknown | Promise<unknown>>;
    };
  }).__hush_e2e;
  const handler = bus?.invoke?.["plugin:shell|open"];
  if (handler) {
    await handler({ path });
  }
  // No-op when unmocked. The default e2e mock catalogue (in
  // `tests/e2e/_mock.ts`) installs a no-op handler so nothing
  // throws even if a test doesn't override; specs that care about
  // the URL hand off a recording handler.
}
