mod dispatch;

use super::{FUNCTION_PARENT, OBJECT_PARENT, define_builtin_data, link_constructor};
use crate::VmError;
use crate::builtin::globals::Owner;
use crate::builtin::roots::{RootKind, RootSpec};
use crate::gc::ActiveRoots;
use crate::heap::{
    Attributes, DescriptorUpdate, EventState, EventTargetState, Object, PlainObject,
    TO_STRING_TAG_SYMBOL,
};
use crate::host::Host;
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::runtime::Invocation;
use crate::value::{JsString, ObjectId, PropertyKey, PropertyKeyRef, Value, ValueRef};
use crate::vm::Vm;

pub(crate) const EVENT_ROOT: &str = "event";
const EVENT_PROTOTYPE: &str = "event.prototype";
pub(crate) const CUSTOM_ROOT: &str = "customEvent";
const CUSTOM_PROTOTYPE: &str = "customEvent.prototype";
pub(crate) const TARGET_ROOT: &str = "eventTarget";
pub(crate) const TARGET_PROTOTYPE: &str = "eventTarget.prototype";
pub(crate) const TRUSTED_DISPATCH_ROOT: &str = "eventTarget.trustedDispatch";

const EVENT: IntrinsicKey = IntrinsicKey::new("Event", Slot::Global, "");
const CUSTOM: IntrinsicKey = IntrinsicKey::new("CustomEvent", Slot::Global, "");
const TARGET: IntrinsicKey = IntrinsicKey::new("EventTarget", Slot::Global, "");
// The getter is installed on instances only. StaticGetter supplies its function name.
const TRUSTED_ROOT: &str = "event.getIsTrusted";
const TRUSTED: IntrinsicKey = IntrinsicKey::new("Event", Slot::StaticGetter, "isTrusted");
const SET_CANCEL_BUBBLE: IntrinsicKey =
    IntrinsicKey::new("Event", Slot::PrototypeSetter, "cancelBubble");
const SET_RETURN_VALUE: IntrinsicKey =
    IntrinsicKey::new("Event", Slot::PrototypeSetter, "returnValue");

pub(crate) const ROOTS: &[RootSpec] = &[
    RootSpec::new(EVENT_ROOT, RootKind::Native(EVENT), FUNCTION_PARENT)
        .fixed(EVENT_PROTOTYPE)
        .backlink(Some(Attributes::METHOD)),
    RootSpec::new(EVENT_PROTOTYPE, RootKind::Plain, OBJECT_PARENT),
    RootSpec::new(CUSTOM_ROOT, RootKind::Native(CUSTOM), Some(EVENT_ROOT))
        .fixed(CUSTOM_PROTOTYPE)
        .backlink(Some(Attributes::METHOD)),
    RootSpec::new(CUSTOM_PROTOTYPE, RootKind::Plain, Some(EVENT_PROTOTYPE)),
    RootSpec::new(TARGET_ROOT, RootKind::Native(TARGET), FUNCTION_PARENT)
        .fixed(TARGET_PROTOTYPE)
        .backlink(Some(Attributes::METHOD)),
    RootSpec::new(TARGET_PROTOTYPE, RootKind::Plain, OBJECT_PARENT),
    RootSpec::new(TRUSTED_ROOT, RootKind::Native(TRUSTED), FUNCTION_PARENT),
    RootSpec::new(
        TRUSTED_DISPATCH_ROOT,
        RootKind::Native(TRUSTED_DISPATCH_KEY),
        FUNCTION_PARENT,
    ),
];

const CONSTRUCTORS: &[IntrinsicDef] = &[
    IntrinsicDef::constructor_with_target(
        EVENT,
        1,
        |vm, _, roots, _, _| vm.throw_type_error(roots, "Event requires new"),
        |vm, host, roots, args, target| construct_event(vm, host, roots, args, target, false),
    ),
    IntrinsicDef::constructor_with_target(
        CUSTOM,
        1,
        |vm, _, roots, _, _| vm.throw_type_error(roots, "CustomEvent requires new"),
        |vm, host, roots, args, target| construct_event(vm, host, roots, args, target, true),
    ),
    IntrinsicDef::constructor_with_target(
        TARGET,
        0,
        |vm, _, roots, _, _| vm.throw_type_error(roots, "EventTarget requires new"),
        construct_target,
    ),
];

