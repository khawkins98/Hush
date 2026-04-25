//! HTTP download orchestrator for Whisper model files.
//!
//! Pure-logic enough that the test suite drives it against a local
//! `wiremock` server — no real Hugging Face round-trips in CI. The
//! Tauri command layer wraps this with progress events and a cancel
//! handle.
//!
//! ## Why this lives in `transcription` and not `download` or `net`
//!
//! The download is for transcription models. Today the catalog (and
//! the SHA discipline) lives next to the inference glue. Splitting
//! into a generic `net::download` would suggest other parts of the
//! app might use it; nothing else does, and we don't want a hidden
//! HTTP-shaped affordance in a privacy-first codebase.
//!
//! ## SHA-256 verification policy
//!
//! Every download must be verified against an expected SHA-256. If
//! the catalog's `sha256` is empty (the initial state before a
//! contributor verifies the upstream hash), the IPC command refuses
//! to start the download and the picker falls back to "place file
//! manually" — see TODO(#41) for the verification process.
//!
//! There is **no trust-on-first-use mode**. The point of SHA
//! verification is to detect tampering between the upstream and the
//! user's disk; trusting a hash we computed during the same download
//! we're trying to verify defeats the purpose.
//!
//! ## Streaming + atomic rename
//!
//! The response body is consumed chunk-by-chunk so the caller can
//! report progress without buffering the whole model (largest is
//! ~3 GB). Bytes are written to a `<filename>.part` sibling file;
//! on completion, atomic rename to the final filename. On failure
//! (network drop, SHA mismatch, cancel), the `.part` file is
//! deleted. This mirrors the standard "crash-safe download" pattern
//! and means a half-finished download never looks like a complete
//! model file to the picker.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use tokio::fs;
use tokio::io::AsyncWriteExt;

/// Receives a per-chunk progress update during a streaming download.
///
/// The callback may be invoked many times per second; implementations
/// should be cheap (e.g. emit a Tauri event, update an atomic counter).
/// `total` is `None` when the server omits `Content-Length`, which
/// shouldn't happen for Hugging Face's CDN but is permitted by the
/// HTTP spec.
pub type ProgressCallback = dyn Fn(ProgressUpdate) + Send + Sync;

#[derive(Debug, Clone, Copy)]
pub struct ProgressUpdate {
    pub bytes_received: u64,
    pub bytes_total: Option<u64>,
}

/// Cooperative cancellation handle handed out by the IPC layer when a
/// download starts. The IPC `cancel` command flips the flag; the
/// download loop checks it after each chunk and aborts cleanly,
/// dropping the partial file.
///
/// Why an `AtomicBool` rather than a tokio `CancellationToken`: the
/// download is one-shot and lives entirely inside `download_with_progress`.
/// Pulling in `tokio_util` for the handful of operations we need would
/// be over-budget; an `AtomicBool` polled per chunk is plenty.
#[derive(Debug, Default, Clone)]
pub struct CancelHandle {
    flag: Arc<AtomicBool>,
}

impl CancelHandle {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.flag.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }
}

