//! Optional console printer service. The VM owns console state; the sink only
//! receives owned notifications.

pub use crate::console::ConsoleEvent;

/// Receives owned-data notifications by reference. Implementations may clone
/// events but must not reenter the VM.
pub trait ConsoleSink: Send + Sync {
    fn write(&self, event: &ConsoleEvent);
}
