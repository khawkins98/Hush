//! Updater — manual version probe (#223) + auto-update channel (#10).
//!
//! Two layers, shipped sequentially:
//!
//! - **Manual probe (`check_for_updates`)** — the lighter half,
//!   shipped under #223. Hits GitHub's
//!   `/repos/{owner}/{repo}/releases/latest`, compares the returned
//!   tag to the bundled `CARGO_PKG_VERSION`, and tells the user one
//!   of three things: up-to-date, an update is available (with a
//!   link to the release page), or the check failed (network down,
//!   GitHub rate-limited, etc.). No code running outside the user's
//!   click — strictly opt-in.
//! - **Background auto-update (#10)** — the heavier half. Wraps
//!   `tauri-plugin-updater`, signs releases with a maintainer-held
//!   keypair, polls a manifest on launch, downloads + verifies +
//!   installs. Gated on a pubkey decision and on the release pipeline
//!   (#222) producing artefacts the manifest can point at. Not in
//!   this module yet.
//!
//! ## Why a manual check ships first
//!
//! `tauri-plugin-updater` needs a signing key + an endpoint
//! manifest before it can do anything useful — neither of those
//! is in place yet. The manual probe needs neither: GitHub
//! Releases is the source of truth, and we only ever read the
//! tag name. Shipping it now gives users a "am I current?"
//! affordance for the entire window between today and #10
//! landing.

use serde::{Deserialize, Serialize};

use crate::ipc::commands::IpcError;

/// Local alias since `IpcResult` lives behind a private type alias
/// in `ipc::commands`. Spelling the `Result` shape inline here
/// keeps the updater module compilable as a sibling.
type UpdaterResult<T> = std::result::Result<T, IpcError>;

/// GitHub repo coordinates the manual probe asks about. Hardcoded
/// rather than configurable because there is exactly one upstream;
/// a build pretending to be Hush but pointing somewhere else is
/// a different application.
const RELEASE_OWNER: &str = "khawkins98";
const RELEASE_REPO: &str = "Hush";

/// Result the IPC command returns to the frontend. The variant
/// drives which dialog branch renders. Wire format is a tagged
/// enum so the frontend can pattern-match on `kind`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    tag = "kind"
)]
pub enum UpdateCheckResult {
    /// The bundled version is the latest. `current` carries the
    /// version string the user is on, so the dialog can render
    /// "You're on 0.2.0 — that's the latest."
    UpToDate { current: String },
    /// A newer release exists. The dialog renders the version,
    /// links to the release page, and stops there — install is
    /// the user's action via the linked browser tab.
    UpdateAvailable {
        current: String,
        latest: String,
        release_url: String,
    },
    /// The check itself failed (offline, GitHub returned a 5xx,
    /// rate-limited, JSON shape unexpected, version unparseable,
    /// …). `reason` is a short user-facing string; the dialog
    /// renders it without quoting the underlying error stack.
    CheckFailed { reason: String },
}

/// Subset of the GitHub Releases API response we care about.
/// Other fields (assets, body, author, …) are deliberately
/// ignored — the manual probe only needs the tag and the URL.
#[derive(Debug, Deserialize)]
struct GhRelease {
    tag_name: String,
    html_url: String,
}

/// Human-readable reason mappings for the failure branch. Kept
/// short so the dialog reads cleanly: full diagnostics live in
/// the tracing log, not in the user's face.
fn map_failure(e: impl std::fmt::Display) -> String {
    let raw = e.to_string();
    if raw.contains("rate limit") || raw.contains("403") {
        "GitHub is rate-limiting the request. Try again in a few minutes.".into()
    } else if raw.contains("dns") || raw.contains("connect") || raw.contains("network") {
        "Couldn't reach GitHub. Check your internet connection.".into()
    } else {
        format!("Couldn't check for updates: {raw}")
    }
}