/// Download `url` to `dest`, streaming the body and verifying the
/// SHA-256 hash matches `expected_sha256_hex` on completion.
///
/// `dest` is where the final file will live; the function writes to
/// `<dest>.part` first and atomically renames on success. Parent
/// directories must already exist (the IPC layer creates the models
/// dir at startup).
///
/// # Cancellation
///
/// If `cancel.is_cancelled()` becomes true between chunks, the
/// download aborts: the `.part` file is removed and an error is
/// returned. The caller can distinguish cancel from other errors by
/// the message, but for the IPC layer's purposes "cancel" looks like
/// any other failure (we just don't event a `download-failed` for it).
pub async fn download_with_progress(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    expected_sha256_hex: &str,
    cancel: &CancelHandle,
    progress: &ProgressCallback,
) -> Result<()> {
    if expected_sha256_hex.trim().is_empty() {
        return Err(anyhow!(
            "missing SHA-256 in catalog — cannot verify download integrity"
        ));
    }

    let part_path = with_part_suffix(dest);

    // Tear down a stale `.part` from a prior interrupted run before
    // we start. The atomic rename at the end means a successful
    // download never leaves a `.part` behind, so anything we find
    // here is junk.
    let _ = fs::remove_file(&part_path).await;

    let response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("HTTP error from {url}"))?;

    let bytes_total = response.content_length();
    let mut hasher = Sha256::new();
    let mut bytes_received: u64 = 0;

    let mut file = fs::File::create(&part_path)
        .await
        .with_context(|| format!("create {}", part_path.display()))?;

    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        if cancel.is_cancelled() {
            // Drop the file handle before removing — Windows blocks
            // unlink while a handle is open.
            drop(file);
            let _ = fs::remove_file(&part_path).await;
            return Err(anyhow!("download cancelled by user"));
        }

        let chunk = chunk.context("read chunk from response stream")?;
        hasher.update(&chunk);
        file.write_all(&chunk)
            .await
            .with_context(|| format!("write to {}", part_path.display()))?;
        bytes_received += chunk.len() as u64;

        progress(ProgressUpdate {
            bytes_received,
            bytes_total,
        });
    }

    // Flush + close before the rename. The atomic rename only buys
    // us crash-safety if the file's data has actually hit the disk.
    file.flush()
        .await
        .with_context(|| format!("flush {}", part_path.display()))?;
    drop(file);

    let actual_sha = format!("{:x}", hasher.finalize());
    if !sha_eq(&actual_sha, expected_sha256_hex) {
        let _ = fs::remove_file(&part_path).await;
        return Err(anyhow!(
            "SHA-256 mismatch: expected {expected_sha256_hex}, got {actual_sha}"
        ));
    }

    fs::rename(&part_path, dest)
        .await
        .with_context(|| format!("rename {} -> {}", part_path.display(), dest.display()))?;

    Ok(())
}

/// Append `.part` to a path, preserving the parent directory. Used
/// for the in-flight download file before the atomic rename.
fn with_part_suffix(dest: &Path) -> std::path::PathBuf {
    let mut s = dest.as_os_str().to_owned();
    s.push(".part");
    std::path::PathBuf::from(s)
}

