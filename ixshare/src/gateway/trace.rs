use std::sync::atomic::{AtomicBool, Ordering};

/// Global switch to toggle verbose gateway logging without restarting services.
pub static TRACE_GATEWAY_LOG: AtomicBool = AtomicBool::new(false);

#[inline]
pub fn trace_logging_enabled() -> bool {
    TRACE_GATEWAY_LOG.load(Ordering::Relaxed)
}

#[inline]
pub fn set_trace_logging(enable: bool) {
    TRACE_GATEWAY_LOG.store(enable, Ordering::SeqCst);
}
