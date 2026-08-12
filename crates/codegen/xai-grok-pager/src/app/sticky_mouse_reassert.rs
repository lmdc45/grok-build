//! Drain-coalesced mouse repair after FocusGained / Resize.
//!
//! Event handlers only set a pending bit; the event loop flushes **once** after
//! draining a batch. That collapses storms without a wall-clock cooldown so a
//! genuine host mode strip on the next drain is still repaired.
//!
//! Pure helpers + pending-flag atomics live here so unit tests can pin the
//! coalesce contract without spinning the full TUI.

use std::sync::atomic::{AtomicBool, Ordering};

/// Coalesce FocusGained/Resize into one CSI write per event-loop drain.
static MOUSE_REASSERT_PENDING: AtomicBool = AtomicBool::new(false);

/// Mark that mouse modes may need repair. Does not write CSI.
pub(crate) fn request_mouse_reassert() {
    MOUSE_REASSERT_PENDING.store(true, Ordering::Release);
}

/// Consume the pending bit. Returns true if a flush should *consider* writing.
///
/// The bit is cleared even when later ownership/readiness guards fail — stale
/// requests must not survive TUI teardown/re-init (see flush call site).
pub(crate) fn take_mouse_reassert_pending() -> bool {
    MOUSE_REASSERT_PENDING.swap(false, Ordering::AcqRel)
}

/// Whether to emit mouse CSI after the pending bit was consumed as `true`.
///
/// Separated for unit tests (and for clear gate order in flush).
#[inline]
pub(crate) fn should_emit_mouse_repair(
    owned: bool,
    fullscreen: bool,
    mouse_enabled: bool,
    tui_ready: bool,
) -> bool {
    owned && fullscreen && mouse_enabled && tui_ready
}

/// N request() calls in one drain → 1 write; empty drain → 0.
#[inline]
pub(crate) fn reassert_writes_for_drain_requests(request_count: usize) -> usize {
    usize::from(request_count > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_flag_coalesces_multiple_requests_into_one_take() {
        // Equivalent to: request×3 → flush_count 1; flush again → 0; request → 1
        let _ = take_mouse_reassert_pending(); // clear any prior
        request_mouse_reassert();
        request_mouse_reassert();
        request_mouse_reassert();
        assert!(
            take_mouse_reassert_pending(),
            "first take after requests must be true"
        );
        assert!(
            !take_mouse_reassert_pending(),
            "second take without request must be false"
        );
        request_mouse_reassert();
        assert!(
            take_mouse_reassert_pending(),
            "new request after take must re-arm"
        );
    }

    #[test]
    fn drain_request_counts_map_to_one_write_per_non_empty_batch() {
        assert_eq!(reassert_writes_for_drain_requests(0), 0);
        assert_eq!(reassert_writes_for_drain_requests(1), 1);
        assert_eq!(reassert_writes_for_drain_requests(50), 1);
    }

    #[test]
    fn emit_gates_require_all_ownership_conditions() {
        assert!(should_emit_mouse_repair(true, true, true, true));
        assert!(!should_emit_mouse_repair(false, true, true, true));
        assert!(!should_emit_mouse_repair(true, false, true, true));
        assert!(!should_emit_mouse_repair(true, true, false, true));
        assert!(!should_emit_mouse_repair(true, true, true, false));
    }

    #[test]
    fn take_clears_pending_even_if_emit_would_be_skipped() {
        // Models flush: swap(false) first, then guards fail → request discarded.
        let _ = take_mouse_reassert_pending();
        request_mouse_reassert();
        let pending = take_mouse_reassert_pending();
        assert!(pending);
        // Guards fail (e.g. TUI no longer ready) — do not re-store pending.
        let emit = should_emit_mouse_repair(true, true, true, /* tui_ready */ false);
        assert!(!emit);
        assert!(
            !take_mouse_reassert_pending(),
            "stale request must not remain after failed emit"
        );
    }
}
