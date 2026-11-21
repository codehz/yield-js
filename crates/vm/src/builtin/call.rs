use crate::builtin::promise::members::THEN;
use crate::builtin::string::utf16_len;
use crate::builtin::symbol_family;
use crate::error::VmError;
use crate::gc::{ActiveRoots, TraceRoots, Tracer};
use crate::heap::{
    Attributes, BoundObject, BoxedPrimitive, CombineKind, CombineState, DescriptorUpdate,
    ITERATOR_SYMBOL, IntegrityLevel, Object, PlainObject, PromiseCallback, PromiseState, Property,
    ReactionJob, Resolution, SPECIES_SYMBOL,
};
use crate::host::Host;
use crate::intrinsic::CallableTarget;
use crate::ops;
use crate::runtime::{Invocation, Microtask, Settlement};
use crate::value::{JsString, ObjectId, PropertyKey, PropertyKeyRef, Value};
use crate::vm::Vm;

// Reserved keys for the opaque, shared derived invocation cell.
pub(crate) const DERIVED_CLASS: u32 = u32::MAX;
pub(crate) const DERIVED_TARGET: u32 = u32::MAX - 1;
pub(crate) const DERIVED_THIS: u32 = u32::MAX - 2;

pub(crate) enum Dispatch<'a> {
    Script {
        func_id: u32,
        env: ObjectId,
    },
    Resolution(ObjectId),
    PromiseCallback(ObjectId),
    Bound(&'a BoundObject),
    Native {
        target: CallableTarget,
        constructable: bool,
    },
}

pub(crate) enum Constructor {
    Script { func_id: u32, env: ObjectId },
    Native(CallableTarget),
}

impl Vm {
    pub(crate) fn is_callable(&self, value: &Value) -> bool {
        let Value::Object(id) = value else {
            return false;
        };
        self.heap.get(*id).is_ok_and(is_callable_object)
    }

    pub(crate) fn materialize_function(
        &mut self,
        func_id: u32,
        env: ObjectId,
    ) -> Result<ObjectId, VmError> {
        if self.build.is_initializer(func_id) {
            return Err(VmError::InvalidState(
                "script function cannot be materialized".to_owned(),
            ));
        }
        let id = self.heap.alloc_function(func_id, env);
        let index = self.function_index(func_id)?;
        let constructable = self.build.functions[index].is_constructable();
        if constructable {
            let prototype = self.heap.alloc_plain();
            self.heap.define_data(
                prototype,
                "constructor",
                Value::Object(id),
                Attributes::METHOD,
            )?;
            self.heap.define_data(
                id,
                "prototype",
                Value::Object(prototype),
                Attributes::FUNCTION_PROTOTYPE,
            )?;
        }
        Ok(id)
    }

    /// Script errors are returned as `Invocation::Thrown`.
    pub(crate) fn invoke(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        callee: &Value,
        args: Vec<Value>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        let root_view = (callee, receiver);
        let roots = &roots.with(&root_view);
        match self.classify(callee)? {
            Some(Dispatch::Script { func_id, env }) => {
                if matches!(callee, Value::Object(id) if self.heap.function(*id).is_some_and(|f| f.class.is_some()))
                {
                    return self.throw_type_error("class constructor requires new");
                }
                if self.build.is_initializer(func_id) {
                    return Err(VmError::InvalidState(
                        "script function is not callable".to_owned(),
                    ));
                }
                let home = match callee {
                    Value::Object(id) => self.heap.function(*id).and_then(|f| f.home),
                    _ => None,
                };
                self.call_function(
                    host,
                    roots,
                    func_id,
                    Some(env),
                    args,
                    receiver,
                    (&Value::Undefined, home),
                )
            }
            Some(Dispatch::Resolution(id)) => {
                self.capability_call(
                    host,
                    roots,
                    id,
                    args.into_iter().next().unwrap_or(Value::Undefined),
                )?;
                Ok(Invocation::Value(Value::Undefined))
            }
            Some(Dispatch::PromiseCallback(id)) => {
                self.call_promise_callback(host, roots, id, args)
            }
            Some(Dispatch::Bound(bound)) => {
                let target = Value::Object(bound.target);
                let bound_this = bound.bound_this.clone();
                let args = bound.args.iter().cloned().chain(args).collect();
                self.invoke(host, roots, &target, args, &bound_this)
            }
            Some(Dispatch::Native { target, .. }) => match target {
                CallableTarget::Host(target) => {
                    self.call_host(host, roots, target, args, receiver, None)
                }
                CallableTarget::Intrinsic(id) => {
                    (id.definition().call)(self, host, roots, args, receiver)
                }
            },
            None => self.throw_type_error("value is not a function"),
        }
    }

    pub(crate) fn constructor(
        &self,
        dispatch: Option<Dispatch<'_>>,
    ) -> Result<Option<Constructor>, VmError> {
        Ok(match dispatch {
            Some(Dispatch::Script { func_id, env })
                if !self.build.is_initializer(func_id)
                    && self.build.functions[self.function_index(func_id)?].is_constructable() =>
            {
                Some(Constructor::Script { func_id, env })
            }
            Some(Dispatch::Native {
                target,
                constructable: true,
            }) => Some(Constructor::Native(target)),
            _ => None,
        })
    }

    pub(crate) fn is_constructor(&self, value: &Value) -> Result<bool, VmError> {
        let dispatch = match self.classify(value)? {
            Some(Dispatch::Bound(bound)) => self.classify(&Value::Object(bound.target))?,
            dispatch => dispatch,
        };
        Ok(self.constructor(dispatch)?.is_some())
    }