macro_rules! event_getter {
    ($name:literal, $read:expr) => {
        IntrinsicDef::call(
            IntrinsicKey::new("Event", Slot::PrototypeGetter, $name),
            0,
            |vm, _, roots, _, receiver| read_event(vm, roots, receiver, false, $read),
        )
    };
}

const EVENT_MEMBERS: &[IntrinsicDef] = &[
    event_getter!("type", |e| Value::string(e.ty.clone())),
    event_getter!("target", |e| e.target.map_or(Value::NULL, Value::object)),
    event_getter!("srcElement", |e| e
        .target
        .map_or(Value::NULL, Value::object)),
    event_getter!("currentTarget", |e| e
        .current_target
        .map_or(Value::NULL, Value::object)),
    IntrinsicDef::call(
        IntrinsicKey::new("Event", Slot::Prototype, "composedPath"),
        0,
        composed_path,
    ),
    event_getter!("eventPhase", |e| Value::number(f64::from(e.phase))),
    IntrinsicDef::call(
        IntrinsicKey::new("Event", Slot::Prototype, "stopPropagation"),
        0,
        |vm, _, roots, _, receiver| update_event(vm, roots, receiver, |e| e.stop = true),
    ),
    event_getter!("cancelBubble", |e| Value::boolean(e.stop)),
    IntrinsicDef::call(SET_CANCEL_BUBBLE, 1, |vm, _, roots, args, receiver| {
        update_event(vm, roots, receiver, |e| {
            if args.first().is_some_and(Value::truthy) {
                e.stop = true;
            }
        })
    }),
    IntrinsicDef::call(
        IntrinsicKey::new("Event", Slot::Prototype, "stopImmediatePropagation"),
        0,
        |vm, _, roots, _, receiver| {
            update_event(vm, roots, receiver, |e| {
                e.stop = true;
                e.stop_immediate = true;
            })
        },
    ),
    event_getter!("bubbles", |e| Value::boolean(e.bubbles)),
    event_getter!("cancelable", |e| Value::boolean(e.cancelable)),
    event_getter!("returnValue", |e| Value::boolean(!e.canceled)),
    IntrinsicDef::call(SET_RETURN_VALUE, 1, |vm, _, roots, args, receiver| {
        update_event(vm, roots, receiver, |e| {
            if !args.first().is_some_and(Value::truthy) {
                cancel(e);
            }
        })
    }),
    IntrinsicDef::call(
        IntrinsicKey::new("Event", Slot::Prototype, "preventDefault"),
        0,
        |vm, _, roots, _, receiver| update_event(vm, roots, receiver, cancel),
    ),
    event_getter!("defaultPrevented", |e| Value::boolean(e.canceled)),
    event_getter!("composed", |e| Value::boolean(e.composed)),
    event_getter!("timeStamp", |e| Value::number(e.time_stamp)),
    IntrinsicDef::call(
        IntrinsicKey::new("Event", Slot::Prototype, "initEvent"),
        1,
        |vm, host, roots, args, receiver| init_event(vm, host, roots, args, receiver, false),
    ),
    IntrinsicDef::call(TRUSTED, 0, |vm, _, roots, _, receiver| {
        read_event(vm, roots, receiver, false, |e| Value::boolean(e.trusted))
    }),
];

const CUSTOM_MEMBERS: &[IntrinsicDef] = &[
    IntrinsicDef::call(
        IntrinsicKey::new("CustomEvent", Slot::PrototypeGetter, "detail"),
        0,
        |vm, _, roots, _, receiver| {
            read_event(vm, roots, receiver, true, |e| {
                e.detail.as_ref().expect("custom event detail").clone()
            })
        },
    ),
    IntrinsicDef::call(
        IntrinsicKey::new("CustomEvent", Slot::Prototype, "initCustomEvent"),
        1,
        |vm, host, roots, args, receiver| init_event(vm, host, roots, args, receiver, true),
    ),
];

pub(crate) const TRUSTED_DISPATCH_KEY: IntrinsicKey =
    IntrinsicKey::new("EventTarget", Slot::Static, "dispatchEvent");

