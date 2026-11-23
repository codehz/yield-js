use std::collections::HashSet;
use std::sync::Arc;

use super::event::{TARGET_PROTOTYPE, TARGET_ROOT, install_accessor, new_target_prototype};
use super::{FUNCTION_PARENT, OBJECT_PARENT, define_builtin_data, link_constructor};
use crate::VmError;
use crate::builtin::globals::Owner;
use crate::builtin::roots::{RootKind, RootSpec};
use crate::gc::ActiveRoots;
use crate::heap::{
    AbortSignalState, AbortStep, Attributes, EventListener, EventState, EventTargetState, Object,
    PlainObject, TO_STRING_TAG_SYMBOL,
};
use crate::host::{Host, TIMER_SERVICE, TimerService};
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::runtime::Invocation;
use crate::value::{ObjectId, PropertyKey, Value, ValueRef};
use crate::vm::Vm;

const CONTROLLER_ROOT: &str = "abortController";
const CONTROLLER_PROTOTYPE: &str = "abortController.prototype";
const SIGNAL_ROOT: &str = "abortSignal";
const SIGNAL_PROTOTYPE: &str = "abortSignal.prototype";

const CONTROLLER: IntrinsicKey = IntrinsicKey::new("AbortController", Slot::Global, "");
const SIGNAL: IntrinsicKey = IntrinsicKey::new("AbortSignal", Slot::Global, "");
const GET_SIGNAL: IntrinsicKey =
    IntrinsicKey::new(CONTROLLER.owner, Slot::PrototypeGetter, "signal");
const ABORT: IntrinsicKey = IntrinsicKey::new(CONTROLLER.owner, Slot::Prototype, "abort");
const ABORTED: IntrinsicKey = IntrinsicKey::new(SIGNAL.owner, Slot::PrototypeGetter, "aborted");
const REASON: IntrinsicKey = IntrinsicKey::new(SIGNAL.owner, Slot::PrototypeGetter, "reason");
const GET_ONABORT: IntrinsicKey = IntrinsicKey::new(SIGNAL.owner, Slot::PrototypeGetter, "onabort");
const SET_ONABORT: IntrinsicKey = IntrinsicKey::new(SIGNAL.owner, Slot::PrototypeSetter, "onabort");
const THROW_IF_ABORTED: IntrinsicKey =
    IntrinsicKey::new(SIGNAL.owner, Slot::Prototype, "throwIfAborted");
const STATIC_ABORT: IntrinsicKey = IntrinsicKey::new(SIGNAL.owner, Slot::Static, "abort");
const ANY: IntrinsicKey = IntrinsicKey::new(SIGNAL.owner, Slot::Static, "any");
const TIMEOUT: IntrinsicKey = IntrinsicKey::new(SIGNAL.owner, Slot::Static, "timeout");

