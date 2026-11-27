//! Callable dispatch, invocation, construction, and super access.

use super::{
    Constructor, DERIVED_CLASS, DERIVED_TARGET, DERIVED_THIS, Dispatch, UNDEFINED_VALUE,
    is_callable_object,
};
use crate::builtin::symbol_family;
use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::heap::{Attributes, Object};
use crate::host::{Host, PlatformServices};
use crate::intrinsic::{CallableTarget, ClassArgs, ClassPrototype, Construction};
use crate::ops;
use crate::runtime::{Arguments, Invocation};
use crate::value::{ObjectId, PropertyKeyRef, Value, ValueRef};
use crate::vm::VmCore;
impl VmCore {
    pub(crate) fn is_callable(&self, value: &Value) -> bool {
        let ValueRef::Object(id) = value.view() else {
            return false;
        };
        self.heap.get(id).is_ok_and(is_callable_object)
    }

    pub(crate) fn materialize_function(
        &mut self,
        func_id: u32,
        env: ObjectId,
    ) -> Result<ObjectId, VmError> {
        let index = self.function_index(func_id)?;
        if self.is_initializer(index) {
            return Err(VmError::InvalidState(
                "script function cannot be materialized".to_owned(),
            ));
        }
        let id = self.heap.alloc_function(func_id, env);
        let function = &self.build.functions[index];
        let constructable = function.is_constructable();
        let generator = function.is_generator;
        let async_generator = generator && function.is_async;
        self.install_script_metadata(id, func_id)?;
        if constructable {
            let prototype = self.heap.alloc_plain();
            self.heap.define_data(
                prototype,
                "constructor",
                Value::object(id),
                Attributes::METHOD,
            )?;
            self.heap.define_data(
                id,
                "prototype",
                Value::object(prototype),
                Attributes::FUNCTION_PROTOTYPE,
            )?;
        }
        if generator {
            // Generator functions inherit the matching
            // %GeneratorFunction.prototype%, whose own `.prototype` is the
            // shared instance prototype. Instances then inherit a fresh
            // `.prototype` object chained to the same shared prototype.
            let function_prototype = if async_generator {
                self.builtins.get("asyncGeneratorFunction.prototype")
            } else {
                self.builtins.get("generatorFunction.prototype")
            };
            let Object::Function(function) = self.heap.get_mut(id)? else {
                unreachable!()
            };
            function.prototype = Some(function_prototype);
            let shared = if async_generator {
                self.heap.async_generator_prototype
            } else {
                self.heap.generator_prototype
            };
            let prototype = self.heap.alloc_plain_with(shared);
            self.heap.define_data(
                id,
                "prototype",
                Value::object(prototype),
                Attributes::FUNCTION_PROTOTYPE,
            )?;
        }
        Ok(id)
    }