const TARGET_MEMBERS: &[IntrinsicDef] = &[
    IntrinsicDef::call(
        IntrinsicKey::new("EventTarget", Slot::Prototype, "addEventListener"),
        2,
        dispatch::add_listener,
    ),
    IntrinsicDef::call(
        IntrinsicKey::new("EventTarget", Slot::Prototype, "removeEventListener"),
        2,
        dispatch::remove_listener,
    ),
    IntrinsicDef::call(
        IntrinsicKey::new("EventTarget", Slot::Prototype, "dispatchEvent"),
        1,
        dispatch::dispatch_untrusted,
    ),
];

const INTERNAL: &[IntrinsicDef] = &[IntrinsicDef::call(
    TRUSTED_DISPATCH_KEY,
    1,
    dispatch::dispatch_trusted,
)];

pub(crate) const OWNER: Owner = Owner::new(
    EVENT_ROOT,
    ROOTS,
    &[
        CONSTRUCTORS,
        EVENT_MEMBERS,
        CUSTOM_MEMBERS,
        TARGET_MEMBERS,
        INTERNAL,
    ],
    Some(install),
);

fn install(vm: &mut Vm, _: &Value) {
    for (root, proto, tag, members) in [
        (EVENT_ROOT, EVENT_PROTOTYPE, EVENT.owner, EVENT_MEMBERS),
        (CUSTOM_ROOT, CUSTOM_PROTOTYPE, CUSTOM.owner, CUSTOM_MEMBERS),
        (TARGET_ROOT, TARGET_PROTOTYPE, TARGET.owner, TARGET_MEMBERS),
    ] {
        let ctor = vm.builtins.get(root);
        let proto = vm.builtins.get(proto);
        define_builtin_data(
            vm,
            proto,
            PropertyKey::Symbol(TO_STRING_TAG_SYMBOL),
            Value::string(tag.into()),
            Attributes::FUNCTION_METADATA,
        );
        install_members(vm, proto, members);
        if root == EVENT_ROOT {
            for (phase, key) in ["NONE", "CAPTURING_PHASE", "AT_TARGET", "BUBBLING_PHASE"]
                .into_iter()
                .enumerate()
            {
                for target in [ctor, proto] {
                    define_builtin_data(
                        vm,
                        target,
                        key,
                        Value::number(phase as f64),
                        Attributes {
                            enumerable: true,
                            ..Attributes::FIXED
                        },
                    );
                }
            }
        }
        link_constructor(vm, ctor, proto);
    }
}

fn install_members(vm: &mut Vm, prototype: ObjectId, members: &[IntrinsicDef]) {
    let attributes = members
        .iter()
        .filter(|def| matches!(def.key.slot, Slot::PrototypeGetter));
    let operations = members
        .iter()
        .filter(|def| matches!(def.key.slot, Slot::Prototype));
    for def in attributes.chain(operations) {
        let key = def.key;
        match key.slot {
            Slot::Prototype => {
                let method = vm.alloc_intrinsic(key);
                define_builtin_data(
                    vm,
                    prototype,
                    key.member,
                    Value::object(method),
                    Attributes::ORDINARY,
                );
            }
            Slot::PrototypeGetter => {
                let setter = match key.member {
                    "cancelBubble" => Some(SET_CANCEL_BUBBLE),
                    "returnValue" => Some(SET_RETURN_VALUE),
                    _ => None,
                };
                let getter = vm.alloc_intrinsic(key);
                let setter = setter.map(|key| vm.alloc_intrinsic(key));
                install_accessor(vm, prototype, key.member, getter, setter, true)
                    .expect("event prototype accessor");
            }
            _ => unreachable!("only regular prototype members are selected"),
        }
    }
}

pub(super) fn install_accessor(
    vm: &mut Vm,
    target: ObjectId,
    name: &str,
    get: ObjectId,
    set: Option<ObjectId>,
    configurable: bool,
) -> Result<(), VmError> {
    assert!(vm.heap.apply_descriptor(
        target,
        name.into(),
        DescriptorUpdate {
            get: Some(Some(get)),
            set: Some(set),
            enumerable: Some(true),
            configurable: Some(configurable),
            ..Default::default()
        },
    )?);
    Ok(())
}

