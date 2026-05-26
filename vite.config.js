import { defineConfig } from "vite";
import { sveltekit } from "@sveltejs/kit/vite";
import path from "path";

const host = process.env.TAURI_DEV_HOST;
const isE2E = process.env.HUSH_E2E === "1";
// `npm run dev` sets HUSH_MOCK=1 → the plain-browser playground with a
// seeded mock IPC bus. `npm run dev:tauri` (what `tauri dev` runs) does
// not, so the real app talks to the real Rust backend.
const isMock = process.env.HUSH_MOCK === "1";

// `import.meta.dirname` is undefined in some toolchains; resolve via
// `process.cwd()` since vite always invokes with cwd at project root.
const projectRoot = process.cwd();

// https://vite.dev/config/
export default defineConfig(async () => ({
  plugins: [sveltekit()],

  // Mock-bus mode swaps the @tauri-apps/api/{core,event,app} +
  // plugin-shell imports for in-tree stubs at `tests/e2e/setup/*-stub.ts`.
  // The stubs route through `window.__hush_e2e`, which is seeded with
  // populated fake data by `mock-defaults.ts` (or, under Playwright, by
  // each test's `installMocks`). Two entry points enable it:
  //   • HUSH_E2E=1   → Playwright (`npm run dev:e2e`)
  //   • HUSH_MOCK=1  → the browser playground (`npm run dev`)
  // `npm run dev:tauri` (what `tauri dev` runs) sets neither, so the
  // real app talks to the real Rust backend.
  resolve: (isE2E || isMock)
    ? {
        alias: {
          "@tauri-apps/api/core": path.resolve(
            projectRoot,
            "tests/e2e/setup/core-stub.ts",
          ),
          "@tauri-apps/api/event": path.resolve(
            projectRoot,
            "tests/e2e/setup/event-stub.ts",
          ),
          // The real `@tauri-apps/api/app` imports `invoke` from its
          // own bundled `./core.js` (a relative path), so our core
          // alias above doesn't intercept its calls. The app stub
          // routes `getName` / `getVersion` / `getTauriVersion`
          // through the same mock bus.
          "@tauri-apps/api/app": path.resolve(
            projectRoot,
            "tests/e2e/setup/app-stub.ts",
          ),
          // External-URL opener (#322). The real plugin reaches
          // `window.__TAURI_INTERNALS__`; the stub no-ops by
          // default and routes through the mock bus for tests
          // that want to assert the URL the user clicked.
          "@tauri-apps/plugin-shell": path.resolve(
            projectRoot,
            "tests/e2e/setup/shell-stub.ts",
          ),
        },
      }
    : {},

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
    },
  },
}));
