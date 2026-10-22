use crate::{Context, Result};
use std::sync::{
    OnceLock,
    atomic::{AtomicBool, Ordering},
};
static CANCELLED: AtomicBool = AtomicBool::new(false);
static HANDLER: OnceLock<Result<()>> = OnceLock::new();
pub(crate) fn install_handler() -> Result<()> {
    HANDLER
        .get_or_init(|| {
            ctrlc::set_handler(|| CANCELLED.store(true, Ordering::SeqCst))
                .context("Install update cancellation handler")
        })
        .clone()
}
pub(crate) fn check() -> Result<()> {
    if CANCELLED.load(Ordering::SeqCst) {
        Err("Update cancelled".into())
    } else {
        Ok(())
    }
}
