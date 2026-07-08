//! Compile progress spinner shown on stderr for slow compilations.
//!
//! The spinner only draws when stderr is a terminal, and only after a grace
//! period so fast compiles produce no output at all. It must be stopped via
//! [`stop`] before anything else is printed so the spinner line is cleared
//! first; error paths that print diagnostics call [`stop`] defensively.

use std::io::{IsTerminal, Write};
use std::sync::Mutex;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const FRAMES: [char; 4] = ['|', '/', '-', '\\'];
const GRACE_PERIOD: Duration = Duration::from_secs(1);
const FRAME_INTERVAL: Duration = Duration::from_millis(120);

struct SpinnerHandle {
    stop_tx: Sender<()>,
    thread: JoinHandle<()>,
}

static ACTIVE: Mutex<Option<SpinnerHandle>> = Mutex::new(None);

/// Start the spinner with the given message. No-op when stderr is not a
/// terminal or a spinner is already running.
pub fn start(message: String) {
    if !std::io::stderr().is_terminal() {
        return;
    }

    let mut active = ACTIVE.lock().expect("spinner lock poisoned");
    if active.is_some() {
        return;
    }

    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let thread = std::thread::spawn(move || {
        let started = Instant::now();
        let mut frame = 0usize;
        let mut drawn = false;
        loop {
            match stop_rx.recv_timeout(FRAME_INTERVAL) {
                Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {}
            }
            if started.elapsed() < GRACE_PERIOD {
                continue;
            }
            let mut err = std::io::stderr().lock();
            let _ = write!(err, "\r{} {}", FRAMES[frame % FRAMES.len()], message);
            let _ = err.flush();
            drawn = true;
            frame += 1;
        }
        if drawn {
            // Overwrite the spinner line with spaces, then return the cursor.
            let width = message.len() + 2;
            let mut err = std::io::stderr().lock();
            let _ = write!(err, "\r{:width$}\r", "");
            let _ = err.flush();
        }
    });

    *active = Some(SpinnerHandle { stop_tx, thread });
}

/// Stop the spinner and clear its line. Idempotent; safe to call from any
/// error path before printing.
pub fn stop() {
    let handle = ACTIVE.lock().expect("spinner lock poisoned").take();
    if let Some(handle) = handle {
        let _ = handle.stop_tx.send(());
        let _ = handle.thread.join();
    }
}
