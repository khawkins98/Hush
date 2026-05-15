# Cutting a release

Maintainer-facing recipe for shipping a new version. The release pipeline lives in [`.github/workflows/release.yml`](../.github/workflows/release.yml); this doc is the human-loop wrapper around it.

## What gets built

A `v*` tag push (or a manual `workflow_dispatch`) runs the build matrix on three OS runners and produces, in parallel:

| Platform | Args | Artefacts |
|---|---|---|
| macOS (Apple Silicon, macOS 26+) | `--target aarch64-apple-darwin` | `Hush_<version>_aarch64.dmg` |
| Linux (Ubuntu) | — | `hush_<version>_amd64.AppImage`, `hush_<version>_amd64.deb` |
| Windows | — | `Hush_<version>_x64.msi`, `Hush_<version>_x64-setup.exe` |

Intel macOS is not in the matrix. macOS 26 (Tahoe) is the project's primary target per CLAUDE.md, and 26 is Apple-Silicon-only — there's nothing for an Intel binary to run on inside the supported window.

The actual deployment target the binary is built against is `MACOSX_DEPLOYMENT_TARGET=14.0`, **not 26.0**: GitHub's `macos-latest` runner uses Xcode 16.4 with the macOS 15 SDK, so we can't deploy-target a version higher than the SDK supports. 14.0 (Sonoma) is the practical floor — well above whisper.cpp's C++17 `<filesystem>` requirement (≥ 10.15), Apple-Silicon-supported (≥ 11.0), comfortably below the SDK ceiling. **macOS 26 remains the *design target* we hands-on test on**; the deployment target is just the technical lower-bound. To bump the deployment target to macOS 26 specifically we'd need to wait for GitHub's runners to ship Xcode 26.x.

`tauri-action` attaches all of them to a single GitHub Release, named after the tag. The release is created as a **draft** so you can review the artefact list before publishing.

## Signing — current state

| Platform | v1 (today) | Roadmap |
|---|---|---|
| macOS | Ad-hoc signed only. Gatekeeper warning on first launch; right-click → Open clears it. Homebrew users can skip it entirely: `brew install --cask --no-quarantine khawkins98/tap/hush`. | Developer ID + notarisation. Needs an `APPLE_*` secrets bundle on the repo. |
| Windows | Unsigned. SmartScreen warning. Click "More info" → "Run anyway". | EV certificate + signtool integration. |
| Linux | AppImage / .deb shipped as-is. | n/a — Linux distros don't sign artefacts the same way. |

The first wave of releases will surface those warnings to users. The README's install section sets the expectation up front; once code-signing lands, both the warnings and the Homebrew `--no-quarantine` caveat can go away.

## Release-cutting steps

### 1. Bump versions

Three files have to agree on the new version. They're not yet linked, so a stale one will silently produce a release where the binary reports an older version than the tag suggests. CI checks all three and will fail the PR if any disagree.

- [`src-tauri/Cargo.toml`](../src-tauri/Cargo.toml) — `[package].version = "x.y.z"`
- [`src-tauri/tauri.conf.json`](../src-tauri/tauri.conf.json) — `"version": "x.y.z"`
- [`package.json`](../package.json) — `"version": "x.y.z"`

### 2. Update the changelog

Move `[Unreleased]` content to a new `[x.y.z] - YYYY-MM-DD` section in [`CHANGELOG.md`](../CHANGELOG.md). Keep the headings (Added / Changed / Fixed / Removed / Deprecated / Security). Empty `[Unreleased]` block stays at the top for the next round.

**Add a release narrative** — a short paragraph immediately below the version heading and before the first `### Added` heading. This is the "elevator pitch" of the release: what the work was really *about* in plain English, not a bullet list. A good narrative covers:
- The headline user-facing additions (1–2 sentences)
- The dominant theme of the maintenance/fix work (e.g. "most of the commit count went into hardening the meeting pipeline")
- Any significant internal/architectural work worth flagging to future contributors

Example:

```markdown
## [x.y.z] - YYYY-MM-DD

vx.y.z ships [headline feature]. Most of the commit count went into [dominant theme]. Internally, [notable architectural change if any].

### Added
```

### 3. Land the bump

```bash
git checkout -b chore/release-x.y.z
# bump the three version files (Cargo.toml, tauri.conf.json, package.json)
git commit -m "chore(release): vx.y.z"
gh pr create
# merge once CI is green
```

### 4. Tag and push

```bash
git checkout main
git pull
git tag vx.y.z
git push origin vx.y.z
```