const CONTROLLER_ROOTS: &[RootSpec] = &[
    RootSpec::new(
        CONTROLLER_ROOT,
        RootKind::Native(CONTROLLER),
        FUNCTION_PARENT,
    )
    .fixed(CONTROLLER_PROTOTYPE)
    .backlink(Some(Attributes::METHOD)),
    RootSpec::new(CONTROLLER_PROTOTYPE, RootKind::Plain, OBJECT_PARENT),
];
const SIGNAL_ROOTS: &[RootSpec] = &[
    RootSpec::new(SIGNAL_ROOT, RootKind::Native(SIGNAL), Some(TARGET_ROOT))
        .fixed(SIGNAL_PROTOTYPE)
        .backlink(Some(Attributes::METHOD)),
    RootSpec::new(SIGNAL_PROTOTYPE, RootKind::Plain, Some(TARGET_PROTOTYPE)),
];
const CONTROLLER_MEMBERS: &[IntrinsicDef] = &[
    IntrinsicDef::constructor_with_target(
        CONTROLLER,
        0,
        |vm, _, roots, _, _| vm.throw_type_error(roots, "AbortController requires new"),
        construct_controller,
    ),
    IntrinsicDef::call(
        GET_SIGNAL,
        0,
        |vm, _, roots, _, receiver| match controller_signal(vm, receiver)? {
            Some(signal) => Ok(Invocation::Value(Value::object(signal))),
            None => vm.throw_type_error(roots, "Incompatible AbortController receiver"),
        },
    ),
    IntrinsicDef::call(ABORT, 0, |vm, host, roots, args, receiver| {
        let Some(signal) = controller_signal(vm, receiver)? else {
            return vm.throw_type_error(roots, "Incompatible AbortController receiver");
        };
        vm.abort_signal(host, roots, signal, first_argument(args))?;
        Ok(Invocation::Value(Value::UNDEFINED))
    }),
];
const SIGNAL_MEMBERS: &[IntrinsicDef] = &[
    IntrinsicDef::constructor_with_target(
        SIGNAL,
        0,
        |vm, _, roots, _, _| vm.throw_type_error(roots, "Illegal AbortSignal constructor"),
        |vm, _, roots, _, _| vm.throw_type_error(roots, "Illegal AbortSignal constructor"),
    ),
    IntrinsicDef::call(STATIC_ABORT, 0, |vm, _, _, args, _| {
        let reason = vm.normalize_abort_reason(first_argument(args));
        Ok(Invocation::Value(Value::object(vm.alloc_abort_signal(
            AbortSignalState {
                reason: Some(reason),
                ..AbortSignalState::default()
            },
        ))))
    }),
    IntrinsicDef::call(ANY, 1, signal_any),
    IntrinsicDef::call(TIMEOUT, 1, signal_timeout),
    IntrinsicDef::call(ABORTED, 0, |vm, _, roots, _, receiver| {
        read_signal(vm, roots, receiver, |state| {
            Invocation::Value(Value::boolean(state.reason.is_some()))
        })
    }),
    IntrinsicDef::call(REASON, 0, |vm, _, roots, _, receiver| {
        read_signal(vm, roots, receiver, |state| {
            Invocation::Value(state.reason.clone().unwrap_or(Value::UNDEFINED))
        })
    }),
    IntrinsicDef::call(GET_ONABORT, 0, get_onabort),
    IntrinsicDef::call(SET_ONABORT, 1, set_onabort),
    IntrinsicDef::call(THROW_IF_ABORTED, 0, |vm, _, roots, _, receiver| {
        read_signal(vm, roots, receiver, |state| match &state.reason {
            Some(reason) => Invocation::Thrown(reason.clone()),
            None => Invocation::Value(Value::UNDEFINED),
        })
    }),
];

pub(crate) const CONTROLLER_OWNER: Owner = Owner::new(
    CONTROLLER_ROOT,
    CONTROLLER_ROOTS,
    &[CONTROLLER_MEMBERS],
    Some(|vm, _| {
        install(
            vm,
            CONTROLLER,
            CONTROLLER_ROOT,
            CONTROLLER_PROTOTYPE,
            CONTROLLER_MEMBERS,
        )
    }),
);
pub(crate) const SIGNAL_OWNER: Owner = Owner::new(
    SIGNAL_ROOT,
    SIGNAL_ROOTS,
    &[SIGNAL_MEMBERS],
    Some(|vm, _| install(vm, SIGNAL, SIGNAL_ROOT, SIGNAL_PROTOTYPE, SIGNAL_MEMBERS)),
);

