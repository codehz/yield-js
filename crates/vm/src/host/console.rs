//! Optional console printer service. The VM owns console state; the sink only
//! receives owned notifications.

use std::sync::Arc;

pub use crate::console::ConsoleEvent;

/// Receives owned-data notifications by reference. Implementations may clone
/// events but must not reenter the VM.
pub trait ConsoleSink: Send + Sync {
    fn write(&self, event: &ConsoleEvent);
}

/// Optional, synchronous console Printer service.
pub struct ConsoleKey;

impl crate::ServiceKey for ConsoleKey {
    type Value = Arc<dyn ConsoleSink>;
    const ID: &'static str = "wintertc.console";
    const VERSION: u32 = 1;
    const IN_FINGERPRINT: bool = false;
}
