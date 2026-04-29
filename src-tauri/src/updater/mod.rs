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

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::ipc::commands::{IpcError, IpcResult};

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

/// Per-request timeout for the update probe. The shared
/// [`crate::ipc::AppState::http`] client is configured with a
/// 600-second timeout for the whisper-model download path
/// (multi-GB GGUF files). The releases.latest payload is tens of
/// KB; 15 s is generous and shortens the worst-case slow-loris
/// hang from ten minutes to one TCP keepalive cycle.
const UPDATE_CHECK_TIMEOUT: Duration = Duration::from_secs(15);

/// Maximum response body size we'll consume from GitHub. The
/// real `/releases/latest` payload is ~5–20 KB; 64 KiB is well
/// over that. Defends against a MITM holding a valid
/// `api.github.com` cert who'd otherwise stream multi-GB JSON
/// to exhaust memory.
const UPDATE_CHECK_MAX_BYTES: usize = 64 * 1024;

/// Run the probe. Errors propagate as the `CheckFailed` variant —
/// a transport-level error is not a panic-worthy event, the user
/// just sees "couldn't check, try again."
pub async fn check_for_updates(client: &reqwest::Client) -> IpcResult<UpdateCheckResult> {
    let url =
        format!("https://api.github.com/repos/{RELEASE_OWNER}/{RELEASE_REPO}/releases/latest");
    check_for_updates_at(client, &url, env!("CARGO_PKG_VERSION")).await
}

