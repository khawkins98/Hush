//! Runtime tuning for the global mimalloc allocator (#612 / #636 follow-up).
//!
//! Why this exists: whisper.cpp allocates and frees tens of MB of scratch
//! per `whisper_full` call. mimalloc (the global allocator since #639)
//! reuses those pages, but its default purge policy — `purge_delay` of
//! 1000 ms plus lazy arena purging — cannot keep up with the meeting
//! pump's churn. Freed pages sit committed-and-dirty in mimalloc's
//! arenas, macOS compresses or swaps them, and the process's *physical
//! footprint* (Activity Monitor's "Memory" column) grows by roughly
//! 1 GB/min during a meeting even though RSS stays bounded by the #612
//! WhisperState recreation. See learnings.md 2026-06-01 for the full
//! investigation (RSS sawtooth vs 40 GB footprint decomposition).
//!
//! Two levers, both applied unless `HUSH_ALLOC_PURGE=0`:
//!
//! 1. `purge_delay = 0` — freed OS pages are decommitted immediately.
//!    On macOS the bundled mimalloc v3 decommits with
//!    `madvise(MADV_FREE_REUSABLE)`, which has correct physical-footprint
//!    accounting (mimalloc issue #1097), so an immediate purge actually
//!    shows up in Activity Monitor.
//! 2. [`force_collect`] after each whisper streaming inference — returns
//!    the just-freed scratch pages from the inference thread's heap to
//!    the OS right away instead of leaving them on thread-local free
//!    lists where the arena purge never sees them.
//!
//! `HUSH_ALLOC_PURGE=0` disables both. This is the A/B knob: the same
//! build can run a baseline meeting (tuning off) and a fix meeting
//! (tuning on) so footprint deltas are attributable to this module alone.

use std::sync::atomic::{AtomicBool, Ordering};

/// Whether allocator purge tuning is active. Set once by [`init`]; read
/// on every [`force_collect`] call.
static PURGE_TUNING_ENABLED: AtomicBool = AtomicBool::new(false);

/// `mi_option_purge_delay`'s index in mimalloc v3's `mi_option_t` enum.
///
/// libmimalloc-sys 0.1.49's `extended` bindings predate the v3 option
/// enum and don't export this constant, so we pin the index ourselves.
/// The index is identical in the bundled v2 and v3 sources
/// (`c_src/mimalloc/*/include/mimalloc.h`). [`init`] guards against
/// index drift across future libmimalloc-sys bumps by checking that the
/// option's compiled-in default (1000 ms) is what we expect *before*
/// writing to it — if the enum ever shifts, we refuse to tune rather
/// than silently set a different option to zero.
const MI_OPTION_PURGE_DELAY: libmimalloc_sys::mi_option_t = 15;

/// The compiled-in default for `purge_delay` in the bundled mimalloc v3
/// (`options.c`): 1000 ms. Used by [`init`]'s index-drift guard.
const EXPECTED_PURGE_DELAY_DEFAULT: std::os::raw::c_long = 1000;

/// Configure mimalloc for aggressive purge-on-free. Call once, early in
/// `run()`, after tracing is initialised (the outcome is logged).
///
/// Safe to call at any point: `mi_option_set` applies to all subsequent
/// purge scheduling regardless of how much allocation already happened.
pub fn init() {
    let disabled = matches!(
        std::env::var("HUSH_ALLOC_PURGE").as_deref(),
        Ok("0") | Ok("off") | Ok("false")
    );
    if disabled {
        tracing::info!("allocator purge tuning disabled (HUSH_ALLOC_PURGE=0) — baseline mode");
        return;
    }

    // Index-drift guard: read before write. A value of 0 means the
    // option was already set (either by a previous `init` call or by an
    // operator-supplied `MIMALLOC_PURGE_DELAY=0`); the expected default
    // means the index still points at `purge_delay`. Anything else —
    // an unexpected operator override or a shifted enum after a
    // libmimalloc-sys bump — and we leave the allocator alone.
    //
    // SAFETY: `mi_option_get` / `mi_option_set` are documented
    // thread-safe and callable at any time after allocator init (which
    // happened long before `run()` since mimalloc is the
    // `#[global_allocator]`).
    let current = unsafe { libmimalloc_sys::mi_option_get(MI_OPTION_PURGE_DELAY) };
    if current != EXPECTED_PURGE_DELAY_DEFAULT && current != 0 {
        tracing::warn!(
            current,
            expected = EXPECTED_PURGE_DELAY_DEFAULT,
            "allocator purge tuning skipped: purge_delay read-back doesn't match the bundled \
             mimalloc default — either MIMALLOC_PURGE_DELAY is set externally or the option \
             enum shifted across a libmimalloc-sys bump"
        );
        return;
    }
    if current != 0 {
        unsafe { libmimalloc_sys::mi_option_set(MI_OPTION_PURGE_DELAY, 0) };
    }

    PURGE_TUNING_ENABLED.store(true, Ordering::Relaxed);
    tracing::info!(
        "allocator purge tuning enabled: mimalloc purge_delay=0 + post-inference force-collect \
         (disable with HUSH_ALLOC_PURGE=0)"
    );
}

/// Force-collect the calling thread's mimalloc heap and purge arena
/// pages back to the OS.
///
/// Call from the thread that just freed a large burst of memory — in
/// practice the whisper inference thread right after `whisper_full`
/// returns (and after the periodic #612 WhisperState drop), so the
/// multi-MB scratch that call freed is decommitted immediately rather
/// than left dirty for macOS to compress. No-op when tuning is
/// disabled. Cost is sub-millisecond against an inference that takes
/// 1–3 s.
pub fn force_collect() {
    if !PURGE_TUNING_ENABLED.load(Ordering::Relaxed) {
        return;
    }
    // SAFETY: `mi_collect` is documented thread-safe; `true` forces a
    // full collect of the calling thread's heap including arena purging.
    unsafe { libmimalloc_sys::mi_collect(true) };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Single combined test: `init` mutates process-global state (env
    /// var read, mimalloc option, the enabled flag), so the disabled and
    /// enabled paths must run sequentially inside one test rather than
    /// as separate tests cargo could schedule in parallel.
    #[test]
    fn init_respects_disable_env_then_enables_and_collects() {
        // Disabled path: flag stays false, force_collect is a no-op.
        // SAFETY: test-only env mutation; this test owns HUSH_ALLOC_PURGE.
        unsafe { std::env::set_var("HUSH_ALLOC_PURGE", "0") };
        init();
        assert!(
            !PURGE_TUNING_ENABLED.load(Ordering::Relaxed),
            "HUSH_ALLOC_PURGE=0 must leave tuning disabled"
        );
        force_collect(); // must be a safe no-op while disabled

        // Enabled path: option is set, flag flips, collect runs for real.
        // SAFETY: as above.
        unsafe { std::env::remove_var("HUSH_ALLOC_PURGE") };
        init();
        assert!(
            PURGE_TUNING_ENABLED.load(Ordering::Relaxed),
            "default init() must enable tuning"
        );
        let delay = unsafe { libmimalloc_sys::mi_option_get(MI_OPTION_PURGE_DELAY) };
        assert_eq!(delay, 0, "purge_delay must read back as 0 after init()");
        force_collect(); // exercises the real mi_collect path

        // Idempotence: a second enabled init() (purge_delay already 0)
        // must not warn-and-disable.
        init();
        assert!(PURGE_TUNING_ENABLED.load(Ordering::Relaxed));
    }
}
