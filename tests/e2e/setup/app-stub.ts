// Stub for `@tauri-apps/api/app` used in Playwright e2e mode.
//
// The real package's `app.js` does `import { invoke } from './core.js'`
// — a relative import to its own bundled copy of core, NOT the
// `@tauri-apps/api/core` alias we redirect in vite.config.js. So the
// `invoke` calls inside `getVersion` / `getName` / `getTauriVersion`
// would bypass our mock bus and hit the real (absent) Tauri runtime,
// throwing "window.__TAURI_INTERNALS__ is undefined".
//
// This stub replaces `getName` / `getVersion` / `getTauriVersion`
// directly with calls that route through `window.__hush_e2e.invoke`,
// matching the pattern used by `core-stub.ts`. Only the symbols the
// app actually uses are stubbed.

type InvokeArgs = Record<string, unknown> | undefined;
type InvokeHandler = (args: InvokeArgs) => Promise<unknown>;

declare global {
  interface Window {
    __hush_e2e?: {
      invoke?: Record<string, InvokeHandler>;
    };
  }
}

async function dispatch<T>(cmd: string): Promise<T> {
  const handler = window.__hush_e2e?.invoke?.[cmd];
  if (!handler) {
    throw new Error(
      `[hush-e2e] unmocked app-info: "${cmd}". ` +
        `Add a handler to window.__hush_e2e.invoke in your test setup.`,
    );
  }
  return handler(undefined) as Promise<T>;
}

export async function getName(): Promise<string> {
  return dispatch<string>("plugin:app|name");
}

export async function getVersion(): Promise<string> {
  return dispatch<string>("plugin:app|version");
}

export async function getTauriVersion(): Promise<string> {
  return dispatch<string>("plugin:app|tauri_version");
}
