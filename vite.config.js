import { defineConfig } from "vite";
import { sveltekit } from "@sveltejs/kit/vite";
import path from "path";

const host = process.env.TAURI_DEV_HOST;
const isE2E = process.env.HUSH_E2E === "1";

// `import.meta.dirname` is undefined in some toolchains; resolve via
// `process.cwd()` since vite always invokes with cwd at project root.
const projectRoot = process.cwd();

// https://vite.dev/config/
export default defineConfig(async () => ({
  plugins: [sveltekit()],

  // E2E mode (set HUSH_E2E=1) swaps the @tauri-apps/api/{core,event}
  // imports for in-tree stubs at `tests/e2e/setup/{core,event}-stub.ts`.
  // The stubs read mock state from `window.__hush_e2e` so a Playwright
  // test can configure command responses and event payloads via
  // `page.addInitScript` before navigation. The dev workflow
  // (`npm run dev`) is unaffected — the alias only activates when the
  // env var is set.
  resolve: isE2E
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