    pub(crate) fn construct_with(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        callee: &Value,
        args: Vec<Value>,
        new_target: &Value,
    ) -> Result<Invocation, VmError> {
        let root_view = (callee, new_target);
        let roots = &roots.with(&root_view);
        let dispatch = self.classify(callee)?;
        if let Some(Dispatch::Bound(bound)) = dispatch {
            let target = Value::Object(bound.target);
            let args = bound.args.iter().cloned().chain(args).collect();
            // Direct construction uses the target's prototype, not the wrapper's.
            let new_target = if ops::strict_equal(callee, new_target) {
                &target
            } else {
                new_target
            };
            return self.construct_with(host, roots, &target, args, new_target);
        }
        let symbol = matches!(&dispatch, Some(Dispatch::Native { target: CallableTarget::Intrinsic(id), .. })
            if *id == symbol_family::CONSTRUCTOR.link());
        let Some(constructor) = self.constructor(dispatch)? else {
            return self.throw_type_error(if symbol {
                "Symbol is not a constructor"
            } else {
                "value is not a constructor"
            });
        };
        if let Value::Object(class) = *callee
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
                    (DERIVED_CLASS, Value::Object(class)),
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
                &Value::Object(context),
                (new_target, self.heap.function(class).and_then(|f| f.home)),
            )? {
                Invocation::Value(Value::Undefined) => self.derived_this(context),
                Invocation::Value(value) if !matches!(value, Value::Object(_)) => self
                    .throw_type_error(
                        "derived constructor returned a primitive other than undefined",
                    ),
                result => Ok(result),
            };
        }
        let prototype_roots = &roots.with(&args);
        let distinct_new_target = !ops::strict_equal(callee, new_target);
        let skip_prototype = matches!(constructor, Constructor::Native(CallableTarget::Intrinsic(id))
            if id.definition().skip_new_target_prototype);
        let prototype = if !skip_prototype
            && (distinct_new_target || matches!(constructor, Constructor::Script { .. }))
        {
            match self.read_property(
                host,
                prototype_roots,
                new_target,
                PropertyKeyRef::String("prototype"),
            )? {
                Invocation::Value(Value::Object(id)) => Some(id),
                Invocation::Value(_) => None,
                thrown @ Invocation::Thrown(_) => return Ok(thrown),
            }
        } else {
            None
        };
        match constructor {
            Constructor::Script { func_id, env } => {
                let prototype = prototype.unwrap_or(self.builtins.get("object.prototype"));
                let instance = Value::Object(self.heap.alloc_plain_with(Some(prototype)));
                let root_view = (&instance, &args);
                let init_roots = &roots.with(&root_view);
                if let Value::Object(class) = callee
                    && self
                        .heap
                        .function(*class)
                        .is_some_and(|f| f.class.is_some())
                    && let Value::Object(receiver) = instance
                    && let Some(error) =
                        self.initialize_class_elements(host, init_roots, *class, receiver, false)?
                {
                    return Ok(Invocation::Thrown(error));
                }
                let roots = &roots.with(&instance);
                let home = match callee {
                    Value::Object(id) => self.heap.function(*id).and_then(|f| f.home),
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
                )? {
                    Invocation::Value(value) if !matches!(value, Value::Object(_)) => {
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
                &Value::Undefined,
                Some(crate::vm::HostConstruction { prototype }),
            ),
            Constructor::Native(CallableTarget::Intrinsic(id)) => {
                let roots = &roots.with(&prototype);
                (id.definition().construct.expect("classified constructor"))(
                    self, host, roots, args, prototype,
                )
            }
        }
    }

    pub(crate) fn derived_this(&mut self, context: ObjectId) -> Result<Invocation, VmError> {
        let record = self.heap.environment(context)?;
        if !record.bindings.contains_key(&DERIVED_CLASS) {
            return Err(VmError::InvalidState(
                "invalid derived constructor context".into(),
            ));
        }
        match record.bindings.get(&DERIVED_THIS) {
            Some(receiver) => Ok(Invocation::Value(receiver.clone())),
            None => Ok(Invocation::Thrown(
                self.reference_error("this is not initialized")?,
            )),
        }
    }

    fn super_parent(&self, class: ObjectId) -> Result<Value, VmError> {
        Ok(self
            .heap
            .prototype_of(class)?
            .map_or(Value::Null, Value::Object))
    }

    pub(crate) fn super_constructor(&self, context: ObjectId) -> Result<Value, VmError> {
        let Value::Object(class) = self.heap.get_binding(context, DERIVED_CLASS)? else {
            return Err(VmError::InvalidState(
                "invalid derived constructor context".into(),
            ));
        };
        self.super_parent(class)
    }

    pub(crate) fn super_construct(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        context: ObjectId,
        callee: &Value,
        args: Vec<Value>,
    ) -> Result<Invocation, VmError> {
        let bindings = &self.heap.environment(context)?.bindings;
        let (Some(Value::Object(class)), Some(new_target @ Value::Object(_))) =
            (bindings.get(&DERIVED_CLASS), bindings.get(&DERIVED_TARGET))
        else {
            return Err(VmError::InvalidState(
                "invalid derived constructor context".into(),
            ));
        };
        self.construct_super_and_initialize(
            host,
            roots,
            *class,
            &new_target.clone(),
            args,
            (callee, Some(context)),
        )
    }

    fn construct_super_and_initialize(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        class: ObjectId,
        new_target: &Value,
        args: Vec<Value>,
        (callee, context): (&Value, Option<ObjectId>),
    ) -> Result<Invocation, VmError> {
        let root_view = (class, new_target, callee, context);
        let roots = &roots.with(&root_view);
        let receiver = match self.construct_with(host, roots, callee, args, new_target)? {
            Invocation::Value(Value::Object(id)) => id,
            Invocation::Value(_) => {
                return self.throw_type_error("super constructor must return an object");
            }
            thrown @ Invocation::Thrown(_) => return Ok(thrown),
        };
        if let Some(context) = context {
            let bindings = &mut self.heap.environment_mut(context)?.bindings;
            let std::collections::btree_map::Entry::Vacant(entry) = bindings.entry(DERIVED_THIS)
            else {
                return Ok(Invocation::Thrown(
                    self.reference_error("this is already initialized")?,
                ));
            };
            entry.insert(Value::Object(receiver));
        }
        if let Some(error) =
            self.initialize_class_elements(host, &roots.with(&receiver), class, receiver, false)?
        {
            return Ok(Invocation::Thrown(error));
        }
        Ok(Invocation::Value(Value::Object(receiver)))
    }

    pub(crate) fn reflect_call(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
        construct: bool,
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let target = args.first().unwrap_or(&Value::Undefined);
        let new_target = args.get(2).unwrap_or(target);
        let list = if construct {
            if !self.is_constructor(target)? {
                return self.throw_type_error("value is not a constructor");
            }
            if args.get(2).is_some() && !self.is_constructor(new_target)? {
                return self.throw_type_error("newTarget is not a constructor");
            }
            args.get(1)
        } else {
            if !self.is_callable(target) {
                return self.throw_type_error("value is not a function");
            }
            args.get(2)
        };
        let values =
            match self.arguments_from_array_like(host, roots, list.unwrap_or(&Value::Undefined))? {
                Ok(values) => values,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
        if construct {
            self.construct_with(host, roots, target, values, new_target)
        } else {
            self.invoke(
                host,
                roots,
                target,
                values,
                args.get(1).unwrap_or(&Value::Undefined),
            )
        }
    }

    pub(crate) fn arguments_from_array_like(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        list: &Value,
    ) -> Result<Result<Vec<Value>, Value>, VmError> {
        let roots = &roots.with(&list);
        if !matches!(list, Value::Object(_)) {
            return Ok(Err(self.type_error("argumentsList must be an object")?));
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
        let Value::Object(id) = value else {
            return Ok(None);
        };
        if let Some(function) = self.heap.function(*id) {
            return Ok(Some(Dispatch::Script {
                func_id: function.func_id,
                env: function.env,
            }));
        }
        Ok(match self.heap.get(*id)? {
            Object::Resolution(_) => Some(Dispatch::Resolution(*id)),
            Object::PromiseCallback { .. } => Some(Dispatch::PromiseCallback(*id)),
            Object::Bound(bound) => Some(Dispatch::Bound(bound)),
            Object::Native(native) => Some(Dispatch::Native {
                target: native.target(),
                constructable: native.constructable(),
            }),
            _ => None,
        })
    }

    pub(crate) fn array_to_string(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        let join =
            match self.read_property(host, roots, receiver, PropertyKeyRef::String("join"))? {
                Invocation::Value(value) => value,
                thrown => return Ok(thrown),
            };
        if self.is_callable(&join) {
            self.invoke(host, roots, &join, Vec::new(), receiver)
        } else {
            let tag = match receiver {
                Value::Object(id) => self.object_tag(*id)?,
                _ => "Object",
            };
            Ok(Invocation::Value(Value::String(
                format!("[object {tag}]").into(),
            )))
        }
    }

    pub(crate) fn object_tag(&self, id: ObjectId) -> Result<&'static str, VmError> {
        Ok(match self.heap.get(self.heap.proxy_target(id)?)? {
            Object::Boxed { primitive, .. } => match primitive {
                BoxedPrimitive::String(_) => "String",
                BoxedPrimitive::Number(_) => "Number",
                BoxedPrimitive::Bool(_) => "Boolean",
                BoxedPrimitive::Symbol(_) => "Symbol",
            },
            Object::Array(_) => "Array",
            Object::Function(_)
            | Object::Native(crate::heap::NativeObject::Intrinsic { .. })
            | Object::Bound(_)
            | Object::PromiseCallback { .. }
            | Object::Resolution(_) => "Function",
            Object::Promise(_) => "Promise",
            _ => "Object",
        })
    }

    pub(crate) fn array_join(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let root_view = (receiver, args);
        let roots = &roots.with(&root_view);
        let Value::Object(id) = receiver else {
            return self.throw_type_error("Array.prototype.join called on null or undefined");
        };
        let length =
            match self.read_property(host, roots, receiver, PropertyKeyRef::String("length"))? {
                Invocation::Value(value) => value,
                thrown => return Ok(thrown),
            };
        let length = match self.coerce_number(host, roots, &length)? {
            Ok(number) => number.trunc().clamp(0.0, 9_007_199_254_740_991.0) as u64,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let separator = match args.first() {
            None | Some(Value::Undefined) => ",".into(),
            Some(value) => match self.coerce_string(host, roots, value)? {
                Ok(string) => string,
                Err(error) => return Ok(Invocation::Thrown(error)),
            },
        };
        let array = matches!(self.heap.get(*id)?, Object::Array(_));
        if array && self.active_array_joins.contains(id) {
            return Ok(Invocation::Value(Value::String(JsString::default())));
        }
        if array {
            self.active_array_joins.insert(*id);
        }
        let result = (|| {
            let mut output = JsString::default();
            for index in 0..length {
                if index > 0 {
                    output.push_str(&separator);
                }
                let item = if array {
                    self.read_array_index(host, roots, *id, index as usize)?
                } else {
                    self.read_property(
                        host,
                        roots,
                        receiver,
                        PropertyKeyRef::String(&index.to_string()),
                    )?
                };
                let value = match item {
                    Invocation::Value(value) => value,
                    thrown => return Ok(thrown),
                };
                if matches!(value, Value::Undefined | Value::Null) {
                    continue;
                }
                match self.coerce_string(host, roots, &value)? {
                    Ok(value) => output.push_str(&value),
                    Err(error) => return Ok(Invocation::Thrown(error)),
                }
            }
            Ok(Invocation::Value(Value::String(output)))
        })();
        if array {
            self.active_array_joins.remove(id);
        }
        result
    }

    pub(crate) fn create_arguments_object(&mut self, args: &[Value]) -> Result<Value, VmError> {
        let id = self.heap.alloc_plain();
        for (index, value) in args.iter().enumerate() {
            self.heap.define_data(
                id,
                PropertyKey::String(index.to_string().into()),
                value.clone(),
                Attributes::ORDINARY,
            )?;
        }
        self.heap.define_data(
            id,
            "length",
            Value::Number(args.len() as f64),
            Attributes::METHOD,
        )?;
        self.heap.define_data(
            id,
            PropertyKey::Symbol(ITERATOR_SYMBOL),
            Value::Object(self.builtins.get("array.values")),
            Attributes::METHOD,
        )?;
        Ok(Value::Object(id))
    }

    pub(crate) fn array_values(&mut self, receiver: &Value) -> Result<Invocation, VmError> {
        let Value::Object(id) = *receiver else {
            return self.throw_type_error("Array.values requires an object");
        };
        let iterator = self.heap.alloc(Object::ArrayIterator {
            object: PlainObject::new(Some(self.builtins.get("arrayIterator.prototype"))),
            array: Some(id),
            next_index: 0,
        });
        Ok(Invocation::Value(Value::Object(iterator)))
    }

    pub(crate) fn array_iterator_next(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&receiver);
        let invalid_receiver = "ArrayIterator.next requires an array iterator";
        let Value::Object(id) = *receiver else {
            return self.throw_type_error(invalid_receiver);
        };
        let Object::ArrayIterator {
            array, next_index, ..
        } = self.heap.get(id)?
        else {
            return self.throw_type_error(invalid_receiver);
        };
        let next = if let Some(source) = *array {
            let roots = &roots.with(&source);
            let index = *next_index;
            let length = match self.heap.get(source)? {
                Object::Array(array) => array.length,
                _ => {
                    let value = match self.read_property(
                        host,
                        roots,
                        &Value::Object(source),
                        PropertyKeyRef::String("length"),
                    )? {
                        Invocation::Value(value) => value,
                        thrown => return Ok(thrown),
                    };
                    match self.coerce_number(host, roots, &value)? {
                        Ok(number) => number.trunc().clamp(0.0, 9_007_199_254_740_991.0) as usize,
                        Err(error) => return Ok(Invocation::Thrown(error)),
                    }
                }
            };
            let next = (index < length).then_some((source, index));
            let Object::ArrayIterator {
                array, next_index, ..
            } = self.heap.get_mut(id)?
            else {
                unreachable!()
            };
            if next.is_some() {
                *next_index = index + 1;
            } else {
                *array = None;
            }
            next
        } else {
            None
        };
        let done = next.is_none();
        let value = if let Some((source, index)) = next {
            let result = if matches!(self.heap.get(source)?, Object::Array(_)) {
                self.read_array_index(host, roots, source, index)?
            } else {
                self.read_property(
                    host,
                    roots,
                    &Value::Object(source),
                    PropertyKeyRef::String(&index.to_string()),
                )?
            };
            match result {
                Invocation::Value(value) => value,
                thrown => return Ok(thrown),
            }
        } else {
            Value::Undefined
        };
        self.iterator_result(value, done)
    }

    pub(crate) fn iterator_result(
        &mut self,
        value: Value,
        done: bool,
    ) -> Result<Invocation, VmError> {
        let result = self.heap.alloc_plain();
        self.heap.set_own_string(result, "value", value)?;
        self.heap
            .set_own_string(result, "done", Value::Bool(done))?;
        Ok(Invocation::Value(Value::Object(result)))
    }

    pub(crate) fn type_error(&mut self, message: &str) -> Result<Value, VmError> {
        self.alloc_error(
            self.builtins.get("typeerror.prototype"),
            "TypeError",
            Some(message.into()),
        )
    }

    pub(crate) fn reference_error(&mut self, message: &str) -> Result<Value, VmError> {
        self.alloc_error(
            self.builtins.get("referenceerror.prototype"),
            "ReferenceError",
            Some(message.into()),
        )
    }
    pub(crate) fn throw_type_error(&mut self, message: &str) -> Result<Invocation, VmError> {
        self.type_error(message).map(Invocation::Thrown)
    }
    pub(crate) fn make_range_error(&mut self, message: &str) -> Result<Value, VmError> {
        self.alloc_error(
            self.builtins.get("rangeerror.prototype"),
            "RangeError",
            Some(message.into()),
        )
    }

    pub(crate) fn throw_range_error(&mut self, message: &str) -> Result<Invocation, VmError> {
        self.make_range_error(message).map(Invocation::Thrown)
    }

    pub(crate) fn construct_object(&mut self, args: &[Value]) -> Result<Invocation, VmError> {
        match args.first() {
            None | Some(Value::Undefined | Value::Null) => {
                Ok(Invocation::Value(Value::Object(self.heap.alloc_plain())))
            }
            Some(value @ Value::Object(_)) => Ok(Invocation::Value(value.clone())),
            Some(value) => {
                let prototype = self.primitive_prototype(value);
                let primitive = match value {
                    Value::String(text) => BoxedPrimitive::String(text.clone()),
                    Value::Number(number) => BoxedPrimitive::Number(*number),
                    Value::Bool(value) => BoxedPrimitive::Bool(*value),
                    Value::Symbol(id) => BoxedPrimitive::Symbol(*id),
                    _ => unreachable!("only boxable primitives reach this arm"),
                };
                Ok(Invocation::Value(Value::Object(self.heap.alloc(
                    Object::Boxed {
                        primitive,
                        object: PlainObject::new(prototype),
                    },
                ))))
            }
        }
    }

    pub(crate) fn primitive_prototype(&self, value: &Value) -> Option<ObjectId> {
        match value {
            Value::String(_) => Some(self.builtins.get("string.prototype")),
            Value::Number(_) => Some(self.builtins.get("number.prototype")),
            Value::Bool(_) => Some(self.builtins.get("boolean.prototype")),
            Value::Symbol(_) => Some(self.builtins.get("symbol.prototype")),
            _ => None,
        }
    }

    pub(crate) fn object_create(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let prototype = match args.first() {
            Some(Value::Null) => None,
            Some(Value::Object(id)) => Some(*id),
            _ => {
                return self.throw_type_error("Object.create requires an object or null prototype");
            }
        };
        let object = self.heap.alloc_plain_with(prototype);
        match args.get(1) {
            None | Some(Value::Undefined) => Ok(Invocation::Value(Value::Object(object))),
            properties => self.define_properties(host, roots, object, properties),
        }
    }

    pub(crate) fn prevent_extensions_builtin_impl(
        &mut self,
        args: &[Value],
        reflect: bool,
    ) -> Result<Invocation, VmError> {
        let target = args.first().unwrap_or(&Value::Undefined);
        if let Value::Object(id) = target {
            let id = self.heap.proxy_target(*id)?;
            self.heap.get_mut(id)?.prevent_extensions();
        } else if reflect {
            return self.throw_type_error("Reflect.preventExtensions requires an object");
        }
        Ok(Invocation::Value(if reflect {
            Value::Bool(true)
        } else {
            target.clone()
        }))
    }

    pub(crate) fn is_extensible_builtin_impl(
        &mut self,
        args: &[Value],
        reflect: bool,
    ) -> Result<Invocation, VmError> {
        let extensible = match args.first() {
            Some(Value::Object(id)) => self.heap.get(self.heap.proxy_target(*id)?)?.is_extensible(),
            _ if reflect => {
                return self.throw_type_error("Reflect.isExtensible requires an object");
            }
            _ => false,
        };
        Ok(Invocation::Value(Value::Bool(extensible)))
    }

    pub(crate) fn get_prototype_of(
        &mut self,
        args: &[Value],
        reflect: bool,
    ) -> Result<Invocation, VmError> {
        let Some(Value::Object(target)) = args.first() else {
            if !reflect
                && let Some(prototype) = args
                    .first()
                    .and_then(|value| self.primitive_prototype(value))
            {
                return Ok(Invocation::Value(Value::Object(prototype)));
            }
            return self.throw_type_error(if reflect {
                "Reflect.getPrototypeOf requires an object"
            } else {
                "Object.getPrototypeOf cannot convert null or undefined to an object"
            });
        };
        Ok(Invocation::Value(
            self.heap
                .get(self.heap.proxy_target(*target)?)?
                .prototype()
                .map_or(Value::Null, Value::Object),
        ))
    }

    pub(crate) fn set_prototype_of(
        &mut self,
        args: &[Value],
        reflect: bool,
    ) -> Result<Invocation, VmError> {
        let target = args.first().unwrap_or(&Value::Undefined);
        if reflect && !matches!(target, Value::Object(_)) {
            return self.throw_type_error("Reflect.setPrototypeOf requires an object target");
        }
        if matches!(target, Value::Undefined | Value::Null) {
            return self.throw_type_error("Object.setPrototypeOf requires a non-nullish target");
        }
        let prototype = match args.get(1) {
            Some(Value::Null) => None,
            Some(Value::Object(id)) => Some(*id),
            _ => {
                return self
                    .throw_type_error("setPrototypeOf requires an object or null prototype");
            }
        };
        let Value::Object(target) = target else {
            return Ok(Invocation::Value(target.clone()));
        };
        let accepted = self.heap.set_prototype(*target, prototype)?;
        if reflect {
            Ok(Invocation::Value(Value::Bool(accepted)))
        } else if accepted {
            Ok(Invocation::Value(Value::Object(*target)))
        } else {
            self.throw_type_error("Object.setPrototypeOf cannot set the prototype")
        }
    }

    pub(crate) fn set_integrity(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
        level: IntegrityLevel,
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let target = args.first().unwrap_or(&Value::Undefined);
        let Value::Object(id) = *target else {
            return Ok(Invocation::Value(target.clone()));
        };
        let base = self.heap.proxy_target(id)?;
        if base == id {
            self.heap.get_mut(id)?.set_integrity_level(level);
            return Ok(Invocation::Value(target.clone()));
        }
        self.heap.get_mut(base)?.prevent_extensions();
        let keys = match self.own_keys(host, roots, id, false)? {
            Ok(keys) => keys,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        for key in &keys {
            let writable = if matches!(level, IntegrityLevel::Frozen) {
                let roots = &roots.with(&keys);
                match self.own_descriptor(host, roots, id, key.borrowed())? {
                    Ok(Some((Property::Data(_), _))) => Some(false),
                    Ok(Some(_)) => None,
                    Ok(None) => continue,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                }
            } else {
                None
            };
            let update = DescriptorUpdate {
                configurable: Some(false),
                writable,
                ..DescriptorUpdate::default()
            };
            if let thrown @ Invocation::Thrown(_) =
                self.apply_property_update(host, roots, id, key.clone(), update)?
            {
                return Ok(thrown);
            }
        }
        Ok(Invocation::Value(target.clone()))
    }

    pub(crate) fn test_integrity(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
        level: IntegrityLevel,
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let result = |value| Ok(Invocation::Value(Value::Bool(value)));
        let Some(Value::Object(id)) = args.first() else {
            return result(true);
        };
        let base = self.heap.proxy_target(*id)?;
        if base == *id {
            return result(self.heap.get(base)?.test_integrity_level(level));
        }
        if self.heap.get(base)?.is_extensible() {
            return result(false);
        }
        let keys = match self.own_keys(host, roots, *id, false)? {
            Ok(keys) => keys,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        for key in &keys {
            let roots = &roots.with(&keys);
            match self.own_descriptor(host, roots, *id, key.borrowed())? {
                Ok(Some((property, attributes)))
                    if attributes.configurable
                        || (matches!(level, IntegrityLevel::Frozen)
                            && matches!(property, Property::Data(_))
                            && attributes.writable) =>
                {
                    return result(false);
                }
                Ok(_) => {}
                Err(error) => return Ok(Invocation::Thrown(error)),
            }
        }
        result(true)
    }

    pub(crate) fn object_keys(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let keys = match args.first() {
            None | Some(Value::Undefined | Value::Null) => {
                return self.throw_type_error("Object.keys requires a non-nullish value");
            }
            Some(Value::Object(id)) => match self.enumerable_string_keys(host, roots, *id)? {
                Ok(keys) => keys.into_iter().map(Value::String).collect(),
                Err(error) => return Ok(Invocation::Thrown(error)),
            },
            Some(Value::String(text)) => (0..utf16_len(text))
                .map(|index| Value::String(index.to_string().into()))
                .collect(),
            _ => Vec::new(),
        };
        Ok(Invocation::Value(Value::Object(
            self.heap.alloc_array(keys),
        )))
    }

    pub(crate) fn get_own_property_descriptor(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let Some(Value::Object(target)) = args.first() else {
            return self
                .throw_type_error("Object.getOwnPropertyDescriptor requires an object in this VM");
        };
        let key = match self.coerce_property_key(
            host,
            roots,
            args.get(1).unwrap_or(&Value::Undefined),
        )? {
            Ok(key) => key,
            Err(thrown) => return Ok(Invocation::Thrown(thrown)),
        };
        match self.own_descriptor(host, roots, *target, key.borrowed())? {
            Ok(Some((property, attributes))) => Ok(Invocation::Value(Value::Object(
                self.descriptor_object(property, attributes)?,
            ))),
            Ok(None) => Ok(Invocation::Value(Value::Undefined)),
            Err(error) => Ok(Invocation::Thrown(error)),
        }
    }

    pub(crate) fn descriptor_object(
        &mut self,
        property: Property,
        attributes: Attributes,
    ) -> Result<ObjectId, VmError> {
        let result = self.heap.alloc_plain();
        match property {
            Property::Data(value) => {
                self.heap.set_own_string(result, "value", value)?;
                self.heap
                    .set_own_string(result, "writable", Value::Bool(attributes.writable))?;
            }
            Property::Accessor { get, set } => {
                self.heap.set_own_string(
                    result,
                    "get",
                    get.map_or(Value::Undefined, Value::Object),
                )?;
                self.heap.set_own_string(
                    result,
                    "set",
                    set.map_or(Value::Undefined, Value::Object),
                )?;
            }
        }
        self.heap
            .set_own_string(result, "enumerable", Value::Bool(attributes.enumerable))?;
        self.heap
            .set_own_string(result, "configurable", Value::Bool(attributes.configurable))?;
        Ok(result)
    }

    pub(crate) fn get_own_property_descriptors(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let Some(Value::Object(target)) = args.first() else {
            return self.throw_type_error(
                "Object.getOwnPropertyDescriptors requires an object in this VM",
            );
        };
        let keys = match self.own_keys(host, roots, *target, false)? {
            Ok(keys) => keys,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let result = self.heap.alloc_plain();
        for key in &keys {
            let view = (result, &keys);
            let roots = &roots.with(&view);
            let (property, attributes) =
                match self.own_descriptor(host, roots, *target, key.borrowed())? {
                    Ok(Some(descriptor)) => descriptor,
                    Ok(None) => continue,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                };
            let descriptor = self.descriptor_object(property, attributes)?;
            self.heap
                .set_own(result, key.clone(), Value::Object(descriptor))?;
        }
        Ok(Invocation::Value(Value::Object(result)))
    }

    pub(crate) fn define_property_builtin(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
        reflect: bool,
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let Some(Value::Object(target)) = args.first() else {
            return self.throw_type_error(if reflect {
                "Reflect.defineProperty requires an object target"
            } else {
                "Object.defineProperty requires an object target"
            });
        };
        let key = match self.coerce_property_key(
            host,
            roots,
            args.get(1).unwrap_or(&Value::Undefined),
        )? {
            Ok(key) => key,
            Err(thrown) => return Ok(Invocation::Thrown(thrown)),
        };
        let update = match self.read_property_descriptor(
            host,
            &roots.with(&key),
            args.get(2).unwrap_or(&Value::Undefined),
        )? {
            Ok(update) => update,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        if reflect {
            return Ok(
                match self.try_apply_property_update(host, roots, *target, key, update)? {
                    Ok(applied) => Invocation::Value(Value::Bool(applied)),
                    Err(error) => Invocation::Thrown(error),
                },
            );
        }
        self.apply_property_update(host, roots, *target, key, update)
    }
    pub(crate) fn object_assign(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let Some(Value::Object(target)) = args.first() else {
            return self.throw_type_error("Object.assign requires an object target");
        };
        let target = *target;
        for source in args.iter().skip(1) {
            let Value::Object(source_id) = source else {
                // Primitive sources currently have no enumerable own keys.
                continue;
            };
            let keys = match self.own_keys(host, roots, *source_id, false)? {
                Ok(keys) => keys,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
            for key in &keys {
                let roots = &roots.with(&keys);
                match self.own_descriptor(host, roots, *source_id, key.borrowed())? {
                    Ok(Some((_, attributes))) if attributes.enumerable => {}
                    Ok(_) => continue,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                }
                let value = match self.read_property(host, roots, source, key.borrowed())? {
                    Invocation::Value(value) => value,
                    thrown @ Invocation::Thrown(_) => return Ok(thrown),
                };
                let invocation = self.set_property(
                    host,
                    roots,
                    target,
                    &Value::Object(target),
                    key.clone(),
                    value,
                    true,
                )?;
                if let thrown @ Invocation::Thrown(_) = invocation {
                    return Ok(thrown);
                }
            }
        }
        Ok(Invocation::Value(Value::Object(target)))
    }

    pub(crate) fn define_properties(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        target: ObjectId,
        properties: Option<&Value>,
    ) -> Result<Invocation, VmError> {
        let root_view = (target, properties);
        let roots = &roots.with(&root_view);
        let (properties, source) = match properties {
            Some(properties @ Value::Object(source)) => (properties, *source),
            None | Some(Value::Undefined | Value::Null) => {
                return self
                    .throw_type_error("Object.defineProperties requires non-nullish properties");
            }
            Some(Value::String(text)) if !text.is_empty() => {
                // A boxed nonempty string has enumerable indices whose values are strings.
                return self.throw_type_error("property descriptor must be an object");
            }
            // The other primitive wrappers have no enumerable own properties.
            _ => return Ok(Invocation::Value(Value::Object(target))),
        };
        let keys = match self.own_keys(host, roots, source, false)? {
            Ok(keys) => keys,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let mut updates = Vec::new();
        let roots = &roots.with(&keys);
        for key in &keys {
            let roots = &roots.with(&updates);
            // Getters can change the enumerability of keys still to be visited.
            match self.own_descriptor(host, roots, source, key.borrowed())? {
                Ok(Some((_, attributes))) if attributes.enumerable => {}
                Ok(_) => continue,
                Err(error) => return Ok(Invocation::Thrown(error)),
            }
            let descriptor = match self.read_property(host, roots, properties, key.borrowed())? {
                Invocation::Value(value) => value,
                thrown => return Ok(thrown),
            };
            match self.read_property_descriptor(host, roots, &descriptor)? {
                Ok(update) => updates.push((key.clone(), update)),
                Err(error) => return Ok(Invocation::Thrown(error)),
            }
        }
        // Conversion errors precede all definitions; application errors keep earlier writes.
        updates.reverse();
        while let Some((key, update)) = updates.pop() {
            let roots = &roots.with(&updates);
            if let thrown @ Invocation::Thrown(_) =
                self.apply_property_update(host, roots, target, key, update)?
            {
                return Ok(thrown);
            }
        }
        Ok(Invocation::Value(Value::Object(target)))
    }

    pub(crate) fn read_property_descriptor(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        descriptor: &Value,
    ) -> Result<Result<DescriptorUpdate, Value>, VmError> {
        let roots = &roots.with(&descriptor);
        let Value::Object(id) = *descriptor else {
            return Ok(Err(
                self.type_error("property descriptor must be an object")?
            ));
        };
        let mut update = DescriptorUpdate::default();
        // ToPropertyDescriptor observes inherited fields, and checks/reads each
        // in order: an earlier getter can affect the fields read afterward.
        for name in [
            "enumerable",
            "configurable",
            "value",
            "writable",
            "get",
            "set",
        ] {
            let roots = &roots.with(&update);
            let key = PropertyKeyRef::String(name);
            match self.has_property_key(host, roots, id, key)? {
                Invocation::Value(value) if value.truthy() => {}
                Invocation::Value(_) => continue,
                Invocation::Thrown(error) => return Ok(Err(error)),
            }
            let value = match self.read_property(host, roots, descriptor, key)? {
                Invocation::Value(value) => value,
                Invocation::Thrown(error) => return Ok(Err(error)),
            };
            match name {
                "enumerable" => update.enumerable = Some(value.truthy()),
                "configurable" => update.configurable = Some(value.truthy()),
                "value" => update.value = Some(value),
                "writable" => update.writable = Some(value.truthy()),
                "get" | "set" => {
                    let handler = match value {
                        Value::Undefined => None,
                        Value::Object(id) if self.is_callable(&value) => Some(id),
                        _ => {
                            return Ok(Err(self.type_error(
                                "descriptor accessor must be callable or undefined",
                            )?));
                        }
                    };
                    if name == "get" {
                        update.get = Some(handler);
                    } else {
                        update.set = Some(handler);
                    }
                }
                _ => unreachable!(),
            }
        }
        if update.is_data() && update.is_accessor() {
            return Ok(Err(
                self.type_error("descriptor cannot mix data and accessor fields")?
            ));
        }
        Ok(Ok(update))
    }

    pub(crate) fn apply_property_update(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        target: ObjectId,
        key: PropertyKey,
        update: DescriptorUpdate,
    ) -> Result<Invocation, VmError> {
        match self.try_apply_property_update(host, &roots.with(&target), target, key, update)? {
            Ok(true) => Ok(Invocation::Value(Value::Object(target))),
            Ok(false) => self.throw_type_error("Cannot define property"),
            Err(error) => Ok(Invocation::Thrown(error)),
        }
    }

    pub(crate) fn try_apply_property_update(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        target: ObjectId,
        key: PropertyKey,
        mut update: DescriptorUpdate,
    ) -> Result<Result<bool, Value>, VmError> {
        let target = self.heap.proxy_target(target)?;
        let roots = &roots.with(&target);
        if self.has_own_function_metadata(target, key.borrowed())? {
            return Ok(Ok(false));
        }
        if key.borrowed().is_str("length")
            && matches!(self.heap.get(target)?, Object::Array(_))
            && let Some(value) = &mut update.value
        {
            // ArraySetLength performs ToUint32 and then a separate ToNumber.
            // Each may call script and observe changes made by the previous one.
            let length = match self.coerce_number(host, roots, value)? {
                Ok(number) => ops::number_to_uint32(number),
                Err(error) => return Ok(Err(error)),
            };
            let number = match self.coerce_number(host, roots, value)? {
                Ok(number) => number,
                Err(error) => return Ok(Err(error)),
            };
            if f64::from(length) != number {
                return Ok(Err(self.make_range_error("Invalid array length")?));
            }
            *value = Value::Number(f64::from(length));
        }
        Ok(Ok(self.heap.apply_descriptor(target, key, update)?))
    }

    pub(crate) fn construct_error(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        name: &str,
        prototype: &'static str,
        args: Vec<Value>,
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let prototype = self.builtins.get(prototype);
        let (errors, message_arg) = if name == "AggregateError" {
            let iterable = args.first().unwrap_or(&Value::Undefined);
            let mut errors = Vec::new();
            if !matches!(iterable, Value::Undefined) {
                let (iterator, next) = match self.get_iterator(host, roots, iterable)? {
                    Ok(iterator) => iterator,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                };
                loop {
                    match self.iterator_step(
                        host,
                        &roots.with(&(&iterator, &next, &errors)),
                        &iterator,
                        &next,
                    )? {
                        Ok(Some(value)) => errors.push(value),
                        Ok(None) => break,
                        Err(error) => return Ok(Invocation::Thrown(error)),
                    }
                }
            }
            (Some(errors), args.get(1))
        } else {
            (None, args.first())
        };
        let roots = &roots.with(&errors);
        let message = match message_arg {
            None | Some(Value::Undefined) => None,
            Some(value) => match self.coerce_string(host, roots, value)? {
                Ok(message) => Some(message),
                Err(error) => return Ok(Invocation::Thrown(error)),
            },
        };
        let error = self.alloc_error(prototype, name, message)?;
        if let (Some(errors), Value::Object(id)) = (errors, &error) {
            let errors = self.heap.alloc_array(errors);
            self.heap
                .define_data(*id, "errors", Value::Object(errors), Attributes::METHOD)?;
        }
        Ok(Invocation::Value(error))
    }

    pub(crate) fn alloc_error(
        &mut self,
        prototype: ObjectId,
        name: &str,
        message: Option<JsString>,
    ) -> Result<Value, VmError> {
        let mut object = PlainObject::new(Some(prototype));
        object.is_error = true;
        let id = self.heap.alloc(Object::Plain(object));
        let mut stack = JsString::from(format!("{name}: "));
        if let Some(message) = message {
            stack.push_str(&message);
            self.heap
                .define_data(id, "message", Value::String(message), Attributes::METHOD)?;
        }
        self.heap
            .define_data(id, "stack", Value::String(stack), Attributes::METHOD)?;
        Ok(Value::Object(id))
    }
    pub(crate) fn alloc_aggregate_error(&mut self, errors: Vec<Value>) -> Result<Value, VmError> {
        let error = self.alloc_error(
            self.builtins.get("aggregateerror.prototype"),
            "AggregateError",
            Some("All promises were rejected".into()),
        )?;
        let Value::Object(id) = error else {
            unreachable!()
        };
        let errors = self.heap.alloc_array(errors);
        self.heap
            .define_data(id, "errors", Value::Object(errors), Attributes::METHOD)?;
        Ok(Value::Object(id))
    }

    pub(crate) fn construct_promise(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: Vec<Value>,
        prototype: Option<ObjectId>,
    ) -> Result<Invocation, VmError> {
        let executor = args.first().cloned().unwrap_or(Value::Undefined);
        if !self.is_callable(&executor) {
            return self.throw_type_error("Promise executor is not a function");
        }
        let promise = self.heap.alloc_promise();
        if let Some(prototype) = prototype {
            let accepted = self.heap.set_prototype(promise, Some(prototype))?;
            debug_assert!(accepted, "fresh Promise accepts its prototype");
        }
        let (resolve_id, reject_id) = self.alloc_capability_pair(promise)?;
        let root_view = (promise, reject_id);
        let roots = &roots.with(&root_view);
        let invocation = self.invoke(
            host,
            roots,
            &executor,
            vec![Value::Object(resolve_id), Value::Object(reject_id)],
            &Value::Undefined,
        )?;
        if let Invocation::Thrown(error) = invocation {
            self.capability_call(host, roots, reject_id, error)?;
        }
        Ok(Invocation::Value(Value::Object(promise)))
    }

    pub(crate) fn symbol_static(
        &mut self,
        receiver: &Value,
        call: impl FnOnce(&mut Self, Vec<Value>) -> Result<Invocation, VmError>,
        args: Vec<Value>,
    ) -> Result<Invocation, VmError> {
        if !matches!(receiver, Value::Object(id) if *id == self.builtins.get("symbol")) {
            return self.throw_type_error("incompatible Symbol receiver");
        }
        call(self, args)
    }

    pub(crate) fn symbol_method(
        &mut self,
        receiver: &Value,
        method: &str,
    ) -> Result<Invocation, VmError> {
        let id = match receiver {
            Value::Symbol(id) => *id,
            Value::Object(id) => match self.heap.get(*id)? {
                Object::Boxed {
                    primitive: BoxedPrimitive::Symbol(id),
                    ..
                } => *id,
                _ => return self.throw_type_error("incompatible Symbol receiver"),
            },
            _ => return self.throw_type_error("incompatible Symbol receiver"),
        };
        let description = self.heap.symbols.get(&id).and_then(Option::as_ref);
        Ok(Invocation::Value(match method {
            "valueOf" => Value::Symbol(id),
            "toString" => {
                let mut text = JsString::from("Symbol(");
                if let Some(description) = description {
                    text.push_str(description);
                }
                text.push(')');
                Value::String(text)
            }
            "get description" => description.cloned().map_or(Value::Undefined, Value::String),
            _ => unreachable!("unknown Symbol method"),
        }))
    }

    /// `Symbol(desc)`: every call produces a fresh identity.
    pub(crate) fn symbol_constructor(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: Vec<Value>,
    ) -> Result<Invocation, VmError> {
        let description = match args.first() {
            None | Some(Value::Undefined) => None,
            Some(value) => match self.coerce_string(host, roots, value)? {
                Ok(description) => Some(description),
                Err(error) => return Ok(Invocation::Thrown(error)),
            },
        };
        let id = self.heap.alloc_symbol(description)?;
        Ok(Invocation::Value(Value::Symbol(id)))
    }

    pub(crate) fn symbol_for(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: Vec<Value>,
    ) -> Result<Invocation, VmError> {
        let key =
            match self.coerce_string(host, roots, args.first().unwrap_or(&Value::Undefined))? {
                Ok(key) => key,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
        let id = self.heap.intern_symbol(key)?;
        Ok(Invocation::Value(Value::Symbol(id)))
    }

    pub(crate) fn symbol_key_for(&mut self, args: Vec<Value>) -> Result<Invocation, VmError> {
        let Some(Value::Symbol(id)) = args.first() else {
            return self.throw_type_error("Symbol.keyFor requires a Symbol");
        };
        let key = self
            .heap
            .symbol_registry
            .iter()
            .find_map(|(key, registered)| (*registered == *id).then(|| key.clone()));
        Ok(Invocation::Value(match key {
            Some(key) => Value::String(key),
            None => Value::Undefined,
        }))
    }

    pub(crate) fn queue_microtask(&mut self, args: Vec<Value>) -> Result<Invocation, VmError> {
        let function = args.first().cloned().unwrap_or(Value::Undefined);
        if !self.is_callable(&function) {
            return self.throw_type_error("queueMicrotask argument is not a function");
        }
        self.microtasks.push_back(Microtask::Call {
            function,
            args: Vec::new(),
        });
        Ok(Invocation::Value(Value::Undefined))
    }

    pub(crate) fn is_intrinsic_promise_then(&self, value: &Value) -> bool {
        static THEN_ID: std::sync::LazyLock<crate::intrinsic::IntrinsicId> =
            std::sync::LazyLock::new(|| THEN.link());
        matches!(value, Value::Object(id) if matches!(self.heap.get(*id),
            Ok(Object::Native(native))
                if native.target() == CallableTarget::Intrinsic(*THEN_ID)))
    }

    pub(crate) fn call_promise_static(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        name: &str,
        args: Vec<Value>,
    ) -> Result<Invocation, VmError> {
        match name {
            "resolve" => {
                let value = args.first().cloned().unwrap_or(Value::Undefined);
                match self.promise_identity(host, roots, &value)? {
                    Ok(Some(id)) => return Ok(Invocation::Value(Value::Object(id))),
                    Ok(None) => {}
                    Err(error) => return Ok(Invocation::Thrown(error)),
                }
                let promise = self.heap.alloc_promise();
                self.resolve_promise(host, roots, promise, value)?;
                Ok(Invocation::Value(Value::Object(promise)))
            }
            "reject" => {
                let reason = args.first().cloned().unwrap_or(Value::Undefined);
                let promise = self.heap.alloc_promise();
                self.settle(promise, Settlement::Rejected(reason))?;
                Ok(Invocation::Value(Value::Object(promise)))
            }
            "all" => self.combine(host, roots, receiver, CombineKind::All, args),
            "allSettled" => self.combine(host, roots, receiver, CombineKind::AllSettled, args),
            "race" => self.combine(host, roots, receiver, CombineKind::Race, args),
            "any" => self.combine(host, roots, receiver, CombineKind::Any, args),
            _ => self.throw_type_error(&format!("Promise.{name} is not a function")),
        }
    }

    pub(crate) fn call_promise_method(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        receiver: ObjectId,
        name: &str,
        args: Vec<Value>,
    ) -> Result<Invocation, VmError> {
        let this = Value::Object(receiver);
        let roots = &roots.with(&receiver);
        match name {
            "catch" => self.invoke_promise_then(
                host,
                roots,
                &this,
                vec![
                    Value::Undefined,
                    args.into_iter().next().unwrap_or(Value::Undefined),
                ],
            ),
            "then" | "finally" => {
                let species = match self.promise_species(host, &roots.with(&args), &this)? {
                    Invocation::Value(value) => value,
                    thrown => return Ok(thrown),
                };
                if name == "finally" {
                    let handler = args.into_iter().next().unwrap_or(Value::Undefined);
                    let handlers = if self.is_callable(&handler) {
                        [false, true]
                            .into_iter()
                            .map(|rejected| {
                                self.alloc_promise_callback(PromiseCallback::Finally {
                                    handler: handler.clone(),
                                    species: species.clone(),
                                    rejected,
                                })
                            })
                            .collect()
                    } else {
                        vec![handler.clone(), handler]
                    };
                    return self.invoke_promise_then(host, roots, &this, handlers);
                }
                let (result, resolve, reject) =
                    match self.new_promise_capability(host, &roots.with(&args), &species)? {
                        Ok(capability) => capability,
                        Err(error) => return Ok(Invocation::Thrown(error)),
                    };
                self.react(
                    receiver,
                    ReactionJob::Script {
                        resolve,
                        reject,
                        on_fulfilled: self.handler_arg(args.first()),
                        on_rejected: self.handler_arg(args.get(1)),
                    },
                )?;
                Ok(Invocation::Value(result))
            }
            _ => self.throw_type_error(&format!("Promise.{name} is not a function")),
        }
    }

    pub(crate) fn invoke_promise_then(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        args: Vec<Value>,
    ) -> Result<Invocation, VmError> {
        let result = self.read_property(
            host,
            &roots.with(&(&args, receiver)),
            receiver,
            PropertyKeyRef::String("then"),
        )?;
        match result {
            Invocation::Value(then) => self.invoke(host, roots, &then, args, receiver),
            thrown => Ok(thrown),
        }
    }

    pub(crate) fn promise_species(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        let intrinsic = Value::Object(self.builtins.get("promise"));
        let constructor = match self.read_property(
            host,
            roots,
            receiver,
            PropertyKeyRef::String("constructor"),
        )? {
            Invocation::Value(Value::Undefined) => return Ok(Invocation::Value(intrinsic)),
            Invocation::Value(value @ Value::Object(_)) => value,
            Invocation::Value(_) => {
                return self.throw_type_error("Promise constructor is not an object");
            }
            thrown => return Ok(thrown),
        };
        match self.read_property(
            host,
            roots,
            &constructor,
            PropertyKeyRef::Symbol(SPECIES_SYMBOL),
        )? {
            Invocation::Value(Value::Undefined | Value::Null) => Ok(Invocation::Value(intrinsic)),
            Invocation::Value(value) if !self.is_constructor(&value)? => {
                self.throw_type_error("Promise species is not a constructor")
            }
            other => Ok(other),
        }
    }

    pub(crate) fn new_promise_capability(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        species: &Value,
    ) -> Result<Result<(Value, Value, Value), Value>, VmError> {
        if *species == Value::Object(self.builtins.get("promise")) {
            let result = self.heap.alloc_promise();
            let (resolve, reject) = self.alloc_resolution_pair(result, false, false)?;
            return Ok(Ok((
                Value::Object(result),
                Value::Object(resolve),
                Value::Object(reject),
            )));
        }
        let executor = self.alloc_promise_callback(PromiseCallback::Executor(None));
        let Value::Object(id) = executor else {
            unreachable!()
        };
        let roots = &roots.with(&id);
        let result = match self.construct_with(host, roots, species, vec![executor], species)? {
            Invocation::Value(value) => value,
            Invocation::Thrown(error) => return Ok(Err(error)),
        };
        if let Object::PromiseCallback {
            callback: PromiseCallback::Executor(Some((resolve, reject))),
            ..
        } = self.heap.get(id)?
            && matches!(result, Value::Object(_))
            && self.is_callable(resolve)
            && self.is_callable(reject)
        {
            return Ok(Ok((result, resolve.clone(), reject.clone())));
        }
        Ok(Err(self.type_error("invalid Promise species capability")?))
    }

    pub(crate) fn alloc_promise_callback(&mut self, callback: PromiseCallback) -> Value {
        Value::Object(self.heap.alloc(Object::PromiseCallback {
            object: PlainObject::new(self.heap.function_prototype),
            callback,
        }))
    }

    pub(crate) fn call_promise_callback(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        args: Vec<Value>,
    ) -> Result<Invocation, VmError> {
        let mut args = args.into_iter();
        let argument = args.next().unwrap_or(Value::Undefined);
        let Object::PromiseCallback { callback, .. } = self.heap.get_mut(id)? else {
            unreachable!()
        };
        match callback {
            PromiseCallback::Executor(values) => {
                if values.as_ref().is_some_and(|(resolve, reject)| {
                    *resolve != Value::Undefined || *reject != Value::Undefined
                }) {
                    return self
                        .throw_type_error("Promise capability executor already initialized");
                }
                *values = Some((argument, args.next().unwrap_or(Value::Undefined)));
                Ok(Invocation::Value(Value::Undefined))
            }
            PromiseCallback::Restore { value, rejected } => Ok(if *rejected {
                Invocation::Thrown(value.clone())
            } else {
                Invocation::Value(value.clone())
            }),
            PromiseCallback::Finally {
                handler,
                species,
                rejected,
            } => {
                let (handler, species, rejected) = (handler.clone(), species.clone(), *rejected);
                let parent_roots = roots;
                let root_view = (&species, &argument);
                let roots = &roots.with(&root_view);
                let value =
                    match self.invoke(host, roots, &handler, Vec::new(), &Value::Undefined)? {
                        Invocation::Value(value) => value,
                        thrown => return Ok(thrown),
                    };
                let cleanup = match self.promise_resolve_with(host, roots, &species, value)? {
                    Invocation::Value(value) => value,
                    thrown => return Ok(thrown),
                };
                let restore = self.alloc_promise_callback(PromiseCallback::Restore {
                    value: argument,
                    rejected,
                });
                self.invoke_promise_then(host, parent_roots, &cleanup, vec![restore])
            }
        }
    }

    pub(crate) fn promise_resolve_with(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        species: &Value,
        value: Value,
    ) -> Result<Invocation, VmError> {
        if matches!(&value, Value::Object(id) if self.heap.promise(*id).is_ok()) {
            match self.read_property(host, roots, &value, PropertyKeyRef::String("constructor"))? {
                Invocation::Value(constructor) if constructor == *species => {
                    return Ok(Invocation::Value(value));
                }
                Invocation::Value(_) => {}
                thrown => return Ok(thrown),
            }
        }
        let (result, resolve, _) =
            match self.new_promise_capability(host, &roots.with(&value), species)? {
                Ok(capability) => capability,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
        match self.invoke(
            host,
            &roots.with(&result),
            &resolve,
            vec![value],
            &Value::Undefined,
        )? {
            Invocation::Value(_) => Ok(Invocation::Value(result)),
            thrown => Ok(thrown),
        }
    }

    pub(crate) fn handler_arg(&self, value: Option<&Value>) -> Option<Value> {
        value.filter(|value| self.is_callable(value)).cloned()
    }

    pub(crate) fn combine(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        kind: CombineKind,
        args: Vec<Value>,
    ) -> Result<Invocation, VmError> {
        let root_view = (&args, receiver);
        let roots = &roots.with(&root_view);
        let iterable = args.first().unwrap_or(&Value::Undefined);
        let result = self.heap.alloc_promise();
        let roots = &roots.with(&result);
        let resolve =
            match self.read_property(host, roots, receiver, PropertyKeyRef::String("resolve"))? {
                Invocation::Value(resolve) if self.is_callable(&resolve) => resolve,
                invocation => {
                    let reason = match invocation {
                        Invocation::Thrown(reason) => reason,
                        Invocation::Value(_) => {
                            self.type_error("Promise.resolve is not callable")?
                        }
                    };
                    self.settle(result, Settlement::Rejected(reason))?;
                    return Ok(Invocation::Value(Value::Object(result)));
                }
            };
        let roots = &roots.with(&resolve);
        let (iterator, next) = match self.get_iterator(host, roots, iterable)? {
            Ok(record) => record,
            Err(error) => {
                self.settle(result, Settlement::Rejected(error))?;
                return Ok(Invocation::Value(Value::Object(result)));
            }
        };
        let root_view = (&iterator, &next);
        let roots = &roots.with(&root_view);
        let mut state = CombineState {
            kind,
            count: 0,
            remaining: 0,
            results: Vec::new(),
            states: Vec::new(),
            first: None,
            elements: Vec::new(),
        };
        loop {
            let roots = &roots.with(&state.elements);
            let element = match self.iterator_step(host, roots, &iterator, &next)? {
                Ok(Some(element)) => element,
                Ok(None) => break,
                Err(error) => {
                    self.settle(result, Settlement::Rejected(error))?;
                    return Ok(Invocation::Value(Value::Object(result)));
                }
            };
            let resolved = match self.invoke(host, roots, &resolve, vec![element], receiver)? {
                Invocation::Value(value) => value,
                Invocation::Thrown(error) => {
                    let _ = self.close_iterator(host, &roots.with(&error), &iterator)?;
                    self.settle(result, Settlement::Rejected(error))?;
                    return Ok(Invocation::Value(Value::Object(result)));
                }
            };
            let index = state.elements.len();
            let slot = self.heap.alloc_promise();
            self.react(
                slot,
                ReactionJob::Combine {
                    result,
                    index,
                    kind,
                },
            )?;
            let (fulfill, reject) = self.alloc_resolution_pair(slot, true, false)?;
            let root_view = (slot, fulfill, reject);
            let roots = &roots.with(&root_view);
            if let Invocation::Thrown(error) = self.invoke_promise_then(
                host,
                roots,
                &resolved,
                vec![Value::Object(fulfill), Value::Object(reject)],
            )? {
                let _ = self.close_iterator(host, &roots.with(&error), &iterator)?;
                self.mark_resolution_called(fulfill)?;
                self.mark_resolution_called(reject)?;
                self.settle(result, Settlement::Rejected(error))?;
                return Ok(Invocation::Value(Value::Object(result)));
            }
            state.elements.push(slot);
            state.results.push(Value::Undefined);
            if kind == CombineKind::AllSettled {
                state.states.push(PromiseState::Pending);
            }
        }
        if state.elements.is_empty() {
            match kind {
                CombineKind::All | CombineKind::AllSettled => {
                    let array = self.heap.alloc_array(Vec::new());
                    self.settle(result, Settlement::Fulfilled(Value::Object(array)))?;
                }
                CombineKind::Race => {}
                CombineKind::Any => {
                    let error = self.alloc_aggregate_error(Vec::new())?;
                    self.settle(result, Settlement::Rejected(error))?;
                }
            }
        } else {
            // Reactions cannot run until this synchronous builtin returns.
            state.count = state.elements.len();
            state.remaining = state.count;
            self.heap.combines.insert(result, state);
        }
        Ok(Invocation::Value(Value::Object(result)))
    }

    pub(crate) fn alloc_capability_pair(
        &mut self,
        promise: ObjectId,
    ) -> Result<(ObjectId, ObjectId), VmError> {
        self.alloc_resolution_pair(promise, false, true)
    }

    pub(crate) fn alloc_resolution_pair(
        &mut self,
        promise: ObjectId,
        direct: bool,
        track_unreachable: bool,
    ) -> Result<(ObjectId, ObjectId), VmError> {
        let resolve_id = self.heap.alloc(Object::Resolution(Resolution {
            promise,
            pair: 0,
            resolve: true,
            direct,
            track_unreachable,
            called: false,
            object: PlainObject::new(self.heap.function_prototype),
        }));
        let reject_id = self.heap.alloc(Object::Resolution(Resolution {
            promise,
            pair: resolve_id,
            resolve: false,
            direct,
            track_unreachable,
            called: false,
            object: PlainObject::new(self.heap.function_prototype),
        }));
        match self.heap.get_mut(resolve_id)? {
            Object::Resolution(resolution) => resolution.pair = reject_id,
            _ => {
                return Err(VmError::InvalidState(
                    "capability allocation did not produce a Resolution".to_owned(),
                ));
            }
        }
        if track_unreachable {
            self.heap.resolutions.insert(resolve_id);
            self.heap.resolutions.insert(reject_id);
        }
        Ok((resolve_id, reject_id))
    }

    pub(crate) fn capability_call(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        id: ObjectId,
        value: Value,
    ) -> Result<(), VmError> {
        let (promise, pair, resolve, direct, called) = match self.heap.get(id)? {
            Object::Resolution(resolution) => (
                resolution.promise,
                resolution.pair,
                resolution.resolve,
                resolution.direct,
                resolution.called,
            ),
            _ => {
                return Err(VmError::InvalidState(format!(
                    "object {id} is not a resolution function"
                )));
            }
        };
        if called || self.resolution_called(pair)? {
            return Ok(());
        }
        self.mark_resolution_called(id)?;
        self.mark_resolution_called(pair)?;
        if resolve && direct {
            self.settle(promise, Settlement::Fulfilled(value))
        } else if resolve {
            self.resolve_promise(host, roots, promise, value)
        } else {
            self.reject_if_open(promise, value)
        }
    }

    pub(crate) fn resolution_called(&self, id: ObjectId) -> Result<bool, VmError> {
        match self.heap.get(id)? {
            Object::Resolution(resolution) => Ok(resolution.called),
            _ => Err(VmError::InvalidState(format!(
                "object {id} is not a resolution function"
            ))),
        }
    }

    pub(crate) fn mark_resolution_called(&mut self, id: ObjectId) -> Result<(), VmError> {
        if let Object::Resolution(resolution) = self.heap.get_mut(id)? {
            resolution.called = true;
        }
        self.heap.resolutions.remove(&id);
        Ok(())
    }

    pub(crate) fn is_open(&self, promise: ObjectId) -> Result<bool, VmError> {
        let promise = self.heap.promise(promise)?;
        Ok(!promise.locked && matches!(promise.state, PromiseState::Pending))
    }

    pub(crate) fn reject_if_open(
        &mut self,
        promise: ObjectId,
        reason: Value,
    ) -> Result<(), VmError> {
        if self.is_open(promise)? {
            self.settle(promise, Settlement::Rejected(reason))?;
        }
        Ok(())
    }

    pub(crate) fn run_call_handler(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        handler: &Option<Value>,
        argument: &Value,
        capability: &(Value, Value),
        rejected: bool,
    ) -> Result<(), VmError> {
        let (resolve, reject) = capability;
        let (callback, value) = match handler {
            Some(handler) => {
                match self.invoke(
                    host,
                    roots,
                    handler,
                    vec![argument.clone()],
                    &Value::Undefined,
                )? {
                    Invocation::Value(value) => (resolve, value),
                    Invocation::Thrown(error) => (reject, error),
                }
            }
            None if rejected => (reject, argument.clone()),
            None => (resolve, argument.clone()),
        };
        let _ = self.invoke(host, roots, callback, vec![value], &Value::Undefined)?;
        Ok(())
    }

    pub(crate) fn run_queue_call(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        function: &Value,
        args: &[Value],
    ) -> Result<(), VmError> {
        if let Invocation::Thrown(error) =
            self.invoke(host, roots, function, args.to_vec(), &Value::Undefined)?
        {
            let error = self.host_value(error);
            host.report_error(error);
        }
        Ok(())
    }

    pub(crate) fn run_combine(
        &mut self,
        result: ObjectId,
        index: usize,
        kind: CombineKind,
        settlement: Settlement,
    ) -> Result<(), VmError> {
        let Some(mut state) = self.heap.combines.get(&result).cloned() else {
            return Ok(());
        };
        if index >= state.count {
            return Err(VmError::InvalidState(format!(
                "combine reaction index {index} is outside {}",
                state.count
            )));
        }
        state.remaining = state.remaining.saturating_sub(1);
        let pending = matches!(self.heap.promise(result)?.state, PromiseState::Pending);
        match kind {
            CombineKind::Race => {
                self.heap.combines.remove(&result);
                if pending {
                    self.settle(result, settlement)?;
                }
                return Ok(());
            }
            CombineKind::All => match settlement {
                Settlement::Rejected(reason) => {
                    self.heap.combines.remove(&result);
                    if pending {
                        self.settle(result, Settlement::Rejected(reason))?;
                    }
                    return Ok(());
                }
                Settlement::Fulfilled(value) => {
                    state.results[index] = value;
                    if state.remaining == 0 && pending {
                        let values = state.results.clone();
                        self.heap.combines.remove(&result);
                        let array = self.heap.alloc_array(values);
                        self.settle(result, Settlement::Fulfilled(Value::Object(array)))?;
                        return Ok(());
                    }
                }
            },
            CombineKind::AllSettled => {
                state.states[index] = settlement.to_state();
                state.results[index] = settlement_value(&settlement);
                if state.remaining == 0 && pending {
                    let records = state
                        .states
                        .iter()
                        .map(record_from_state)
                        .collect::<Vec<_>>();
                    self.heap.combines.remove(&result);
                    let mut objects = Vec::with_capacity(records.len());
                    for (status, payload) in records {
                        objects.push(self.settlement_record(status, payload)?);
                    }
                    let array = self.heap.alloc_array(objects);
                    self.settle(result, Settlement::Fulfilled(Value::Object(array)))?;
                    return Ok(());
                }
            }
            CombineKind::Any => match settlement {
                Settlement::Fulfilled(value) => {
                    self.heap.combines.remove(&result);
                    if pending {
                        self.settle(result, Settlement::Fulfilled(value))?;
                    }
                    return Ok(());
                }
                Settlement::Rejected(reason) => {
                    state.results[index] = reason;
                    if state.remaining == 0 && pending {
                        self.heap.combines.remove(&result);
                        let error = self.alloc_aggregate_error(state.results)?;
                        self.settle(result, Settlement::Rejected(error))?;
                        return Ok(());
                    }
                }
            },
        }
        self.heap.combines.insert(result, state);
        Ok(())
    }

    pub(crate) fn settlement_record(
        &mut self,
        status: &str,
        payload: (&str, Value),
    ) -> Result<Value, VmError> {
        let id = self.heap.alloc_plain();
        self.heap
            .set_own_string(id, "status", Value::String(status.into()))?;
        self.heap.set_own_string(id, payload.0, payload.1)?;
        Ok(Value::Object(id))
    }
}

pub(crate) fn is_callable_object(object: &Object) -> bool {
    matches!(
        object,
        Object::Function(_)
            | Object::Resolution(_)
            | Object::Bound(_)
            | Object::PromiseCallback { .. }
            | Object::Native(_)
    )
}

impl TraceRoots for DescriptorUpdate {
    fn trace(&self, tracer: &mut Tracer<'_>) -> Result<(), VmError> {
        (&self.value, self.get, self.set).trace(tracer)
    }
}

impl TraceRoots for Property {
    fn trace(&self, tracer: &mut Tracer<'_>) -> Result<(), VmError> {
        match self {
            Property::Data(value) => value.trace(tracer),
            Property::Accessor { get, set } => (*get, *set).trace(tracer),
        }
    }
}

impl TraceRoots for Invocation {
    fn trace(&self, tracer: &mut Tracer<'_>) -> Result<(), VmError> {
        match self {
            Invocation::Value(value) | Invocation::Thrown(value) => value.trace(tracer),
        }
    }
}

fn settlement_value(settlement: &Settlement) -> Value {
    match settlement {
        Settlement::Fulfilled(value) | Settlement::Rejected(value) => value.clone(),
    }
}

fn record_from_state(state: &PromiseState) -> (&'static str, (&'static str, Value)) {
    match state {
        PromiseState::Fulfilled(value) => ("fulfilled", ("value", value.clone())),
        PromiseState::Rejected(value) => ("rejected", ("reason", value.clone())),
        PromiseState::Pending => ("pending", ("value", Value::Undefined)),
    }
}
