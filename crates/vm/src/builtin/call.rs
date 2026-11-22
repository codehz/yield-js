static UNDEFINED_VALUE: Value = Value::UNDEFINED;
use crate::builtin::promise::members::THEN;
use crate::builtin::string::{utf16_len, utf16_unit_string};
use crate::builtin::symbol_family;
use crate::error::VmError;
use crate::gc::{ActiveRoots, TraceRoots, Tracer};
use crate::heap::{
    Attributes, BoundObject, BoxedPrimitive, CombineKind, CombineState, DescriptorUpdate, Heap,
    ITERATOR_SYMBOL, IntegrityLevel, Object, OwnProperty, PlainObject, PromiseCallback,
    PromiseState, Property, ReactionJob, Resolution, SPECIES_SYMBOL, TO_STRING_TAG_SYMBOL,
    string_own_property,
};
use crate::host::Host;
use crate::intrinsic::{CallableTarget, Construction};
use crate::ops;
use crate::runtime::{Invocation, Microtask, Settlement};
use crate::value::{JsString, ObjectId, PropertyKey, PropertyKeyRef, Value, ValueRef};
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
                if matches!(callee.view(), ValueRef::Object(id) if self.heap.function(id).is_some_and(|f| f.class.is_some()))
                {
                    return self.throw_type_error("class constructor requires new");
                }
                let (callee_id, home) = match callee.view() {
                    ValueRef::Object(id) => (Some(id), self.heap.function(id).and_then(|f| f.home)),
                    _ => (None, None),
                };
                self.call_function(
                    host,
                    roots,
                    func_id,
                    Some(env),
                    args,
                    receiver,
                    (&Value::UNDEFINED, home),
                    callee_id,
                )
            }
            Some(Dispatch::Resolution(id)) => {
                self.capability_call(
                    host,
                    roots,
                    id,
                    args.into_iter().next().unwrap_or(Value::UNDEFINED),
                )?;
                Ok(Invocation::Value(Value::UNDEFINED))
            }
            Some(Dispatch::PromiseCallback(id)) => {
                self.call_promise_callback(host, roots, id, args)
            }
            Some(Dispatch::Bound(bound)) => {
                let target = Value::object(bound.target);
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
            Some(Dispatch::Script { func_id, env }) => {
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
        let dispatch = match self.classify(value)? {
            Some(Dispatch::Bound(bound)) => self.classify(&Value::object(bound.target))?,
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
            let target = Value::object(bound.target);
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
                    self.derived_this(context)
                }
                Invocation::Value(value) if !matches!(value.view(), ValueRef::Object(_)) => self
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
                match id.definition().construct.expect("classified constructor") {
                    Construction::WithPrototype(construct) => {
                        let roots = &roots.with(&prototype);
                        construct(self, host, roots, args, prototype)
                    }
                    Construction::WithNewTarget(construct) => {
                        construct(self, host, roots, args, new_target)
                    }
                }
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

    pub(crate) fn super_construct(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        context: ObjectId,
        callee: &Value,
        args: Vec<Value>,
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
            Invocation::Value(value) => {
                let ValueRef::Object(id) = value.view() else {
                    return self.throw_type_error("super constructor must return an object");
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
                    self.reference_error("this is already initialized")?,
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

    pub(crate) fn reflect_call(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
        construct: bool,
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let target = args.first().unwrap_or(&UNDEFINED_VALUE);
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
            match self.arguments_from_array_like(host, roots, list.unwrap_or(&UNDEFINED_VALUE))? {
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
                args.get(1).unwrap_or(&UNDEFINED_VALUE),
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
        if !matches!(list.view(), ValueRef::Object(_)) {
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
        let ValueRef::Object(id) = value.view() else {
            return Ok(None);
        };
        if let Some(function) = self.heap.function(id) {
            return Ok(Some(Dispatch::Script {
                func_id: function.func_id,
                env: function.env,
            }));
        }
        Ok(match self.heap.get(id)? {
            Object::Resolution(_) => Some(Dispatch::Resolution(id)),
            Object::PromiseCallback { .. } => Some(Dispatch::PromiseCallback(id)),
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
        if matches!(receiver.view(), ValueRef::Null | ValueRef::Undefined) {
            return self.throw_type_error("Array.prototype.toString called on null or undefined");
        }
        let roots = &roots.with(receiver);
        let mut boxed = None;
        let join = match self.read_to_object_property(
            host,
            roots,
            receiver,
            &mut boxed,
            PropertyKeyRef::String("join"),
        )? {
            Invocation::Value(value) => value,
            thrown => return Ok(thrown),
        };
        if self.is_callable(&join) {
            let receiver = Value::object(boxed.unwrap_or_else(|| self.coercible_object(receiver)));
            self.invoke(host, roots, &join, Vec::new(), &receiver)
        } else {
            match boxed {
                Some(id) => self.object_to_string(host, roots, &Value::object(id)),
                None => self.object_to_string(host, roots, receiver),
            }
        }
    }

    pub(crate) fn object_to_string(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        let tag = match receiver.view() {
            ValueRef::Object(id) if self.heap.is_environment(id) == Some(true) => {
                return self.throw_type_error("incompatible Object.prototype receiver");
            }
            ValueRef::Object(id) => self.object_tag(id)?,
            ValueRef::Undefined => {
                return Ok(Invocation::Value(Value::string(
                    "[object Undefined]".into(),
                )));
            }
            ValueRef::Null => {
                return Ok(Invocation::Value(Value::string("[object Null]".into())));
            }
            ValueRef::String(_) => "String",
            ValueRef::Number(_) => "Number",
            ValueRef::BigInt(_) => "BigInt",
            ValueRef::Bool(_) => "Boolean",
            ValueRef::Symbol(_) => "Symbol",
        };
        let custom_tag = match self.read_to_object_property(
            host,
            &roots.with(receiver),
            receiver,
            &mut None,
            PropertyKeyRef::Symbol(TO_STRING_TAG_SYMBOL),
        )? {
            Invocation::Value(value) => value,
            thrown => return Ok(thrown),
        };
        if let ValueRef::String(custom_tag) = custom_tag.view() {
            let mut result = JsString::from("[object ");
            result.push_str(custom_tag);
            result.push(']');
            return Ok(Invocation::Value(Value::string(result)));
        }
        Ok(Invocation::Value(Value::string(
            format!("[object {tag}]").into(),
        )))
    }

    pub(crate) fn object_tag(&self, id: ObjectId) -> Result<&'static str, VmError> {
        let target = self.heap.proxy_target(id)?;
        Ok(match self.heap.get(target)? {
            Object::Date { .. } if id == target => "Date",
            Object::Boxed { primitive, .. } => match primitive {
                BoxedPrimitive::String(_) => "String",
                BoxedPrimitive::Number(_) => "Number",
                BoxedPrimitive::BigInt(_) => "BigInt",
                BoxedPrimitive::Bool(_) => "Boolean",
                BoxedPrimitive::Symbol(_) => "Symbol",
            },
            Object::Array(_) => "Array",
            Object::Function(_)
            | Object::Native(_)
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
        if matches!(receiver.view(), ValueRef::Null | ValueRef::Undefined) {
            return self.throw_type_error("Array.prototype.join called on null or undefined");
        }
        let mut boxed = None;
        let length = match self.read_to_object_property(
            host,
            roots,
            receiver,
            &mut boxed,
            PropertyKeyRef::String("length"),
        )? {
            Invocation::Value(value) => value,
            thrown => return Ok(thrown),
        };
        let (length, separator) = {
            let roots = &roots.with(&boxed);
            let length = match self.coerce_number(host, roots, &length)? {
                Ok(number) => number.trunc().clamp(0.0, 9_007_199_254_740_991.0) as u64,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
            let separator = match args.first().unwrap_or(&UNDEFINED_VALUE) {
                value if matches!(value.view(), ValueRef::Undefined) => ",".into(),
                value => match self.coerce_string(host, roots, value)? {
                    Ok(string) => string,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                },
            };
            (length, separator)
        };
        if let ValueRef::String(text) = receiver.view() {
            // Every index below the string's fixed length is an own data property.
            if separator.is_empty() {
                return Ok(Invocation::Value(receiver.clone()));
            }
            let mut output = JsString::default();
            for (index, unit) in text.units().enumerate() {
                if index > 0 {
                    output.push_str(&separator);
                }
                output.push_unit(unit);
            }
            return Ok(Invocation::Value(Value::string(output)));
        }
        let array = match receiver.view() {
            ValueRef::Object(id) if matches!(self.heap.get(id)?, Object::Array(_)) => Some(id),
            _ => None,
        };
        if let Some(id) = array
            && !self.active_array_joins.insert(id)
        {
            return Ok(Invocation::Value(Value::string(JsString::default())));
        }
        let result = (|| {
            let mut output = JsString::default();
            for index in 0..length {
                if index > 0 {
                    output.push_str(&separator);
                }
                let item = if let Some(id) = array {
                    self.read_array_index(host, roots, id, index as usize)?
                } else {
                    self.read_to_object_property(
                        host,
                        roots,
                        receiver,
                        &mut boxed,
                        PropertyKeyRef::String(&index.to_string()),
                    )?
                };
                let value = match item {
                    Invocation::Value(value) => value,
                    thrown => return Ok(thrown),
                };
                if matches!(value.view(), ValueRef::Undefined | ValueRef::Null) {
                    continue;
                }
                let roots = &roots.with(&boxed);
                match self.coerce_string(host, roots, &value)? {
                    Ok(value) => output.push_str(&value),
                    Err(error) => return Ok(Invocation::Thrown(error)),
                }
            }
            Ok(Invocation::Value(Value::string(output)))
        })();
        if let Some(id) = array {
            self.active_array_joins.remove(&id);
        }
        result
    }

    pub(crate) fn create_arguments_object(
        heap: &mut Heap,
        iterator: ObjectId,
        args: &[Value],
    ) -> Result<Value, VmError> {
        let id = heap.alloc_plain();
        for (index, value) in args.iter().enumerate() {
            heap.define_data(
                id,
                PropertyKey::String(index.to_string().into()),
                value.clone(),
                Attributes::ORDINARY,
            )?;
        }
        heap.define_data(
            id,
            "length",
            Value::number(args.len() as f64),
            Attributes::METHOD,
        )?;
        heap.define_data(
            id,
            PropertyKey::Symbol(ITERATOR_SYMBOL),
            Value::object(iterator),
            Attributes::METHOD,
        )?;
        Ok(Value::object(id))
    }

    pub(crate) fn array_iterator(
        &mut self,
        receiver: &Value,
        kind: crate::heap::ArrayIterationKind,
    ) -> Result<Invocation, VmError> {
        let id = match self.array_receiver(receiver)? {
            Ok(id) => id,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let iterator = self.heap.alloc(Object::ArrayIterator {
            object: PlainObject::new(Some(self.builtins.get("arrayIterator.prototype"))),
            array: Some(id),
            kind,
            next_index: 0,
        });
        Ok(Invocation::Value(Value::object(iterator)))
    }

    pub(crate) fn array_iterator_next(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        use crate::heap::ArrayIterationKind;

        let invalid_receiver = "ArrayIterator.next requires an array iterator";
        let ValueRef::Object(id) = receiver.view() else {
            return self.throw_type_error(invalid_receiver);
        };
        let Object::ArrayIterator {
            array,
            next_index,
            kind,
            ..
        } = self.heap.get(id)?
        else {
            return self.throw_type_error(invalid_receiver);
        };
        let Some(source) = *array else {
            return self.iterator_result(Value::UNDEFINED, true);
        };
        let (index, kind) = (*next_index, *kind);
        let view = (id, source);
        let roots = &roots.with(&view);
        let length = match self.length_of_array_like(host, roots, source)? {
            Ok(length) => length,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let Object::ArrayIterator {
            array, next_index, ..
        } = self.heap.get_mut(id)?
        else {
            unreachable!()
        };
        if index >= length {
            *array = None;
            return self.iterator_result(Value::UNDEFINED, true);
        }
        *next_index = index + 1;
        let key = Value::number(index as f64);
        let value = if kind == ArrayIterationKind::Keys {
            key
        } else {
            let value = match self.read_array_like_index(host, roots, source, index)? {
                Invocation::Value(value) => value,
                thrown => return Ok(thrown),
            };
            if kind == ArrayIterationKind::Entries {
                Value::object(self.heap.alloc_array(vec![key, value]))
            } else {
                value
            }
        };
        self.iterator_result(value, false)
    }

    pub(crate) fn iterator_result(
        &mut self,
        value: Value,
        done: bool,
    ) -> Result<Invocation, VmError> {
        let result = self.heap.alloc_plain();
        self.heap.set_own_string(result, "value", value)?;
        self.heap
            .set_own_string(result, "done", Value::boolean(done))?;
        Ok(Invocation::Value(Value::object(result)))
    }

    /// Same result object, for callers that cannot report a script throw.
    pub(crate) fn iterator_result_value(
        &mut self,
        value: Value,
        done: bool,
    ) -> Result<Value, VmError> {
        match self.iterator_result(value, done)? {
            Invocation::Value(result) => Ok(result),
            Invocation::Thrown(_) => unreachable!("iteration results are never thrown"),
        }
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
        let value = args.first().unwrap_or(&UNDEFINED_VALUE);
        match value.view() {
            ValueRef::Undefined | ValueRef::Null => {
                Ok(Invocation::Value(Value::object(self.heap.alloc_plain())))
            }
            _ => Ok(Invocation::Value(Value::object(
                self.coercible_object(value),
            ))),
        }
    }

    /// Materialize an already validated ToObject operand, preserving object identity.
    pub(crate) fn coercible_object(&mut self, value: &Value) -> ObjectId {
        let primitive = match value.view() {
            ValueRef::Object(id) => return id,
            ValueRef::String(text) => BoxedPrimitive::String(text.clone()),
            ValueRef::Number(number) => BoxedPrimitive::Number(number),
            ValueRef::BigInt(value) => BoxedPrimitive::BigInt(value.clone()),
            ValueRef::Bool(value) => BoxedPrimitive::Bool(value),
            ValueRef::Symbol(id) => BoxedPrimitive::Symbol(id),
            _ => unreachable!("ToObject requires a non-nullish operand"),
        };
        self.heap.alloc(Object::Boxed {
            primitive,
            object: PlainObject::new(self.primitive_prototype(value)),
        })
    }

    pub(crate) fn primitive_prototype(&self, value: &Value) -> Option<ObjectId> {
        match value.view() {
            ValueRef::String(_) => Some(self.builtins.get("string.prototype")),
            ValueRef::Number(_) => Some(self.builtins.get("number.prototype")),
            ValueRef::BigInt(_) => Some(self.builtins.get("bigint.prototype")),
            ValueRef::Bool(_) => Some(self.builtins.get("boolean.prototype")),
            ValueRef::Symbol(_) => Some(self.builtins.get("symbol.prototype")),
            _ => None,
        }
    }

    pub(crate) fn object_create(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let prototype = match args.first().map(Value::view) {
            Some(ValueRef::Null) => None,
            Some(ValueRef::Object(id)) => Some(id),
            _ => {
                return self.throw_type_error("Object.create requires an object or null prototype");
            }
        };
        let object = self.heap.alloc_plain_with(prototype);
        match args
            .get(1)
            .filter(|value| !matches!(value.view(), ValueRef::Undefined))
        {
            None => Ok(Invocation::Value(Value::object(object))),
            properties => self.define_properties(host, roots, object, properties),
        }
    }

    pub(crate) fn prevent_extensions_builtin_impl(
        &mut self,
        args: &[Value],
        reflect: bool,
    ) -> Result<Invocation, VmError> {
        let target = args.first().unwrap_or(&UNDEFINED_VALUE);
        if let ValueRef::Object(id) = target.view() {
            let id = self.heap.proxy_target(id)?;
            // A view over a resizable buffer can still gain and lose elements,
            // so it cannot be made non-extensible.
            if self
                .heap
                .typed_array(id)?
                .is_some_and(|view| view.byte_length.is_none())
            {
                return if reflect {
                    Ok(Invocation::Value(Value::boolean(false)))
                } else {
                    self.throw_type_error(
                        "cannot prevent extensions on a length-tracking TypedArray",
                    )
                };
            }
            self.heap.get_mut(id)?.prevent_extensions();
        } else if reflect {
            return self.throw_type_error("Reflect.preventExtensions requires an object");
        }
        Ok(Invocation::Value(if reflect {
            Value::boolean(true)
        } else {
            target.clone()
        }))
    }

    pub(crate) fn is_extensible_builtin_impl(
        &mut self,
        args: &[Value],
        reflect: bool,
    ) -> Result<Invocation, VmError> {
        let extensible = match args.first().map(Value::view) {
            Some(ValueRef::Object(id)) => {
                self.heap.get(self.heap.proxy_target(id)?)?.is_extensible()
            }
            _ if reflect => {
                return self.throw_type_error("Reflect.isExtensible requires an object");
            }
            _ => false,
        };
        Ok(Invocation::Value(Value::boolean(extensible)))
    }

    pub(crate) fn get_prototype_of(
        &mut self,
        args: &[Value],
        reflect: bool,
    ) -> Result<Invocation, VmError> {
        let Some(ValueRef::Object(target)) = args.first().map(Value::view) else {
            if !reflect
                && let Some(prototype) = args
                    .first()
                    .and_then(|value| self.primitive_prototype(value))
            {
                return Ok(Invocation::Value(Value::object(prototype)));
            }
            return self.throw_type_error(if reflect {
                "Reflect.getPrototypeOf requires an object"
            } else {
                "Object.getPrototypeOf cannot convert null or undefined to an object"
            });
        };
        Ok(Invocation::Value(
            self.heap
                .get(self.heap.proxy_target(target)?)?
                .prototype()
                .map_or(Value::NULL, Value::object),
        ))
    }

    pub(crate) fn set_prototype_of(
        &mut self,
        args: &[Value],
        reflect: bool,
    ) -> Result<Invocation, VmError> {
        let target = args.first().unwrap_or(&UNDEFINED_VALUE);
        if reflect && !matches!(target.view(), ValueRef::Object(_)) {
            return self.throw_type_error("Reflect.setPrototypeOf requires an object target");
        }
        if matches!(target.view(), ValueRef::Undefined | ValueRef::Null) {
            return self.throw_type_error("Object.setPrototypeOf requires a non-nullish target");
        }
        let prototype = match args.get(1).map(Value::view) {
            Some(ValueRef::Null) => None,
            Some(ValueRef::Object(id)) => Some(id),
            _ => {
                return self
                    .throw_type_error("setPrototypeOf requires an object or null prototype");
            }
        };
        let ValueRef::Object(target_id) = target.view() else {
            return Ok(Invocation::Value(target.clone()));
        };
        let accepted = self.heap.set_prototype(target_id, prototype)?;
        if reflect {
            Ok(Invocation::Value(Value::boolean(accepted)))
        } else if accepted {
            Ok(Invocation::Value(Value::object(target_id)))
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
        let target = args.first().unwrap_or(&UNDEFINED_VALUE);
        let ValueRef::Object(id) = target.view() else {
            return Ok(Invocation::Value(target.clone()));
        };
        let base = self.heap.proxy_target(id)?;
        // A typed array's elements are defined one by one, so a non-empty view
        // cannot be frozen; the per-key path reports that as a TypeError.
        if base == id && self.heap.typed_array(id)?.is_none() {
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
        let result = |value| Ok(Invocation::Value(Value::boolean(value)));
        let Some(ValueRef::Object(id)) = args.first().map(Value::view) else {
            return result(true);
        };
        let base = self.heap.proxy_target(id)?;
        if base == id {
            return result(self.heap.get(base)?.test_integrity_level(level));
        }
        if self.heap.get(base)?.is_extensible() {
            return result(false);
        }
        let keys = match self.own_keys(host, roots, id, false)? {
            Ok(keys) => keys,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        for key in &keys {
            let roots = &roots.with(&keys);
            match self.own_descriptor(host, roots, id, key.borrowed())? {
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
        let keys = match args.first().map(Value::view) {
            None | Some(ValueRef::Undefined | ValueRef::Null) => {
                return self.throw_type_error("Object.keys requires a non-nullish value");
            }
            Some(ValueRef::Object(id)) => match self.enumerable_string_keys(host, roots, id)? {
                Ok(keys) => keys.into_iter().map(Value::string).collect(),
                Err(error) => return Ok(Invocation::Thrown(error)),
            },
            Some(ValueRef::String(text)) => (0..utf16_len(text))
                .map(|index| Value::string(index.to_string().into()))
                .collect(),
            _ => Vec::new(),
        };
        Ok(Invocation::Value(Value::object(
            self.heap.alloc_array(keys),
        )))
    }

    /// `Object.getOwnPropertyNames` / `Object.getOwnPropertySymbols`.
    ///
    /// [[OwnPropertyKeys]] order is preserved, so proxy traps run once and the
    /// filter keeps string keys ahead of symbol keys. Primitive strings expose
    /// their UTF-16 index keys and `length`; other primitives have none.
    pub(crate) fn object_own_property_keys(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
        symbols: bool,
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let mut values = Vec::new();
        match args.first().map(Value::view) {
            None | Some(ValueRef::Undefined | ValueRef::Null) => {
                return self.throw_type_error(if symbols {
                    "Object.getOwnPropertySymbols requires a non-nullish value"
                } else {
                    "Object.getOwnPropertyNames requires a non-nullish value"
                });
            }
            Some(ValueRef::Object(id)) => {
                let keys = match self.own_keys(host, roots, id, false)? {
                    Ok(keys) => keys,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                };
                for key in keys {
                    match key {
                        PropertyKey::String(name) if !symbols => values.push(Value::string(name)),
                        PropertyKey::Symbol(symbol) if symbols => {
                            values.push(Value::symbol(symbol))
                        }
                        _ => {}
                    }
                }
            }
            Some(ValueRef::String(text)) if !symbols => {
                for index in 0..utf16_len(text) {
                    values.push(Value::string(index.to_string().into()));
                }
                values.push(Value::string("length".into()));
            }
            Some(_) => {}
        }
        Ok(Invocation::Value(Value::object(
            self.heap.alloc_array(values),
        )))
    }

    pub(crate) fn object_values_or_entries(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
        entries: bool,
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let mut result = Vec::new();
        match args.first().map(|value| (value, value.view())) {
            None | Some((_, ValueRef::Undefined | ValueRef::Null)) => {
                return self.throw_type_error("Object.values/entries requires a non-nullish value");
            }
            Some((target, ValueRef::Object(id))) => {
                let keys = match self.own_keys(host, roots, id, false)? {
                    Ok(keys) => keys,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                };
                let roots = &roots.with(&keys);
                for key in &keys {
                    let PropertyKey::String(name) = key else {
                        continue;
                    };
                    let roots = &roots.with(&result);
                    match self.own_descriptor(host, roots, id, key.borrowed())? {
                        Ok(Some((_, attrs))) if attrs.enumerable => {}
                        Ok(_) => continue,
                        Err(error) => return Ok(Invocation::Thrown(error)),
                    }
                    let value = match self.read_property(host, roots, target, key.borrowed())? {
                        Invocation::Value(value) => value,
                        thrown @ Invocation::Thrown(_) => return Ok(thrown),
                    };
                    result.push(if entries {
                        Value::object(
                            self.heap
                                .alloc_array(vec![Value::string(name.clone()), value]),
                        )
                    } else {
                        value
                    });
                }
            }
            Some((_, ValueRef::String(text))) => {
                for (index, unit) in text.units().enumerate() {
                    let value = Value::string(utf16_unit_string(unit));
                    result.push(if entries {
                        Value::object(
                            self.heap
                                .alloc_array(vec![Value::string(index.to_string().into()), value]),
                        )
                    } else {
                        value
                    });
                }
            }
            _ => {}
        }
        Ok(Invocation::Value(Value::object(
            self.heap.alloc_array(result),
        )))
    }

    pub(crate) fn object_from_entries(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let result = self.heap.alloc_plain();
        let root_view = (args, result);
        let roots = &roots.with(&root_view);
        let iterable = args.first().unwrap_or(&UNDEFINED_VALUE);
        let iterator_pair = match self.get_iterator(host, roots, iterable)? {
            Ok(pair) => pair,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let roots = &roots.with(&iterator_pair);
        let (iterator, next) = &iterator_pair;
        loop {
            let item = match self.iterator_step(host, roots, iterator, next)? {
                Ok(Some(item)) => item,
                Ok(None) => return Ok(Invocation::Value(Value::object(result))),
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
            let roots = &roots.with(&item);
            if let Err(error) = self.add_object_entry(host, roots, result, &item)? {
                // IteratorClose preserves the original throw over a return-method throw.
                self.close_iterator(host, &roots.with(&error), iterator)?;
                return Ok(Invocation::Thrown(error));
            }
        }
    }

    fn add_object_entry(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        result: ObjectId,
        item: &Value,
    ) -> Result<Result<(), Value>, VmError> {
        if !matches!(item.view(), ValueRef::Object(_)) {
            return Ok(Err(
                self.type_error("Object.fromEntries entry must be an object")?
            ));
        }
        let key = match self.read_property(host, roots, item, PropertyKeyRef::String("0"))? {
            Invocation::Value(key) => key,
            Invocation::Thrown(error) => return Ok(Err(error)),
        };
        let roots = &roots.with(&key);
        let value = match self.read_property(host, roots, item, PropertyKeyRef::String("1"))? {
            Invocation::Value(value) => value,
            Invocation::Thrown(error) => return Ok(Err(error)),
        };
        let key = match self.coerce_property_key(host, &roots.with(&value), &key)? {
            Ok(key) => key,
            Err(error) => return Ok(Err(error)),
        };
        self.heap.set_own(result, key, value)?;
        Ok(Ok(()))
    }

    pub(crate) fn get_own_property_descriptor(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
        reflect: bool,
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let target = args.first().unwrap_or(&UNDEFINED_VALUE);
        if reflect && !matches!(target.view(), ValueRef::Object(_)) {
            return self.throw_type_error("Reflect.getOwnPropertyDescriptor requires an object");
        }
        if matches!(target.view(), ValueRef::Null | ValueRef::Undefined) {
            return self
                .throw_type_error("Object.getOwnPropertyDescriptor requires a non-nullish value");
        }
        let key =
            match self.coerce_property_key(host, roots, args.get(1).unwrap_or(&UNDEFINED_VALUE))? {
                Ok(key) => key,
                Err(thrown) => return Ok(Invocation::Thrown(thrown)),
            };
        let descriptor = match target.view() {
            ValueRef::Object(id) => match self.own_descriptor(host, roots, id, key.borrowed())? {
                Ok(descriptor) => descriptor,
                Err(error) => return Ok(Invocation::Thrown(error)),
            },
            ValueRef::String(text) => {
                return Ok(Invocation::Value(
                    match string_own_property(text, key.borrowed()) {
                        Some(property) => Value::object(self.string_descriptor(property)?),
                        None => Value::UNDEFINED,
                    },
                ));
            }
            _ => None,
        };
        Ok(Invocation::Value(match descriptor {
            Some((property, attributes)) => {
                Value::object(self.descriptor_object(property, attributes)?)
            }
            None => Value::UNDEFINED,
        }))
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
                self.heap.set_own_string(
                    result,
                    "writable",
                    Value::boolean(attributes.writable),
                )?;
            }
            Property::Accessor { get, set } => {
                self.heap.set_own_string(
                    result,
                    "get",
                    get.map_or(Value::UNDEFINED, Value::object),
                )?;
                self.heap.set_own_string(
                    result,
                    "set",
                    set.map_or(Value::UNDEFINED, Value::object),
                )?;
            }
        }
        self.heap
            .set_own_string(result, "enumerable", Value::boolean(attributes.enumerable))?;
        self.heap.set_own_string(
            result,
            "configurable",
            Value::boolean(attributes.configurable),
        )?;
        Ok(result)
    }

    pub(crate) fn get_own_property_descriptors(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let target = args.first().unwrap_or(&UNDEFINED_VALUE);
        let target = match target.view() {
            ValueRef::Object(id) => id,
            ValueRef::Null | ValueRef::Undefined => {
                return self.throw_type_error(
                    "Object.getOwnPropertyDescriptors requires a non-nullish value",
                );
            }
            primitive => {
                let result = self.heap.alloc_plain();
                if let ValueRef::String(text) = primitive {
                    for (index, unit) in text.units().enumerate() {
                        let descriptor = self.string_descriptor(OwnProperty::StringIndex(unit))?;
                        self.heap.set_own_string(
                            result,
                            &index.to_string(),
                            Value::object(descriptor),
                        )?;
                    }
                    let length =
                        self.string_descriptor(OwnProperty::StringLength(text.unit_len()))?;
                    self.heap
                        .set_own_string(result, "length", Value::object(length))?;
                }
                return Ok(Invocation::Value(Value::object(result)));
            }
        };
        let keys = match self.own_keys(host, roots, target, false)? {
            Ok(keys) => keys,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let result = self.heap.alloc_plain();
        for key in &keys {
            let view = (result, &keys);
            let roots = &roots.with(&view);
            let (property, attributes) =
                match self.own_descriptor(host, roots, target, key.borrowed())? {
                    Ok(Some(descriptor)) => descriptor,
                    Ok(None) => continue,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                };
            let descriptor = self.descriptor_object(property, attributes)?;
            self.heap
                .set_own(result, key.clone(), Value::object(descriptor))?;
        }
        Ok(Invocation::Value(Value::object(result)))
    }

    fn string_descriptor(&mut self, property: OwnProperty<'_>) -> Result<ObjectId, VmError> {
        let attributes = property.attributes();
        self.descriptor_object(property.into_owned(), attributes)
    }

    pub(crate) fn define_property_builtin(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
        reflect: bool,
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let Some(ValueRef::Object(target)) = args.first().map(Value::view) else {
            return self.throw_type_error(if reflect {
                "Reflect.defineProperty requires an object target"
            } else {
                "Object.defineProperty requires an object target"
            });
        };
        let key = match self.coerce_property_key(
            host,
            roots,
            args.get(1).unwrap_or(&Value::UNDEFINED),
        )? {
            Ok(key) => key,
            Err(thrown) => return Ok(Invocation::Thrown(thrown)),
        };
        let update = match self.read_property_descriptor(
            host,
            &roots.with(&key),
            args.get(2).unwrap_or(&Value::UNDEFINED),
        )? {
            Ok(update) => update,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        if reflect {
            return Ok(
                match self.try_apply_property_update(host, roots, target, key, update)? {
                    Ok(applied) => Invocation::Value(Value::boolean(applied)),
                    Err(error) => Invocation::Thrown(error),
                },
            );
        }
        self.apply_property_update(host, roots, target, key, update)
    }
    pub(crate) fn object_assign(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let target = args.first().unwrap_or(&UNDEFINED_VALUE);
        if matches!(target.view(), ValueRef::Null | ValueRef::Undefined) {
            return self.throw_type_error("Object.assign requires a non-nullish target");
        }
        let target_id = self.coercible_object(target);
        let target = Value::object(target_id);
        let roots = &roots.with(&target);
        for source in args.iter().skip(1) {
            let ValueRef::Object(source_id) = source.view() else {
                if let ValueRef::String(text) = source.view() {
                    for (index, unit) in text.units().enumerate() {
                        let invocation = self.set_property(
                            host,
                            roots,
                            target_id,
                            &target,
                            PropertyKey::String(index.to_string().into()),
                            Value::string(utf16_unit_string(unit)),
                            true,
                        )?;
                        if let thrown @ Invocation::Thrown(_) = invocation {
                            return Ok(thrown);
                        }
                    }
                }
                continue;
            };
            let keys = match self.own_keys(host, roots, source_id, false)? {
                Ok(keys) => keys,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
            for key in &keys {
                let roots = &roots.with(&keys);
                match self.own_descriptor(host, roots, source_id, key.borrowed())? {
                    Ok(Some((_, attributes))) if attributes.enumerable => {}
                    Ok(_) => continue,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                }
                let value = match self.read_property(host, roots, source, key.borrowed())? {
                    Invocation::Value(value) => value,
                    thrown @ Invocation::Thrown(_) => return Ok(thrown),
                };
                let invocation =
                    self.set_property(host, roots, target_id, &target, key.clone(), value, true)?;
                if let thrown @ Invocation::Thrown(_) = invocation {
                    return Ok(thrown);
                }
            }
        }
        Ok(Invocation::Value(target))
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
        let source = match properties.map(|value| (value, value.view())) {
            Some((_, ValueRef::Object(source))) => source,
            None | Some((_, ValueRef::Undefined | ValueRef::Null)) => {
                return self
                    .throw_type_error("Object.defineProperties requires non-nullish properties");
            }
            Some((_, ValueRef::String(text))) if !text.is_empty() => {
                return self.throw_type_error("property descriptor must be an object");
            }
            _ => return Ok(Invocation::Value(Value::object(target))),
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
            let descriptor =
                match self.read_property(host, roots, properties.unwrap(), key.borrowed())? {
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
        Ok(Invocation::Value(Value::object(target)))
    }

    pub(crate) fn read_property_descriptor(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        descriptor: &Value,
    ) -> Result<Result<DescriptorUpdate, Value>, VmError> {
        let roots = &roots.with(&descriptor);
        let ValueRef::Object(id) = descriptor.view() else {
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
                    let handler = match value.view() {
                        ValueRef::Undefined => None,
                        ValueRef::Object(id) if self.is_callable(&value) => Some(id),
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
            Ok(true) => Ok(Invocation::Value(Value::object(target))),
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
        // Integer-indexed exotic [[DefineOwnProperty]].
        match self.heap.typed_index(target, key.borrowed())? {
            crate::heap::TypedIndex::Ordinary => {}
            crate::heap::TypedIndex::Invalid => return Ok(Ok(false)),
            crate::heap::TypedIndex::Valid(index) => {
                if update.configurable == Some(false)
                    || update.enumerable == Some(false)
                    || update.writable == Some(false)
                    || update.is_accessor()
                {
                    return Ok(Ok(false));
                }
                if let Some(value) = update.value {
                    match self.typed_set_element(host, roots, target, index, &value)? {
                        Ok(()) => {}
                        Err(error) => return Ok(Err(error)),
                    }
                }
                return Ok(Ok(true));
            }
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
            *value = Value::number(f64::from(length));
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
            let iterable = args.first().unwrap_or(&UNDEFINED_VALUE);
            let mut errors = Vec::new();
            if !matches!(iterable.view(), ValueRef::Undefined) {
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
        let message = match message_arg.unwrap_or(&Value::UNDEFINED) {
            value if matches!(value.view(), ValueRef::Undefined) => None,
            value => match self.coerce_string(host, roots, value)? {
                Ok(message) => Some(message),
                Err(error) => return Ok(Invocation::Thrown(error)),
            },
        };
        let error = self.alloc_error(prototype, name, message)?;
        if let (Some(errors), ValueRef::Object(id)) = (errors, error.view()) {
            let errors = self.heap.alloc_array(errors);
            self.heap
                .define_data(id, "errors", Value::object(errors), Attributes::METHOD)?;
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
                .define_data(id, "message", Value::string(message), Attributes::METHOD)?;
        }
        self.heap
            .define_data(id, "stack", Value::string(stack), Attributes::METHOD)?;
        Ok(Value::object(id))
    }
    pub(crate) fn alloc_aggregate_error(&mut self, errors: Vec<Value>) -> Result<Value, VmError> {
        let error = self.alloc_error(
            self.builtins.get("aggregateerror.prototype"),
            "AggregateError",
            Some("All promises were rejected".into()),
        )?;
        let ValueRef::Object(id) = error.view() else {
            unreachable!()
        };
        let errors = self.heap.alloc_array(errors);
        self.heap
            .define_data(id, "errors", Value::object(errors), Attributes::METHOD)?;
        Ok(error)
    }

    pub(crate) fn construct_promise(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: Vec<Value>,
        prototype: Option<ObjectId>,
    ) -> Result<Invocation, VmError> {
        let executor = args.first().cloned().unwrap_or(Value::UNDEFINED);
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
            vec![Value::object(resolve_id), Value::object(reject_id)],
            &Value::UNDEFINED,
        )?;
        if let Invocation::Thrown(error) = invocation {
            self.capability_call(host, roots, reject_id, error)?;
        }
        Ok(Invocation::Value(Value::object(promise)))
    }

    pub(crate) fn symbol_static(
        &mut self,
        _receiver: &Value,
        call: impl FnOnce(&mut Self, Vec<Value>) -> Result<Invocation, VmError>,
        args: Vec<Value>,
    ) -> Result<Invocation, VmError> {
        // `Symbol.for` and `Symbol.keyFor` do not read `this`, so an extracted
        // method reference is callable in real engines and in this VM.
        call(self, args)
    }

    pub(crate) fn symbol_method(
        &mut self,
        receiver: &Value,
        method: &str,
    ) -> Result<Invocation, VmError> {
        let id = match receiver.view() {
            ValueRef::Symbol(id) => id,
            ValueRef::Object(id) => match self.heap.get(id)? {
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
            "valueOf" => Value::symbol(id),
            "toString" => {
                let mut text = JsString::from("Symbol(");
                if let Some(description) = description {
                    text.push_str(description);
                }
                text.push(')');
                Value::string(text)
            }
            "get description" => description.cloned().map_or(Value::UNDEFINED, Value::string),
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
        let description = match args.first().unwrap_or(&Value::UNDEFINED) {
            value if matches!(value.view(), ValueRef::Undefined) => None,
            value => match self.coerce_string(host, roots, value)? {
                Ok(description) => Some(description),
                Err(error) => return Ok(Invocation::Thrown(error)),
            },
        };
        let id = self.heap.alloc_symbol(description)?;
        Ok(Invocation::Value(Value::symbol(id)))
    }

    pub(crate) fn symbol_for(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: Vec<Value>,
    ) -> Result<Invocation, VmError> {
        let key =
            match self.coerce_string(host, roots, args.first().unwrap_or(&Value::UNDEFINED))? {
                Ok(key) => key,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
        let id = self.heap.intern_symbol(key)?;
        Ok(Invocation::Value(Value::symbol(id)))
    }

    pub(crate) fn symbol_key_for(&mut self, args: Vec<Value>) -> Result<Invocation, VmError> {
        let Some(ValueRef::Symbol(id)) = args.first().map(Value::view) else {
            return self.throw_type_error("Symbol.keyFor requires a Symbol");
        };
        let key = self
            .heap
            .symbol_registry
            .iter()
            .find_map(|(key, registered)| (*registered == id).then(|| key.clone()));
        Ok(Invocation::Value(match key {
            Some(key) => Value::string(key),
            None => Value::UNDEFINED,
        }))
    }

    pub(crate) fn queue_microtask(&mut self, args: Vec<Value>) -> Result<Invocation, VmError> {
        let function = args.first().cloned().unwrap_or(Value::UNDEFINED);
        if !self.is_callable(&function) {
            return self.throw_type_error("queueMicrotask argument is not a function");
        }
        self.microtasks.push_back(Microtask::Call {
            function,
            args: Vec::new(),
        });
        Ok(Invocation::Value(Value::UNDEFINED))
    }

    pub(crate) fn is_intrinsic_promise_then(&self, value: &Value) -> bool {
        static THEN_ID: std::sync::LazyLock<crate::intrinsic::IntrinsicId> =
            std::sync::LazyLock::new(|| THEN.link());
        matches!(value.view(), ValueRef::Object(id) if matches!(self.heap.get(id),
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
                if !matches!(receiver.view(), ValueRef::Object(_)) {
                    return self.throw_type_error("incompatible Promise receiver");
                }
                let value = args.into_iter().next().unwrap_or(Value::UNDEFINED);
                self.promise_resolve_with(host, roots, receiver, value)
            }
            "reject" => {
                let reason = args.into_iter().next().unwrap_or(Value::UNDEFINED);
                let roots = &roots.with(&receiver);
                let (result, _, reject) =
                    match self.new_promise_capability(host, &roots.with(&reason), receiver)? {
                        Ok(capability) => capability,
                        Err(error) => return Ok(Invocation::Thrown(error)),
                    };
                self.call_promise_capability(host, roots, result, &reject, reason)
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
        receiver: &Value,
        name: &str,
        args: Vec<Value>,
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&receiver);
        match name {
            "catch" => self.invoke_promise_then(
                host,
                roots,
                receiver,
                vec![
                    Value::UNDEFINED,
                    args.into_iter().next().unwrap_or(Value::UNDEFINED),
                ],
            ),
            "then" | "finally" => {
                let ValueRef::Object(id) = receiver.view() else {
                    return self.throw_type_error("incompatible Promise receiver");
                };
                if name == "then" && self.heap.get(id)?.as_promise().is_none() {
                    return self.throw_type_error("incompatible Promise receiver");
                }
                let species = match self.promise_species(host, &roots.with(&args), receiver)? {
                    Invocation::Value(value) => value,
                    thrown => return Ok(thrown),
                };
                if name == "finally" {
                    let handler = args.into_iter().next().unwrap_or(Value::UNDEFINED);
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
                    return self.invoke_promise_then(host, roots, receiver, handlers);
                }
                let (result, resolve, reject) =
                    match self.new_promise_capability(host, &roots.with(&args), &species)? {
                        Ok(capability) => capability,
                        Err(error) => return Ok(Invocation::Thrown(error)),
                    };
                self.react(
                    id,
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
        let intrinsic = Value::object(self.builtins.get("promise"));
        let constructor = match self.read_property(
            host,
            roots,
            receiver,
            PropertyKeyRef::String("constructor"),
        )? {
            Invocation::Value(value) if matches!(value.view(), ValueRef::Undefined) => {
                return Ok(Invocation::Value(intrinsic));
            }
            Invocation::Value(value) if matches!(value.view(), ValueRef::Object(_)) => value,
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
            Invocation::Value(value)
                if matches!(value.view(), ValueRef::Undefined | ValueRef::Null) =>
            {
                Ok(Invocation::Value(intrinsic))
            }
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
        if species.view() == ValueRef::Object(self.builtins.get("promise")) {
            let result = self.heap.alloc_promise();
            let (resolve, reject) = self.alloc_resolution_pair(result, false)?;
            return Ok(Ok((
                Value::object(result),
                Value::object(resolve),
                Value::object(reject),
            )));
        }
        let executor = self.alloc_promise_callback(PromiseCallback::Executor(None));
        let ValueRef::Object(id) = executor.view() else {
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
            && matches!(result.view(), ValueRef::Object(_))
            && self.is_callable(resolve)
            && self.is_callable(reject)
        {
            return Ok(Ok((result, resolve.clone(), reject.clone())));
        }
        Ok(Err(self.type_error("invalid Promise species capability")?))
    }

    pub(crate) fn alloc_promise_callback(&mut self, callback: PromiseCallback) -> Value {
        let length = match callback {
            PromiseCallback::Executor(_) => 2.0,
            PromiseCallback::Finally { .. } | PromiseCallback::CombineElement { .. } => 1.0,
            PromiseCallback::Restore { .. } => 0.0,
        };
        Value::object(self.heap.alloc(Object::PromiseCallback {
            object: PlainObject::callable(self.heap.function_prototype, length, "".into()),
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
        let argument = args.next().unwrap_or(Value::UNDEFINED);
        let Object::PromiseCallback { callback, .. } = self.heap.get_mut(id)? else {
            unreachable!()
        };
        match callback {
            PromiseCallback::Executor(values) => {
                if values.as_ref().is_some_and(|(resolve, reject)| {
                    *resolve != Value::UNDEFINED || *reject != Value::UNDEFINED
                }) {
                    return self
                        .throw_type_error("Promise capability executor already initialized");
                }
                *values = Some((argument, args.next().unwrap_or(Value::UNDEFINED)));
                Ok(Invocation::Value(Value::UNDEFINED))
            }
            PromiseCallback::Restore { value, rejected } => Ok(if *rejected {
                Invocation::Thrown(value.clone())
            } else {
                Invocation::Value(value.clone())
            }),
            PromiseCallback::CombineElement {
                state,
                index,
                rejected,
            } => {
                let (state, index, rejected) = (*state, *index, *rejected);
                self.call_combine_element(host, roots, state, index, rejected, argument)
            }
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
                    match self.invoke(host, roots, &handler, Vec::new(), &Value::UNDEFINED)? {
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
        let roots = &roots.with(&species);
        if matches!(value.view(), ValueRef::Object(id) if self.heap.promise(id).is_ok()) {
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
        self.call_promise_capability(host, roots, result, &resolve, value)
    }

    fn call_promise_capability(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        result: Value,
        callback: &Value,
        value: Value,
    ) -> Result<Invocation, VmError> {
        match self.invoke(
            host,
            &roots.with(&result),
            callback,
            vec![value],
            &Value::UNDEFINED,
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
        let iterable = args.first().unwrap_or(&UNDEFINED_VALUE);
        let (result, resolve, reject) = match self.new_promise_capability(host, roots, receiver)? {
            Ok(capability) => capability,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let capability_roots = (&result, &resolve, &reject);
        let roots = &roots.with(&capability_roots);
        let reject_result = |vm: &mut Self, host: &mut dyn Host, roots: &ActiveRoots<'_>, error| {
            vm.call_promise_capability(host, roots, result.clone(), &reject, error)
        };
        let input_resolve =
            match self.read_property(host, roots, receiver, PropertyKeyRef::String("resolve"))? {
                Invocation::Value(resolve) if self.is_callable(&resolve) => resolve,
                Invocation::Value(_) => {
                    let error = self.type_error("Promise.resolve is not callable")?;
                    return reject_result(self, host, roots, error);
                }
                Invocation::Thrown(error) => return reject_result(self, host, roots, error),
            };
        let roots = &roots.with(&input_resolve);
        let (iterator, next) = match self.get_iterator(host, roots, iterable)? {
            Ok(record) => record,
            Err(error) => return reject_result(self, host, roots, error),
        };
        let state = if kind == CombineKind::Race {
            None
        } else {
            let id = self.heap.alloc_plain_with(None);
            self.heap.combines.insert(
                id,
                CombineState {
                    kind,
                    // Keep synchronous element callbacks from finishing before iteration ends.
                    remaining: 1,
                    results: Vec::new(),
                    called: Vec::new(),
                    capability: if kind == CombineKind::Any {
                        reject.clone()
                    } else {
                        resolve.clone()
                    },
                },
            );
            Some(id)
        };
        let iteration_roots = (&iterator, &next, state);
        let roots = &roots.with(&iteration_roots);
        loop {
            let element = match self.iterator_step(host, roots, &iterator, &next)? {
                Ok(Some(element)) => element,
                Ok(None) => break,
                Err(error) => return reject_result(self, host, roots, error),
            };
            let resolved =
                match self.invoke(host, roots, &input_resolve, vec![element], receiver)? {
                    Invocation::Value(value) => value,
                    Invocation::Thrown(error) => {
                        let _ = self.close_iterator(host, &roots.with(&error), &iterator)?;
                        return reject_result(self, host, roots, error);
                    }
                };
            let (fulfill_element, reject_element) = if let Some(state) = state {
                let data = self.combine_state_mut(state)?;
                let index = data.results.len();
                data.results.push(Value::UNDEFINED);
                data.called.push(false);
                data.remaining += 1;
                let fulfill_element = if kind == CombineKind::Any {
                    resolve.clone()
                } else {
                    self.alloc_promise_callback(PromiseCallback::CombineElement {
                        state,
                        index,
                        rejected: false,
                    })
                };
                let reject_element = if kind == CombineKind::All {
                    reject.clone()
                } else {
                    self.alloc_promise_callback(PromiseCallback::CombineElement {
                        state,
                        index,
                        rejected: true,
                    })
                };
                (fulfill_element, reject_element)
            } else {
                (resolve.clone(), reject.clone())
            };
            if let Invocation::Thrown(error) = self.invoke_promise_then(
                host,
                roots,
                &resolved,
                vec![fulfill_element, reject_element],
            )? {
                let _ = self.close_iterator(host, &roots.with(&error), &iterator)?;
                return reject_result(self, host, roots, error);
            }
        }
        if let Some(state) = state
            && let Invocation::Thrown(error) = self.finish_combine_element(host, roots, state)?
        {
            if kind == CombineKind::Any {
                return Ok(Invocation::Thrown(error));
            }
            return reject_result(self, host, roots, error);
        }
        Ok(Invocation::Value(result))
    }

    pub(crate) fn alloc_capability_pair(
        &mut self,
        promise: ObjectId,
    ) -> Result<(ObjectId, ObjectId), VmError> {
        self.alloc_resolution_pair(promise, true)
    }

    pub(crate) fn alloc_resolution_pair(
        &mut self,
        promise: ObjectId,
        track_unreachable: bool,
    ) -> Result<(ObjectId, ObjectId), VmError> {
        let resolve_id = self.heap.alloc(Object::Resolution(Resolution {
            promise,
            pair: 0,
            resolve: true,
            track_unreachable,
            called: false,
            object: PlainObject::callable(self.heap.function_prototype, 1.0, "".into()),
        }));
        let reject_id = self.heap.alloc(Object::Resolution(Resolution {
            promise,
            pair: resolve_id,
            resolve: false,
            track_unreachable,
            called: false,
            object: PlainObject::callable(self.heap.function_prototype, 1.0, "".into()),
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
        let (promise, pair, resolve, called) = match self.heap.get(id)? {
            Object::Resolution(resolution) => (
                resolution.promise,
                resolution.pair,
                resolution.resolve,
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
        if resolve {
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
                    &Value::UNDEFINED,
                )? {
                    Invocation::Value(value) => (resolve, value),
                    Invocation::Thrown(error) => (reject, error),
                }
            }
            None if rejected => (reject, argument.clone()),
            None => (resolve, argument.clone()),
        };
        let _ = self.invoke(host, roots, callback, vec![value], &Value::UNDEFINED)?;
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
            self.invoke(host, roots, function, args.to_vec(), &Value::UNDEFINED)?
        {
            let error = self.host_value(error);
            host.report_error(error);
        }
        Ok(())
    }

    fn combine_state_mut(&mut self, id: ObjectId) -> Result<&mut CombineState, VmError> {
        self.heap
            .combines
            .get_mut(&id)
            .ok_or_else(|| VmError::InvalidState(format!("missing Promise combinator state {id}")))
    }

    fn call_combine_element(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        state: ObjectId,
        index: usize,
        rejected: bool,
        argument: Value,
    ) -> Result<Invocation, VmError> {
        let data = self.combine_state_mut(state)?;
        let called = data.called.get_mut(index).ok_or_else(|| {
            VmError::InvalidState(format!("invalid Promise combinator index {index}"))
        })?;
        if *called {
            return Ok(Invocation::Value(Value::UNDEFINED));
        }
        *called = true;
        let all_settled = data.kind == CombineKind::AllSettled;
        let value = if all_settled {
            if rejected {
                self.settlement_record("rejected", ("reason", argument))?
            } else {
                self.settlement_record("fulfilled", ("value", argument))?
            }
        } else {
            argument
        };
        self.combine_state_mut(state)?.results[index] = value;
        match self.finish_combine_element(host, roots, state)? {
            Invocation::Value(_) if all_settled => Ok(Invocation::Value(Value::UNDEFINED)),
            completion => Ok(completion),
        }
    }

    // Release one remaining count, either for an element or for iterator exhaustion.
    fn finish_combine_element(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        state: ObjectId,
    ) -> Result<Invocation, VmError> {
        let data = self.combine_state_mut(state)?;
        data.remaining -= 1;
        if data.remaining != 0 {
            return Ok(Invocation::Value(Value::UNDEFINED));
        }
        let kind = data.kind;
        let capability = data.capability.clone();
        // Keep the array lengths intact for previously retained element callbacks.
        let values = data
            .results
            .iter_mut()
            .map(|value| std::mem::replace(value, Value::UNDEFINED))
            .collect();
        let value = if kind == CombineKind::Any {
            self.alloc_aggregate_error(values)?
        } else {
            Value::object(self.heap.alloc_array(values))
        };
        self.invoke(
            host,
            &roots.with(&state),
            &capability,
            vec![value],
            &Value::UNDEFINED,
        )
    }

    pub(crate) fn settlement_record(
        &mut self,
        status: &str,
        payload: (&str, Value),
    ) -> Result<Value, VmError> {
        let id = self.heap.alloc_plain();
        self.heap
            .set_own_string(id, "status", Value::string(status.into()))?;
        self.heap.set_own_string(id, payload.0, payload.1)?;
        Ok(Value::object(id))
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
