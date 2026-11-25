//! Optional wall-clock service for `Date` and file timestamps, plus the
//! deterministic fallback and system-clock implementations.

use std::borrow::Cow;

/// Supplies wall-clock time and the local time zone to `Date` and file
/// timestamps. Implementations must not reenter the VM.
pub trait TimeService: Send + Sync {
    /// Current Unix epoch milliseconds, within the ECMAScript Date range.
    fn now_ms(&self) -> i64;

    /// The host's local time zone as a valid IANA identifier.
    fn time_zone(&self) -> Cow<'_, str>;
}

/// Fallback time source for VMs without a `TimeService`: a fixed
/// instant at the Unix epoch in `Etc/UTC`, so runs stay deterministic.
pub struct DummyTimeService;

impl TimeService for DummyTimeService {
    fn now_ms(&self) -> i64 {
        0
    }

    fn time_zone(&self) -> Cow<'_, str> {
        Cow::Borrowed("Etc/UTC")
    }
}

/// System-clock implementation with the local zone supplied at construction.
pub struct SystemTimeService {
    zone: String,
}

impl SystemTimeService {
    pub fn new(zone: impl Into<String>) -> Self {
        Self { zone: zone.into() }
    }
}

impl TimeService for SystemTimeService {
    fn now_ms(&self) -> i64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(elapsed) => elapsed.as_millis() as i64,
            Err(error) => -(error.duration().as_nanos().div_ceil(1_000_000) as i64),
        }
    }

    fn time_zone(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.zone)
    }
}