/// Case-insensitive compare for hex SHA strings. Catalog values may be
/// uppercase or lowercase; the hasher always produces lowercase.
fn sha_eq(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Digest;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Spawn a wiremock server returning `body` with a 200 OK on `GET /file`,
    /// and return the server + the URL.
    async fn serve_file(body: &[u8]) -> (MockServer, String) {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/file"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body.to_vec()))
            .mount(&server)
            .await;
        let url = format!("{}/file", server.uri());
        (server, url)
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        format!("{:x}", hasher.finalize())
    }

    fn no_progress() -> Box<ProgressCallback> {
        Box::new(|_| {})
    }

    #[tokio::test]
    async fn happy_path_downloads_and_renames_atomically() {
        let body = b"the quick brown fox";
        let expected_sha = sha256_hex(body);

        let (_server, url) = serve_file(body).await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("model.bin");

        let cancel = CancelHandle::new();
        let progress = no_progress();
        let client = reqwest::Client::new();

        download_with_progress(&client, &url, &dest, &expected_sha, &cancel, &progress)
            .await
            .expect("happy path should succeed");

        // The final file exists; the .part sibling does not.
        assert!(dest.exists());
        assert!(!dir.path().join("model.bin.part").exists());

        let written = std::fs::read(&dest).unwrap();
        assert_eq!(written, body);
    }

    #[tokio::test]
    async fn progress_callback_fires_with_increasing_bytes_received() {
        let body = vec![0x42_u8; 64 * 1024]; // big enough for multiple chunks
        let expected_sha = sha256_hex(&body);

        let (_server, url) = serve_file(&body).await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("model.bin");

        let updates = Arc::new(std::sync::Mutex::new(Vec::<ProgressUpdate>::new()));
        let updates_clone = Arc::clone(&updates);
        let progress: Box<ProgressCallback> = Box::new(move |u| {
            updates_clone.lock().unwrap().push(u);
        });

        let cancel = CancelHandle::new();
        let client = reqwest::Client::new();
        download_with_progress(&client, &url, &dest, &expected_sha, &cancel, &progress)
            .await
            .unwrap();

        let updates = updates.lock().unwrap();
        assert!(!updates.is_empty(), "progress callback never fired");
        let last = updates.last().unwrap();
        assert_eq!(last.bytes_received, body.len() as u64);
        assert_eq!(last.bytes_total, Some(body.len() as u64));
        // Monotonic non-decreasing.
        let mut prev = 0;
        for u in updates.iter() {
            assert!(u.bytes_received >= prev);
            prev = u.bytes_received;
        }
    }

    #[tokio::test]
    async fn sha_mismatch_deletes_partial_and_errors() {
        let body = b"the quick brown fox";
        let wrong_sha = "0".repeat(64); // valid hex shape, deliberately wrong

        let (_server, url) = serve_file(body).await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("model.bin");

        let cancel = CancelHandle::new();
        let progress = no_progress();
        let client = reqwest::Client::new();

        let err = download_with_progress(&client, &url, &dest, &wrong_sha, &cancel, &progress)
            .await
            .expect_err("SHA mismatch must surface as error");
        assert!(err.to_string().contains("SHA-256 mismatch"), "got: {err}");

        // Neither final nor .part should remain — a corrupt download
        // should not look like a complete file to the picker.
        assert!(!dest.exists());
        assert!(!dir.path().join("model.bin.part").exists());
    }

    #[tokio::test]
    async fn empty_expected_sha_fails_fast_without_request() {
        // The IPC layer also gates on this, but defending in depth:
        // the orchestrator itself refuses an empty hash so we can't
        // accidentally bypass the check by calling it directly.
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("model.bin");
        let cancel = CancelHandle::new();
        let progress = no_progress();
        let client = reqwest::Client::new();

        let err = download_with_progress(
            &client,
            "http://this.url.must.never.be.contacted.invalid/file",
            &dest,
            "",
            &cancel,
            &progress,
        )
        .await
        .expect_err("empty SHA must error");
        assert!(err.to_string().contains("SHA-256"), "got: {err}");
        assert!(!dest.exists());
    }

    #[tokio::test]
    async fn cancel_during_download_drops_partial_and_errors() {
        // Build a body big enough that the download spans multiple
        // chunks; flip the cancel flag from the progress callback the
        // first time we see any bytes at all.
        let body = vec![0xAB_u8; 256 * 1024];
        let expected_sha = sha256_hex(&body);

        let (_server, url) = serve_file(&body).await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("model.bin");

        let cancel = CancelHandle::new();
        let cancel_clone = cancel.clone();
        let progress: Box<ProgressCallback> = Box::new(move |_| {
            cancel_clone.cancel();
        });

        let client = reqwest::Client::new();
        let err = download_with_progress(&client, &url, &dest, &expected_sha, &cancel, &progress)
            .await
            .expect_err("cancel must surface as error");
        assert!(err.to_string().contains("cancelled"), "got: {err}");

        assert!(!dest.exists());
        assert!(!dir.path().join("model.bin.part").exists());
    }

    #[test]
    fn part_suffix_appends_dot_part() {
        let dest = std::path::PathBuf::from("/tmp/models/foo.bin");
        let part = with_part_suffix(&dest);
        assert_eq!(part, std::path::PathBuf::from("/tmp/models/foo.bin.part"));
    }

    #[test]
    fn sha_compare_is_case_insensitive() {
        assert!(sha_eq("ABCDEF", "abcdef"));
        assert!(sha_eq("0123abcd", "0123ABCD"));
        assert!(!sha_eq("0123", "0124"));
    }
}
