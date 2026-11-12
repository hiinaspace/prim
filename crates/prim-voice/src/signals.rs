//! SIGINT/SIGTERM stop the client; SIGUSR1 asks for a device reopen. The handlers only
//! store to atomics.

use std::sync::atomic::{AtomicBool, Ordering};

pub static TERM: AtomicBool = AtomicBool::new(false);
pub static USR1: AtomicBool = AtomicBool::new(false);

extern "C" fn on_term(_: libc::c_int) {
    TERM.store(true, Ordering::SeqCst);
}

extern "C" fn on_usr1(_: libc::c_int) {
    USR1.store(true, Ordering::SeqCst);
}

pub fn install() {
    // SAFETY: the handlers are async-signal-safe (an atomic store).
    unsafe {
        libc::signal(
            libc::SIGINT,
            on_term as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGTERM,
            on_term as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGUSR1,
            on_usr1 as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
    }
}
