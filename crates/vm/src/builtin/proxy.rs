//! Proxy internal methods. Traps may mutate the target, so invariants are
//! checked against descriptors obtained after calling the trap.

use crate::builtin::globals::Owner;
use crate::builtin::{ROOT_FUNCTION_PROTOTYPE, ROOT_PROXY, RootKind, RootSpec};
use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::heap::{Attributes, DescriptorUpdate, Object, OwnProperty, PlainObject, Property};
use crate::host::{Host, PlatformServices};
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::ops;
use crate::runtime::{Arguments, Invocation};
use crate::value::{JsString, ObjectId, PropertyKey, PropertyKeyRef, Value, ValueRef};
use crate::vm::VmCore;

const CONSTRUCTOR: IntrinsicKey = IntrinsicKey::new("Proxy", Slot::Global, "");
const REVOCABLE: IntrinsicKey = IntrinsicKey::new("Proxy", Slot::Static, "revocable");

impl<P: crate::host::PlatformServices> BuiltinTable<P> {
    const PROXY_INTRINSICS: &[IntrinsicDef<P>] = &[
        IntrinsicDef::constructor(
            CONSTRUCTOR,
            2,
            |vm, _, roots, _, _| vm.throw_type_error(roots, "Proxy requires new"),
            |vm, _, roots, args, _| vm.construct_proxy(roots, &args),
        )
        .without_new_target_prototype(),
        IntrinsicDef::call(REVOCABLE, 2, |vm, _, roots, args, _| {
            vm.proxy_revocable(roots, &args)
        }),
    ];
}

impl<P: crate::host::PlatformServices> BuiltinTable<P> {
    pub(crate) const OWNER: Owner<P> = Owner::new(
        ROOT_PROXY,
        PROXY_ROOTS,
        &[Self::PROXY_INTRINSICS],
        Some(install),
    );
}

fn install(vm: &mut VmCore, _: &Value) {
    let method = vm.alloc_intrinsic(REVOCABLE);
    super::define_builtin_data(
        vm,
        vm.builtins.get(ROOT_PROXY),
        REVOCABLE.member,
        Value::object(method),
        Attributes::METHOD,
    );
}

pub(crate) const PROXY_ROOTS: &[RootSpec] = &[RootSpec::new(
    ROOT_PROXY,
    RootKind::Native(CONSTRUCTOR),
    Some(ROOT_FUNCTION_PROTOTYPE),
)];

macro_rules! unwrap_script {
    ($expression:expr) => {
        match $expression? {
            Ok(value) => value,
            Err(error) => return Ok(Err(error)),
        }
    };
}

impl VmCore {
    pub(crate) fn proxy_base(
        &mut self,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
    ) -> Result<Result<ObjectId, Value>, VmError> {
        match self.heap.proxy_target(id)? {
            Some(target) => Ok(Ok(target)),
            None => Ok(Err(self.type_error(roots, "Proxy has been revoked")?)),
        }
    }

