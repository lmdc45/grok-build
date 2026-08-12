//! Terminal restore sequences for signal handler context.
//!
//! See <https://invisible-island.net/xterm/ctlseqs/ctlseqs.html> (DEC
//! Private Mode Reset / "Mouse Tracking" section) for the full spec.
//!
//! # Sticky mouse tracking after unclean exit
//!
//! Hosts that honor DEC private modes (xterm.js / VS Code integrated
//! terminal, Windows Terminal via ConPTY, JediTerm, …) keep mouse reporting
//! on until they receive the CSI `?100xl` resets. If the process dies without
//! writing those bytes — `TerminateProcess`, `kill -9`, hard crash before
//! teardown — the *next* shell in that tab receives SGR mouse reports as raw
//! text (`[M…`, `[MC…`).
//!
//! Mitigations (split by lifecycle — do not merge into one CTRL_CLOSE path):
//! 1. **Init**: always write [`MOUSE_TRACKING_RESET`] before enabling mouse.
//! 2. **Real exit / Drop / panic**: write [`RESTORE_SEQ`] (leave alt-screen,
//!    clear mouse). The host also clears mouse on process exit + strips mouse
//!    enables from PTY replay.
//! 3. **Windows CTRL_CLOSE**: do **not** write [`RESTORE_SEQ`]. Reload may
//!    keep ConPTY + process alive; leaving alt-screen would trash a live TUI.
//!    Do nothing destructive — repair is **idempotent** mouse/focus re-enable
//!    on genuine FocusGained (any host), not a reload-specific flag.
//! 4. **FocusGained** (pager): while fullscreen + mouse desired, re-send
//!    [`MOUSE_ENABLE_SEQ`] (+ focus). No EnterAlternateScreen. DECSET enables
//!    are idempotent; no DECRQM probe required.
//!
//! Do **not** try to clear sticky modes by typing a shell one-liner into the
//! terminal's stdin: that pollutes the prompt and never reaches xterm.js as
//! *app output*.

// -----------------------------------------------------------------------
// Canonical list of DEC private modes we enable.
//
//   Mode    Purpose                                          Enabled by
//   ----    -------                                          ----------
//   ?1000   Normal mouse tracking (X11 press/release)        EnableMouseCapture
//   ?1002   Button-event mouse tracking (cell-motion held)   EnableMouseCapture
//   ?1003   All-motion mouse tracking (any movement)         EnableMouseCapture
//   ?1015   RXVT extended mouse reporting (coords >223)      EnableMouseCapture
//   ?1006   SGR extended mouse reporting format (preferred)  EnableMouseCapture
//   ?2004   Bracketed paste mode                             EnableBracketedPaste
//   ?1004   Focus reporting (focus in/out events)            EnableFocusChange
//   ?25     Cursor visibility (show)                         cursor::Hide
//   ?1049   Alternate screen buffer                          EnterAlternateScreen
//   ?2026   Synchronized update                              BeginSynchronizedUpdate
//   CSI<u   Kitty keyboard protocol pop                      PushKeyboardEnhancementFlags
// -----------------------------------------------------------------------

