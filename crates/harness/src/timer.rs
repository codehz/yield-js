use std::collections::{BTreeMap, btree_map::Entry};
use std::sync::{Arc, Mutex};

use yield_vm::binding::{
    Bindings, CallCx, HostFault, JsResult, JsValue, LiveRegistration, OperationDriver,
    OperationToken, Registration,
};
use yield_vm::host::{Completion, TimerService};
use yield_vm::{Decode, Encode, Host, Vm, operation};

const MAX_DATE_MS: u64 = 8_640_000_000_000_000;

#[derive(Debug, Default)]
pub struct FakeClock(Mutex<ClockState>);

pub struct FakeClockKey;

impl yield_vm::ServiceKey for FakeClockKey {
    type Value = Arc<FakeClock>;
    const ID: &'static str = "clock";
    const VERSION: u32 = 1;
}

#[derive(Debug, Default)]
struct ClockState {
    now_ms: u64,
    sub_ms: std::time::Duration,
    deadlines: BTreeMap<OperationToken, (u128, Delivery)>,
    arm_count: usize,
    fail_start: bool,
    fail_resume: bool,
    fail_checkpoint: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Delivery {
    Operation,
    Wake,
}

impl yield_vm::TestClock for FakeClock {
    fn read(&self) -> std::time::Duration {
        let state = self.0.lock().unwrap();
        std::time::Duration::from_millis(state.now_ms) + state.sub_ms
    }
}

impl FakeClock {
    /// Advances only the monotonic time domain without delivering registrations.
    pub fn advance_monotonic(&self, delta: std::time::Duration) {
        let mut state = self.0.lock().unwrap();
        state.sub_ms = state.sub_ms.saturating_add(delta);
    }

    pub fn registrations(&self) -> Vec<(OperationToken, u128)> {
        self.0
            .lock()
            .unwrap()
            .deadlines
            .iter()
            .map(|(&token, &(deadline, _))| (token, deadline))
            .collect()
    }

    pub fn arm_count(&self) -> usize {
        self.0.lock().unwrap().arm_count
    }

    pub fn now_ms(&self) -> i64 {
        self.0.lock().unwrap().now_ms as i64
    }

    pub fn set_time(&self, now: u64) -> Result<(), HostFault> {
        if now > MAX_DATE_MS {
            return Err(HostFault::new("clock time outside ECMAScript Date range"));
        }
        let mut state = self.0.lock().unwrap();
        if !state.deadlines.is_empty() {
            return Err(HostFault::new("cannot set time with active timers"));
        }
        if now < state.now_ms {
            return Err(HostFault::new("cannot move clock backwards"));
        }
        state.now_ms = now;
        Ok(())
    }

    pub fn set_fail_start(&self, fail: bool) {
        self.0.lock().unwrap().fail_start = fail;
    }

    pub fn set_fail_resume(&self, fail: bool) {
        self.0.lock().unwrap().fail_resume = fail;
    }

    pub fn set_fail_checkpoint(&self, fail: bool) {
        self.0.lock().unwrap().fail_checkpoint = fail;
    }

