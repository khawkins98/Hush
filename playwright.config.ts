import { defineConfig, devices } from "@playwright/test";

// Playwright drives the SvelteKit dev server in `HUSH_E2E=1` mode so
// `vite.config.js` swaps `@tauri-apps/api/{core,event}` for the
// in-tree stubs in `tests/e2e/setup/`. The real Tauri runtime is not
// involved — these tests are frontend-only smoke against a mocked
// IPC. Full-stack flows (HUD lifecycle, hotkey, real audio, real
// model download) live behind issue #57 (tauri-driver path).

export default defineConfig({
  testDir: "./tests/e2e",
  testMatch: "**/*.spec.ts",
  // Test files that begin with `_` (helpers) are excluded.
  testIgnore: ["**/setup/**", "**/_*"],

  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  workers: process.env.CI ? 1 : undefined,
  reporter: process.env.CI ? "github" : "list",

  use: {
    baseURL: "http://localhost:1420",
    trace: "on-first-retry",
  },

  projects: [
    {
      name: "chromium",
      use: { ...devices["Desktop Chrome"] },
    },
  ],

  // Spawn the SvelteKit dev server with the e2e stubs aliased in.
  // Reuses an existing server if the port is already taken (so
  // `npm run dev:e2e` in one tab + `npm run test:e2e` in another
  // works for iterative debugging).
  webServer: {
    command: "npm run dev:e2e",
    url: "http://localhost:1420",
    reuseExistingServer: !process.env.CI,
    timeout: 120_000,
  },
});
