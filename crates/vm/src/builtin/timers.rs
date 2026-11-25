//! WindowOrWorkerGlobalScope timers; the platform supplies wakeups only.

use std::sync::Arc;

use super::{
    FUNCTION_PARENT,
    globals::Owner,
    roots::{RootKind, RootSpec},
};
use crate::VmError;
use crate::gc::ActiveRoots;
use crate::host::{Host, TimerService};
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::runtime::Invocation;
use crate::value::Value;
use crate::vm::VmCore;

macro_rules! timer_function {
    ($owner:ident, $name:literal, $length:literal, $call:expr) => {
        pub(super) const $owner: Owner = Owner::new(
            $name,
            &[RootSpec::new(
                $name,
                RootKind::Native(IntrinsicKey::new($name, Slot::Global, "")),
                FUNCTION_PARENT,
            )],
            &[&[IntrinsicDef::call(
                IntrinsicKey::new($name, Slot::Global, ""),
                $length,
                $call,
            )]],
            None,
        );
    };
}

timer_function!(SET_TIMEOUT, "setTimeout", 1, |vm, host, roots, args, _| {
    set(vm, host, roots, args, false)
});
timer_function!(
    SET_INTERVAL,
    "setInterval",
    1,
    |vm, host, roots, args, _| { set(vm, host, roots, args, true) }
);
timer_function!(CLEAR_TIMEOUT, "clearTimeout", 0, clear);
timer_function!(CLEAR_INTERVAL, "clearInterval", 0, clear);

fn set(
    vm: &mut VmCore,
    host: &Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    repeat: bool,
) -> Result<Invocation, VmError> {
    let roots = &roots.with(&args);
    let Some(callback) = args.first().filter(|callback| vm.is_callable(callback)) else {
        return vm.throw_type_error(roots, "Timer callback must be callable");
    };
    let period = match long(vm, host, roots, args.get(1))? {
        Ok(number) => number.max(0) as u32,
        Err(error) => return Ok(Invocation::Thrown(error)),
    };
    if host.services().get::<Arc<dyn TimerService>>().is_none() {
        return Ok(Invocation::Thrown(vm.alloc_dom_exception(
            "NotSupportedError".into(),
            "Timer service is not available".into(),
        )));
    }
    let Some(id) = vm.allocate_timer_id() else {
        return vm.throw_range_error(roots, "Timer id space exhausted");
    };
    let callback = callback.clone();
    let mut args = args;
    args.drain(..args.len().min(2));
    vm.schedule_script_timer(host, id, callback, args, repeat, period)
        .map_err(|fault| VmError::HostFault(fault.0))?;
    Ok(Invocation::Value(Value::number(f64::from(id))))
}

fn clear(
    vm: &mut VmCore,
    host: &Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    _: &Value,
) -> Result<Invocation, VmError> {
    match long(vm, host, roots, args.first())? {
        Ok(id) => vm.clear_script_timer(id),
        Err(error) => return Ok(Invocation::Thrown(error)),
    }
    Ok(Invocation::Value(Value::UNDEFINED))
}

fn long(
    vm: &mut VmCore,
    host: &Host,
    roots: &ActiveRoots<'_>,
    value: Option<&Value>,
) -> Result<Result<i32, Value>, VmError> {
    Ok(vm
        .coerce_number(host, roots, value.unwrap_or(&Value::UNDEFINED))?
        .map(|number| crate::ops::number_to_uint32(number) as i32))
}