    /// Script errors are returned as `Invocation::Thrown`.
    pub(crate) fn invoke<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        callee: &Value,
        args: Arguments<'_>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        let root_view = (callee, receiver);
        let roots = &roots.with(&root_view);
        let roots = &roots.with(&args);
        match self.classify(callee)? {
            Some(Dispatch::Script {
                func_id,
                env,
                callee,
                home,
                class,
            }) => {
                if class {
                    return self.throw_type_error(roots, "class constructor requires new");
                }
                self.call_function(
                    host,
                    roots,
                    func_id,
                    Some(env),
                    args,
                    receiver,
                    (&Value::UNDEFINED, home),
                    Some(callee),
                )
            }
            Some(Dispatch::Resolution(id)) => {
                self.capability_call(host, roots, id, args.get(0)?.clone())?;
                Ok(Invocation::Value(Value::UNDEFINED))
            }
            Some(Dispatch::PromiseCallback(id)) => {
                self.call_promise_callback(host, roots, id, args)
            }
            Some(Dispatch::ProxyRevoker(id)) => self.revoke_proxy(id),
            Some(Dispatch::Proxy {
                id, callable: true, ..
            }) => self.invoke_proxy(host, roots, id, args, receiver, false),
            Some(Dispatch::Bound(bound)) => {
                let target = Value::object(bound.target);
                let bound_this = bound.bound_this.clone();
                let mut joined = bound.args.clone();
                joined.extend(args.tail(0)?);
                let roots = &roots.with(&joined);
                self.invoke(
                    host,
                    roots,
                    &target,
                    Arguments::Values(&joined),
                    &bound_this,
                )
            }
            Some(Dispatch::Native { target, .. }) => match target {
                CallableTarget::Host(target) => {
                    self.call_host(host, roots, target, args, receiver, None)
                }
                CallableTarget::Intrinsic(id) => {
                    let values = args.to_vec()?;
                    (id.definition::<P>().call)(self, host, roots, values, receiver)
                }
            },
            Some(Dispatch::Proxy { .. }) | None => {
                self.throw_type_error(roots, "value is not a function")
            }
        }
    }

    fn invoke_proxy<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        proxy: ObjectId,
        args: Arguments<'_>,
        receiver_or_new_target: &Value,
        construct: bool,
    ) -> Result<Invocation, VmError> {
        let Object::Proxy { slots, .. } = self.heap.get(proxy)? else {
            unreachable!("classified proxy")
        };
        let Some((target, handler)) = *slots else {
            return self.throw_type_error(roots, "Proxy has been revoked");
        };
        // A trap getter can revoke the proxy while these slots are in use.
        let live = (target, handler);
        let roots = &roots.with(&live);
        let target = Value::object(target);
        let name = if construct { "construct" } else { "apply" };
        let method = match self.proxy_get_method(host, roots, handler, name)? {
            Ok(Some(method)) => method,
            Ok(None) if construct => {
                return self.construct_with(host, roots, &target, args, receiver_or_new_target);
            }
            Ok(None) => return self.invoke(host, roots, &target, args, receiver_or_new_target),
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let array = Value::object(self.heap.alloc_array(args.to_vec()?));
        let forwarded = receiver_or_new_target.clone();
        let trap_args = if construct {
            [target, array, forwarded]
        } else {
            [target, forwarded, array]
        };
        match self.invoke(
            host,
            roots,
            &method,
            Arguments::Values(&trap_args),
            &Value::object(handler),
        )? {
            Invocation::Value(value)
                if construct && !matches!(value.view(), ValueRef::Object(_)) =>
            {
                self.throw_type_error(roots, "Proxy construct trap must return an object")
            }
            result => Ok(result),
        }
    }

    pub(crate) fn constructor(
        &self,
        dispatch: Option<Dispatch<'_>>,
    ) -> Result<Option<Constructor>, VmError> {
        Ok(match dispatch {
            Some(Dispatch::Script { func_id, env, .. }) => {
                let index = self.function_index(func_id)?;
                (!self.is_initializer(index) && self.build.functions[index].is_constructable())
                    .then_some(Constructor::Script { func_id, env })
            }
            Some(Dispatch::Native {
                target,
                constructable: true,
            }) => Some(Constructor::Native(target)),
            _ => None,
        })
    }

    pub(crate) fn is_constructor(&self, value: &Value) -> Result<bool, VmError> {
        let mut dispatch = self.classify(value)?;
        loop {
            match dispatch {
                Some(Dispatch::Bound(bound)) => {
                    dispatch = self.classify(&Value::object(bound.target))?;
                }
                Some(Dispatch::Proxy { constructible, .. }) => return Ok(constructible),
                _ => return Ok(self.constructor(dispatch)?.is_some()),
            }
        }
    }

    pub(crate) fn construct_with<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        callee: &Value,
        args: Arguments<'_>,
        new_target: &Value,
    ) -> Result<Invocation, VmError> {
        let root_view = (callee, new_target);
        let roots = &roots.with(&root_view);
        let dispatch = self.classify(callee)?;
        if let Some(Dispatch::Bound(bound)) = dispatch {
            let target = Value::object(bound.target);
            let mut joined = bound.args.clone();
            joined.extend(args.tail(0)?);
            // Direct construction uses the target's prototype, not the wrapper's.
            let new_target = if ops::strict_equal(callee, new_target) {
                &target
            } else {
                new_target
            };
            let roots = &roots.with(&joined);
            return self.construct_with(
                host,
                roots,
                &target,
                Arguments::Values(&joined),
                new_target,
            );
        }
        if let Some(Dispatch::Proxy {
            id, constructible, ..
        }) = dispatch
        {
            let roots = &roots.with(&args);
            if !constructible {
                return self.throw_type_error(roots, "value is not a constructor");
            }
            return self.invoke_proxy(host, roots, id, args, new_target, true);
        }
        let symbol = matches!(&dispatch, Some(Dispatch::Native { target: CallableTarget::Intrinsic(id), .. })
            if *id == symbol_family::CONSTRUCTOR.link());
        let Some(constructor) = self.constructor(dispatch)? else {
            return self.throw_type_error(
                roots,
                if symbol {
                    "Symbol is not a constructor"
                } else {
                    "value is not a constructor"
                },
            );
        };
        if let ValueRef::Object(class) = callee.view()
            && let Some(record) = self.heap.function(class).and_then(|f| f.class.as_ref())
            && record.source.derived
        {
            let default = record.source.default_constructor;
            let Constructor::Script { func_id, env } = constructor else {
                unreachable!()
            };
            if default {
                let parent = self.super_parent(class)?;
                return self.construct_super_and_initialize(
                    host,
                    roots,
                    class,
                    new_target,
                    args,
                    (&parent, None),
                );
            }
            let context = self.heap.alloc_environment(
                None,
                std::collections::BTreeMap::from([
                    (DERIVED_CLASS, Value::object(class)),
                    (DERIVED_TARGET, new_target.clone()),
                ]),
            );
            let roots = &roots.with(&context);
            return match self.call_function(
                host,
                roots,
                func_id,
                Some(env),
                args,
                &Value::object(context),
                (new_target, self.heap.function(class).and_then(|f| f.home)),
                Some(class),
            )? {
                Invocation::Value(value) if matches!(value.view(), ValueRef::Undefined) => {
                    self.derived_this(roots, context)
                }
                Invocation::Value(value) if !matches!(value.view(), ValueRef::Object(_)) => self
                    .throw_type_error(
                        roots,
                        "derived constructor returned a primitive other than undefined",
                    ),
                result => Ok(result),
            };
        }
        let prototype_roots = &roots.with(&args);
        let distinct_new_target = !ops::strict_equal(callee, new_target);
        let skip_prototype = matches!(constructor, Constructor::Native(CallableTarget::Intrinsic(id))
            if id.definition::<P>().skip_new_target_prototype);
        let prototype = if !skip_prototype
            && (distinct_new_target || matches!(constructor, Constructor::Script { .. }))
        {
            match self.read_property(
                host,
                prototype_roots,
                new_target,
                PropertyKeyRef::String("prototype"),
            )? {
                Invocation::Value(value) => match value.view() {
                    ValueRef::Object(id) => Some(id),
                    _ => None,
                },
                thrown @ Invocation::Thrown(_) => return Ok(thrown),
            }
        } else {
            None
        };
        match constructor {
            Constructor::Script { func_id, env } => {
                let prototype = prototype.unwrap_or(self.builtins.get("object.prototype"));
                let receiver = self.heap.alloc_plain_with(Some(prototype));
                let instance = Value::object(receiver);
                let root_view = (&instance, &args);
                let init_roots = &roots.with(&root_view);
                if let ValueRef::Object(class) = callee.view()
                    && self.heap.function(class).is_some_and(|f| f.class.is_some())
                    && let ValueRef::Object(receiver) = instance.view()
                    && let Some(error) =
                        self.initialize_class_elements(host, init_roots, class, receiver, false)?
                {
                    return Ok(Invocation::Thrown(error));
                }
                let roots = &roots.with(&instance);
                let home = match callee.view() {
                    ValueRef::Object(id) => self.heap.function(id).and_then(|f| f.home),
                    _ => None,
                };
                match self.call_function(
                    host,
                    roots,
                    func_id,
                    Some(env),
                    args,
                    &instance,
                    (new_target, home),
                    None,
                )? {
                    Invocation::Value(value) if !matches!(value.view(), ValueRef::Object(_)) => {
                        Ok(Invocation::Value(instance))
                    }
                    result => Ok(result),
                }
            }
            Constructor::Native(CallableTarget::Host(target)) => self.call_host(
                host,
                roots,
                target,
                args,
                &Value::UNDEFINED,
                Some(crate::vm::HostConstruction { prototype }),
            ),
            Constructor::Native(CallableTarget::Intrinsic(id)) => {
                let values = args.to_vec()?;
                match id
                    .definition::<P>()
                    .construct
                    .expect("classified constructor")
                {
                    Construction::WithPrototype(construct) => {
                        let roots = &roots.with(&prototype);
                        construct(self, host, roots, values, prototype)
                    }
                    Construction::WithNewTarget(construct) => {
                        construct(self, host, roots, values, new_target)
                    }
                    Construction::Class(class, initialize) => {
                        let live = (&prototype, &values);
                        let roots = &roots.with(&live);
                        let prototype = if skip_prototype {
                            ClassPrototype::Deferred(new_target)
                        } else {
                            ClassPrototype::Resolved(
                                prototype.unwrap_or_else(|| self.builtins.get(class.prototype)),
                            )
                        };
                        initialize(
                            self,
                            host,
                            roots,
                            prototype,
                            class.key.owner,
                            ClassArgs::Values(&values),
                        )
                    }
                }
            }
        }
    }

    pub(crate) fn derived_this(
        &mut self,
        roots: &ActiveRoots<'_>,
        context: ObjectId,
    ) -> Result<Invocation, VmError> {
        let record = self.heap.environment(context)?;
        if !record.bindings.contains_key(&DERIVED_CLASS) {
            return Err(VmError::InvalidState(
                "invalid derived constructor context".into(),
            ));
        }
        match record.bindings.get(&DERIVED_THIS) {
            Some(receiver) => Ok(Invocation::Value(receiver.clone())),
            None => Ok(Invocation::Thrown(
                self.reference_error(roots, "this is not initialized")?,
            )),
        }
    }

    fn super_parent(&self, class: ObjectId) -> Result<Value, VmError> {
        Ok(self
            .heap
            .prototype_of(class)?
            .map_or(Value::NULL, Value::object))
    }

    pub(crate) fn super_constructor(&self, context: ObjectId) -> Result<Value, VmError> {
        let ValueRef::Object(class) = self.heap.get_binding(context, DERIVED_CLASS)?.view() else {
            return Err(VmError::InvalidState(
                "invalid derived constructor context".into(),
            ));
        };
        self.super_parent(class)
    }

    pub(crate) fn super_construct<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        context: ObjectId,
        callee: &Value,
        args: Arguments<'_>,
    ) -> Result<Invocation, VmError> {
        let bindings = &self.heap.environment(context)?.bindings;
        let (Some(ValueRef::Object(class)), Some(new_target)) = (
            bindings.get(&DERIVED_CLASS).map(|value| value.view()),
            bindings
                .get(&DERIVED_TARGET)
                .filter(|value| matches!(value.view(), ValueRef::Object(_))),
        ) else {
            return Err(VmError::InvalidState(
                "invalid derived constructor context".into(),
            ));
        };
        let new_target = new_target.clone();
        self.construct_super_and_initialize(
            host,
            roots,
            class,
            &new_target,
            args,
            (callee, Some(context)),
        )
    }

    fn construct_super_and_initialize<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        class: ObjectId,
        new_target: &Value,
        args: Arguments<'_>,
        (callee, context): (&Value, Option<ObjectId>),
    ) -> Result<Invocation, VmError> {
        let root_view = (class, new_target, callee, context);
        let roots = &roots.with(&root_view);
        let receiver = match self.construct_with(host, roots, callee, args, new_target)? {
            Invocation::Value(value) => {
                let ValueRef::Object(id) = value.view() else {
                    return self.throw_type_error(roots, "super constructor must return an object");
                };
                id
            }
            thrown @ Invocation::Thrown(_) => return Ok(thrown),
        };
        if let Some(context) = context {
            let bindings = &mut self.heap.environment_mut(context)?.bindings;
            let std::collections::btree_map::Entry::Vacant(entry) = bindings.entry(DERIVED_THIS)
            else {
                return Ok(Invocation::Thrown(
                    self.reference_error(roots, "this is already initialized")?,
                ));
            };
            entry.insert(Value::object(receiver));
        }
        if let Some(error) =
            self.initialize_class_elements(host, &roots.with(&receiver), class, receiver, false)?
        {
            return Ok(Invocation::Thrown(error));
        }
        Ok(Invocation::Value(Value::object(receiver)))
    }

    pub(crate) fn reflect_call<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        args: &[Value],
        construct: bool,
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let target = args.first().unwrap_or(&UNDEFINED_VALUE);
        let new_target = args.get(2).unwrap_or(target);
        let list = if construct {
            if !self.is_constructor(target)? {
                return self.throw_type_error(roots, "value is not a constructor");
            }
            if args.get(2).is_some() && !self.is_constructor(new_target)? {
                return self.throw_type_error(roots, "newTarget is not a constructor");
            }
            args.get(1)
        } else {
            if !self.is_callable(target) {
                return self.throw_type_error(roots, "value is not a function");
            }
            args.get(2)
        };
        let values =
            match self.arguments_from_array_like(host, roots, list.unwrap_or(&UNDEFINED_VALUE))? {
                Ok(values) => values,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
        if construct {
            self.construct_with(host, roots, target, Arguments::Values(&values), new_target)
        } else {
            self.invoke(
                host,
                roots,
                target,
                Arguments::Values(&values),
                args.get(1).unwrap_or(&UNDEFINED_VALUE),
            )
        }
    }

    pub(crate) fn arguments_from_array_like<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        list: &Value,
    ) -> Result<Result<Vec<Value>, Value>, VmError> {
        let roots = &roots.with(&list);
        if !matches!(list.view(), ValueRef::Object(_)) {
            return Ok(Err(
                self.type_error(roots, "argumentsList must be an object")?
            ));
        }
        let length =
            match self.read_property(host, roots, list, PropertyKeyRef::String("length"))? {
                Invocation::Value(value) => value,
                Invocation::Thrown(error) => return Ok(Err(error)),
            };
        let length = match self.coerce_number(host, roots, &length)? {
            // Casting NaN to an integer yields zero, as required by ToLength.
            Ok(number) => number.trunc().clamp(0.0, 9_007_199_254_740_991.0) as u64,
            Err(error) => return Ok(Err(error)),
        };
        let mut values = Vec::new();
        for index in 0..length {
            let roots = &roots.with(&values);
            match self.read_property(
                host,
                roots,
                list,
                PropertyKeyRef::String(&index.to_string()),
            )? {
                Invocation::Value(value) => values.push(value),
                Invocation::Thrown(error) => return Ok(Err(error)),
            }
        }
        Ok(Ok(values))
    }

    pub(crate) fn classify(&self, value: &Value) -> Result<Option<Dispatch<'_>>, VmError> {
        let ValueRef::Object(id) = value.view() else {
            return Ok(None);
        };
        if let Some(function) = self.heap.function(id) {
            return Ok(Some(Dispatch::Script {
                func_id: function.func_id,
                env: function.env,
                callee: id,
                home: function.home,
                class: function.class.is_some(),
            }));
        }
        Ok(match self.heap.get(id)? {
            Object::Resolution(_) => Some(Dispatch::Resolution(id)),
            Object::PromiseCallback { .. } => Some(Dispatch::PromiseCallback(id)),
            Object::ProxyRevoker { .. } => Some(Dispatch::ProxyRevoker(id)),
            Object::Proxy {
                callable,
                constructible,
                ..
            } => Some(Dispatch::Proxy {
                id,
                callable: *callable,
                constructible: *constructible,
            }),
            Object::Bound(bound) => Some(Dispatch::Bound(bound)),
            Object::Native(native) => Some(Dispatch::Native {
                target: native.target(),
                constructable: native.constructable(),
            }),
            _ => None,
        })
    }
}