    pub(crate) fn proxy_prototype<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
    ) -> Result<Result<Option<ObjectId>, Value>, VmError> {
        let Object::Proxy { slots, .. } = *self.heap.get(id)? else {
            return Ok(Ok(self.heap.prototype_of(id)?));
        };
        let Some((target, handler)) = slots else {
            return Ok(Err(self.type_error(roots, "Proxy has been revoked")?));
        };
        let roots = &roots.with(&slots);
        let Some(result) = unwrap_script!(self.proxy_trap(
            host,
            roots,
            handler,
            "getPrototypeOf",
            vec![Value::object(target)]
        )) else {
            return self.proxy_prototype(host, roots, target);
        };
        let prototype = match result.view() {
            ValueRef::Null => None,
            ValueRef::Object(id) => Some(id),
            _ => {
                return Ok(Err(self.type_error(
                    roots,
                    "Proxy getPrototypeOf trap must return an object or null",
                )?));
            }
        };
        let roots = &roots.with(&prototype);
        if !unwrap_script!(self.proxy_is_extensible(host, roots, target))
            && prototype != unwrap_script!(self.proxy_prototype(host, roots, target))
        {
            return Ok(Err(self.type_error(
                roots,
                "Proxy getPrototypeOf trap contradicts a non-extensible target",
            )?));
        }
        Ok(Ok(prototype))
    }

    pub(crate) fn proxy_set_prototype<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        prototype: Option<ObjectId>,
    ) -> Result<Result<bool, Value>, VmError> {
        let Object::Proxy { slots, .. } = *self.heap.get(id)? else {
            return Ok(Ok(self.heap.set_prototype(id, prototype)?));
        };
        let Some((target, handler)) = slots else {
            return Ok(Err(self.type_error(roots, "Proxy has been revoked")?));
        };
        let live = (target, handler, prototype);
        let roots = &roots.with(&live);
        let Some(result) = unwrap_script!(self.proxy_trap(
            host,
            roots,
            handler,
            "setPrototypeOf",
            vec![
                Value::object(target),
                prototype.map_or(Value::NULL, Value::object)
            ]
        )) else {
            return self.proxy_set_prototype(host, roots, target, prototype);
        };
        if !result.truthy() {
            return Ok(Ok(false));
        }
        if !unwrap_script!(self.proxy_is_extensible(host, roots, target))
            && prototype != unwrap_script!(self.proxy_prototype(host, roots, target))
        {
            return Ok(Err(self.type_error(
                roots,
                "Proxy setPrototypeOf trap contradicts a non-extensible target",
            )?));
        }
        Ok(Ok(true))
    }

    pub(crate) fn proxy_prevent_extensions<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
    ) -> Result<Result<bool, Value>, VmError> {
        let Object::Proxy { slots, .. } = *self.heap.get(id)? else {
            if self
                .heap
                .typed_array(id)?
                .is_some_and(|view| view.byte_length.is_none())
            {
                return Ok(Ok(false));
            }
            self.heap.get_mut(id)?.prevent_extensions();
            return Ok(Ok(true));
        };
        let Some((target, handler)) = slots else {
            return Ok(Err(self.type_error(roots, "Proxy has been revoked")?));
        };
        let roots = &roots.with(&slots);
        let Some(result) = unwrap_script!(self.proxy_trap(
            host,
            roots,
            handler,
            "preventExtensions",
            vec![Value::object(target)]
        )) else {
            return self.proxy_prevent_extensions(host, roots, target);
        };
        let accepted = result.truthy();
        if accepted && unwrap_script!(self.proxy_is_extensible(host, roots, target)) {
            return Ok(Err(self.type_error(
                roots,
                "Proxy preventExtensions trap left the target extensible",
            )?));
        }
        Ok(Ok(accepted))
    }

    pub(crate) fn proxy_define_property<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        key: PropertyKey,
        update: DescriptorUpdate,
    ) -> Result<Result<bool, Value>, VmError> {
        let Object::Proxy { slots, .. } = *self.heap.get(id)? else {
            return self.try_apply_property_update(host, roots, id, key, update);
        };
        let Some((target, handler)) = slots else {
            return Ok(Err(self.type_error(roots, "Proxy has been revoked")?));
        };
        let roots = &roots.with(&slots);
        let inputs = (&key, &update);
        let trap_roots = &roots.with(&inputs);
        let Some(method) =
            unwrap_script!(self.proxy_get_method(host, trap_roots, handler, "defineProperty"))
        else {
            return self.proxy_define_property(host, roots, target, key, update);
        };
        let descriptor = self.heap.alloc_plain();
        let fields = [
            ("value", update.value.clone()),
            ("writable", update.writable.map(Value::boolean)),
            (
                "get",
                update
                    .get
                    .map(|get| get.map_or(Value::UNDEFINED, Value::object)),
            ),
            (
                "set",
                update
                    .set
                    .map(|set| set.map_or(Value::UNDEFINED, Value::object)),
            ),
            ("enumerable", update.enumerable.map(Value::boolean)),
            ("configurable", update.configurable.map(Value::boolean)),
        ];
        for (name, value) in fields {
            if let Some(value) = value {
                self.heap
                    .define_data(descriptor, name, value, Attributes::ORDINARY)?;
            }
        }
        let args = [
            Value::object(target),
            key_value(key.borrowed()),
            Value::object(descriptor),
        ];
        match self.invoke(
            host,
            trap_roots,
            &method,
            Arguments::Values(&args),
            &Value::object(handler),
        )? {
            Invocation::Thrown(error) => return Ok(Err(error)),
            Invocation::Value(value) if !value.truthy() => return Ok(Ok(false)),
            Invocation::Value(_) => {}
        }
        let current = unwrap_script!(self.own_descriptor(host, trap_roots, target, key.borrowed()));
        let property = current.as_ref().map(|(property, _)| property);
        let extensible =
            unwrap_script!(self.proxy_is_extensible(host, &trap_roots.with(&property), target));
        let setting_fixed = update.configurable == Some(false);
        let invalid = match &current {
            None => !extensible || setting_fixed,
            Some((property, attrs)) => {
                setting_fixed && attrs.configurable
                    || !attrs.configurable
                        && attrs.writable
                        && matches!(property, Property::Data(_))
                        && update.writable == Some(false)
                    || update
                        .complete(extensible, Some(OwnProperty::Stored(property, *attrs)))
                        .is_none()
            }
        };
        if invalid {
            return Ok(Err(self.type_error(
                roots,
                "Proxy defineProperty trap reported an incompatible descriptor",
            )?));
        }
        Ok(Ok(true))
    }

    pub(crate) fn proxy_is_extensible<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
    ) -> Result<Result<bool, Value>, VmError> {
        let Object::Proxy { slots, .. } = *self.heap.get(id)? else {
            return Ok(Ok(self.heap.get(id)?.is_extensible()));
        };
        let Some((target, handler)) = slots else {
            return Ok(Err(self.type_error(roots, "Proxy has been revoked")?));
        };
        let roots = &roots.with(&slots);
        let Some(result) = unwrap_script!(self.proxy_trap(
            host,
            roots,
            handler,
            "isExtensible",
            vec![Value::object(target)]
        )) else {
            return self.proxy_is_extensible(host, roots, target);
        };
        let result = result.truthy();
        if result != unwrap_script!(self.proxy_is_extensible(host, roots, target)) {
            return Ok(Err(self.type_error(
                roots,
                "Proxy isExtensible trap result does not match the target",
            )?));
        }
        Ok(Ok(result))
    }

    pub(crate) fn proxy_get_method<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        handler: ObjectId,
        name: &str,
    ) -> Result<Result<Option<Value>, Value>, VmError> {
        let handler = Value::object(handler);
        match self.read_property(host, roots, &handler, PropertyKeyRef::String(name))? {
            Invocation::Value(value)
                if matches!(value.view(), ValueRef::Undefined | ValueRef::Null) =>
            {
                Ok(Ok(None))
            }
            Invocation::Value(value) if !self.is_callable(&value) => {
                Ok(Err(self.type_error(roots, "Proxy trap is not callable")?))
            }
            Invocation::Value(value) => Ok(Ok(Some(value))),
            Invocation::Thrown(error) => Ok(Err(error)),
        }
    }

    fn proxy_revocable(
        &mut self,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let proxy = match self.create_proxy(roots, args)? {
            Ok(proxy) => proxy,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let revoke = self.heap.alloc(Object::ProxyRevoker {
            object: PlainObject::callable(
                Some(self.builtins.get(ROOT_FUNCTION_PROTOTYPE)),
                0.0,
                "".into(),
            ),
            proxy: Some(proxy),
        });
        let result = self.heap.alloc_plain();
        self.heap
            .define_data(result, "proxy", Value::object(proxy), Attributes::ORDINARY)?;
        self.heap.define_data(
            result,
            "revoke",
            Value::object(revoke),
            Attributes::ORDINARY,
        )?;
        Ok(Invocation::Value(Value::object(result)))
    }

    pub(crate) fn revoke_proxy(&mut self, id: ObjectId) -> Result<Invocation, VmError> {
        let Object::ProxyRevoker { proxy, .. } = self.heap.get_mut(id)? else {
            return Err(VmError::InvalidState("expected a proxy revoker".into()));
        };
        if let Some(proxy) = proxy.take() {
            let Object::Proxy { slots, .. } = self.heap.get_mut(proxy)? else {
                return Err(VmError::InvalidState("revoker has an invalid proxy".into()));
            };
            *slots = None;
        }
        Ok(Invocation::Value(Value::UNDEFINED))
    }

    fn construct_proxy(
        &mut self,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        Ok(match self.create_proxy(roots, args)? {
            Ok(proxy) => Invocation::Value(Value::object(proxy)),
            Err(error) => Invocation::Thrown(error),
        })
    }

    fn create_proxy(
        &mut self,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Result<ObjectId, Value>, VmError> {
        let Some(ValueRef::Object(target)) = args.first().map(Value::view) else {
            return Ok(Err(
                self.type_error(roots, "Proxy requires an object target")?
            ));
        };
        let Some(ValueRef::Object(handler)) = args.get(1).map(Value::view) else {
            return Ok(Err(
                self.type_error(roots, "Proxy requires an object handler")?
            ));
        };
        let target_value = Value::object(target);
        let callable = self.is_callable(&target_value);
        let constructible = self.is_constructor(&target_value)?;
        Ok(Ok(self.heap.alloc(Object::Proxy {
            slots: Some((target, handler)),
            callable,
            constructible,
            object: PlainObject::new(None),
        })))
    }

    fn proxy_trap<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        handler: ObjectId,
        name: &str,
        args: Vec<Value>,
    ) -> Result<Result<Option<Value>, Value>, VmError> {
        let receiver = Value::object(handler);
        let trap_roots_view = (&receiver, &args);
        let lookup_roots = &roots.with(&trap_roots_view);
        let Some(method) = unwrap_script!(self.proxy_get_method(host, lookup_roots, handler, name))
        else {
            return Ok(Ok(None));
        };
        match self.invoke(host, roots, &method, Arguments::Values(&args), &receiver)? {
            Invocation::Value(value) => Ok(Ok(Some(value))),
            Invocation::Thrown(error) => Ok(Err(error)),
        }
    }

    pub(crate) fn proxy_get<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        target: ObjectId,
        handler: ObjectId,
        key: PropertyKeyRef<'_>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        let root_view = (target, handler, key, receiver);
        let roots = &roots.with(&root_view);
        let result = self.proxy_trap(
            host,
            roots,
            handler,
            "get",
            vec![Value::object(target), key_value(key), receiver.clone()],
        )?;
        let value = match result {
            Ok(None) => {
                return self.read_property_with_receiver(host, roots, target, receiver, key);
            }
            Ok(Some(value)) => value,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let roots = &roots.with(&value);
        match self.own_descriptor(host, roots, target, key)? {
            Err(error) => return Ok(Invocation::Thrown(error)),
            Ok(Some((property, attrs))) if !attrs.configurable => {
                let valid = match property {
                    Property::Data(fixed) if !attrs.writable => ops::same_value(&fixed, &value),
                    Property::Accessor { get: None, .. } => value == Value::UNDEFINED,
                    _ => true,
                };
                if !valid {
                    return self
                        .throw_type_error(roots, "Proxy get trap contradicts a fixed property");
                }
            }
            _ => {}
        }
        Ok(Invocation::Value(value))
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "Explicit active roots at a guest call boundary"
    )]
    pub(crate) fn proxy_set<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        proxy: (ObjectId, ObjectId),
        receiver: &Value,
        key: PropertyKey,
        value: Value,
        strict: bool,
    ) -> Result<Invocation, VmError> {
        let (target, handler) = proxy;
        let proxy_view = (proxy, receiver);
        let roots = &roots.with(&proxy_view);
        let inputs = (&key, &value);
        let trap_roots = &roots.with(&inputs);
        match self.proxy_trap(
            host,
            trap_roots,
            handler,
            "set",
            vec![
                Value::object(target),
                key_value(key.borrowed()),
                value.clone(),
                receiver.clone(),
            ],
        )? {
            Ok(None) => {
                return self.set_property(host, roots, target, receiver, key, value, strict);
            }
            Err(error) => return Ok(Invocation::Thrown(error)),
            Ok(Some(result)) if !result.truthy() => {
                return if strict {
                    self.throw_type_error(roots, "Proxy set trap rejected the write")
                } else {
                    Ok(Invocation::Value(Value::boolean(false)))
                };
            }
            _ => {}
        }
        match self.own_descriptor(host, trap_roots, target, key.borrowed())? {
            Err(error) => return Ok(Invocation::Thrown(error)),
            Ok(Some((property, attrs))) if !attrs.configurable => {
                let valid = match property {
                    Property::Data(fixed) if !attrs.writable => ops::same_value(&fixed, &value),
                    Property::Accessor { set: None, .. } => false,
                    _ => true,
                };
                if !valid {
                    return self
                        .throw_type_error(roots, "Proxy set trap contradicts a fixed property");
                }
            }
            _ => {}
        }
        Ok(Invocation::Value(Value::boolean(true)))
    }

    pub(crate) fn proxy_presence<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        target: ObjectId,
        handler: ObjectId,
        key: PropertyKeyRef<'_>,
        delete: bool,
    ) -> Result<Invocation, VmError> {
        let root_view = (target, handler, key);
        let roots = &roots.with(&root_view);
        let name = if delete { "deleteProperty" } else { "has" };
        let flag = match self.proxy_trap(
            host,
            roots,
            handler,
            name,
            vec![Value::object(target), key_value(key)],
        )? {
            Err(error) => return Ok(Invocation::Thrown(error)),
            Ok(Some(value)) => value.truthy(),
            Ok(None) if delete => {
                return self.delete_property(host, roots, &Value::object(target), &key_value(key));
            }
            Ok(None) => return self.has_property_key(host, roots, target, key),
        };
        if flag == delete {
            let attrs = match self.own_descriptor(host, roots, target, key)? {
                Ok(Some((_, attrs))) => Some(attrs),
                Ok(None) => None,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
            if let Some(attrs) = attrs {
                let protected = if !attrs.configurable {
                    true
                } else {
                    match self.proxy_is_extensible(host, roots, target)? {
                        Ok(extensible) => !extensible,
                        Err(error) => return Ok(Invocation::Thrown(error)),
                    }
                };
                if protected {
                    return self.throw_type_error(
                        roots,
                        "Proxy trap contradicts a protected own property",
                    );
                }
            }
        }
        Ok(Invocation::Value(Value::boolean(flag)))
    }

    pub(crate) fn own_keys<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        include_metadata: bool,
    ) -> Result<Result<Vec<PropertyKey>, Value>, VmError> {
        let roots = &roots.with(&id);
        let Object::Proxy { slots, .. } = *self.heap.get(id)? else {
            if let Some(keys) = self.heap.typed_own_keys(id, include_metadata)? {
                return Ok(Ok(keys));
            }
            return Ok(Ok(self.heap.get(id)?.own_keys(include_metadata).collect()));
        };
        let Some((target, handler)) = slots else {
            return Ok(Err(self.type_error(roots, "Proxy has been revoked")?));
        };
        let trap_slots = (target, handler);
        let roots = &roots.with(&trap_slots);
        let Some(result) = unwrap_script!(self.proxy_trap(
            host,
            roots,
            handler,
            "ownKeys",
            vec![Value::object(target)]
        )) else {
            return self.own_keys(host, roots, target, include_metadata);
        };
        if !matches!(result.view(), ValueRef::Object(_)) {
            return Ok(Err(
                self.type_error(roots, "Proxy ownKeys trap must return an object")?
            ));
        }
        let roots = &roots.with(&result);
        let length =
            match self.read_property(host, roots, &result, PropertyKeyRef::String("length"))? {
                Invocation::Value(value) => value,
                Invocation::Thrown(error) => return Ok(Err(error)),
            };
        let mut keys = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        let iterable = if length == Value::UNDEFINED {
            match self.read_property(
                host,
                roots,
                &result,
                PropertyKeyRef::Symbol(crate::heap::ITERATOR_SYMBOL),
            )? {
                Invocation::Value(value)
                    if matches!(value.view(), ValueRef::Undefined | ValueRef::Null) =>
                {
                    None
                }
                Invocation::Value(method) => Some(unwrap_script!(
                    self.get_iterator_from_method(host, roots, &result, &method)
                )),
                Invocation::Thrown(error) => return Ok(Err(error)),
            }
        } else {
            None
        };
        let roots = &roots.with(&iterable);
        let length = unwrap_script!(self.coerce_number(host, roots, &length))
            .clamp(0.0, 9_007_199_254_740_991.0) as u64;
        let mut index = 0;
        loop {
            let roots = &roots.with(&keys);
            let entry = if let Some((iterator, next)) = &iterable {
                let Some(value) = unwrap_script!(self.iterator_step(host, roots, iterator, next))
                else {
                    break;
                };
                Invocation::Value(value)
            } else {
                if index >= length {
                    break;
                }
                let entry = self.read_property(
                    host,
                    roots,
                    &result,
                    PropertyKeyRef::String(&index.to_string()),
                )?;
                index += 1;
                entry
            };
            let entry = match entry {
                Invocation::Value(value) => value,
                Invocation::Thrown(error) => return Ok(Err(error)),
            };
            let key = if matches!(entry.view(), ValueRef::String(_)) {
                PropertyKey::String(entry.into_string().expect("ownKeys entry is a string"))
            } else if let ValueRef::Symbol(key) = entry.view() {
                PropertyKey::Symbol(key)
            } else {
                let error =
                    self.type_error(roots, "Proxy ownKeys entries must be strings or symbols")?;
                if let Some((iterator, _)) = &iterable {
                    self.close_iterator(host, &roots.with(&error), iterator)?;
                }
                return Ok(Err(error));
            };
            if !seen.insert(key.clone()) {
                if let Some((iterator, _)) = &iterable {
                    self.close_iterator(host, roots, iterator)?;
                }
                return Ok(Err(self.type_error(
                    roots,
                    "Proxy ownKeys trap returned a duplicate key",
                )?));
            }
            keys.push(key);
        }
        let roots = &roots.with(&keys);
        let extensible = unwrap_script!(self.proxy_is_extensible(host, roots, target));
        let target_keys = unwrap_script!(self.own_keys(host, roots, target, false));
        let roots = &roots.with(&target_keys);
        let mut omitted_protected_key = false;
        for key in &target_keys {
            let descriptor =
                unwrap_script!(self.own_descriptor(host, roots, target, key.borrowed()));
            omitted_protected_key |= (!extensible
                || descriptor.is_some_and(|(_, attrs)| !attrs.configurable))
                && !seen.contains(key);
        }
        if omitted_protected_key {
            return Ok(Err(self.type_error(
                roots,
                "Proxy ownKeys trap omitted a protected key",
            )?));
        }
        if !extensible && target_keys.len() != keys.len() {
            return Ok(Err(self.type_error(
                roots,
                "Proxy ownKeys trap changed keys of a non-extensible target",
            )?));
        }
        Ok(Ok(keys))
    }

    pub(crate) fn enumerable_string_keys<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
    ) -> Result<Result<Vec<JsString>, Value>, VmError> {
        let roots = &roots.with(&id);
        let ordinary = !matches!(self.heap.get(id)?, Object::Proxy { .. });
        let keys = unwrap_script!(self.own_keys(host, roots, id, ordinary));
        let mut names = Vec::new();
        let roots = &roots.with(&keys);
        for key in &keys {
            if let PropertyKey::String(key) = key
                && (ordinary
                    || unwrap_script!(self.own_descriptor(
                        host,
                        roots,
                        id,
                        PropertyKeyRef::JsString(key)
                    ))
                    .is_some_and(|(_, attrs)| attrs.enumerable))
            {
                names.push(key.clone());
            }
        }
        Ok(Ok(names))
    }

    pub(crate) fn own_descriptor<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        key: PropertyKeyRef<'_>,
    ) -> Result<Result<Option<(Property, Attributes)>, Value>, VmError> {
        let root_view = (id, key);
        let roots = &roots.with(&root_view);
        let Object::Proxy { slots, .. } = *self.heap.get(id)? else {
            let property = self.heap.own_property(id, key)?;
            return Ok(Ok(property.map(|property| {
                let attrs = property.attributes();
                (property.into_owned(), attrs)
            })));
        };
        let Some((target, handler)) = slots else {
            return Ok(Err(self.type_error(roots, "Proxy has been revoked")?));
        };
        let trap_slots = (target, handler);
        let roots = &roots.with(&trap_slots);
        let Some(result) = unwrap_script!(self.proxy_trap(
            host,
            roots,
            handler,
            "getOwnPropertyDescriptor",
            vec![Value::object(target), key_value(key)]
        )) else {
            return self.own_descriptor(host, roots, target, key);
        };
        if !matches!(result.view(), ValueRef::Undefined | ValueRef::Object(_)) {
            return Ok(Err(self.type_error(
                roots,
                "Proxy descriptor trap must return an object or undefined",
            )?));
        }
        let roots = &roots.with(&result);
        let current = unwrap_script!(self.own_descriptor(host, roots, target, key));
        let current_property = current.as_ref().map(|(property, _)| property);
        let roots = &roots.with(&current_property);
        let invalid = |vm: &mut Self| {
            Ok(Err(vm.type_error(
                roots,
                "Proxy descriptor trap reported an incompatible descriptor",
            )?))
        };
        if result == Value::UNDEFINED {
            let Some((_, attrs)) = current else {
                return Ok(Ok(None));
            };
            return if !attrs.configurable
                || !unwrap_script!(self.proxy_is_extensible(host, roots, target))
            {
                invalid(self)
            } else {
                Ok(Ok(None))
            };
        }
        let extensible = unwrap_script!(self.proxy_is_extensible(host, roots, target));
        let update = unwrap_script!(self.read_property_descriptor(host, roots, &result));
        let Some((property, attrs)) = update.complete(true, None) else {
            unreachable!()
        };
        if !attrs.configurable
            && current.as_ref().is_none_or(|(old_property, old)| {
                old.configurable
                    || matches!(
                        (&property, old_property),
                        (Property::Data(_), Property::Data(_))
                    ) && !attrs.writable
                        && old.writable
            })
        {
            return invalid(self);
        }
        let mut complete = DescriptorUpdate {
            enumerable: Some(attrs.enumerable),
            configurable: Some(attrs.configurable),
            ..DescriptorUpdate::default()
        };
        match &property {
            Property::Data(value) => {
                complete.value = Some(value.clone());
                complete.writable = Some(attrs.writable);
            }
            Property::Accessor { get, set } => {
                complete.get = Some(*get);
                complete.set = Some(*set);
            }
        }
        if complete
            .complete(
                extensible,
                current
                    .as_ref()
                    .map(|(property, attrs)| OwnProperty::Stored(property, *attrs)),
            )
            .is_none()
        {
            return invalid(self);
        }
        Ok(Ok(Some((property, attrs))))
    }

    pub(crate) fn for_in_next<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        iterator: ObjectId,
    ) -> Result<Result<Option<JsString>, Value>, VmError> {
        let roots = &roots.with(&iterator);
        loop {
            let Object::ForInIterator {
                current,
                pending,
                visited,
                string_indices,
            } = self.heap.get_mut(iterator)?
            else {
                return Err(VmError::InvalidState("expected a for-in cursor".into()));
            };
            if let Some(index) = string_indices.next() {
                return Ok(Ok(Some(index.to_string().into())));
            }
            let Some(id) = *current else {
                return Ok(Ok(None));
            };
            if let Some(keys) = pending {
                if let Some(key) = keys.pop_front() {
                    if visited.contains(&key)
                        || ops::canonical_index_key(PropertyKeyRef::JsString(&key))
                            .is_some_and(|index| index < string_indices.end)
                    {
                        continue;
                    }
                    let Some((_, attrs)) = unwrap_script!(self.own_descriptor(
                        host,
                        roots,
                        id,
                        PropertyKeyRef::JsString(&key)
                    )) else {
                        continue;
                    };
                    let Object::ForInIterator { visited, .. } = self.heap.get_mut(iterator)? else {
                        unreachable!()
                    };
                    if attrs.enumerable {
                        visited.insert(key.clone());
                        return Ok(Ok(Some(key)));
                    }
                    visited.insert(key);
                } else {
                    let prototype = unwrap_script!(self.proxy_prototype(host, roots, id));
                    let Object::ForInIterator {
                        current, pending, ..
                    } = self.heap.get_mut(iterator)?
                    else {
                        unreachable!()
                    };
                    *current = prototype;
                    *pending = None;
                }
            } else {
                // Snapshot each level's keys on first reaching it, but recheck
                // its descriptors as keys are visited (including after await).
                let keys = unwrap_script!(self.own_keys(host, roots, id, false))
                    .into_iter()
                    .filter_map(|key| match key {
                        PropertyKey::String(key) => Some(key),
                        PropertyKey::Symbol(_) => None,
                    })
                    .collect();
                let Object::ForInIterator { pending, .. } = self.heap.get_mut(iterator)? else {
                    unreachable!()
                };
                *pending = Some(keys);
            }
        }
    }
}

fn key_value(key: PropertyKeyRef<'_>) -> Value {
    match key {
        PropertyKeyRef::String(key) => Value::string(key.into()),
        PropertyKeyRef::JsString(key) => Value::string(key.clone()),
        PropertyKeyRef::Symbol(key) => Value::symbol(key),
    }
}

pub(crate) struct BuiltinTable<P>(std::marker::PhantomData<P>);
