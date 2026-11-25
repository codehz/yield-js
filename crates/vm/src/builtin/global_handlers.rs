//! The global object's HTML event handler attributes share the listener list.

use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::heap::EventListener;
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::runtime::Invocation;
use crate::value::{Value, ValueRef};
use crate::vm::VmCore;

pub(crate) const TYPES: [&str; 3] = ["error", "unhandledrejection", "rejectionhandled"];

macro_rules! attributes {
    ($(($index:literal, $name:literal)),+ $(,)?) => {
        &[$(
            IntrinsicDef::call(IntrinsicKey::new("globalThis", Slot::StaticGetter, $name), 0,
                |vm, _, roots, _, receiver| access(vm, roots, receiver, $index, None)),
            IntrinsicDef::call(IntrinsicKey::new("globalThis", Slot::StaticSetter, $name), 1,
                |vm, _, roots, args, receiver| access(vm, roots, receiver, $index,
                    Some(args.into_iter().next().unwrap_or(Value::UNDEFINED)))),
        )+]
    };
}

pub(crate) const INTRINSICS: &[IntrinsicDef] = attributes![
    (0, "onerror"),
    (1, "onunhandledrejection"),
    (2, "onrejectionhandled"),
];

pub(super) fn install(vm: &mut VmCore) {
    let global = vm.builtins.get("globalThis");
    for [getter, setter] in INTRINSICS.as_chunks::<2>().0 {
        let get = vm.alloc_intrinsic(getter.key);
        let set = vm.alloc_intrinsic(setter.key);
        super::event::install_accessor(vm, global, getter.key.member, get, Some(set), true)
            .expect("global event handler accessor");
    }
}

fn access(
    vm: &mut VmCore,
    roots: &ActiveRoots<'_>,
    receiver: &Value,
    index: usize,
    set: Option<Value>,
) -> Result<Invocation, VmError> {
    let global = vm.builtins.get("globalThis");
    if !matches!(receiver.view(), ValueRef::Object(id) if id == global) {
        return vm.throw_type_error(roots, "Incompatible global handler receiver");
    }
    let target = vm.event_target_mut(global)?.expect("global EventTarget");
    let current = target.global_handlers[index];
    let registration = current.map(|id| {
        target
            .listeners
            .binary_search_by_key(&id, |entry| entry.id)
            .expect("global handler has a listener")
    });
    let Some(value) = set else {
        return Ok(Invocation::Value(registration.map_or(Value::NULL, |i| {
            Value::object(target.listeners[i].callback)
        })));
    };
    match (value.view(), registration) {
        (ValueRef::Object(callback), Some(i)) => target.listeners[i].callback = callback,
        (ValueRef::Object(callback), None) => {
            let id = target.next_id;
            target.next_id = id.checked_add(1).ok_or_else(|| {
                VmError::InvalidState("EventTarget listener IDs exhausted".into())
            })?;
            target.listeners.push(EventListener {
                id,
                ty: TYPES[index].into(),
                callback,
                capture: false,
                once: false,
                passive: false,
                signal: None,
                global_handler: true,
            });
            target.global_handlers[index] = Some(id);
        }
        (_, Some(i)) => {
            target.listeners.remove(i);
            target.global_handlers[index] = None;
        }
        (_, None) => {}
    }
    Ok(Invocation::Value(Value::UNDEFINED))
}