The workflow fires on the tag push. Watch progress at the [Actions](https://github.com/khawkins98/Hush/actions) page; the slow leg is the macOS Apple Silicon build (whisper.cpp's GGML compile + the per-arch toolchain). Each leg typically takes 10–20 min depending on cache state.

### 5. Review and publish

- Open the [Releases](https://github.com/khawkins98/Hush/releases) page; the new release is in draft.
- Confirm all three platform artefacts are present (apple-silicon `.dmg`, `.AppImage` + `.deb`, `.msi` + `.exe`).
- Paste the CHANGELOG entry into the release body (replacing the placeholder install copy).
- Click **Publish**.
- Confirm the Homebrew tap was updated: [khawkins98/homebrew-tap/Casks/hush.rb](https://github.com/khawkins98/homebrew-tap/blob/main/Casks/hush.rb) should show the new version and the correct SHA256. If the workflow step failed (e.g. `GITHUB_TOKEN` lacked write access to the tap repo), update it manually — see the "Homebrew tap" section below.

Optionally, post-publish: download one artefact per platform and smoke-test the install path on a clean machine. Record any platform-specific install failures in `learnings.md` so the next release-cutter can avoid them.

## Dry-run via workflow_dispatch

To smoke the build matrix without cutting a real tag:

```bash
gh workflow run release.yml
```

Or: Actions → Release → "Run workflow" in the GitHub UI. The workflow publishes to a draft release tagged `dispatch-<run-id>` so dispatches don't pollute the tag list. Delete the draft after inspection.

## What can go wrong (early-days notes)

These are the things that have bitten previous attempts on similar Tauri pipelines; we'll record what hits us in our own runs as it happens.

- **Homebrew tap update fails**: the "Update Homebrew tap" workflow step clones `khawkins98/homebrew-tap` using `GITHUB_TOKEN`. The default `GITHUB_TOKEN` in Actions has write access to the repo that owns the workflow but **not** to other repos. If the step fails with a 403, create a Personal Access Token (PAT) with `repo` scope for `khawkins98/homebrew-tap`, add it as a repo secret named `TAP_GITHUB_TOKEN`, and update the workflow step to use `${{ secrets.TAP_GITHUB_TOKEN }}` instead.
- **macOS i8mm**: whisper.cpp's GGML uses an aarch64 intrinsic that needs `-march=armv8.6-a`. The workflow sets `CFLAGS` / `CXXFLAGS` for the apple-silicon leg; if a new Whisper revision changes the flag, builds break with a `requires target feature 'i8mm'` clang error.
- **Linux deps**: `tauri-action` pulls webkit2gtk-4.1, librsvg, libxdo, libasound and friends. The workflow installs them explicitly via apt; a new transitive dep would need adding to that list.
- **Windows code-signing**: when this lands, the `.msi` step also needs a working signtool integration — adding the cert without wiring signtool produces an unsigned artefact silently.

## After publishing

- The README's [Install](../README.md#install) section auto-pivots to the latest release via the GitHub Releases link — no edit needed.
- The Homebrew tap is updated automatically by the release workflow. Users on the tap get the new version with `brew upgrade --cask hush`.
- Users can hit **Settings → About → Check for updates** and see the new version surfaced; macOS users can also use **Hush → Check for Updates…** from the menu bar.
- Hush does **not** auto-update yet. Users have to download and install manually. Auto-update lives behind [#10](https://github.com/khawkins98/Hush/issues/10) — gated on a signing-key decision and on this pipeline producing artefacts the updater plugin can point at.

## Homebrew tap

The tap lives at [khawkins98/homebrew-tap](https://github.com/khawkins98/homebrew-tap). The release workflow patches `Casks/hush.rb` automatically on every tag push. If you need to update it manually (e.g. after a dry-run or a workflow failure):

```bash
# Compute the SHA256 of the released DMG (replace x.y.z with the new version)
curl -fsSL "https://github.com/khawkins98/Hush/releases/download/vx.y.z/Hush_x.y.z_aarch64.dmg" \
  | shasum -a 256

# Clone the tap, edit Casks/hush.rb (bump version + sha256), then commit + push
git clone https://github.com/khawkins98/homebrew-tap.git
# … edit Casks/hush.rb …
git commit -am "chore: update hush cask to vx.y.z"
git push
```

To verify the cask locally before a release:

```bash
brew tap khawkins98/tap
brew audit --cask khawkins98/tap/hush
```
