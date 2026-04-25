// Stub for `@tauri-apps/api/core` used in Playwright e2e mode.
//
// The real package routes through Tauri's IPC bridge, which doesn't
// exist when the SvelteKit dev server runs in a plain browser.
// This stub reads handler functions from `window.__hush_e2e.invoke`
// (set by tests via `page.addInitScript`) and dispatches `invoke()`
// calls into them.
//
// Tests configure handlers like:
//   await page.addInitScript(() => {
//     (window as any).__hush_e2e = {
//       invoke: {
//         get_first_run_completed: async () => false,
//         model_list: async () => [...],
//         // ...
//       },
//     };
//   });
//
// Unmocked invokes throw — that catches drift between the frontend
// and the test fixtures, instead of silently passing with `undefined`.
//
// Only the symbols actually imported by the app are stubbed. Adding a
// new `invoke` call site? Add the handler shape here and a default in
// `tauri-mock.ts`.

type InvokeArgs = Record<string, unknown> | undefined;
type InvokeHandler = (args: InvokeArgs) => Promise<unknown>;

declare global {
  interface Window {
    __hush_e2e?: {
      invoke?: Record<string, InvokeHandler>;
    };
  }
}

export async function invoke<T = unknown>(
  cmd: string,
  args?: InvokeArgs,
): Promise<T> {
  const handlers = window.__hush_e2e?.invoke;
  const handler = handlers?.[cmd];
  if (!handler) {
    throw new Error(
      `[hush-e2e] unmocked invoke: "${cmd}". ` +
        `Add a handler to window.__hush_e2e.invoke in your test setup.`,
    );
  }
  return handler(args) as Promise<T>;
}
