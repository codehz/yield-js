use super::{
    FUNCTION_PARENT, OBJECT_PARENT, define_builtin_data, install_getter, link_constructor,
};
use crate::builtin::globals::Owner;
use crate::builtin::roots::{RootKind, RootSpec};
use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::heap::{
    ASYNC_DISPOSE_SYMBOL, Attributes, DISPOSE_SYMBOL, DisposableResource, DisposeHint, Object,
    PlainObject, TO_STRING_TAG_SYMBOL,
};
use crate::host::Host;
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::runtime::{Invocation, Settlement};
use crate::value::{ObjectId, PropertyKey, Value, ValueRef};
use crate::vm::VmCore;

#[derive(Clone, Copy)]
enum StackMethod {
    Use,
    Adopt,
    Defer,
    Move,
    Disposed,
    Dispose,
}

macro_rules! stack_family {
    ($module:ident, $name:literal, $root:literal, $prototype:literal, $async:literal,
     $dispose:literal, $symbol:ident) => {
        pub(crate) mod $module {
            use super::*;

            const CONSTRUCTOR: IntrinsicKey = IntrinsicKey::new($name, Slot::Global, "");
            const USE: IntrinsicKey = IntrinsicKey::new($name, Slot::Prototype, "use");
            const ADOPT: IntrinsicKey = IntrinsicKey::new($name, Slot::Prototype, "adopt");
            const DEFER: IntrinsicKey = IntrinsicKey::new($name, Slot::Prototype, "defer");
            const MOVE: IntrinsicKey = IntrinsicKey::new($name, Slot::Prototype, "move");
            const DISPOSED: IntrinsicKey =
                IntrinsicKey::new($name, Slot::PrototypeGetter, "disposed");
            const DISPOSE: IntrinsicKey = IntrinsicKey::new($name, Slot::Prototype, $dispose);

            const ROOTS: &[RootSpec] = &[
                RootSpec::new($root, RootKind::Native(CONSTRUCTOR), FUNCTION_PARENT)
                    .fixed($prototype),
                RootSpec::new($prototype, RootKind::Plain, OBJECT_PARENT),
            ];

            macro_rules! operation {
                ($key:ident, $length:literal, $method:ident) => {
                    IntrinsicDef::call($key, $length, |vm, host, roots, args, receiver| {
                        vm.disposable_stack_method(
                            host,
                            roots,
                            $async,
                            $prototype,
                            StackMethod::$method,
                            receiver,
                            &args,
                        )
                    })
                };
            }

            const INTRINSICS: &[IntrinsicDef] = &[
                IntrinsicDef::constructor(
                    CONSTRUCTOR,
                    0,
                    |vm, _, roots, _, _| {
                        vm.throw_type_error(roots, concat!($name, " requires new"))
                    },
                    |vm, _, _, _, prototype| {
                        let prototype = prototype.unwrap_or_else(|| vm.builtins.get($prototype));
                        Ok(Invocation::Value(Value::object(vm.alloc_disposable_stack(
                            $async,
                            prototype,
                            Vec::new(),
                        ))))
                    },
                ),
                operation!(USE, 1, Use),
                operation!(ADOPT, 2, Adopt),
                operation!(DEFER, 1, Defer),
                operation!(MOVE, 0, Move),
                operation!(DISPOSED, 0, Disposed),
                operation!(DISPOSE, 0, Dispose),
            ];

            pub(crate) const OWNER: Owner = Owner::new($root, ROOTS, &[INTRINSICS], Some(install));

            fn install(vm: &mut VmCore, _: &Value) {
                let prototype = vm.builtins.get($prototype);
                link_constructor(vm, vm.builtins.get($root), prototype);
                for (name, key) in [
                    ("use", USE),
                    ("adopt", ADOPT),
                    ("defer", DEFER),
                    ("move", MOVE),
                ] {
                    let method = Value::object(vm.alloc_intrinsic(key));
                    define_builtin_data(vm, prototype, name, method, Attributes::METHOD);
                }
                let dispose = Value::object(vm.alloc_intrinsic(DISPOSE));
                define_builtin_data(vm, prototype, $dispose, dispose.clone(), Attributes::METHOD);
                define_builtin_data(
                    vm,
                    prototype,
                    PropertyKey::Symbol($symbol),
                    dispose,
                    Attributes::METHOD,
                );
                install_getter(vm, prototype, "disposed".into(), DISPOSED);
                define_builtin_data(
                    vm,
                    prototype,
                    PropertyKey::Symbol(TO_STRING_TAG_SYMBOL),
                    Value::string($name.into()),
                    Attributes::FUNCTION_METADATA,
                );
            }
        }
    };
}

stack_family!(
    sync,
    "DisposableStack",
    "disposableStack",
    "disposableStack.prototype",
    false,
    "dispose",
    DISPOSE_SYMBOL
);
stack_family!(
    asynchronous,
    "AsyncDisposableStack",
    "asyncDisposableStack",
    "asyncDisposableStack.prototype",
    true,
    "disposeAsync",
    ASYNC_DISPOSE_SYMBOL
);