/// Raw CSI sequences to disable every mouse-tracking mode the pager enables.
pub const MOUSE_TRACKING_RESET: &[u8] = b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1015l\x1b[?1006l";

/// Raw CSI sequences to disable mouse tracking and bracketed paste.
pub const MOUSE_PASTE_RESET: &[u8] =
    b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1015l\x1b[?1006l\x1b[?2004l";

/// Full escape sequence to restore the terminal to a sane state (real exit only).
pub const RESTORE_SEQ: &[u8] =
    b"\x1b[?2026l\x1b[?25h\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1015l\x1b[?1006l\x1b[?2004l\x1b[?1004l\x1b[<u\x1b[?1049l";

/// Non-destructive **mouse** modes Grok wants while the fullscreen TUI is active.
/// Safe to re-send after host strip (DECSET enables are idempotent).
/// Does **not** enter/leave the alternate screen and does **not** toggle focus
/// reporting (`?1004`) — re-firing focus enable on every FocusGained can storm
/// CSI I / freeze some hosts. Focus is re-enabled separately via crossterm.
///
/// Modes: normal/button/any tracking, urxvt + SGR encoding.
pub const MOUSE_ENABLE_SEQ: &[u8] =
    b"\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1015h\x1b[?1006h";

/// Legacy full display reassert (enter alt + mouse + focus + paste).
/// **Not** used on FocusGained — alt re-entry can wipe the buffer on ConPTY.
/// Kept for tests / rare recovery when stranded on the main buffer after a
/// mistaken RESTORE while the process still lives.
pub const REASSERT_DISPLAY_SEQ: &[u8] = b"\
\x1b[?1049h\
\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1015h\x1b[?1006h\
\x1b[?1004h\
\x1b[?2004h\
\x1b[?25l";

/// Write [`MOUSE_TRACKING_RESET`] to `w` (app *output* path, typically stderr).
pub fn write_mouse_tracking_reset(w: &mut impl std::io::Write) -> std::io::Result<usize> {
    w.write(MOUSE_TRACKING_RESET)
}

/// Write [`MOUSE_PASTE_RESET`] to `w` and flush.
pub fn write_mouse_paste_reset(w: &mut impl std::io::Write) -> std::io::Result<()> {
    w.write_all(MOUSE_PASTE_RESET)?;
    w.flush()
}

/// Write [`REASSERT_DISPLAY_SEQ`] (raw CSI, no crossterm state).
pub fn write_reassert_display(w: &mut impl std::io::Write) -> std::io::Result<()> {
    w.write_all(REASSERT_DISPLAY_SEQ)?;
    w.flush()
}

/// Write [`MOUSE_ENABLE_SEQ`] only (raw CSI). Does not touch alt-screen.
pub fn write_mouse_enable(w: &mut impl std::io::Write) -> std::io::Result<()> {
    w.write_all(MOUSE_ENABLE_SEQ)?;
    w.flush()
}

/// Write [`RESTORE_SEQ`] then [`MOUSE_TRACKING_RESET`] to both stdout and
/// stderr via raw OS writes (no Rust locks / buffering).
///
/// Use at TUI **init** (clear sticky from a prior unclean exit) and **real
/// teardown** only — not on Windows CTRL_CLOSE (soft Reload may survive).
pub fn clear_sticky_terminal_modes_raw() {
    restore_in_signal_handler();
    write_raw_both(MOUSE_TRACKING_RESET);
}

/// Write terminal restore sequences using raw OS writes (stdout + stderr).
///
/// Async-signal-safe on Unix (`write(2)` only). On Windows uses `WriteFile`
/// on both standard handles. For **real exit / crash / init clear** only.
#[cfg(unix)]
pub fn restore_in_signal_handler() {
    write_raw_both(RESTORE_SEQ);
}

#[cfg(windows)]
pub fn restore_in_signal_handler() {
    write_raw_both(RESTORE_SEQ);
}

#[cfg(not(any(unix, windows)))]
pub fn restore_in_signal_handler() {}

#[cfg(unix)]
fn write_raw_both(bytes: &[u8]) {
    unsafe {
        for fd in [1i32, 2i32] {
            libc::write(fd, bytes.as_ptr() as *const libc::c_void, bytes.len());
        }
    }
}

#[cfg(windows)]
fn write_raw_both(bytes: &[u8]) {
    use windows_sys::Win32::Storage::FileSystem::WriteFile;
    use windows_sys::Win32::System::Console::{
        GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
    };
    unsafe {
        for handle_id in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            let h = GetStdHandle(handle_id);
            if h.is_null() || h == -1isize as *mut std::ffi::c_void {
                continue;
            }
            let mut written: u32 = 0;
            let _ = WriteFile(
                h,
                bytes.as_ptr(),
                bytes.len() as u32,
                &mut written,
                std::ptr::null_mut(),
            );
        }
    }
}

#[cfg(not(any(unix, windows)))]
fn write_raw_both(_bytes: &[u8]) {}

// ---------------------------------------------------------------------------
// Soft host signal (CTRL_CLOSE): no display teardown
// ---------------------------------------------------------------------------
// On CTRL_CLOSE write nothing destructive. Live recovery is the pager's
// drain-coalesced FocusGained/Resize mouse reassert of [`MOUSE_ENABLE_SEQ`].

// ---------------------------------------------------------------------------
// DECRQM helpers (pure; for tests + optional probe)
// ---------------------------------------------------------------------------

/// Build DECRQM request for private mode `mode` (e.g. 1049, 1006).
/// Reply form: `CSI ? Pm ; Ps $ y` where Ps=1 set, Ps=2 reset.
pub fn decrqm_request(mode: u16) -> String {
    format!("\x1b[?{mode}$p")
}

/// Parse a DECRQM reply from raw terminal input.
/// Returns `(mode, ps)` where `ps`: 1=set, 2=reset, 0/3/4=other.
pub fn parse_decrqm_reply(data: &str) -> Option<(u16, u8)> {
    // CSI ? Pm ; Ps $ y
    let re = regex_lite_decrqm(data)?;
    Some(re)
}

/// Minimal parse without regex crate dependency.
fn regex_lite_decrqm(data: &str) -> Option<(u16, u8)> {
    // Find \x1b[? <digits> ; <digit> $ y
    let bytes = data.as_bytes();
    let mut i = 0;
    while i + 6 < bytes.len() {
        if bytes[i] == 0x1b && bytes[i + 1] == b'[' && bytes[i + 2] == b'?' {
            let mut j = i + 3;
            let mut pm: u32 = 0;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                pm = pm * 10 + (bytes[j] - b'0') as u32;
                j += 1;
                if pm > 65535 {
                    break;
                }
            }
            if j < bytes.len() && bytes[j] == b';' {
                j += 1;
                if j < bytes.len() && bytes[j].is_ascii_digit() {
                    let ps = bytes[j] - b'0';
                    j += 1;
                    if j + 1 < bytes.len() && bytes[j] == b'$' && bytes[j + 1] == b'y' {
                        return Some((pm as u16, ps));
                    }
                }
            }
        }
        i += 1;
    }
    None
}

/// True if DECRQM says the mode is reset (Ps=2) or unknown (treat as need reassert).
pub fn decrqm_indicates_reset(ps: u8) -> bool {
    matches!(ps, 0 | 2 | 4) // 0 unknown, 2 reset, 4 permanently reset
}

/// Install a Windows console-control handler.
///
/// On CTRL_CLOSE / Ctrl-C / etc.: **do not** write RESTORE_SEQ (soft Reload
/// may keep the process alive). Do nothing destructive. Real exit paths still
/// call [`restore_in_signal_handler`] / Drop teardown. FocusGained reasserts
/// mouse/focus modes idempotently for any host.
///
/// Safe to call more than once (idempotent).
#[cfg(windows)]
pub fn install_console_ctrl_restore() -> bool {
    use std::sync::atomic::{AtomicBool, Ordering};
    static INSTALLED: AtomicBool = AtomicBool::new(false);
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return true;
    }
    // CTRL_C_EVENT=0, CTRL_BREAK=1, CTRL_CLOSE=2, CTRL_LOGOFF=5, CTRL_SHUTDOWN=6
    unsafe extern "system" fn handler(ctrl_type: u32) -> i32 {
        match ctrl_type {
            0 | 1 | 2 | 5 | 6 => {
                // Intentionally empty: no RESTORE_SEQ (process may survive Reload).
                // FALSE: continue to default terminate if the OS is killing us.
                0
            }
            _ => 0,
        }
    }
    unsafe {
        windows_sys::Win32::System::Console::SetConsoleCtrlHandler(Some(handler), 1) != 0
    }
}

