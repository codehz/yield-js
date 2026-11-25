//! Per-VM host declarations and resources, linked before the VM starts.

use std::any::Any;

use crate::binding::{Bindings, BuiltinServices, JsValue};
use crate::value::{JsString, Value};

mod console;
mod crypto;
mod time;
mod timer;

pub use console::{ConsoleEvent, ConsoleSink};
pub use crypto::{CryptoService, SubtleCryptoRequest, SubtleCryptoService};
pub use time::{DummyTimeService, SystemTimeService, TimeService};
pub use timer::TimerService;

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

    /// Resolves an embedder service by its exact type. Cache the returned
    /// borrow for repeated reads; callbacks use their declared service scope.
    pub fn bind_service<T: Any>(&self) -> Option<&T> {
        self.bindings.bind_service::<T>()
    }

    pub(crate) fn builtin_services(&self) -> BuiltinServices<'_> {
        self.bindings.builtin_services()
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

pub trait GcRoots {
    /// Visits only values from this VM, without re-entering it. Used for
    /// collection during execution, not explicit collection or snapshots.
    fn visit(&self, visit: &mut dyn FnMut(&Value));
}

/// Overrides the fresh VM's `navigator.userAgent`, not Fetch request headers.
/// Restore retains the value from the snapshot.
pub struct UserAgent(pub String);