fn install(
    vm: &mut Vm,
    key: IntrinsicKey,
    root: &'static str,
    prototype: &'static str,
    members: &[IntrinsicDef],
) {
    let constructor = vm.builtins.get(root);
    let prototype = vm.builtins.get(prototype);
    for def in members {
        let key = def.key;
        match key.slot {
            Slot::Static | Slot::Prototype => {
                let target = if key.slot == Slot::Static {
                    constructor
                } else {
                    prototype
                };
                let method = vm.alloc_intrinsic(key);
                define_builtin_data(
                    vm,
                    target,
                    key.member,
                    Value::object(method),
                    Attributes::ORDINARY,
                );
            }
            Slot::PrototypeGetter => {
                let getter = vm.alloc_intrinsic(key);
                let setter = (key == GET_ONABORT).then(|| vm.alloc_intrinsic(SET_ONABORT));
                install_accessor(vm, prototype, key.member, getter, setter, true)
                    .expect("abort prototype accessor");
            }
            _ => {}
        }
    }
    define_builtin_data(
        vm,
        prototype,
        PropertyKey::Symbol(TO_STRING_TAG_SYMBOL),
        Value::string(key.owner.into()),
        Attributes::FUNCTION_METADATA,
    );
    link_constructor(vm, constructor, prototype);
}

fn first_argument(args: Vec<Value>) -> Value {
    args.into_iter().next().unwrap_or(Value::UNDEFINED)
}

fn construct_controller(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    _: Vec<Value>,
    new_target: &Value,
) -> Result<Invocation, VmError> {
    let prototype = match new_target_prototype(vm, host, roots, new_target, CONTROLLER_PROTOTYPE)? {
        Ok(prototype) => prototype,
        Err(error) => return Ok(Invocation::Thrown(error)),
    };
    let signal = vm.alloc_abort_signal(AbortSignalState::default());
    let id = vm.heap.alloc(Object::AbortController {
        object: PlainObject::new(Some(prototype)),
        signal,
    });
    Ok(Invocation::Value(Value::object(id)))
}

fn controller_signal(vm: &Vm, receiver: &Value) -> Result<Option<ObjectId>, VmError> {
    Ok(match receiver.view() {
        ValueRef::Object(id) => match vm.heap.get(id)? {
            Object::AbortController { signal, .. } => Some(*signal),
            _ => None,
        },
        _ => None,
    })
}

fn signal_receiver(vm: &Vm, receiver: &Value) -> Result<Option<ObjectId>, VmError> {
    Ok(match receiver.view() {
        ValueRef::Object(id) if vm.abort_signal_state(id)?.is_some() => Some(id),
        _ => None,
    })
}

fn read_signal(
    vm: &mut Vm,
    roots: &ActiveRoots<'_>,
    receiver: &Value,
    read: impl FnOnce(&AbortSignalState) -> Invocation,
) -> Result<Invocation, VmError> {
    if let ValueRef::Object(id) = receiver.view()
        && let Some(signal) = vm.abort_signal_state(id)?
    {
        return Ok(read(signal));
    }
    vm.throw_type_error(roots, "Incompatible AbortSignal receiver")
}

fn get_onabort(
    vm: &mut Vm,
    _: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    _: Vec<Value>,
    receiver: &Value,
) -> Result<Invocation, VmError> {
    if let ValueRef::Object(id) = receiver.view()
        && let Object::AbortSignal { target, signal, .. } = vm.heap.get(id)?
    {
        let callback = signal
            .onabort
            .and_then(|id| target.listeners.iter().find(|listener| listener.id == id));
        return Ok(Invocation::Value(
            callback.map_or(Value::NULL, |listener| Value::object(listener.callback)),
        ));
    }
    vm.throw_type_error(roots, "Incompatible AbortSignal receiver")
}

