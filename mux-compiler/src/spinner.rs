//! Compile progress spinner shown on stderr for slow compilations.
//!
//! The spinner only draws when stderr is a terminal, and only after a grace
//! period so fast compiles produce no output at all. It must be stopped via
//! [`stop`] before anything else is printed so the spinner line is cleared
//! first; error paths that print diagnostics call [`stop`] defensively.

use std::io::{IsTerminal, Write};
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
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

/// Lock the spinner registry, recovering from a poisoned lock: the guarded
/// state is a plain `Option` that stays valid even if a holder panicked, and
/// spinner cleanup must never itself panic.
fn lock_active() -> std::sync::MutexGuard<'static, Option<SpinnerHandle>> {
    ACTIVE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Whether enough time has passed since compilation started to show the
/// spinner. Fast compiles finish inside the grace period and stay silent.
fn past_grace_period(elapsed: Duration, grace: Duration) -> bool {
    elapsed >= grace
}

/// Render one spinner frame, returning the cursor to column 0 first.
fn render_frame(frame: usize, message: &str) -> String {
    format!("\r{} {}", FRAMES[frame % FRAMES.len()], message)
}

/// Overwrite `&lt;frame&gt; &lt;message&gt;` with spaces, then return the cursor.
fn render_clear(message: &str) -> String {
    format!("\r{:width$}\r", "", width = message.len() + 2)
}

/// Draw frames to `out` until the stop channel signals (or disconnects),
/// honoring the grace period; clears the line if anything was drawn.
fn run_loop(
    out: &mut impl Write,
    stop_rx: &Receiver<()>,
    message: &str,
    grace: Duration,
    interval: Duration,
) {
    let started = Instant::now();
    let mut frame = 0usize;
    let mut drawn = false;
    loop {
        match stop_rx.recv_timeout(interval) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }
        if !past_grace_period(started.elapsed(), grace) {
            continue;
        }
        let _ = out.write_all(render_frame(frame, message).as_bytes());
        let _ = out.flush();
        drawn = true;
        frame += 1;
    }
    if drawn {
        let _ = out.write_all(render_clear(message).as_bytes());
        let _ = out.flush();
    }
}

/// Start the spinner with the given message. No-op when stderr is not a
/// terminal or a spinner is already running.
pub fn start(message: String) {
    if !std::io::stderr().is_terminal() {
        return;
    }
    start_unchecked(message);
}

/// Start the spinner thread without the TTY check (separated for tests).
fn start_unchecked(message: String) {
    // Check-and-set is done with the lock held, but the spawn itself happens
    // with the lock released so that a failed spawn cannot poison the mutex.
    {
        let mut active = lock_active();
        if active.is_some() {
            return;
        }
        // Reserve the slot before dropping the lock.
        *active = None;
    }

    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let spawned = std::thread::Builder::new()
        .name("mux-spinner".to_string())
        .spawn(move || {
            run_loop(
                &mut std::io::stderr(),
                &stop_rx,
                &message,
                GRACE_PERIOD,
                FRAME_INTERVAL,
            );
        });

    // The spinner is purely cosmetic: if the OS cannot spawn another thread,
    // skip it rather than aborting an otherwise valid compile.
    let mut active = lock_active();
    if let Ok(thread) = spawned {
        *active = Some(SpinnerHandle { stop_tx, thread });
    }
}

/// Stop the spinner and clear its line. Idempotent; safe to call from any
/// error path before printing.
pub fn stop() {
    let handle = lock_active().take();
    if let Some(handle) = handle {
        let _ = handle.stop_tx.send(());
        let _ = handle.thread.join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_cycle_through_ascii_and_wrap() {
        assert_eq!(render_frame(0, "compiling x"), "\r| compiling x");
        assert_eq!(render_frame(1, "compiling x"), "\r/ compiling x");
        assert_eq!(render_frame(2, "compiling x"), "\r- compiling x");
        assert_eq!(render_frame(3, "compiling x"), "\r\\ compiling x");
        assert_eq!(render_frame(4, "compiling x"), "\r| compiling x");
    }

    #[test]
    fn clear_overwrites_frame_and_message_with_spaces() {
        let clear = render_clear("hi");
        assert!(clear.starts_with('\r') && clear.ends_with('\r'));
        // "\r" + one space per char of "<frame> <message>" + "\r"
        assert_eq!(clear.len(), 2 + "hi".len() + 2);
        assert!(clear[1..clear.len() - 1].chars().all(|c| c == ' '));
    }

    #[test]
    fn grace_period_gates_drawing() {
        assert!(!past_grace_period(Duration::ZERO, GRACE_PERIOD));
        assert!(!past_grace_period(
            GRACE_PERIOD - Duration::from_millis(1),
            GRACE_PERIOD
        ));
        assert!(past_grace_period(GRACE_PERIOD, GRACE_PERIOD));
        assert!(past_grace_period(GRACE_PERIOD * 2, GRACE_PERIOD));
    }

    #[test]
    fn run_loop_draws_after_grace_and_clears_line() {
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let stopper = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            let _ = stop_tx.send(());
        });

        let mut out: Vec<u8> = Vec::new();
        run_loop(
            &mut out,
            &stop_rx,
            "msg",
            Duration::ZERO,
            Duration::from_millis(1),
        );
        stopper.join().expect("stopper thread");

        let text = String::from_utf8(out).expect("spinner output is ASCII");
        assert!(text.contains("\r| msg"), "missing first frame: {text:?}");
        assert!(
            text.ends_with(&render_clear("msg")),
            "line not cleared at the end: {text:?}"
        );
    }

    #[test]
    fn run_loop_stays_silent_inside_grace_period() {
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        // Already-disconnected channel: the loop exits on the first recv.
        drop(stop_tx);

        let mut out: Vec<u8> = Vec::new();
        run_loop(
            &mut out,
            &stop_rx,
            "msg",
            GRACE_PERIOD,
            Duration::from_millis(1),
        );
        assert!(out.is_empty(), "drew inside grace period: {out:?}");
    }

    #[test]
    fn global_start_and_stop_lifecycle() {
        // Stopping with no spinner running is a no-op.
        stop();
        assert!(lock_active().is_none());

        // start_unchecked registers exactly one spinner; a second start is
        // ignored while one is active. The 1s grace period means the thread
        // draws nothing before the immediate stop below.
        start_unchecked("test spinner".to_string());
        assert!(lock_active().is_some());
        start_unchecked("ignored while active".to_string());
        stop();
        assert!(lock_active().is_none());

        // stop is idempotent.
        stop();

        // The public entry point is a no-op without a TTY and starts a
        // spinner with one; either way stop returns to the idle state.
        start("tty dependent".to_string());
        stop();
        assert!(lock_active().is_none());
    }
}
