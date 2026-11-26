//! Statically typed, optional built-in capabilities owned by a host.

use std::borrow::Cow;
use std::rc::Rc;
use std::sync::Arc;

use crate::binding::{LiveRegistration, OperationToken};
use crate::fetch::{FetchRegistration, FetchService, HopRequest};
use crate::{HostFault, Value};

use super::{
    ConsoleEvent, ConsoleSink, CryptoService, Diagnostic, Diagnostics, GcRoots, PlatformServices,
    SubtleCryptoRequest, SubtleCryptoService, TimeService, TimerService, UserAgent,
};

/// The uninhabited service type for an unconfigured capability.
#[derive(Clone)]
pub enum Absent {}

/// The platform service bundle used by [`crate::host!`].
pub struct Platform<
    C = Absent,
    E = Absent,
    D = Absent,
    F = Absent,
    G = Absent,
    S = Absent,
    T = Absent,
    R = Absent,
> {
    pub console: Option<C>,
    pub crypto: Option<E>,
    pub diagnostics: Option<D>,
    pub fetch: Option<F>,
    pub gc_roots: Option<G>,
    pub subtle_crypto: Option<S>,
    pub time: Option<T>,
    pub timer: Option<R>,
    pub user_agent: Option<UserAgent>,
}

impl<C, E, D, F, G, S, T, R> Default for Platform<C, E, D, F, G, S, T, R> {
    fn default() -> Self {
        Self {
            console: None,
            crypto: None,
            diagnostics: None,
            fetch: None,
            gc_roots: None,
            subtle_crypto: None,
            time: None,
            timer: None,
            user_agent: None,
        }
    }
}

impl<C, E, D, F, G, S, T, R> PlatformServices for Platform<C, E, D, F, G, S, T, R>
where
    C: ConsoleSink + 'static,
    E: CryptoService + 'static,
    D: Diagnostics + 'static,
    F: FetchService + 'static,
    G: GcRoots + 'static,
    S: SubtleCryptoService + Clone + 'static,
    T: TimeService + 'static,
    R: TimerService + 'static,
{
    type Console = C;
    type Crypto = E;
    type Diagnostics = D;
    type Fetch = F;
    type GcRoots = G;
    type SubtleCrypto = S;
    type Time = T;
    type Timer = R;

    fn console(&self) -> Option<&C> {
        self.console.as_ref()
    }
    fn crypto(&self) -> Option<&E> {
        self.crypto.as_ref()
    }
    fn diagnostics(&self) -> Option<&D> {
        self.diagnostics.as_ref()
    }
    fn fetch(&self) -> Option<&F> {
        self.fetch.as_ref()
    }
    fn gc_roots(&self) -> Option<&G> {
        self.gc_roots.as_ref()
    }
    fn subtle_crypto(&self) -> Option<&S> {
        self.subtle_crypto.as_ref()
    }
    fn time(&self) -> Option<&T> {
        self.time.as_ref()
    }
    fn timer(&self) -> Option<&R> {
        self.timer.as_ref()
    }
    fn user_agent(&self) -> Option<&UserAgent> {
        self.user_agent.as_ref()
    }
}

macro_rules! service {
    ($service:ident for $handle:ident { $(fn $method:ident($($arg:ident: $ty:ty),*) $(-> $out:ty)?;)* }) => {
        impl $service for Absent {
            $(fn $method(&self, $(_: $ty),*) $(-> $out)? { match *self {} })*
        }
        impl<T: $service + ?Sized> $service for $handle<T> {
            $(fn $method(&self, $($arg: $ty),*) $(-> $out)? { (**self).$method($($arg),*) })*
        }
    };
}

service!(ConsoleSink for Arc { fn write(event: &ConsoleEvent); });
service!(CryptoService for Arc { fn fill_random(bytes: &mut [u8]) -> Result<(), HostFault>; });
service!(Diagnostics for Rc { fn report(event: Diagnostic<'_>); });
service!(FetchService for Arc {
    fn start(token: OperationToken, request: HopRequest) -> Result<Box<dyn FetchRegistration>, HostFault>;
});
service!(GcRoots for Rc { fn visit(visit: &mut dyn FnMut(&Value)); });
service!(SubtleCryptoService for Arc {
    fn execute(request: SubtleCryptoRequest) -> Result<Vec<u8>, HostFault>;
});
service!(TimeService for Arc {
    fn now_ms() -> i64;
    fn time_zone() -> Cow<'_, str>;
});
service!(TimerService for Arc {
    fn arm(token: OperationToken, delay_ms: u64) -> Result<Box<dyn LiveRegistration>, HostFault>;
});
