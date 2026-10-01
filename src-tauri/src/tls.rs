//! Process-wide rustls crypto provider.
//!
//! reqwest 0.13 is built with `rustls-no-provider` — the same choice
//! Tauri's own crates make — so it links no crypto backend of its own.
//! The alternative, reqwest's `rustls` feature, pulls in `aws-lc-rs`: a
//! C/assembly build we'd rather not add to every compile. The catch is
//! that building a client with no process-default provider panics, so
//! every path that constructs a `reqwest::Client` must call
//! [`ensure_crypto_provider`] first. `ring` is already in the tree via
//! `tauri-plugin-updater`, which installs it the same way.

use std::sync::Once;

/// Install `ring` as the process-default rustls provider. Idempotent
/// and cheap after the first call. Losing a race to another installer
/// (e.g. the updater plugin) is fine — any installed provider will do.
pub fn ensure_crypto_provider() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// A default client with the provider guaranteed. For tests and other
/// call sites that would otherwise use `reqwest::Client::new()` — never
/// call that directly: it panics unless some *other* code already
/// installed the provider, so tests pass or fail by execution order.
pub fn client() -> reqwest::Client {
    ensure_crypto_provider();
    reqwest::Client::new()
}