fn read_event(
    vm: &mut Vm,
    roots: &ActiveRoots<'_>,
    receiver: &Value,
    custom: bool,
    read: impl FnOnce(&EventState) -> Value,
) -> Result<Invocation, VmError> {
    if let Some(id) = event_receiver(vm, receiver, custom)?
        && let Object::Event { event, .. } = vm.heap.get(id)?
    {
        return Ok(Invocation::Value(read(event)));
    }
    incompatible_receiver(vm, roots, custom)
}

fn event_receiver(vm: &Vm, receiver: &Value, custom: bool) -> Result<Option<ObjectId>, VmError> {
    if let ValueRef::Object(id) = receiver.view()
        && let Object::Event { event, .. } = vm.heap.get(id)?
        && (!custom || event.detail.is_some())
    {
        return Ok(Some(id));
    }
    Ok(None)
}

fn incompatible_receiver(
    vm: &mut Vm,
    roots: &ActiveRoots<'_>,
    custom: bool,
) -> Result<Invocation, VmError> {
    vm.throw_type_error(
        roots,
        if custom {
            "Incompatible CustomEvent receiver"
        } else {
            "Incompatible Event receiver"
        },
    )
}

fn update_event(
    vm: &mut Vm,
    roots: &ActiveRoots<'_>,
    receiver: &Value,
    update: impl FnOnce(&mut EventState),
) -> Result<Invocation, VmError> {
    if let ValueRef::Object(id) = receiver.view()
        && let Object::Event { event, .. } = vm.heap.get_mut(id)?
    {
        update(event);
        return Ok(Invocation::Value(Value::UNDEFINED));
    }
    incompatible_receiver(vm, roots, false)
}

fn cancel(event: &mut EventState) {
    if event.cancelable && !event.in_passive {
        event.canceled = true;
    }
}

fn composed_path(
    vm: &mut Vm,
    _: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    _: Vec<Value>,
    receiver: &Value,
) -> Result<Invocation, VmError> {
    if let ValueRef::Object(id) = receiver.view()
        && let Object::Event { event, .. } = vm.heap.get(id)?
    {
        // This VM has no shadow trees; every invocation target is visible.
        let path = event.path.iter().copied().map(Value::object).collect();
        return Ok(Invocation::Value(Value::object(vm.heap.alloc_array(path))));
    }
    incompatible_receiver(vm, roots, false)
}

fn init_event(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    receiver: &Value,
    custom: bool,
) -> Result<Invocation, VmError> {
    let Some(id) = event_receiver(vm, receiver, custom)? else {
        return incompatible_receiver(vm, roots, custom);
    };
    let Some(ty) = args.first() else {
        return vm.throw_type_error(roots, "Event type is required");
    };
    let live = (&args, receiver);
    let roots = &roots.with(&live);
    let ty = match vm.coerce_string(host, roots, ty)? {
        Ok(ty) => ty,
        Err(error) => return Ok(Invocation::Thrown(error)),
    };
    let Object::Event { event, .. } = vm.heap.get_mut(id)? else {
        unreachable!("checked Event receiver");
    };
    if !event.dispatching {
        event.initialized = true;
        event.stop = false;
        event.stop_immediate = false;
        event.canceled = false;
        event.trusted = false;
        event.target = None;
        event.ty = ty;
        event.bubbles = args.get(1).is_some_and(Value::truthy);
        event.cancelable = args.get(2).is_some_and(Value::truthy);
        if custom {
            event.detail = Some(detail_or_null(args.get(3).unwrap_or(&Value::UNDEFINED)));
        }
    }
    Ok(Invocation::Value(Value::UNDEFINED))
}

fn detail_or_null(value: &Value) -> Value {
    if matches!(value.view(), ValueRef::Undefined) {
        Value::NULL
    } else {
        value.clone()
    }
}

pub(super) fn new_target_prototype(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    new_target: &Value,
    fallback: &'static str,
) -> Result<Result<ObjectId, Value>, VmError> {
    Ok(
        match vm.read_property(host, roots, new_target, PropertyKeyRef::String("prototype"))? {
            Invocation::Value(value) => Ok(match value.view() {
                ValueRef::Object(id) => id,
                _ => vm.builtins.get(fallback),
            }),
            Invocation::Thrown(error) => Err(error),
        },
    )
}

