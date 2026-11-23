//! VM-owned timer registrations. Platform tokens only authorize a wake;
//! script tasks carry the independently checked timer key.

use std::sync::Arc;
use std::time::Duration;

use crate::binding::{HostFault, OperationToken};
use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::host::Host;
use crate::runtime::{Arguments, Invocation, PendingOperation};
use crate::snapshot::{
    DeliveryTarget, OrdinaryTask, ScriptTimer, TIMEOUT_OPERATION_NAME, TIMEOUT_OPERATION_REJECTION,
    TIMEOUT_OPERATION_SCHEMA, TimerKey, TimerState, TimerTarget,
};
use crate::value::Value;

use super::Vm;

impl Vm {
    /// Delivers a one-shot timer wake without executing guest code.
    pub fn wake_timer(&mut self, token: OperationToken) -> Result<(), VmError> {
        if token.epoch != self.epoch {
            return Ok(());
        }
        let Some(operation) = self.operations.get(&token.id) else {
            return Ok(());
        };
        if operation.live.is_none() {
            return Ok(());
        }
        let DeliveryTarget::Timeout {
            target,
            deadline,
            sequence,
        } = operation.target
        else {
            return Err(VmError::InvalidState(
                "operation token is not a timer".into(),
            ));
        };
        let elapsed = self.clock.elapsed();
        if deadline <= elapsed {
            self.queue_due_timers(elapsed);
        } else {
            drop(self.remove_operation(token.id));
            match self.arm_new_timeout(
                target,
                deadline,
                Self::timeout_remaining(deadline, elapsed),
                sequence,
            ) {
                Ok(id) => {
                    if let TimerTarget::Script(key) = target {
                        self.script_timers
                            .get_mut(&key.id)
                            .expect("timer registration owns a record")
                            .state = TimerState::Armed(id);
                    }
                }
                Err(fault) => {
                    self.discard_timer(target);
                    return Err(VmError::HostFault(fault.0));
                }
            }
        }
        Ok(())
    }

    pub(super) fn queue_due_timers(&mut self, elapsed: Duration) {
        while let Some((&(deadline, _), &id)) = self.timer_deadlines.first_key_value() {
            if deadline > elapsed {
                break;
            }
            let operation = self
                .remove_operation(id)
                .expect("indexed timer has an operation");
            let DeliveryTarget::Timeout { target, .. } = operation.target else {
                unreachable!("timer index references non-timer operation")
            };
            drop(operation);
            let task = match target {
                TimerTarget::Abort(signal) => OrdinaryTask::AbortTimeout(signal),
                TimerTarget::Script(key) => {
                    self.script_timers
                        .get_mut(&key.id)
                        .expect("timer registration owns a record")
                        .state = TimerState::Queued;
                    OrdinaryTask::ScriptTimer(key)
                }
            };
            self.ordinary_tasks.push_back(task);
        }
    }

    pub(super) fn arm_new_timeout(
        &mut self,
        target: TimerTarget,
        deadline: Duration,
        delay_ms: u64,
        sequence: u64,
    ) -> Result<u64, HostFault> {
        let token = self.allocate_operation_token()?;
        let live = Self::arm_timeout(&self.bindings, token, delay_ms)?;
        self.operations.insert(
            token.id,
            PendingOperation {
                name: TIMEOUT_OPERATION_NAME.to_owned(),
                schema: TIMEOUT_OPERATION_SCHEMA,
                recovery: crate::binding::Recovery::Reissue,
                rejection: TIMEOUT_OPERATION_REJECTION,
                state: Vec::new(),
                target: DeliveryTarget::Timeout {
                    target,
                    deadline,
                    sequence,
                },
                signal: None,
                declaration: None,
                live: Some(live),
            },
        );
        self.timer_deadlines.insert((deadline, sequence), token.id);
        Ok(token.id)
    }

    pub(super) fn discard_timer(&mut self, target: TimerTarget) {
        if let TimerTarget::Script(key) = target {
            self.script_timers.remove(&key.id);
        }
    }

