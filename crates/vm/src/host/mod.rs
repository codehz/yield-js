//! Per-VM host declarations and resources, linked before the VM starts.

use std::any::Any;

use crate::binding::{Bindings, JsValue};
use crate::fetch::FetchService;
use crate::value::{JsString, Value};

mod console;
mod crypto;
mod platform;
mod time;
mod timer;

pub use console::{ConsoleEvent, ConsoleSink};
pub use crypto::{CryptoService, SubtleCryptoCompletion, SubtleCryptoRequest, SubtleCryptoService};
pub use platform::{Absent, Platform};
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

/// Built-in capabilities of a host. Unconfigured slots return `None`.
pub trait PlatformServices: 'static {
    type Console: ConsoleSink;
    type Crypto: CryptoService;
    type Diagnostics: Diagnostics;
    type Fetch: FetchService;
    type GcRoots: GcRoots;
    type SubtleCrypto: SubtleCryptoService;
    type Time: TimeService;
    type Timer: TimerService;

    fn console(&self) -> Option<&Self::Console>;
    fn crypto(&self) -> Option<&Self::Crypto>;
    fn diagnostics(&self) -> Option<&Self::Diagnostics>;
    fn fetch(&self) -> Option<&Self::Fetch>;
    fn gc_roots(&self) -> Option<&Self::GcRoots>;
    fn subtle_crypto(&self) -> Option<&Self::SubtleCrypto>;
    fn time(&self) -> Option<&Self::Time>;
    fn timer(&self) -> Option<&Self::Timer>;
    fn user_agent(&self) -> Option<&UserAgent>;
}

/// The declarations and resources consumed by one VM at startup or restore.
///
/// Use [`crate::host!`] to register modules and services, or `Host::default()`
/// to run a VM with no external capabilities.
pub struct Host<P: PlatformServices = Platform> {
    bindings: Bindings,
    platform: P,
}

impl<P: PlatformServices> Host<P> {
    pub(crate) fn linked(bindings: Bindings, platform: P) -> Self {
        Self { bindings, platform }
    }

    /// Borrows the built-in capabilities of this host.
    pub fn platform(&self) -> &P {
        &self.platform
    }

    /// Reuses the linked application bindings with a different platform.
    #[doc(hidden)]
    pub fn with_platform<Q: PlatformServices>(self, platform: Q) -> Host<Q> {
        Host::linked(self.bindings, platform)
    }

    pub(crate) fn bindings(&self) -> &Bindings {
        &self.bindings
    }

    /// Resolves an embedder service by its exact type. Cache the returned
    /// borrow for repeated reads; callbacks use their declared service scope.
    pub fn bind_service<T: Any>(&self) -> Option<&T> {
        self.bindings.bind_service::<T>()
    }
}

impl<P: PlatformServices> std::fmt::Debug for Host<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Host")
            .field("bindings", &self.bindings)
            .finish_non_exhaustive()
    }
}

impl Default for Host {
    fn default() -> Self {
        Self::linked(Bindings::empty(), Platform::default())
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