fn construct_event(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    new_target: &Value,
    custom: bool,
) -> Result<Invocation, VmError> {
    let Some(ty) = args.first() else {
        return vm.throw_type_error(roots, "Event type is required");
    };
    let live = (&args, new_target);
    let roots = &roots.with(&live);
    let ty = match vm.coerce_string(host, roots, ty)? {
        Ok(ty) => ty,
        Err(error) => return Ok(Invocation::Thrown(error)),
    };
    let undefined = Value::UNDEFINED;
    let options = args.get(1).unwrap_or(&undefined);
    let mut bubbles = false;
    let mut cancelable = false;
    let mut composed = false;
    let mut detail = custom.then_some(Value::NULL);
    match options.view() {
        ValueRef::Undefined | ValueRef::Null => {}
        ValueRef::Object(_) => {
            // Web IDL reads base dictionary members first, in lexicographic order.
            for (key, flag) in [
                ("bubbles", &mut bubbles),
                ("cancelable", &mut cancelable),
                ("composed", &mut composed),
            ] {
                match vm.read_property(host, roots, options, PropertyKeyRef::String(key))? {
                    Invocation::Value(value) => *flag = value.truthy(),
                    thrown @ Invocation::Thrown(_) => return Ok(thrown),
                }
            }
            if custom {
                detail = match vm.read_property(
                    host,
                    roots,
                    options,
                    PropertyKeyRef::String("detail"),
                )? {
                    Invocation::Value(value) => Some(detail_or_null(&value)),
                    thrown @ Invocation::Thrown(_) => return Ok(thrown),
                };
            }
        }
        _ => return vm.throw_type_error(roots, "Event options must be an object"),
    }
    let roots = &roots.with(&detail);
    let fallback = if custom {
        CUSTOM_PROTOTYPE
    } else {
        EVENT_PROTOTYPE
    };
    let prototype = match new_target_prototype(vm, host, roots, new_target, fallback)? {
        Ok(prototype) => prototype,
        Err(error) => return Ok(Invocation::Thrown(error)),
    };
    let event = EventState {
        ty,
        detail,
        bubbles,
        cancelable,
        composed,
        time_stamp: vm.monotonic_millis(host),
        initialized: true,
        ..EventState::default()
    };
    Ok(Invocation::Value(vm.alloc_event(Some(prototype), event)?))
}

impl Vm {
    /// Allocate an initialized CustomEvent for a host binding.
    pub(crate) fn alloc_custom_event(
        &mut self,
        ty: JsString,
        detail: Value,
        bubbles: bool,
        cancelable: bool,
    ) -> Result<Value, VmError> {
        let event = EventState {
            ty,
            detail: Some(detail),
            bubbles,
            cancelable,
            time_stamp: self.clock.elapsed_ms() as f64,
            initialized: true,
            ..EventState::default()
        };
        self.alloc_event(None, event)
    }

    /// Allocate an event without consulting the corresponding global constructor.
    pub(crate) fn alloc_event(
        &mut self,
        prototype: Option<ObjectId>,
        event: EventState,
    ) -> Result<Value, VmError> {
        let prototype = prototype.unwrap_or_else(|| {
            self.builtins.get(if event.detail.is_some() {
                CUSTOM_PROTOTYPE
            } else {
                EVENT_PROTOTYPE
            })
        });
        let id = self.heap.alloc(Object::Event {
            object: PlainObject::new(Some(prototype)),
            event,
        });
        let getter = self.builtins.get(TRUSTED_ROOT);
        install_accessor(self, id, TRUSTED.member, getter, None, false)?;
        Ok(Value::object(id))
    }
}

fn construct_target(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    _: Vec<Value>,
    new_target: &Value,
) -> Result<Invocation, VmError> {
    let prototype = match new_target_prototype(vm, host, roots, new_target, TARGET_PROTOTYPE)? {
        Ok(prototype) => prototype,
        Err(error) => return Ok(Invocation::Thrown(error)),
    };
    Ok(Invocation::Value(Value::object(vm.heap.alloc(
        Object::EventTarget {
            object: PlainObject::new(Some(prototype)),
            target: EventTargetState::default(),
        },
    ))))
}