    pub(crate) fn allocate_timer_id(&mut self) -> Option<i32> {
        if self.script_timers.len() == i32::MAX as usize {
            return None;
        }
        loop {
            let id = self.next_timer_id;
            self.next_timer_id = if id == i32::MAX { 1 } else { id + 1 };
            if !self.script_timers.contains_key(&id) {
                return Some(id);
            }
        }
    }

    pub(crate) fn schedule_script_timer(
        &mut self,
        id: i32,
        callback: Value,
        arguments: Vec<Value>,
        repeat: bool,
        period: u32,
    ) -> Result<(), HostFault> {
        let (sequence, nesting, state) = self.arm_script_timer(id, period)?;
        self.script_timers.insert(
            id,
            ScriptTimer {
                sequence,
                callback,
                arguments: Arc::new(arguments),
                repeat,
                period,
                nesting,
                state,
            },
        );
        Ok(())
    }

    fn arm_script_timer(
        &mut self,
        id: i32,
        period: u32,
    ) -> Result<(u64, u32, TimerState), HostFault> {
        let delay = if self.timer_nesting > 5 {
            period.max(4)
        } else {
            period
        };
        let deadline = self
            .clock
            .elapsed()
            .saturating_add(Duration::from_millis(u64::from(delay)));
        let sequence = self.next_operation_id;
        let key = TimerKey { id, sequence };
        let operation = self.arm_new_timeout(
            TimerTarget::Script(key),
            deadline,
            u64::from(delay),
            sequence,
        )?;
        Ok((
            sequence,
            self.timer_nesting.saturating_add(1),
            TimerState::Armed(operation),
        ))
    }

    pub(crate) fn clear_script_timer(&mut self, id: i32) {
        let Some(timer) = self.script_timers.remove(&id) else {
            return;
        };
        match timer.state {
            TimerState::Armed(operation) => {
                self.remove_operation(operation);
            }
            TimerState::Queued => {
                let key = TimerKey {
                    id,
                    sequence: timer.sequence,
                };
                self.ordinary_tasks.retain(
                    |task| !matches!(task, OrdinaryTask::ScriptTimer(queued) if *queued == key),
                );
            }
        }
    }

    pub(super) fn run_script_timer(
        &mut self,
        host: &mut dyn Host,
        key: TimerKey,
    ) -> Result<(), VmError> {
        let Some(timer) = self
            .script_timers
            .get(&key.id)
            .filter(|timer| timer.sequence == key.sequence && timer.state == TimerState::Queued)
        else {
            return Ok(());
        };
        let callback = timer.callback.clone();
        let arguments = timer.arguments.clone();
        let receiver = Value::object(self.builtins.get("globalThis"));
        let roots = &ActiveRoots::EMPTY.with(&callback);
        let roots = &roots.with(arguments.as_ref());
        self.timer_nesting = timer.nesting;
        let result = self
            .invoke(
                host,
                roots,
                &callback,
                Arguments::Values(&arguments),
                &receiver,
            )
            .and_then(|outcome| {
                if let Invocation::Thrown(error) = outcome {
                    self.report_exception(host, roots, error)?;
                }
                Ok(())
            });
        let result = result.and_then(|()| self.finish_script_timer(key));
        self.timer_nesting = 0;
        if result.is_err()
            && self
                .script_timers
                .get(&key.id)
                .is_some_and(|timer| timer.sequence == key.sequence)
        {
            self.script_timers.remove(&key.id);
        }
        result
    }

    fn finish_script_timer(&mut self, key: TimerKey) -> Result<(), VmError> {
        let Some(timer) = self
            .script_timers
            .get(&key.id)
            .filter(|timer| timer.sequence == key.sequence)
        else {
            return Ok(());
        };
        if !timer.repeat {
            self.script_timers.remove(&key.id);
            return Ok(());
        }
        let (sequence, nesting, state) = self
            .arm_script_timer(key.id, timer.period)
            .map_err(|fault| VmError::HostFault(fault.0))?;
        let timer = self
            .script_timers
            .get_mut(&key.id)
            .expect("interval exists");
        timer.sequence = sequence;
        timer.nesting = nesting;
        timer.state = state;
        Ok(())
    }
}
