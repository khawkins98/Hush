# Tauri-driver E2E (Path B, #57)

Full-stack end-to-end tests that drive a real built Hush binary
through `tauri-driver` + WebdriverIO. Complements the Playwright
suite (`tests/e2e/`, Path A), which mocks the Tauri IPC layer.

> **Status:** scaffold only. The infrastructure lands here; the
> spec coverage starts at one smoke test and grows as Path A's
> mock-shaped gaps surface. CI integration is deliberately not
> wired yet — the Tauri WebDriver guide flags macOS support as
> "the rough edge" and Hush's primary dev platform is macOS, so
> the suite needs hands-on validation on Linux first. See the
> "Running on Linux CI" section below for the eventual workflow.

## What Path B catches that Path A doesn't

Path A (Playwright + mocked IPC) catches frontend regressions
cheaply but mocks the Tauri IPC layer entirely. The flows it can
*not* cover, all of which Path B can:

- Real `invoke` round-trips — would have caught the
  `pub use commands::*` regression where the frontend got
  `command_not_registered` errors at runtime.
- Real `listen` events — `audio:level` pump, `hotkey:toggle` /
  `hotkey:ptt-press` / `hotkey:ptt-release`, the
  `model:download-progress` byte-stream.
- HUD secondary-window lifecycle — Path A can't observe windows
  beyond the one `page.goto('/')` it drives.
- Hotkey registration — global-shortcut + rdev are platform-OS
  deep.
- Clipboard write — Path A mocks the clipboard plugin; Path B
  verifies the actual paste-target ends up correct.
- Real model download against `wiremock` — closer to production
  than a fully-stubbed `invoke`.

## Running

### Prerequisites

1. **Install `tauri-driver`** (Linux + Windows; macOS support is in
   progress upstream):

       cargo install tauri-driver --locked

2. **Build the app** so there's a binary for the driver to launch:

       npm run tauri build -- --debug

   The driver expects the binary at the standard
   `src-tauri/target/debug/bundle/...` path produced by Tauri's
   bundler. The exact platform-specific path is wired in
   `wdio.conf.ts`.

3. **Linux only**: install `xvfb` if running headless and
   `pulseaudio` (or PipeWire) if any spec exercises the audio path.

### Commands

    # Run the full Path B suite
    npm run test:e2e:tauri

    # Run a single spec
    npm run test:e2e:tauri -- --spec tests/e2e-tauri/specs/sidebar-nav.spec.ts

    # Headed (Linux): drop the Xvfb wrapper, see the window
    HEADED=1 npm run test:e2e:tauri

The runner is configured in `wdio.conf.ts` at the repo root.
Specs live in `tests/e2e-tauri/specs/`. Output goes to
`tests/e2e-tauri/.wdio-logs/` (gitignored).

## Spec layout

    tests/e2e-tauri/
      ├── README.md           — this file
      ├── tsconfig.json       — strict TS, separate from the
      │                          frontend's tsconfig.json so
      │                          WebdriverIO globals don't leak
      ├── specs/
      │     └── sidebar-nav.spec.ts
      └── .wdio-logs/         — gitignored runner output

## Running on Linux CI (deferred)

The eventual CI shape, when the suite is mature enough to gate on:

```yaml
e2e-tauri:
  name: Frontend e2e (tauri-driver, Linux)
  runs-on: ubuntu-latest
  needs: [rust, frontend]
  steps:
    - uses: actions/checkout@v4
    - name: Install Linux system deps
      run: |
        sudo apt-get update -qq
        sudo apt-get install -y --no-install-recommends \
          libwebkit2gtk-4.1-dev xvfb \
          pulseaudio
    - name: Install Rust + tauri-driver
      uses: dtolnay/rust-toolchain@stable
    - run: cargo install tauri-driver --locked
    - run: npm ci
    - name: Build app (debug)
      run: npm run tauri build -- --debug
    - name: Start virtual display + audio
      run: |
        Xvfb :99 -screen 0 1280x800x24 &
        echo "DISPLAY=:99" >> "$GITHUB_ENV"
        pulseaudio --start --exit-idle-time=-1
        pacmd load-module module-null-sink sink_name=hush-test
    - run: npm run test:e2e:tauri
```

The "deferred" aspect is the macOS quirk: tauri-driver's macOS
back-end is reportedly the least-mature path
(<https://v2.tauri.app/develop/tests/webdriver/>), so the Linux
job ships first. macOS hands-on smoke remains the fallback for
the dev loop on macOS.