fn set_onabort(
    vm: &mut Vm,
    _: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    receiver: &Value,
) -> Result<Invocation, VmError> {
    let Some(id) = signal_receiver(vm, receiver)? else {
        return vm.throw_type_error(roots, "Incompatible AbortSignal receiver");
    };
    let value = first_argument(args);
    let callback = match value.view() {
        // LegacyTreatNonObjectAsNull preserves objects, even if not callable.
        ValueRef::Object(id) => Some(id),
        _ => None,
    };
    let current = vm.abort_signal_state(id)?.expect("checked signal").onabort;
    if let Some(callback) = callback {
        let target = vm.event_target_mut(id)?.expect("signal is an event target");
        if let Some(current) = current {
            let listener = target
                .listeners
                .iter_mut()
                .find(|listener| listener.id == current)
                .expect("event handler has a listener");
            listener.callback = callback;
        } else {
            let next = target.next_id;
            target.next_id = next.checked_add(1).ok_or_else(|| {
                VmError::InvalidState("EventTarget listener IDs exhausted".into())
            })?;
            target.listeners.push(EventListener {
                id: next,
                ty: "abort".into(),
                callback,
                capture: false,
                once: false,
                passive: false,
                global_handler: false,
                signal: None,
            });
            vm.abort_signal_state_mut(id)?
                .expect("checked signal")
                .onabort = Some(next);
        }
    } else if let Some(current) = current {
        vm.remove_listener_id(id, current)?;
        vm.abort_signal_state_mut(id)?
            .expect("checked signal")
            .onabort = None;
    }
    Ok(Invocation::Value(Value::UNDEFINED))
}

fn signal_timeout(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    _: &Value,
) -> Result<Invocation, VmError> {
    // Web IDL limits [EnforceRange] unsigned long long to the JS safe integer range.
    const MAX_DELAY: f64 = 9_007_199_254_740_991.0;
    let delay = match vm.coerce_number(host, roots, &first_argument(args))? {
        Ok(number) => number.trunc(),
        Err(error) => return Ok(Invocation::Thrown(error)),
    };
    if !(0.0..=MAX_DELAY).contains(&delay) {
        return vm.throw_type_error(roots, "AbortSignal.timeout delay is out of range");
    }
    if vm
        .bindings
        .service::<Arc<dyn TimerService>>(TIMER_SERVICE)
        .is_none()
    {
        return Ok(Invocation::Thrown(vm.alloc_dom_exception(
            "NotSupportedError".into(),
            "Timer service is not available".into(),
        )));
    }
    let signal = vm.alloc_abort_signal(AbortSignalState::default());
    vm.schedule_timeout(signal, delay as u64)
        .map_err(|fault| VmError::HostFault(fault.0))?;
    Ok(Invocation::Value(Value::object(signal)))
}