/// Variant the IPC entry point and tests both call. Splitting it
/// out lets a wiremock test point `url` at a local server without
/// needing a network round trip to api.github.com, and lets a unit
/// test override the "current" version without rebuilding the crate
/// to flip `CARGO_PKG_VERSION`.
async fn check_for_updates_at(
    client: &reqwest::Client,
    url: &str,
    current_version: &str,
) -> IpcResult<UpdateCheckResult> {
    let current = current_version.to_owned();

    let response = match client
        .get(url)
        .timeout(UPDATE_CHECK_TIMEOUT)
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
            format!("GitHub returned an error ({status}). Try again in a minute.")
        };
        tracing::warn!(?status, "check_for_updates: non-success status");
        return Ok(UpdateCheckResult::CheckFailed { reason });
    }

    // Read the body with an explicit size cap so a MITM can't push
    // a multi-GB JSON document through. We deliberately read into a
    // bounded `Vec<u8>` first and parse JSON ourselves rather than
    // relying on `response.json()` (which has no body cap).
    let body_bytes = match response.bytes().await {
        Ok(b) if b.len() > UPDATE_CHECK_MAX_BYTES => {
            tracing::warn!(
                len = b.len(),
                cap = UPDATE_CHECK_MAX_BYTES,
                "check_for_updates: response body exceeded cap"
            );
            return Ok(UpdateCheckResult::CheckFailed {
                reason: "GitHub returned an unexpectedly large response.".into(),
            });
        }
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(error = ?e, "check_for_updates: body read failed");
            return Ok(UpdateCheckResult::CheckFailed {
                reason: map_failure(e),
            });
        }
    };

    let release: GhRelease = match serde_json::from_slice(&body_bytes) {
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
            // Build-configuration defect — the bundled
            // `CARGO_PKG_VERSION` doesn't parse as semver. Surface
            // as `Internal` (not `Settings`, which would render the
            // wrong error copy on the frontend); we'd rather know.
            return Err(IpcError::Internal(format!(
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

    // -- HTTP-level wiremock tests ----------------------------------------
    //
    // These exercise the whole request/response handling against a
    // local mock server, complementing the unit tests above (which
    // cover the pure helpers in isolation). The transport-shape
    // branches — non-success status, oversize body, malformed JSON —
    // are nearly impossible to exercise without a fake HTTP endpoint.

    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Builds the URL the production code would build, but pointed
    /// at the mock server.
    fn mock_url(server: &MockServer) -> String {
        format!(
            "{}/repos/{}/{}/releases/latest",
            server.uri(),
            RELEASE_OWNER,
            RELEASE_REPO
        )
    }

    fn release_json(tag: &str) -> serde_json::Value {
        serde_json::json!({
            "tag_name": tag,
            "html_url": format!("https://github.com/khawkins98/Hush/releases/tag/{tag}"),
        })
    }

    #[tokio::test]
    async fn returns_up_to_date_when_tag_matches_current() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/repos/{RELEASE_OWNER}/{RELEASE_REPO}/releases/latest"
            )))
            .and(header("Accept", "application/vnd.github+json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(release_json("v0.2.0")))
            .expect(1)
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let result = check_for_updates_at(&client, &mock_url(&server), "0.2.0")
            .await
            .unwrap();

        assert_eq!(
            result,
            UpdateCheckResult::UpToDate {
                current: "0.2.0".into()
            }
        );
    }

    #[tokio::test]
    async fn returns_update_available_when_tag_is_newer() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(release_json("v0.3.0")))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let result = check_for_updates_at(&client, &mock_url(&server), "0.2.0")
            .await
            .unwrap();

        match result {
            UpdateCheckResult::UpdateAvailable {
                current,
                latest,
                release_url,
            } => {
                assert_eq!(current, "0.2.0");
                assert_eq!(latest, "0.3.0");
                assert!(
                    release_url.ends_with("/v0.3.0"),
                    "release_url should carry the tag: {release_url}"
                );
            }
            other => panic!("expected UpdateAvailable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unprefixed_tag_normalises_correctly() {
        // Tag is "0.3.0" without the leading `v`. Production stripping
        // logic should still parse it.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(release_json("0.3.0")))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let result = check_for_updates_at(&client, &mock_url(&server), "0.2.0")
            .await
            .unwrap();

        assert!(matches!(
            result,
            UpdateCheckResult::UpdateAvailable { ref latest, .. } if latest == "0.3.0"
        ));
    }

    #[tokio::test]
    async fn maps_404_to_no_releases_published_yet() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let result = check_for_updates_at(&client, &mock_url(&server), "0.2.0")
            .await
            .unwrap();

        match result {
            UpdateCheckResult::CheckFailed { reason } => {
                assert!(
                    reason.contains("No releases published"),
                    "404 should map to the no-releases copy, got: {reason}"
                );
            }
            other => panic!("expected CheckFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn maps_5xx_to_generic_failure_copy() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let result = check_for_updates_at(&client, &mock_url(&server), "0.2.0")
            .await
            .unwrap();

        match result {
            UpdateCheckResult::CheckFailed { reason } => {
                assert!(
                    reason.contains("503") && reason.contains("Try again"),
                    "5xx should mention status + retry hint, got: {reason}"
                );
            }
            other => panic!("expected CheckFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rejects_oversize_body() {
        // Build a JSON payload larger than UPDATE_CHECK_MAX_BYTES so
        // the cap branch fires. Real GitHub responses are ~5–20 KB;
        // our cap is 64 KiB, so a 128 KiB filler is comfortably over.
        let huge_field = "x".repeat(128 * 1024);
        let body = serde_json::json!({
            "tag_name": "v0.3.0",
            "html_url": "https://example.test/",
            "body": huge_field,
        });
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let result = check_for_updates_at(&client, &mock_url(&server), "0.2.0")
            .await
            .unwrap();

        match result {
            UpdateCheckResult::CheckFailed { reason } => {
                assert!(
                    reason.contains("unexpectedly large"),
                    "oversize body should map to size-cap copy, got: {reason}"
                );
            }
            other => panic!("expected CheckFailed for oversize body, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn malformed_json_maps_to_unexpected_shape() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json at all"))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let result = check_for_updates_at(&client, &mock_url(&server), "0.2.0")
            .await
            .unwrap();

        match result {
            UpdateCheckResult::CheckFailed { reason } => {
                assert!(
                    reason.contains("unexpected response shape"),
                    "non-JSON body should map to shape copy, got: {reason}"
                );
            }
            other => panic!("expected CheckFailed for non-JSON, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn nonsense_tag_maps_to_unparseable_failure() {
        // Server says the latest release is tagged "release-2026-spring".
        // semver can't parse that — the check should land in
        // CheckFailed with a copy that quotes the original tag.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(release_json("release-2026-spring")),
            )
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let result = check_for_updates_at(&client, &mock_url(&server), "0.2.0")
            .await
            .unwrap();

        match result {
            UpdateCheckResult::CheckFailed { reason } => {
                assert!(
                    reason.contains("release-2026-spring"),
                    "unparseable tag should be quoted in the failure copy, got: {reason}"
                );
            }
            other => panic!("expected CheckFailed for non-semver tag, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn malformed_current_version_returns_internal_error() {
        // The current version doesn't reach the network, but the
        // probe still fires the HTTP request first. Mock a normal
        // response so the body is consumed cleanly, then assert the
        // post-network parse failure surfaces as IpcError::Internal.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(release_json("v0.3.0")))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let result = check_for_updates_at(&client, &mock_url(&server), "not-a-version").await;

        match result {
            Err(IpcError::Internal(msg)) => {
                assert!(
                    msg.contains("not-a-version"),
                    "Internal error should quote the bad version, got: {msg}"
                );
            }
            other => panic!("expected IpcError::Internal, got {other:?}"),
        }
    }
}
