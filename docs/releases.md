# Cutting a release

Maintainer-facing recipe for shipping a new version. The release pipeline lives in [`.github/workflows/release.yml`](../.github/workflows/release.yml); this doc is the human-loop wrapper around it.

## What gets built

A `v*` tag push (or a manual `workflow_dispatch`) runs the build matrix on three OS runners and produces, in parallel:

| Platform | Args | Artefacts |
|---|---|---|
| macOS (Apple Silicon, macOS 26+) | `--target aarch64-apple-darwin` | `Hush_<version>_aarch64.dmg` |
| Linux (Ubuntu) | — | `hush_<version>_amd64.AppImage`, `hush_<version>_amd64.deb` |
| Windows | — | `Hush_<version>_x64.msi`, `Hush_<version>_x64-setup.exe` |

Intel macOS is not in the matrix. macOS 26 (Tahoe) is the project's primary target per CLAUDE.md, and 26 is Apple-Silicon-only — there's nothing for an Intel binary to run on inside the supported window. The workflow's `MACOSX_DEPLOYMENT_TARGET=26.0` env enforces the floor at the Tauri build step.

`tauri-action` attaches all of them to a single GitHub Release, named after the tag. The release is created as a **draft** so you can review the artefact list before publishing.

## Signing — current state

| Platform | v1 (today) | Roadmap |
|---|---|---|
| macOS | Ad-hoc signed only. Gatekeeper warning on first launch; right-click → Open clears it. | Developer ID + notarisation. Needs an `APPLE_*` secrets bundle on the repo. |
| Windows | Unsigned. SmartScreen warning. Click "More info" → "Run anyway". | EV certificate + signtool integration. |
| Linux | AppImage / .deb shipped as-is. | n/a — Linux distros don't sign artefacts the same way. |

The first wave of releases will surface those warnings to users. The README's "First-launch warnings" section sets the expectation up front; once code-signing lands, both the warnings and that README block can go away.

## Release-cutting steps

### 1. Bump versions

Three files have to agree on the new version. They're not yet linked, so a stale one will silently produce a release where the binary reports an older version than the tag suggests.

- [`src-tauri/Cargo.toml`](../src-tauri/Cargo.toml) — `[package].version = "0.2.0"`
- [`src-tauri/tauri.conf.json`](../src-tauri/tauri.conf.json) — `"version": "0.2.0"`
- [`src-tauri/Cargo.lock`](../src-tauri/Cargo.lock) — runs `cargo build` and re-commits; the lockfile entry for `hush` updates to match.

### 2. Update the changelog

Move `[Unreleased]` content to a new `[0.2.0] — YYYY-MM-DD` section in [`CHANGELOG.md`](../CHANGELOG.md). Keep the headings (Added / Changed / Fixed / Removed / Deprecated / Security). Empty `[Unreleased]` block stays at the top for the next round.

### 3. Land the bump

```bash
git checkout -b chore/release-0.2.0
# edits + cargo build to update the lockfile
git commit -m "chore(release): v0.2.0"
gh pr create
# merge once CI is green
```

### 4. Tag and push

```bash
git checkout main
git pull
git tag v0.2.0
git push origin v0.2.0
```

The workflow fires on the tag push. Watch progress at the [Actions](https://github.com/khawkins98/Hush/actions) page; the slow leg is the macOS Apple Silicon build (whisper.cpp's GGML compile + the per-arch toolchain). Each leg typically takes 10–20 min depending on cache state.

### 5. Review and publish

- Open the [Releases](https://github.com/khawkins98/Hush/releases) page; the new release is in draft.
- Confirm all four artefact families are present (apple-silicon `.dmg`, intel `.dmg`, `.AppImage` + `.deb`, `.msi` + `.exe`).
- Paste the CHANGELOG entry into the release body (replacing the placeholder install copy).
- Click **Publish**.

Optionally, post-publish: download one artefact per platform and smoke-test the install path on a clean machine. Notable failure modes in early releases: macOS `.dmg` failing to mount, Linux `.AppImage` missing exec bit, Windows installer being flagged by AV.

## Dry-run via workflow_dispatch

To smoke the build matrix without cutting a real tag:

```bash
gh workflow run release.yml
```

Or: Actions → Release → "Run workflow" in the GitHub UI. The workflow publishes to a draft release tagged `dispatch-<run-id>` so dispatches don't pollute the tag list. Delete the draft after inspection.

## What can go wrong (early-days notes)

These are the things that have bitten previous attempts on similar Tauri pipelines; we'll record what hits us in our own runs as it happens.

- **macOS i8mm**: whisper.cpp's GGML uses an aarch64 intrinsic that needs `-march=armv8.6-a`. The workflow sets `CFLAGS` / `CXXFLAGS` for the apple-silicon leg; if a new Whisper revision changes the flag, builds break with a `requires target feature 'i8mm'` clang error.
- **Linux deps**: `tauri-action` pulls webkit2gtk-4.1, librsvg, libxdo, libasound and friends. The workflow installs them explicitly via apt; a new transitive dep would need adding to that list.
- **Windows code-signing**: when this lands, the `.msi` step also needs a working signtool integration — adding the cert without wiring signtool produces an unsigned artefact silently.

## After publishing

- The README's [Install](../README.md#install) section auto-pivots to the latest release via the GitHub Releases link — no edit needed.
- Users can hit **Settings → About → Check for updates** and see the new version surfaced; macOS users can also use **Hush → Check for Updates…** from the menu bar.
- Hush does **not** auto-update yet. Users have to download and install manually. Auto-update lives behind [#10](https://github.com/khawkins98/Hush/issues/10) — gated on a signing-key decision and on this pipeline producing artefacts the updater plugin can point at.