fn signal_any(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    _: &Value,
) -> Result<Invocation, VmError> {
    let iterable = first_argument(args);
    if !matches!(iterable.view(), ValueRef::Object(_)) {
        return vm.throw_type_error(roots, "AbortSignal.any requires an iterable object");
    }
    let pair = match vm.get_iterator(host, roots, &iterable)? {
        Ok(pair) => pair,
        Err(error) => return Ok(Invocation::Thrown(error)),
    };
    let roots = &roots.with(&pair);
    let (iterator, next) = &pair;
    let mut inputs = Vec::new();
    loop {
        let roots = &roots.with(&inputs);
        let value = match vm.iterator_step(host, roots, iterator, next)? {
            Ok(Some(value)) => value,
            Ok(None) => break,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let Some(id) = signal_receiver(vm, &value)? else {
            let error = vm.type_error(roots, "AbortSignal.any requires AbortSignal elements")?;
            // Conversion of an element, unlike a failed iterator step, closes the iterator.
            // The conversion exception takes precedence over a throw from return().
            vm.close_iterator(host, &roots.with(&error), iterator)?;
            return Ok(Invocation::Thrown(error));
        };
        inputs.push(id);
    }

    let mut result = AbortSignalState::default();
    for &id in &inputs {
        let input = vm.abort_signal_state(id)?.expect("checked signal");
        if let Some(reason) = &input.reason {
            result.reason = Some(reason.clone());
            return Ok(Invocation::Value(Value::object(
                vm.alloc_abort_signal(result),
            )));
        }
    }
    result.dependent = true;
    // A composite stores only its ultimate sources. Keep first occurrence order
    // when different inputs lead to the same source.
    let mut seen = HashSet::new();
    for id in inputs {
        let input = vm.abort_signal_state(id)?.expect("checked signal");
        let sources = if input.dependent {
            input.sources.as_slice()
        } else {
            std::slice::from_ref(&id)
        };
        result
            .sources
            .extend(sources.iter().copied().filter(|id| seen.insert(*id)));
    }
    let id = vm.alloc_abort_signal(result);
    let count = vm
        .abort_signal_state(id)?
        .expect("new signal")
        .sources
        .len();
    for index in 0..count {
        let source = vm.abort_signal_state(id)?.expect("new signal").sources[index];
        vm.abort_signal_state_mut(source)?
            .expect("source signal")
            .dependents
            .push(id);
    }
    Ok(Invocation::Value(Value::object(id)))
}

impl Vm {
    pub(crate) fn abort_signal_state(
        &self,
        id: ObjectId,
    ) -> Result<Option<&AbortSignalState>, VmError> {
        Ok(match self.heap.get(id)? {
            Object::AbortSignal { signal, .. } => Some(signal),
            _ => None,
        })
    }

    pub(crate) fn abort_signal_state_mut(
        &mut self,
        id: ObjectId,
    ) -> Result<Option<&mut AbortSignalState>, VmError> {
        Ok(match self.heap.get_mut(id)? {
            Object::AbortSignal { signal, .. } => Some(signal),
            _ => None,
        })
    }

    fn alloc_abort_signal(&mut self, signal: AbortSignalState) -> ObjectId {
        self.heap.alloc(Object::AbortSignal {
            object: PlainObject::new(Some(self.builtins.get(SIGNAL_PROTOTYPE))),
            target: EventTargetState::default(),
            signal,
        })
    }

    fn normalize_abort_reason(&mut self, reason: Value) -> Value {
        if matches!(reason.view(), ValueRef::Undefined) {
            self.alloc_dom_exception("AbortError".into(), "This operation was aborted".into())
        } else {
            reason
        }
    }

    pub(crate) fn abort_signal(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        reason: Value,
    ) -> Result<(), VmError> {
        let signal = self.abort_signal_state(id)?.ok_or_else(|| {
            VmError::InvalidState("internal abort requires an AbortSignal".into())
        })?;
        if signal.reason.is_some() {
            return Ok(());
        }
        let reason = self.normalize_abort_reason(reason);
        let signal = self.abort_signal_state_mut(id)?.expect("checked signal");
        signal.reason = Some(reason.clone());
        let mut affected = Vec::new();
        // The source graph is already flattened; registration order is dispatch order.
        let count = signal.dependents.len();
        for index in 0..count {
            let dependent = self
                .abort_signal_state(id)?
                .expect("checked signal")
                .dependents[index];
            let signal = self
                .abort_signal_state_mut(dependent)?
                .expect("dependent signal");
            if signal.reason.is_none() {
                signal.reason = Some(reason.clone());
                affected.push(dependent);
            }
        }
        // All reasons are visible before any callbacks, and GC from a source callback
        // must keep every pending dependent (including those only weakly linked) alive.
        let live = (id, &affected);
        let roots = &roots.with(&live);
        for signal in std::iter::once(id).chain(affected.iter().copied()) {
            let steps = std::mem::take(
                &mut self
                    .abort_signal_state_mut(signal)?
                    .expect("affected signal")
                    .steps,
            );
            for step in steps {
                match step {
                    AbortStep::Listener { target, id } => {
                        if self.heap.contains(target) {
                            self.remove_listener_id(target, id)?;
                        }
                    }
                    AbortStep::Operation(id) => self.abort_operation(id, reason.clone())?,
                }
            }
            let time_stamp = self.monotonic_millis(host);
            let event = self.alloc_event(
                None,
                EventState {
                    ty: "abort".into(),
                    initialized: true,
                    time_stamp,
                    ..EventState::default()
                },
            )?;
            let ValueRef::Object(event) = event.view() else {
                unreachable!("event allocation returns an object")
            };
            if let Invocation::Thrown(_) = self.dispatch_event(host, roots, signal, event, true)? {
                return Err(VmError::InvalidState(
                    "AbortSignal event failed internal dispatch".into(),
                ));
            }
        }
        Ok(())
    }
}