/// Run the probe. Errors propagate as the `CheckFailed` variant —
/// a transport-level error is not a panic-worthy event, the user
/// just sees "couldn't check, try again."
pub async fn check_for_updates(client: &reqwest::Client) -> UpdaterResult<UpdateCheckResult> {
    let current = env!("CARGO_PKG_VERSION").to_owned();

    let url =
        format!("https://api.github.com/repos/{RELEASE_OWNER}/{RELEASE_REPO}/releases/latest");
    let response = match client
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = ?e, "check_for_updates: network error");
            return Ok(UpdateCheckResult::CheckFailed {
                reason: map_failure(e),
            });
        }
    };

    if !response.status().is_success() {
        let status = response.status();
        // 404 is a real signal: the repo has no releases yet
        // (a fresh fork, or the upstream is paused). Any other
        // status is a transport-shaped failure.
        let reason = if status == reqwest::StatusCode::NOT_FOUND {
            "No releases published yet on GitHub.".to_owned()
        } else {
            format!("GitHub returned {status}.")
        };
        tracing::warn!(?status, "check_for_updates: non-success status");
        return Ok(UpdateCheckResult::CheckFailed { reason });
    }

    let release: GhRelease = match response.json().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = ?e, "check_for_updates: JSON decode failed");
            return Ok(UpdateCheckResult::CheckFailed {
                reason: "GitHub returned an unexpected response shape.".into(),
            });
        }
    };

    // Strip a leading `v` if present — release tags are `v0.2.0`,
    // semver crate parses `0.2.0`. Tolerate either by normalising
    // here.
    let latest_str = release.tag_name.trim_start_matches('v').to_owned();

    let current_v = match semver::Version::parse(&current) {
        Ok(v) => v,
        Err(e) => {
            // Build configuration bug — the bundled CARGO_PKG_VERSION
            // doesn't parse as semver. Surface it; we'd rather know.
            return Err(IpcError::Settings(format!(
                "current version {current} is not valid semver: {e}"
            )));
        }
    };
    let latest_v = match semver::Version::parse(&latest_str) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = ?e, tag = %release.tag_name, "check_for_updates: latest tag not semver");
            return Ok(UpdateCheckResult::CheckFailed {
                reason: format!(
                    "Latest release tag '{}' is not a recognisable version.",
                    release.tag_name
                ),
            });
        }
    };

    if latest_v > current_v {
        Ok(UpdateCheckResult::UpdateAvailable {
            current,
            latest: latest_str,
            release_url: release.html_url,
        })
    } else {
        Ok(UpdateCheckResult::UpToDate { current })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_failure_recognises_rate_limit() {
        let m = map_failure("error 403 rate limit exceeded");
        assert!(m.contains("rate-limiting"), "got: {m}");
    }

    #[test]
    fn map_failure_recognises_offline() {
        let m = map_failure("dns lookup error");
        assert!(m.contains("Couldn't reach GitHub"), "got: {m}");
    }

    #[test]
    fn map_failure_falls_through_for_unknown_errors() {
        let m = map_failure("xyz unknown garbage");
        assert!(m.contains("Couldn't check"), "got: {m}");
        assert!(m.contains("xyz"), "got: {m}");
    }

    #[test]
    fn update_check_result_serialises_with_kind_tag() {
        let json = serde_json::to_string(&UpdateCheckResult::UpToDate {
            current: "0.1.0".into(),
        })
        .unwrap();
        assert!(json.contains("\"kind\":\"upToDate\""), "got: {json}");
        assert!(json.contains("\"current\":\"0.1.0\""), "got: {json}");
    }

    #[test]
    fn update_available_carries_all_three_fields() {
        let json = serde_json::to_string(&UpdateCheckResult::UpdateAvailable {
            current: "0.1.0".into(),
            latest: "0.2.0".into(),
            release_url: "https://github.com/khawkins98/Hush/releases/tag/v0.2.0".into(),
        })
        .unwrap();
        assert!(json.contains("\"kind\":\"updateAvailable\""), "got: {json}");
        assert!(json.contains("\"latest\":\"0.2.0\""), "got: {json}");
        assert!(json.contains("releaseUrl"), "got: {json}");
    }
}
