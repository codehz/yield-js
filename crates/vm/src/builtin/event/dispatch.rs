use super::event_receiver;
use crate::VmError;
use crate::gc::ActiveRoots;
use crate::heap::{EventListener, EventState, EventTargetState, Object};
use crate::host::Host;
use crate::runtime::{Arguments, HostSlot, Invocation};
use crate::value::{JsString, ObjectId, PropertyKeyRef, Value, ValueRef};
use crate::vm::Vm;
use std::collections::BTreeSet;

pub(super) fn add_listener(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    receiver: &Value,
) -> Result<Invocation, VmError> {
    change_listener(vm, host, roots, args, receiver, true)
}

pub(super) fn remove_listener(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    receiver: &Value,
) -> Result<Invocation, VmError> {
    change_listener(vm, host, roots, args, receiver, false)
}

fn change_listener(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    receiver: &Value,
    add: bool,
) -> Result<Invocation, VmError> {
    let Some(target) = target_receiver(vm, receiver)? else {
        return vm.throw_type_error(roots, "Incompatible EventTarget receiver");
    };
    let [ty, callback, ..] = args.as_slice() else {
        return vm.throw_type_error(roots, "Event type and callback are required");
    };
    let live = (&args, receiver);
    let roots = &roots.with(&live);
    let ty = match vm.coerce_string(host, roots, ty)? {
        Ok(ty) => ty,
        Err(error) => return Ok(Invocation::Thrown(error)),
    };
    let callback = match callback.view() {
        ValueRef::Object(id) => Some(id),
        ValueRef::Null | ValueRef::Undefined => None,
        _ => return vm.throw_type_error(roots, "Event listener must be an object or null"),
    };

    let undefined = Value::UNDEFINED;
    let options = args.get(2).unwrap_or(&undefined);
    let (mut capture, mut once, mut passive) = (false, false, false);
    if matches!(options.view(), ValueRef::Object(_)) {
        // Read dictionary members in Web IDL order, even for a null callback.
        for (key, flag) in [
            ("capture", &mut capture),
            ("once", &mut once),
            ("passive", &mut passive),
        ] {
            match vm.read_property(host, roots, options, PropertyKeyRef::String(key))? {
                Invocation::Value(value) => *flag = value.truthy(),
                thrown @ Invocation::Thrown(_) => return Ok(thrown),
            }
            if !add {
                break;
            }
        }
    } else {
        capture = options.truthy();
    }

    if let Some(callback) = callback {
        let state = vm
            .event_target_mut(target)?
            .expect("checked EventTarget receiver");
        let existing = state.listeners.iter().position(|listener| {
            listener.ty == ty && listener.callback == callback && listener.capture == capture
        });
        match (add, existing) {
            (true, None) => {
                let id = state.next_id;
                state.next_id = id.checked_add(1).ok_or_else(|| {
                    VmError::InvalidState("EventTarget listener IDs exhausted".into())
                })?;
                state.listeners.push(EventListener {
                    id,
                    ty,
                    callback,
                    capture,
                    once,
                    passive,
                });
            }
            (false, Some(index)) => {
                state.listeners.remove(index);
            }
            _ => {}
        }
    }
    Ok(Invocation::Value(Value::UNDEFINED))
}

pub(super) fn dispatch_untrusted(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    receiver: &Value,
) -> Result<Invocation, VmError> {
    dispatch_call(vm, host, roots, &args, receiver, false)
}

pub(super) fn dispatch_trusted(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    receiver: &Value,
) -> Result<Invocation, VmError> {
    dispatch_call(vm, host, roots, &args, receiver, true)
}

fn dispatch_call(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    args: &[Value],
    receiver: &Value,
    trusted: bool,
) -> Result<Invocation, VmError> {
    let Some(target) = target_receiver(vm, receiver)? else {
        return vm.throw_type_error(roots, "Incompatible EventTarget receiver");
    };
    let Some(event) = event_receiver(vm, args.first().unwrap_or(&Value::UNDEFINED), false)? else {
        return vm.throw_type_error(roots, "dispatchEvent requires an Event");
    };
    vm.dispatch_event(host, roots, target, event, trusted)
}

fn target_receiver(vm: &Vm, value: &Value) -> Result<Option<ObjectId>, VmError> {
    if let ValueRef::Object(id) = value.view()
        && vm.event_target(id)?.is_some()
    {
        return Ok(Some(id));
    }
    Ok(None)
}

struct ListenerPass<'a> {
    ty: &'a JsString,
    phase: u8,
    capture: bool,
}

impl Vm {
    // Read only the VM-managed parent slot; never consult JS properties.
    fn dispatch_path(
        &mut self,
        roots: &ActiveRoots<'_>,
        target: ObjectId,
    ) -> Result<Result<Vec<ObjectId>, Value>, VmError> {
        let mut path = vec![target];
        let mut seen = BTreeSet::new();
        let mut current = target;
        while let Some(instance) = self.host_instances.get(&current) {
            let Some(slot) = self.bindings.class_event_parent(instance.class) else {
                break;
            };
            let Some(HostSlot::Js(parent)) = instance.slots.get(slot) else {
                return Err(VmError::InvalidState(
                    "Invalid EventTarget parent slot".into(),
                ));
            };
            let parent = match parent.view() {
                ValueRef::Undefined | ValueRef::Null => break,
                ValueRef::Object(parent) if self.event_target(parent)?.is_some() => parent,
                _ => {
                    let roots = &roots.with(&path);
                    return Ok(Err(
                        self.type_error(roots, "EventTarget parent must be an EventTarget")?
                    ));
                }
            };
            if parent == target || !seen.insert(parent) {
                return Ok(Err(self.alloc_dom_exception(
                    "HierarchyRequestError".into(),
                    "EventTarget parent chain contains a cycle".into(),
                )));
            }
            path.push(parent);
            current = parent;
        }
        Ok(Ok(path))
    }