    /// Advances the VM attached to this clock, draining work after each timer.
    pub fn advance(&self, vm: &mut Vm, host: &mut dyn Host, ms: u64) -> Result<(), String> {
        let target = self.advance_target(ms).map_err(|error| error.to_string())?;
        while let Some((token, delivery)) =
            self.take_due(target).map_err(|error| error.to_string())?
        {
            match delivery {
                Delivery::Operation => {
                    vm.complete_operation(token, Completion::Fulfilled(JsValue::undefined()))
                }
                Delivery::Wake => vm.wake_timer(token),
            }
            .map_err(|error| error.to_string())?;
            vm.run_until_idle(host).map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    fn arm(
        self: &Arc<Self>,
        token: OperationToken,
        delay: u64,
        resuming: bool,
        delivery: Delivery,
    ) -> Result<ArmedTimer, HostFault> {
        let mut state = self.0.lock().unwrap();
        if resuming && state.fail_resume {
            return Err(HostFault::new("timer resume failed"));
        }
        if !resuming && state.fail_start {
            return Err(HostFault::new("timer start failed"));
        }
        let deadline = u128::from(state.now_ms) + u128::from(delay);
        match state.deadlines.entry(token) {
            Entry::Occupied(_) => return Err(HostFault::new("timer already registered")),
            Entry::Vacant(entry) => entry.insert((deadline, delivery)),
        };
        state.arm_count += 1;
        Ok(ArmedTimer {
            clock: Arc::clone(self),
            token,
        })
    }

    fn remaining(&self, token: OperationToken) -> Result<u32, HostFault> {
        let state = self.0.lock().unwrap();
        if state.fail_checkpoint {
            return Err(HostFault::new("timer checkpoint failed"));
        }
        let deadline = state
            .deadlines
            .get(&token)
            .ok_or_else(|| HostFault::new("timer is not registered"))?;
        u32::try_from(deadline.0.saturating_sub(u128::from(state.now_ms)))
            .map_err(|_| HostFault::new("remaining timer delay overflow"))
    }

    fn disarm(&self, token: OperationToken) {
        self.0.lock().unwrap().deadlines.remove(&token);
    }

    fn advance_target(&self, ms: u64) -> Result<u64, HostFault> {
        let now = self.0.lock().unwrap().now_ms;
        now.checked_add(ms)
            .filter(|&target| target <= MAX_DATE_MS)
            .ok_or_else(|| HostFault::new("clock advance outside ECMAScript Date range"))
    }

    /// Removes one due registration at a time so its callback can affect the rest.
    fn take_due(&self, target: u64) -> Result<Option<(OperationToken, Delivery)>, HostFault> {
        let mut state = self.0.lock().unwrap();
        if target < state.now_ms {
            return Err(HostFault::new("cannot move clock backwards"));
        }
        let next = state
            .deadlines
            .iter()
            .map(|(&token, &(deadline, delivery))| (deadline, token, delivery))
            .filter(|&(deadline, _, _)| deadline <= u128::from(target))
            .min();
        if let Some((deadline, token, delivery)) = next {
            state.now_ms = state.now_ms.max(deadline as u64);
            state.deadlines.remove(&token);
            Ok(Some((token, delivery)))
        } else {
            state.now_ms = target;
            Ok(None)
        }
    }
}

impl TimerService for FakeClock {
    fn arm(
        self: Arc<Self>,
        token: OperationToken,
        delay_ms: u64,
    ) -> Result<Box<dyn LiveRegistration>, HostFault> {
        Ok(Box::new(FakeClock::arm(
            &self,
            token,
            delay_ms,
            false,
            Delivery::Wake,
        )?))
    }
}

#[operation(name = "timer", recovery = resume, rejection = "timer unavailable")]
#[derive(Encode, Decode)]
pub struct Timer {
    remaining_ms: u32,
}

impl Timer {
    pub fn from_delay(cx: &mut CallCx<'_>, delay: &JsValue) -> JsResult<Self> {
        let ms = cx.number(delay)?;
        if !(0.0..=f64::from(u32::MAX)).contains(&ms) || ms.fract() != 0.0 {
            return Err(cx.type_error("expected a delay in unsigned 32-bit milliseconds"));
        }
        Ok(Self {
            remaining_ms: ms as u32,
        })
    }

    fn clock(bindings: &Bindings) -> Result<&Arc<FakeClock>, HostFault> {
        bindings
            .service::<FakeClockKey>()
            .ok_or_else(|| HostFault::new("missing clock service"))
    }
}

impl OperationDriver for Timer {
    type Live = ArmedTimer;

    fn start(&self, token: OperationToken, bindings: &Bindings) -> Result<ArmedTimer, HostFault> {
        FakeClock::arm(
            Self::clock(bindings)?,
            token,
            u64::from(self.remaining_ms),
            false,
            Delivery::Operation,
        )
    }

    fn resume(&self, token: OperationToken, bindings: &Bindings) -> Result<ArmedTimer, HostFault> {
        FakeClock::arm(
            Self::clock(bindings)?,
            token,
            u64::from(self.remaining_ms),
            true,
            Delivery::Operation,
        )
    }
}

pub struct ArmedTimer {
    clock: Arc<FakeClock>,
    token: OperationToken,
}

impl LiveRegistration for ArmedTimer {}

impl Registration<Timer> for ArmedTimer {
    fn checkpoint(&self) -> Result<Option<Timer>, HostFault> {
        let remaining_ms = self.clock.remaining(self.token)?;
        Ok(Some(Timer { remaining_ms }))
    }
}

impl Drop for ArmedTimer {
    fn drop(&mut self) {
        self.clock.disarm(self.token);
    }
}
