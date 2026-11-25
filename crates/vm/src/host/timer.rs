//! Optional one-shot wakeup service for script timers and
//! `AbortSignal.timeout`.

use std::sync::Arc;

use crate::binding::{HostFault, LiveRegistration, OperationToken};

/// Arms a platform wakeup without carrying any guest values into its callback.
/// The scheduler calls [`crate::Vm::wake_timer`] on the VM thread, then drains the turn.
pub trait TimerService: Send + Sync {
    fn arm(
        self: Arc<Self>,
        token: OperationToken,
        delay_ms: u64,
    ) -> Result<Box<dyn LiveRegistration>, HostFault>;
}
