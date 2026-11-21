use std::collections::{BTreeMap, btree_map::Entry};
use std::sync::{Arc, Mutex};

use yield_vm::binding::{
    Bindings, CallCx, CodecError, HostFault, JsResult, JsValue, OperationDriver, OperationToken,
    Registration,
};
use yield_vm::host::Completion;
use yield_vm::{Host, Vm, operation};

#[derive(Default)]
pub struct FakeClock(Mutex<ClockState>);

#[derive(Default)]
struct ClockState {
    now_ms: u64,
    deadlines: BTreeMap<OperationToken, u64>,
    arm_count: usize,
    fail_start: bool,
    fail_resume: bool,
    fail_checkpoint: bool,
}

impl FakeClock {
    pub fn registrations(&self) -> Vec<(OperationToken, u64)> {
        self.0
            .lock()
            .unwrap()
            .deadlines
            .iter()
            .map(|(&token, &deadline)| (token, deadline))
            .collect()
    }

    pub fn arm_count(&self) -> usize {
        self.0.lock().unwrap().arm_count
    }

    pub fn set_time(&self, now: u64) -> Result<(), HostFault> {
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
        while let Some(token) = self.take_due(target).map_err(|error| error.to_string())? {
            vm.complete_operation(token, Completion::Fulfilled(JsValue::undefined()))
                .map_err(|error| error.to_string())?;
            vm.run_until_idle(host).map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    fn arm(
        self: &Arc<Self>,
        token: OperationToken,
        delay: u32,
        resuming: bool,
    ) -> Result<ArmedTimer, HostFault> {
        let mut state = self.0.lock().unwrap();
        if resuming && state.fail_resume {
            return Err(HostFault::new("timer resume failed"));
        }
        if !resuming && state.fail_start {
            return Err(HostFault::new("timer start failed"));
        }
        let deadline = state
            .now_ms
            .checked_add(u64::from(delay))
            .ok_or_else(|| HostFault::new("timer deadline overflow"))?;
        match state.deadlines.entry(token) {
            Entry::Occupied(_) => return Err(HostFault::new("timer already registered")),
            Entry::Vacant(entry) => entry.insert(deadline),
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
        u32::try_from(deadline.saturating_sub(state.now_ms))
            .map_err(|_| HostFault::new("remaining timer delay overflow"))
    }

    fn disarm(&self, token: OperationToken) {
        self.0.lock().unwrap().deadlines.remove(&token);
    }

    fn advance_target(&self, ms: u64) -> Result<u64, HostFault> {
        self.0
            .lock()
            .unwrap()
            .now_ms
            .checked_add(ms)
            .ok_or_else(|| HostFault::new("clock advance overflow"))
    }

    /// Removes one due registration at a time so its callback can affect the rest.
    fn take_due(&self, target: u64) -> Result<Option<OperationToken>, HostFault> {
        let mut state = self.0.lock().unwrap();
        if target < state.now_ms {
            return Err(HostFault::new("cannot move clock backwards"));
        }
        let next = state
            .deadlines
            .iter()
            .map(|(&token, &deadline)| (deadline, token))
            .filter(|&(deadline, _)| deadline <= target)
            .min();
        if let Some((deadline, token)) = next {
            state.now_ms = state.now_ms.max(deadline);
            state.deadlines.remove(&token);
            Ok(Some(token))
        } else {
            state.now_ms = target;
            Ok(None)
        }
    }
}

#[operation(name = "timer", schema = 1, recovery = resume, rejection = "timer unavailable", encode = Timer::encode, decode = Timer::decode)]
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

    fn encode(&self, out: &mut Vec<u8>) -> Result<(), CodecError> {
        out.extend_from_slice(&self.remaining_ms.to_le_bytes());
        Ok(())
    }

    fn decode(input: &[u8], _bindings: &Bindings) -> Result<Self, CodecError> {
        let bytes = input
            .try_into()
            .map_err(|_| CodecError::new("timer state must be four bytes"))?;
        Ok(Self {
            remaining_ms: u32::from_le_bytes(bytes),
        })
    }

    fn clock(bindings: &Bindings) -> Result<&Arc<FakeClock>, HostFault> {
        bindings
            .service::<Arc<FakeClock>>("clock")
            .ok_or_else(|| HostFault::new("missing clock service"))
    }
}

impl OperationDriver for Timer {
    type Live = ArmedTimer;

    fn start(&self, token: OperationToken, bindings: &Bindings) -> Result<ArmedTimer, HostFault> {
        Self::clock(bindings)?.arm(token, self.remaining_ms, false)
    }

    fn resume(&self, token: OperationToken, bindings: &Bindings) -> Result<ArmedTimer, HostFault> {
        Self::clock(bindings)?.arm(token, self.remaining_ms, true)
    }
}

pub struct ArmedTimer {
    clock: Arc<FakeClock>,
    token: OperationToken,
}

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
