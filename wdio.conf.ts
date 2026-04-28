/// WebdriverIO config for the Path B (full-stack) E2E suite (#57).
///
/// Drives a real built Hush binary through `tauri-driver`. Pre-#202
/// every e2e ran in Path A — Playwright against the SvelteKit dev
/// server with the Tauri IPC layer mocked — so flows that depend
/// on a real `invoke` round-trip, real `listen` events, or
/// secondary-window lifecycle (HUD show/hide) had no automated
/// coverage. Path B closes that gap.
///
/// **Status: scaffold.** The infrastructure runs locally on Linux
/// once the prerequisites in `tests/e2e-tauri/README.md` are
/// installed; CI integration is deferred until macOS support in
/// `tauri-driver` matures (Hush's primary dev platform is macOS).
///
/// Platform notes:
/// - **Linux**: stable. Use `xvfb-run npm run test:e2e:tauri` for
///   headless. The binary path below points at the Tauri-bundled
///   debug artefact.
/// - **macOS**: tauri-driver's macOS back-end is the rough edge
///   per <https://v2.tauri.app/develop/tests/webdriver/>. Until
///   the upstream guide marks it stable, hands-on smoke remains
///   the dev-loop story for macOS.
/// - **Windows**: also supported by tauri-driver but not in
///   Hush's matrix today (no Windows artefacts shipped yet).

import { spawn, type ChildProcess } from "node:child_process";
import { existsSync } from "node:fs";
import { resolve } from "node:path";
import type { Options } from "@wdio/types";

// Resolve the debug binary tauri-driver should launch. The Tauri
// bundler emits to `src-tauri/target/{debug|release}/bundle/<...>`
// per-platform; for Path B we always run the debug build (faster
// rebuild, panic backtraces useful when a spec finds a real bug).
const platform = process.platform;
const projectRoot = resolve(__dirname);
const targetDir = resolve(projectRoot, "src-tauri/target/debug");

function resolveAppPath(): string {
  if (platform === "darwin") {
    return resolve(targetDir, "bundle/macos/Hush.app/Contents/MacOS/hush");
  }
  if (platform === "win32") {
    return resolve(targetDir, "hush.exe");
  }
  // Linux: tauri-driver expects the unbundled binary, not the
  // .deb / .AppImage. The bundler still produces the binary at
  // `target/debug/hush` regardless of which bundle format it
  // wraps it into.
  return resolve(targetDir, "hush");
}

const appPath = resolveAppPath();

// Track the tauri-driver child so the WDIO `onComplete` hook can
// shut it down. Without this the driver lingers until the next
// `npm run test:e2e:tauri` invocation hits a port collision.
let tauriDriver: ChildProcess | undefined;

export const config: Options.Testrunner = {
  runner: "local",

  tsConfigPath: "./tests/e2e-tauri/tsconfig.json",

  specs: ["./tests/e2e-tauri/specs/**/*.spec.ts"],

  maxInstances: 1, // tauri-driver is a single-binary driver

  // tauri-driver listens on 4444 by default; the WDIO host config
  // matches that. The capabilities object is "the standard
  // WebDriver shape, with Tauri-specific bits passed through the
  // `tauri:options` namespace" per the upstream docs.
  hostname: "127.0.0.1",
  port: 4444,
  capabilities: [
    {
      browserName: "wry",
      "tauri:options": {
        application: appPath,
      },
    },
  ],

  logLevel: "info",
  outputDir: resolve(projectRoot, "tests/e2e-tauri/.wdio-logs"),

  framework: "mocha",
  reporters: ["spec"],
  mochaOpts: {
    ui: "bdd",
    timeout: 60_000,
  },

  // Spawn `tauri-driver` before the first session opens. The
  // driver itself is a Rust binary the user installs via
  // `cargo install tauri-driver --locked` (see
  // `tests/e2e-tauri/README.md` for the prereq list).
  onPrepare(): Promise<void> | void {
    if (!existsSync(appPath)) {
      throw new Error(
        `tauri-driver: app binary not found at ${appPath}. ` +
          `Did you forget to \`npm run tauri build -- --debug\` first?`,
      );
    }
    tauriDriver = spawn("tauri-driver", [], {
      stdio: ["ignore", "inherit", "inherit"],
    });
    tauriDriver.on("error", (err) => {
      // Most common cause: the binary isn't installed.
      // eslint-disable-next-line no-console
      console.error(
        `tauri-driver failed to start (${err.message}). Install via ` +
          `\`cargo install tauri-driver --locked\`.`,
      );
    });
  },

  onComplete(): void {
    if (tauriDriver && !tauriDriver.killed) {
      tauriDriver.kill("SIGTERM");
    }
  },
};
