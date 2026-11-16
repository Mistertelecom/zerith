//! Ctrl+C / SIGTERM handling without a dependency.
//!
//! The handler only stores an exit code in an atomic (the one thing that is
//! safe in a signal handler). A watcher thread notices it, pulls the turn's
//! `Cancel` token, and on the *second* signal exits the process at once, so a
//! stuck network read can still be interrupted by pressing Ctrl+C twice.
//!
//! Exit codes follow the shell convention 128 + signal: 130 for SIGINT
//! (Ctrl+C), 143 for SIGTERM, SIGHUP and the Windows break/close/shutdown
//! events.
//!
//! Limits, stated: the `Cancel` token is checked between stream chunks and
//! between tool calls, so a blocked read or a running shell command ends at
//! its own timeout, not at the signal. The only unconditional stop is the
//! second signal.

use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use std::time::Duration;

use zerith_providers::Cancel;

static COUNT: AtomicU32 = AtomicU32::new(0);
static CODE: AtomicI32 = AtomicI32::new(0);

/// Records a signal. Called from the handler; atomics only.
fn record(code: i32) {
    // The first signal decides the exit code.
    let _ = CODE.compare_exchange(0, code, Ordering::SeqCst, Ordering::SeqCst);
    COUNT.fetch_add(1, Ordering::SeqCst);
}

/// The exit code of the first signal received, if any.
pub fn received() -> Option<u8> {
    match CODE.load(Ordering::SeqCst) {
        0 => None,
        c => Some(c as u8),
    }
}

#[cfg(unix)]
mod sys {
    extern "C" {
        fn signal(signum: i32, handler: usize) -> usize;
    }
    extern "C" fn on_signal(sig: i32) {
        super::record(128 + sig);
    }
    pub fn install() {
        // SIGHUP, SIGINT, SIGTERM.
        for sig in [1, 2, 15] {
            // SAFETY: `on_signal` is an `extern "C" fn(i32)` that only touches
            // atomics; `signal` is the libc function of that name.
            unsafe {
                signal(sig, on_signal as extern "C" fn(i32) as usize);
            }
        }
    }
}

#[cfg(windows)]
mod sys {
    type Handler = unsafe extern "system" fn(u32) -> i32;
    extern "system" {
        fn SetConsoleCtrlHandler(handler: Option<Handler>, add: i32) -> i32;
    }
    unsafe extern "system" fn on_ctrl(kind: u32) -> i32 {
        // CTRL_C_EVENT = 0; BREAK, CLOSE, LOGOFF, SHUTDOWN are the "terminate"
        // family.
        super::record(if kind == 0 { 130 } else { 143 });
        1
    }
    pub fn install() {
        // SAFETY: `on_ctrl` matches PHANDLER_ROUTINE and only touches atomics.
        unsafe {
            SetConsoleCtrlHandler(Some(on_ctrl), 1);
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod sys {
    pub fn install() {}
}

/// Installs the handlers and starts the watcher. Call once per process.
pub fn install(cancel: Cancel) {
    sys::install();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(50));
        let n = COUNT.load(Ordering::SeqCst);
        if n >= 1 {
            cancel.cancel();
        }
        if n >= 2 {
            std::process::exit(CODE.load(Ordering::SeqCst));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_signal_decides_the_exit_code() {
        assert_eq!(received(), None);
        record(143);
        record(130);
        assert_eq!(received(), Some(143));
        assert_eq!(COUNT.load(Ordering::SeqCst), 2);
    }
}
