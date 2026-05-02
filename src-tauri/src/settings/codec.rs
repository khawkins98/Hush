//! Typed encode/decode helpers for the K/V settings store (#431).
//!
//! Settings are persisted as raw strings in the `settings` table —
//! the store has no type system. Per-key encode/decode lives on the
//! caller, and prior to this module the `if v { "true" } else
//! { "false" }` literal was inlined at every `set_*_inner` IPC call
//! site. The architecture audit flagged that as latent drift waiting
//! to happen: a future contributor adds a new bool row, picks
//! `"1"`/`"0"` (or `"on"`/`"off"`) instead of the existing
//! convention, and the parse helpers silently treat the value as
//! absent / fall back to the default.
//!
//! The codec is intentionally minimal — one primitive per shape we
//! actually have on disk today. The per-key `parse_*_setting` helpers
//! in [`crate::ipc`] still own the per-key default policy (some keys
//! treat absence as on, some as off); this module just gives them a
//! shared decode primitive so adding a new bool row doesn't introduce
//! a new "what string did we agree to write" decision.
//!
//! When a future setting needs a richer encoding (e.g. an enum with
//! more than two variants), prefer adding a small per-type module
//! beside this one — see [`crate::meeting::MeetingAutostartMode`]'s
//! `from_setting` / `as_setting` for the existing pattern — rather
//! than expanding this primitive into a generic serialiser.

/// Canonical bool encoding for settings rows. The pair `"true"` /
/// `"false"` matches every existing on-disk bool today; any other
/// shape would need a migration.
pub const BOOL_TRUE: &str = "true";
pub const BOOL_FALSE: &str = "false";

/// Encode a `bool` for persistence. Always returns one of the two
/// canonical literals — never `"1"`, `"on"`, or platform-specific
/// shorthand.
pub fn encode_bool(v: bool) -> &'static str {
    if v {
        BOOL_TRUE
    } else {
        BOOL_FALSE
    }
}

/// Decode a persisted bool row. Strict by design — only the
/// canonical literals decode. `None` covers absent rows and
/// unparseable junk; the caller decides the per-key default.
///
/// Symmetric with [`encode_bool`]: every value `encode_bool` produces
/// round-trips back through this function. The `parse_*_setting`
/// helpers in `crate::ipc` are thin wrappers that bake in the
/// per-key default.
pub fn decode_bool(raw: Option<&str>) -> Option<bool> {
    match raw {
        Some(BOOL_TRUE) => Some(true),
        Some(BOOL_FALSE) => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_bool_returns_canonical_literals() {
        assert_eq!(encode_bool(true), "true");
        assert_eq!(encode_bool(false), "false");
    }

    #[test]
    fn decode_bool_round_trips_canonical_literals() {
        assert_eq!(decode_bool(Some("true")), Some(true));
        assert_eq!(decode_bool(Some("false")), Some(false));
    }

    #[test]
    fn decode_bool_rejects_non_canonical_shapes() {
        // Strict — no "1"/"0", no "yes"/"no", no "on"/"off". A
        // contributor adding a new row in one of these shapes will
        // hit `None` and quickly notice the round-trip break;
        // silent-acceptance was the failure mode this codec is
        // closing.
        for raw in ["1", "0", "yes", "no", "on", "off", "True", "FALSE", ""] {
            assert_eq!(decode_bool(Some(raw)), None, "raw {raw:?}");
        }
        assert_eq!(decode_bool(None), None);
    }

    #[test]
    fn encode_then_decode_is_identity() {
        for v in [true, false] {
            let raw = encode_bool(v);
            assert_eq!(decode_bool(Some(raw)), Some(v));
        }
    }
}
