//! Shared memory sampler for diagnostic tests (macOS).
//!
//! Test-only. Shells out to `ps` / `footprint` / `vmmap` so the numbers
//! are directly comparable to what `npm run memwatch` reports and to the
//! figures recorded in `learnings.md`.
//!
//! **Read footprint, not RSS.** `docs/memory-debugging.md` exists
//! largely because leaks in this codebase have repeatedly hidden in
//! compressed dirty pages that RSS does not count. Both are reported;
//! footprint is the one that matters.

/// Physical footprint, RSS, and the IOAccelerator summary line for the
/// current process.
///
/// IOAccelerator is the #641 signal: ORT's Apple Silicon prebuilts
/// dispatch through Metal Performance Shaders even on the CPU execution
/// provider, and each `session.run` was observed to allocate regions
/// pinned to the `Session` lifetime. Its *absence* means no Metal
/// dispatch at all; a *constant* region count means bounded use;
/// monotonic growth is the bug.
#[cfg(target_os = "macos")]
pub(crate) fn sample_memory() -> String {
    let pid = std::process::id().to_string();

    let rss_kb = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .unwrap_or_default();

    let footprint = std::process::Command::new("footprint")
        .arg(&pid)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| {
            s.lines()
                .find(|l| l.contains("Footprint:"))
                .and_then(|l| l.split("Footprint:").nth(1))
                .map(|v| v.trim().to_owned())
        })
        .unwrap_or_else(|| "n/a".into());

    let ioaccel = std::process::Command::new("vmmap")
        .args(["-summary", &pid])
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .find(|l| l.to_lowercase().contains("ioaccelerator"))
                .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        })
        .unwrap_or_else(|| "IOAccelerator: none".into());

    format!("rss={rss_kb}KB footprint={footprint} | {ioaccel}")
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn sample_memory() -> String {
    "memory sampling is macOS-only".to_owned()
}
