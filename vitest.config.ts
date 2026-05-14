// Unit-test configuration for Hush frontend.
//
// Separate from vite.config.js to avoid the conditional HUSH_E2E=1
// alias logic (which swaps @tauri-apps/api/* for Playwright stubs)
// and the Tauri dev-server settings interfering with vitest's
// module resolution.
//
// The sveltekit() plugin handles:
//   - Svelte compiler (needed for .svelte.ts rune modules)
//   - $lib path alias
//   - verbatimModuleSyntax TS config from .svelte-kit/tsconfig.json
//
// Tauri APIs (@tauri-apps/api/core, /event) are vi.mock()'d in each
// test file that imports a state module.

import { defineConfig } from "vitest/config";
import { sveltekit } from "@sveltejs/kit/vite";

export default defineConfig({
  plugins: [sveltekit()],

  test: {
    // Include all test files under src/
    include: ["src/**/*.{test,spec}.{js,ts}"],

    // jsdom provides window/document/localStorage for state modules
    // that reference these (e.g. nav.svelte.ts reads localStorage).
    environment: "jsdom",

    // Expose vi, describe, it, expect globally — consistent with the
    // Playwright globals already in scope in e2e tests.
    globals: true,
  },
});