pub(crate) use asynchronous::OWNER as ASYNC_OWNER;
pub(crate) use sync::OWNER as SYNC_OWNER;

impl VmCore {
    fn alloc_disposable_stack(
        &mut self,
        async_: bool,
        prototype: ObjectId,
        records: Vec<DisposableResource>,
    ) -> ObjectId {
        self.heap.alloc(Object::DisposableStack {
            object: PlainObject::new(Some(prototype)),
            async_,
            disposed: false,
            records,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn disposable_stack_method(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        async_: bool,
        intrinsic_prototype: &'static str,
        method: StackMethod,
        receiver: &Value,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let result =
            (async_ && matches!(method, StackMethod::Dispose)).then(|| self.heap.alloc_promise());
        let brand = match receiver.view() {
            ValueRef::Object(id) => match self.heap.get(id)? {
                Object::DisposableStack {
                    async_: brand,
                    disposed,
                    ..
                } if *brand == async_ => Some((id, *disposed)),
                _ => None,
            },
            _ => None,
        };
        let Some((id, disposed)) = brand else {
            let error = self.type_error(roots, "incompatible disposable stack receiver")?;
            return if let Some(result) = result {
                self.settle(result, Settlement::Rejected(error))?;
                Ok(Invocation::Value(Value::object(result)))
            } else {
                Ok(Invocation::Thrown(error))
            };
        };
        match method {
            StackMethod::Disposed => return Ok(Invocation::Value(Value::boolean(disposed))),
            StackMethod::Dispose => {
                if disposed {
                    if let Some(result) = result {
                        self.settle(result, Settlement::Fulfilled(Value::UNDEFINED))?;
                        return Ok(Invocation::Value(Value::object(result)));
                    }
                    return Ok(Invocation::Value(Value::UNDEFINED));
                }
                let records = self.take_disposable_stack_records(id)?;
                self.retarget_stack_registrations(id, None);
                return self.dispose_records(host, roots, records, result);
            }
            _ if disposed => {
                return Ok(Invocation::Thrown(
                    self.reference_error(roots, "stack is disposed")?,
                ));
            }
            StackMethod::Move => {
                let records = self.take_disposable_stack_records(id)?;
                let prototype = self.builtins.get(intrinsic_prototype);
                let target = self.alloc_disposable_stack(async_, prototype, records);
                self.retarget_stack_registrations(id, Some(target));
                return Ok(Invocation::Value(Value::object(target)));
            }
            _ => {}
        }

        let undefined = Value::UNDEFINED;
        let value = args.first().unwrap_or(&undefined);
        let hint = if async_ {
            DisposeHint::Async
        } else {
            DisposeHint::Sync
        };
        let (record, returned) = match method {
            StackMethod::Use => {
                // AddDisposableResource captures the list before invoking any
                // getter. A reentrant move transfers that list; a disposal
                // consumes it. Track its current owner only during lookup.
                self.stack_registrations.push(Some(id));
                let resource = self.create_disposable_resource(host, roots, value, async_);
                let target = self.stack_registrations.pop().expect("active registration");
                match resource? {
                    Ok(Some(record)) => {
                        if let Some(target) = target {
                            self.push_disposable_stack_record(target, record)?;
                        }
                    }
                    Ok(None) => {}
                    Err(error) => return Ok(Invocation::Thrown(error)),
                }
                return Ok(Invocation::Value(value.clone()));
            }
            StackMethod::Adopt => {
                let callback = args.get(1).unwrap_or(&undefined);
                if !self.is_callable(callback) {
                    return self.throw_type_error(roots, "adopt callback must be callable");
                }
                (
                    DisposableResource::Adopt {
                        value: value.clone(),
                        method: callback.clone(),
                        hint,
                    },
                    value.clone(),
                )
            }
            StackMethod::Defer => {
                if !self.is_callable(value) {
                    return self.throw_type_error(roots, "defer callback must be callable");
                }
                (
                    DisposableResource::Defer {
                        method: value.clone(),
                        hint,
                    },
                    Value::UNDEFINED,
                )
            }
            _ => unreachable!(),
        };
        self.push_disposable_stack_record(id, record)?;
        Ok(Invocation::Value(returned))
    }

    fn push_disposable_stack_record(
        &mut self,
        id: ObjectId,
        record: DisposableResource,
    ) -> Result<(), VmError> {
        let Object::DisposableStack {
            disposed: false,
            records,
            ..
        } = self.heap.get_mut(id)?
        else {
            unreachable!("registration list must have a live owner")
        };
        records.push(record);
        Ok(())
    }

    fn retarget_stack_registrations(&mut self, from: ObjectId, to: Option<ObjectId>) {
        for target in &mut self.stack_registrations {
            if *target == Some(from) {
                *target = to;
            }
        }
    }

    fn take_disposable_stack_records(
        &mut self,
        id: ObjectId,
    ) -> Result<Vec<DisposableResource>, VmError> {
        let Object::DisposableStack {
            disposed, records, ..
        } = self.heap.get_mut(id)?
        else {
            unreachable!("receiver was brand-checked")
        };
        *disposed = true;
        Ok(std::mem::take(records))
    }
}