#[cfg(not(windows))]
pub fn install_console_ctrl_restore() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn position_of(needle: &[u8]) -> usize {
        RESTORE_SEQ
            .windows(needle.len())
            .position(|w| w == needle)
            .unwrap_or_else(|| {
                panic!(
                    "RESTORE_SEQ must contain {:?}",
                    std::str::from_utf8(needle).unwrap_or("<binary>")
                )
            })
    }

    #[test]
    fn restore_seq_pops_kitty_before_alt_screen_leave() {
        assert!(position_of(b"\x1b[<u") < position_of(b"\x1b[?1049l"));
    }

    #[test]
    fn restore_seq_includes_all_modes() {
        for needle in [
            b"\x1b[?2026l".as_slice(),
            b"\x1b[?25h".as_slice(),
            b"\x1b[?1000l".as_slice(),
            b"\x1b[?1002l".as_slice(),
            b"\x1b[?1003l".as_slice(),
            b"\x1b[?1015l".as_slice(),
            b"\x1b[?1006l".as_slice(),
            b"\x1b[?2004l".as_slice(),
            b"\x1b[?1004l".as_slice(),
            b"\x1b[<u".as_slice(),
            b"\x1b[?1049l".as_slice(),
        ] {
            position_of(needle);
        }
    }

    #[test]
    fn restore_seq_ends_synchronized_update_first() {
        let end_sync = b"\x1b[?2026l";
        assert_eq!(&RESTORE_SEQ[..end_sync.len()], end_sync);
    }

    #[test]
    fn mouse_tracking_reset_is_subset_of_mouse_paste_and_restore() {
        assert!(
            MOUSE_PASTE_RESET
                .windows(MOUSE_TRACKING_RESET.len())
                .any(|w| w == MOUSE_TRACKING_RESET)
                || MOUSE_PASTE_RESET.starts_with(MOUSE_TRACKING_RESET)
        );
        for mode in [b"?1000l", b"?1002l", b"?1003l", b"?1015l", b"?1006l"] {
            let needle = {
                let mut v = b"\x1b[".to_vec();
                v.extend_from_slice(mode);
                v
            };
            assert!(
                MOUSE_TRACKING_RESET.windows(needle.len()).any(|w| w == needle),
                "MOUSE_TRACKING_RESET must contain {:?}",
                std::str::from_utf8(mode).unwrap()
            );
        }
    }

    #[test]
    fn write_mouse_tracking_reset_bytes() {
        let mut buf = Vec::new();
        let n = write_mouse_tracking_reset(&mut buf).unwrap();
        assert_eq!(n, MOUSE_TRACKING_RESET.len());
        assert_eq!(buf, MOUSE_TRACKING_RESET);
    }

    #[test]
    fn write_mouse_paste_reset_bytes() {
        let mut buf = Vec::new();
        write_mouse_paste_reset(&mut buf).unwrap();
        assert_eq!(buf, MOUSE_PASTE_RESET);
    }

    #[test]
    fn reassert_display_seq_enters_alt_and_mouse() {
        let s = std::str::from_utf8(REASSERT_DISPLAY_SEQ).expect("utf8");
        assert!(s.contains("\x1b[?1049h"));
        assert!(s.contains("\x1b[?1006h"));
        assert!(s.contains("\x1b[?1004h"));
        assert!(!s.contains("\x1b[?1049l"), "reassert must not leave alt");
    }

    #[test]
    fn mouse_enable_seq_is_mouse_only_no_focus_no_alt() {
        // Contract for pager FocusGained repair: re-send mouse DECSET only.
        // Focus reporting (?1004) is enabled once at startup — not here.
        let s = std::str::from_utf8(MOUSE_ENABLE_SEQ).expect("utf8");
        assert!(s.contains("\x1b[?1000h"));
        assert!(s.contains("\x1b[?1002h"));
        assert!(s.contains("\x1b[?1003h"));
        assert!(s.contains("\x1b[?1015h"));
        assert!(s.contains("\x1b[?1006h"));
        assert!(
            !s.contains("1004"),
            "must not embed focus reporting — re-firing ?1004h storms CSI I"
        );
        assert!(!s.contains("1049"), "must not enter/leave alt screen");
    }

    #[test]
    fn decrqm_request_format() {
        assert_eq!(decrqm_request(1049), "\x1b[?1049$p");
        assert_eq!(decrqm_request(1006), "\x1b[?1006$p");
    }

    #[test]
    fn parse_decrqm_reply_set_and_reset() {
        assert_eq!(parse_decrqm_reply("\x1b[?1049;1$y"), Some((1049, 1)));
        assert_eq!(parse_decrqm_reply("\x1b[?1049;2$y"), Some((1049, 2)));
        assert_eq!(parse_decrqm_reply("noise\x1b[?1006;2$ymore"), Some((1006, 2)));
        assert_eq!(parse_decrqm_reply("hello"), None);
    }

    #[test]
    fn decrqm_indicates_reset_ps() {
        assert!(!decrqm_indicates_reset(1));
        assert!(decrqm_indicates_reset(2));
        assert!(decrqm_indicates_reset(0));
    }
}