    pub(crate) fn event_target(&self, id: ObjectId) -> Result<Option<&EventTargetState>, VmError> {
        Ok(match self.heap.get(id)? {
            Object::EventTarget { target, .. } => Some(target),
            _ => None,
        })
    }

    pub(crate) fn event_target_mut(
        &mut self,
        id: ObjectId,
    ) -> Result<Option<&mut EventTargetState>, VmError> {
        Ok(match self.heap.get_mut(id)? {
            Object::EventTarget { target, .. } => Some(target),
            _ => None,
        })
    }

    /// Dispatch by internal brand, without consulting public methods on the target.
    pub(crate) fn dispatch_event(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        target: ObjectId,
        event: ObjectId,
        trusted: bool,
    ) -> Result<Invocation, VmError> {
        let live = (target, event);
        let roots = &roots.with(&live);
        if self.event_target(target)?.is_none() {
            return self.throw_type_error(roots, "Incompatible EventTarget receiver");
        }
        let Object::Event { event: state, .. } = self.heap.get(event)? else {
            return self.throw_type_error(roots, "dispatchEvent requires an Event");
        };
        if !state.initialized || state.dispatching {
            return Ok(Invocation::Thrown(self.alloc_dom_exception(
                "InvalidStateError".into(),
                "Event is uninitialized or already being dispatched".into(),
            )));
        }

        let path = match self.dispatch_path(roots, target)? {
            Ok(path) => path,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let roots = &roots.with(&path);
        let state = self.dispatch_state(event)?;
        state.dispatching = true;
        state.trusted = trusted;
        state.target = Some(target);
        state.path.clone_from(&path);
        let ty = state.ty.clone();
        let bubbles = state.bubbles;

        let result = (|| {
            for &current in path.iter().rev() {
                let phase = if current == target { 2 } else { 1 };
                let pass = ListenerPass {
                    ty: &ty,
                    phase,
                    capture: true,
                };
                self.invoke_event_listeners(host, roots, event, current, pass)?;
            }
            for &current in path.iter().take(if bubbles { path.len() } else { 1 }) {
                let phase = if current == target { 2 } else { 3 };
                let pass = ListenerPass {
                    ty: &ty,
                    phase,
                    capture: false,
                };
                self.invoke_event_listeners(host, roots, event, current, pass)?;
            }
            Ok::<(), VmError>(())
        })();

        // Restore the dispatch state even when a listener yields a VM/host fault.
        let state = self.dispatch_state(event)?;
        state.phase = 0;
        state.current_target = None;
        state.path.clear();
        state.dispatching = false;
        state.stop = false;
        state.stop_immediate = false;
        state.in_passive = false;
        result?;
        Ok(Invocation::Value(Value::boolean(!state.canceled)))
    }

    fn dispatch_state(&mut self, id: ObjectId) -> Result<&mut EventState, VmError> {
        let Object::Event { event, .. } = self.heap.get_mut(id)? else {
            unreachable!("checked Event receiver");
        };
        Ok(event)
    }

    fn invoke_event_listeners(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        event: ObjectId,
        current: ObjectId,
        pass: ListenerPass<'_>,
    ) -> Result<(), VmError> {
        let state = self.dispatch_state(event)?;
        if state.stop {
            return Ok(());
        }
        state.phase = pass.phase;
        state.current_target = Some(current);
        let ids: Vec<_> = self
            .event_target(current)?
            .expect("branded dispatch path")
            .listeners
            .iter()
            .filter(|listener| &listener.ty == pass.ty && listener.capture == pass.capture)
            .map(|listener| listener.id)
            .collect();

        let receiver = Value::object(current);
        let args = [Value::object(event)];
        for id in ids {
            if self.dispatch_state(event)?.stop_immediate {
                break;
            }
            let listeners = &mut self
                .event_target_mut(current)?
                .expect("branded dispatch path")
                .listeners;
            // IDs are increasing in registration order. A removed and re-added
            // listener has a fresh ID and is not part of this pass.
            let Ok(index) = listeners.binary_search_by_key(&id, |listener| listener.id) else {
                continue;
            };
            let listener = &listeners[index];
            let callback = Value::object(listener.callback);
            let passive = listener.passive;
            if listener.once {
                listeners.remove(index);
            }
            let roots = &roots.with(&callback);
            self.dispatch_state(event)?.in_passive = passive;
            let result = self.invoke_event_listener(host, roots, &callback, &receiver, &args);
            self.dispatch_state(event)?.in_passive = false;
            match result {
                Ok(Invocation::Value(_)) => {}
                Ok(Invocation::Thrown(error)) | Err(VmError::Thrown(error)) => {
                    host.report_error(self.host_value(error));
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    fn invoke_event_listener(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        callback: &Value,
        current_target: &Value,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let args = Arguments::Values(args);
        if self.is_callable(callback) {
            return self.invoke(host, roots, callback, args, current_target);
        }
        match self.read_property(host, roots, callback, PropertyKeyRef::String("handleEvent"))? {
            Invocation::Value(method) => self.invoke(host, roots, &method, args, callback),
            thrown @ Invocation::Thrown(_) => Ok(thrown),
        }
    }
}
