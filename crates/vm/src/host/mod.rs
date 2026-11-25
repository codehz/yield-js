//! Per-VM host declarations and resources. All platform capabilities are
//! installed through typed service keys before the VM starts.

use std::rc::Rc;

use crate::binding::{Bindings, JsValue, ServiceKey, Services};
use crate::value::{JsString, Value};

mod console;
mod crypto;
mod time;
mod timer;

pub use console::{ConsoleEvent, ConsoleKey, ConsoleSink};
pub use crypto::{
    CRYPTO_SUBTLE_PROTOCOL_VERSION, CryptoKey, CryptoService, SubtleCryptoKey, SubtleCryptoRequest,
    SubtleCryptoService,
};
pub use time::{DummyTimeService, SystemTimeService, TimeKey, TimeService};
pub use timer::{TimerKey, TimerService};

/// How a host operation completed. Values are produced outside a call, so
/// they must be constants; host code cannot mint object handles after the
/// call that created them.
#[derive(Debug, Clone)]
pub enum Completion {
    Fulfilled(JsValue),
    Rejected(JsValue),
}

/// The declarations and resources consumed by one VM at startup or restore.
///
/// Use [`crate::host!`] to register modules and services, or `Host::default()`
/// to run a VM with no external capabilities.
#[derive(Debug)]
pub struct Host {
    bindings: Bindings,
}

impl Host {
    pub(crate) fn linked(bindings: Bindings) -> Self {
        Self { bindings }
    }

    pub(crate) fn bindings(&self) -> &Bindings {
        &self.bindings
    }

    pub(crate) fn services(&self) -> &Services {
        self.bindings.services()
    }
}

impl Default for Host {
    fn default() -> Self {
        Self::linked(Bindings::empty())
    }
}

/// Default host reporting after script error and rejection event handling, or
/// a diagnostic for a module initialization exception.
pub enum Diagnostic<'a> {
    Uncaught {
        error: JsValue,
        diagnostic: &'a JsString,
    },
    StartupException {
        diagnostic: &'a JsString,
    },
    UnhandledRejection {
        promise: JsValue,
        reason: JsValue,
    },
}

pub trait Diagnostics {
    /// Reports an event synchronously. Do not re-enter the VM or retain its
    /// temporary object handles for use in a later binding call.
    fn report(&self, event: Diagnostic<'_>);
}

pub struct DiagnosticsKey;

impl ServiceKey for DiagnosticsKey {
    type Value = Rc<dyn Diagnostics>;
    const ID: &'static str = "yield:host/diagnostics";
    const VERSION: u32 = 1;
    const IN_FINGERPRINT: bool = false;
}

pub trait GcRoots {
    /// Visits only values from this VM, without re-entering it. Used for
    /// collection during execution, not explicit collection or snapshots.
    fn visit(&self, visit: &mut dyn FnMut(&Value));
}

pub struct GcRootsKey;

impl ServiceKey for GcRootsKey {
    type Value = Rc<dyn GcRoots>;
    const ID: &'static str = "yield:host/gc-roots";
    const VERSION: u32 = 1;
    const IN_FINGERPRINT: bool = false;
}

/// Overrides the fresh VM's `navigator.userAgent`, not Fetch request headers.
/// Restore retains the value from the snapshot.
pub struct UserAgentKey;

impl ServiceKey for UserAgentKey {
    type Value = String;
    const ID: &'static str = "yield:host/user-agent";
    const VERSION: u32 = 1;
    const IN_FINGERPRINT: bool = false;
}
