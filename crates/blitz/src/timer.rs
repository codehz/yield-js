//! Wake the Blitz document when a VM timer is due. Only the session thread
//! delivers operation tokens to the VM.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, mpsc};
use std::task::Waker;
use std::time::{Duration, Instant};

use yield_vm::HostFault;
use yield_vm::binding::{LiveRegistration, OperationToken};
use yield_vm::host::TimerService;

#[derive(Clone, Default)]
pub(crate) struct Timers(Arc<Mutex<State>>);

#[derive(Default)]
struct State {
    deadlines: BTreeMap<OperationToken, Instant>,
    waker: Option<Waker>,
}

struct Registration {
    timers: Timers,
    token: OperationToken,
    _cancel: Option<mpsc::Sender<()>>,
}

impl LiveRegistration for Registration {}

impl Drop for Registration {
    fn drop(&mut self) {
        self.timers.0.lock().unwrap().deadlines.remove(&self.token);
    }
}

impl TimerService for Timers {
    fn arm(
        &self,
        token: OperationToken,
        delay_ms: u64,
    ) -> Result<Box<dyn LiveRegistration>, HostFault> {
        let delay = Duration::from_millis(delay_ms);
        let deadline = Instant::now()
            .checked_add(delay)
            .ok_or_else(|| HostFault::new("Blitz timer deadline overflow"))?;
        self.0.lock().unwrap().deadlines.insert(token, deadline);
        let mut registration = Registration {
            timers: self.clone(),
            token,
            _cancel: None,
        };
        if delay.is_zero() {
            // The VM thread drains this deadline at the end of its turn.
            return Ok(Box::new(registration));
        }
        let (cancel, canceled) = mpsc::channel();
        let timers = self.clone();
        registration._cancel = Some(cancel);
        std::thread::Builder::new()
            .spawn(move || {
                if canceled.recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    == Err(mpsc::RecvTimeoutError::Timeout)
                {
                    let waker = {
                        let state = timers.0.lock().unwrap();
                        if state.deadlines.contains_key(&token) {
                            state.waker.clone()
                        } else {
                            None
                        }
                    };
                    if let Some(waker) = waker {
                        waker.wake();
                    }
                }
            })
            .map_err(|error| HostFault::new(error.to_string()))?;
        Ok(Box::new(registration))
    }
}

impl Timers {
    pub(crate) fn set_waker(&self, waker: &Waker) {
        let mut state = self.0.lock().unwrap();
        if !state.waker.as_ref().is_some_and(|old| old.will_wake(waker)) {
            state.waker = Some(waker.clone());
        }
    }

    pub(crate) fn due(&self) -> Option<OperationToken> {
        let now = Instant::now();
        self.0
            .lock()
            .unwrap()
            .deadlines
            .iter()
            .find_map(|(&token, &deadline)| (deadline <= now).then_some(token))
    }
}
