use std::time::{Duration, Instant};

use gtk::glib;

/// Iterates the thread-default main context until `condition` returns true
/// or `timeout` elapses. Always drains pending events before returning.
pub fn spin_main_context_until(timeout: Duration, mut condition: impl FnMut() -> bool) {
    let context = glib::MainContext::ref_thread_default();
    let deadline = Instant::now() + timeout;

    while Instant::now() < deadline {
        while context.pending() {
            context.iteration(false);
        }
        if condition() {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }

    while context.pending() {
        context.iteration(false);
    }
}
