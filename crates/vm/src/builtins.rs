//! VM builtins: promises, microtasks, and object model methods.
//!
//! Script-visible `Error` and `TypeError` are objects, including VM-generated
//! TypeErrors. The `AggregateError` rejection from `Promise.any` is a string.
//!
//! `queueMicrotask` enqueues a VM microtask. It does not call the host, so
//! `Host::call` cannot re-enter the VM from this path. A throw inside that
//! callback has no result promise and is dropped: unhandled rejections are not
//! tracked here.

use std::collections::BTreeMap;

use crate::error::VmError;
use crate::heap::{
    Attributes, BoundObject, BuiltinStatic, CombineKind, CombineState, DescriptorUpdate,
    ITERATOR_SYMBOL, Object, PlainObject, PromiseState, Property, ReactionJob, Resolution,
};
use crate::host::Host;
use crate::ops;
use crate::runtime::{Invocation, Microtask, Settlement};
use crate::value::{ObjectId, PropertyKey, PropertyKeyRef, Value};
use crate::vm::Vm;

pub(crate) const ROOT_PROMISE: &str = "promise";
pub(crate) const ROOT_PROMISE_PROTOTYPE: &str = "promise.prototype";
pub(crate) const ROOT_QUEUE_MICROTASK: &str = "queueMicrotask";
pub(crate) const ROOT_OBJECT_PROTOTYPE: &str = "object.prototype";
pub(crate) const ROOT_ARRAY_PROTOTYPE: &str = "array.prototype";
pub(crate) const ROOT_ARRAY_ITERATOR_PROTOTYPE: &str = "arrayIterator.prototype";
pub(crate) const ROOT_ERROR_PROTOTYPE: &str = "error.prototype";
pub(crate) const ROOT_TYPEERROR_PROTOTYPE: &str = "typeerror.prototype";
pub(crate) const ROOT_ERROR: &str = "error";
pub(crate) const ROOT_TYPEERROR: &str = "typeerror";
pub(crate) const ROOT_SYMBOL: &str = "symbol";
pub(crate) const ROOT_REFLECT: &str = "reflect";
pub(crate) const ROOT_OBJECT: &str = "object";

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct BuiltinRoots {
    pub promise: ObjectId,
    pub promise_prototype: ObjectId,
    pub queue_microtask: ObjectId,
    pub object_prototype: ObjectId,
    pub array_prototype: ObjectId,
    pub array_iterator_prototype: ObjectId,
    pub error_prototype: ObjectId,
    pub typeerror_prototype: ObjectId,
    pub error: ObjectId,
    pub typeerror: ObjectId,
    pub symbol: ObjectId,
    pub reflect: ObjectId,
    pub object: ObjectId,
}

impl BuiltinRoots {
    pub(crate) fn to_map(self) -> BTreeMap<String, ObjectId> {
        BTreeMap::from([
            (ROOT_PROMISE.to_owned(), self.promise),
            (ROOT_PROMISE_PROTOTYPE.to_owned(), self.promise_prototype),
            (ROOT_QUEUE_MICROTASK.to_owned(), self.queue_microtask),
            (ROOT_OBJECT_PROTOTYPE.to_owned(), self.object_prototype),
            (ROOT_ARRAY_PROTOTYPE.to_owned(), self.array_prototype),
            (
                ROOT_ARRAY_ITERATOR_PROTOTYPE.to_owned(),
                self.array_iterator_prototype,
            ),
            (ROOT_ERROR_PROTOTYPE.to_owned(), self.error_prototype),
            (
                ROOT_TYPEERROR_PROTOTYPE.to_owned(),
                self.typeerror_prototype,
            ),
            (ROOT_ERROR.to_owned(), self.error),
            (ROOT_TYPEERROR.to_owned(), self.typeerror),
            (ROOT_SYMBOL.to_owned(), self.symbol),
            (ROOT_REFLECT.to_owned(), self.reflect),
            (ROOT_OBJECT.to_owned(), self.object),
        ])
    }
}

pub(crate) enum BuiltinCall {
    Constructor,
    ErrorConstructor { name: String },
    FunctionMethod { name: String },
    ObjectMethod { name: String },
    QueueMicrotask,
    Static(String),
    Method { name: String },
    Resolution { id: ObjectId },
    SymbolConstructor,
    SymbolFor,
    SymbolKeyFor,
    ArrayValues,
    ArrayIteratorNext,
    IteratorIdentity,
    ReflectOwnKeys,
    ObjectConstructor,
    ObjectKeys,
    ObjectGetOwnPropertyDescriptor,
    ObjectDefineProperty,
    ObjectDefineProperties,
}

enum Dispatch<'a> {
    Script { func_id: u32, env: ObjectId },
    Builtin(BuiltinCall),
    Bound(&'a BoundObject),
}

const PROMISE_STATICS: [&str; 6] = ["resolve", "reject", "all", "allSettled", "race", "any"];
const PROMISE_METHODS: [&str; 3] = ["then", "catch", "finally"];

impl Vm {
    pub(crate) fn init_builtins(&mut self) {
        let object_prototype = self.heap.alloc_plain();
        self.heap.object_prototype = Some(object_prototype);
        let object = self.heap.alloc_builtin("Object", None);
        self.heap
            .define_data(
                object,
                "prototype",
                Value::Object(object_prototype),
                Attributes::FIXED,
            )
            .expect("Object stores its prototype");
        self.heap
            .define_data(
                object_prototype,
                "constructor",
                Value::Object(object),
                Attributes::METHOD,
            )
            .expect("Object.prototype stores its constructor");
        for name in [
            "keys",
            "getOwnPropertyDescriptor",
            "defineProperty",
            "defineProperties",
        ] {
            let method = self.heap.alloc_builtin("Object", Some(name));
            self.heap
                .define_data(object, name, Value::Object(method), Attributes::METHOD)
                .expect("Object stores statics");
        }
        let has_own = self.heap.alloc_builtin("Object", Some("hasOwnProperty"));
        self.heap
            .define_data(
                object_prototype,
                "hasOwnProperty",
                Value::Object(has_own),
                Attributes::METHOD,
            )
            .expect("Object.prototype stores methods");

        let array_prototype = self.heap.alloc_plain();
        let array_iterator_prototype = self.heap.alloc_plain();
        self.heap.array_prototype = Some(array_prototype);
        let values = Value::Object(self.heap.alloc_builtin("Array", Some("values")));
        let next = Value::Object(self.heap.alloc_builtin("ArrayIterator", Some("next")));
        let identity = Value::Object(self.heap.alloc_builtin("Iterator", Some("identity")));
        self.heap
            .define_data(
                array_prototype,
                "values",
                values.clone(),
                Attributes::METHOD,
            )
            .expect("Array.prototype stores values");
        self.heap
            .define_data(
                array_prototype,
                PropertyKey::Symbol(ITERATOR_SYMBOL),
                values,
                Attributes::METHOD,
            )
            .expect("Array.prototype stores Symbol.iterator");
        self.heap
            .define_data(array_iterator_prototype, "next", next, Attributes::METHOD)
            .expect("Array iterator prototype stores next");
        self.heap
            .define_data(
                array_iterator_prototype,
                PropertyKey::Symbol(ITERATOR_SYMBOL),
                identity,
                Attributes::METHOD,
            )
            .expect("Array iterator prototype stores Symbol.iterator");

        let prototype = self.heap.alloc_plain();
        let promise = self.heap.alloc_builtin("Promise", None);
        let queue = self.heap.alloc_builtin("queueMicrotask", None);
        for name in PROMISE_STATICS {
            let id = self.heap.alloc_builtin("Promise", Some(name));
            self.heap
                .define_data(promise, name, Value::Object(id), Attributes::METHOD)
                .expect("Promise constructor stores statics");
        }
        for name in PROMISE_METHODS {
            let id = self.heap.alloc_builtin("Promise", Some(name));
            self.heap
                .define_data(prototype, name, Value::Object(id), Attributes::METHOD)
                .expect("Promise.prototype stores methods");
        }
        self.heap
            .define_data(
                prototype,
                "constructor",
                Value::Object(promise),
                Attributes::METHOD,
            )
            .expect("Promise.prototype.constructor");
        self.heap
            .define_data(
                promise,
                "prototype",
                Value::Object(prototype),
                Attributes::FIXED,
            )
            .expect("Promise.prototype");
        self.heap.promise_prototype = Some(prototype);

        let error_prototype = self.heap.alloc_plain();
        let typeerror_prototype = self.heap.alloc_plain_with(Some(error_prototype));
        for (id, name) in [
            (error_prototype, "Error"),
            (typeerror_prototype, "TypeError"),
        ] {
            self.heap
                .define_data(
                    id,
                    "name",
                    Value::String(name.to_owned()),
                    Attributes::METHOD,
                )
                .expect("Error prototype stores name");
        }
        self.heap
            .define_data(
                error_prototype,
                "message",
                Value::String(String::new()),
                Attributes::METHOD,
            )
            .expect("Error.prototype stores message");
        let error = self.heap.alloc_builtin("Error", None);
        let typeerror = self.heap.alloc_builtin("TypeError", None);
        let symbol = self.heap.alloc_builtin("Symbol", None);
        for name in ["for", "keyFor"] {
            let id = self.heap.alloc_builtin("Symbol", Some(name));
            self.heap
                .define_data(symbol, name, Value::Object(id), Attributes::METHOD)
                .expect("Symbol constructor stores statics");
        }
        self.heap
            .define_data(
                symbol,
                "iterator",
                Value::Symbol(ITERATOR_SYMBOL),
                Attributes::FIXED,
            )
            .expect("Symbol constructor stores the iterator symbol");
        let reflect = self.heap.alloc_plain();
        let own_keys = self.heap.alloc_builtin("Reflect", Some("ownKeys"));
        self.heap
            .define_data(
                reflect,
                "ownKeys",
                Value::Object(own_keys),
                Attributes::METHOD,
            )
            .expect("Reflect stores ownKeys");
        self.builtins = BuiltinRoots {
            promise,
            promise_prototype: prototype,
            queue_microtask: queue,
            object_prototype,
            array_prototype,
            array_iterator_prototype,
            error_prototype,
            typeerror_prototype,
            error,
            typeerror,
            symbol,
            reflect,
            object,
        };
    }

    pub(crate) fn is_callable(&self, value: &Value) -> bool {
        let Value::Object(id) = value else {
            return false;
        };
        self.heap.get(*id).is_ok_and(is_callable_object)
    }

    pub(crate) fn builtin_id(&self, builtin: u32) -> Result<ObjectId, VmError> {
        match builtin {
            0 => Ok(self.builtins.promise),
            1 => Ok(self.builtins.queue_microtask),
            2 => Ok(self.builtins.error),
            3 => Ok(self.builtins.typeerror),
            4 => Ok(self.builtins.symbol),
            5 => Ok(self.builtins.reflect),
            6 => Ok(self.builtins.object),
            _ => Err(VmError::InvalidState(format!(
                "builtin {builtin} does not exist"
            ))),
        }
    }

    pub(crate) fn materialize_function(
        &mut self,
        func_id: u32,
        env: ObjectId,
    ) -> Result<ObjectId, VmError> {
        if func_id == self.build.script {
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
        callee: &Value,
        args: Vec<Value>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        match self.classify(callee)? {
            Some(Dispatch::Script { func_id, env }) => {
                if func_id == self.build.script {
                    return Err(VmError::InvalidState(
                        "script function is not callable".to_owned(),
                    ));
                }
                self.call_function(host, func_id, Some(env), args, receiver)
            }
            Some(Dispatch::Builtin(call)) => self.call_builtin(host, call, args, receiver),
            Some(Dispatch::Bound(bound)) => {
                let target = Value::Object(bound.target);
                let bound_this = bound.bound_this.clone();
                let args = bound.args.iter().cloned().chain(args).collect();
                self.invoke(host, &target, args, &bound_this)
            }
            None => self.throw_type_error("value is not a function"),
        }
    }

    pub(crate) fn construct(
        &mut self,
        host: &mut dyn Host,
        callee: &Value,
        args: Vec<Value>,
    ) -> Result<Invocation, VmError> {
        match self.classify(callee)? {
            Some(Dispatch::Script { func_id, env }) => {
                if func_id == self.build.script {
                    return Err(VmError::InvalidState(
                        "script function is not callable".to_owned(),
                    ));
                }
                let index = self.function_index(func_id)?;
                let constructable = self.build.functions[index].is_constructable();
                if !constructable {
                    return self.throw_type_error("value is not a constructor");
                }
                let prototype_value =
                    match self.read_member(host, callee, &Value::String("prototype".to_owned()))? {
                        Invocation::Value(value) => value,
                        thrown @ Invocation::Thrown(_) => return Ok(thrown),
                    };
                let prototype = match prototype_value {
                    Value::Object(id) if self.heap.is_environment(id) != Some(true) => Some(id),
                    _ => None,
                };
                let instance = self.heap.alloc_plain_with(prototype);
                match self.call_function(
                    host,
                    func_id,
                    Some(env),
                    args,
                    &Value::Object(instance),
                )? {
                    Invocation::Thrown(value) => Ok(Invocation::Thrown(value)),
                    Invocation::Value(Value::Object(id)) => {
                        Ok(Invocation::Value(Value::Object(id)))
                    }
                    Invocation::Value(_) => Ok(Invocation::Value(Value::Object(instance))),
                }
            }
            Some(Dispatch::Builtin(BuiltinCall::Constructor)) => self.construct_promise(host, args),
            Some(Dispatch::Builtin(BuiltinCall::ObjectConstructor)) => self.construct_object(&args),
            Some(Dispatch::Builtin(BuiltinCall::ErrorConstructor { name })) => {
                self.construct_error(&name, args)
            }
            Some(Dispatch::Builtin(BuiltinCall::SymbolConstructor)) => {
                self.throw_type_error("Symbol is not a constructor")
            }
            _ => self.throw_type_error("value is not a constructor"),
        }
    }

    fn classify(&self, value: &Value) -> Result<Option<Dispatch<'_>>, VmError> {
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
            Object::BuiltinStatic(function) => static_call(function).map(Dispatch::Builtin),
            Object::Resolution(_) => Some(Dispatch::Builtin(BuiltinCall::Resolution { id: *id })),
            Object::Bound(bound) => Some(Dispatch::Bound(bound)),
            _ => None,
        })
    }

    pub(crate) fn call_builtin(
        &mut self,
        host: &mut dyn Host,
        call: BuiltinCall,
        args: Vec<Value>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        match call {
            BuiltinCall::Constructor => self.throw_type_error("Promise constructor requires new"),
            BuiltinCall::ObjectConstructor => self.construct_object(&args),
            BuiltinCall::ObjectKeys => self.object_keys(&args),
            BuiltinCall::ObjectGetOwnPropertyDescriptor => self.get_own_property_descriptor(&args),
            BuiltinCall::ObjectDefineProperty => self.object_define_property(host, &args),
            BuiltinCall::ObjectDefineProperties => self.object_define_properties(host, &args),
            BuiltinCall::ErrorConstructor { name } => self.construct_error(&name, args),
            BuiltinCall::FunctionMethod { name } => {
                self.call_function_method(host, &name, args, receiver)
            }
            BuiltinCall::ObjectMethod { name } => match name.as_str() {
                "hasOwnProperty" => {
                    let id = match *receiver {
                        Value::Object(id) if self.heap.is_environment(id) == Some(false) => id,
                        _ => {
                            return self.throw_type_error("incompatible Object.prototype receiver");
                        }
                    };
                    let key = ops::property_key(args.first().unwrap_or(&Value::Undefined))?;
                    Ok(Invocation::Value(Value::Bool(self.heap.has_own(id, &key)?)))
                }
                _ => Err(VmError::InvalidState(format!(
                    "unknown Object method {name}"
                ))),
            },
            BuiltinCall::ArrayValues => self.array_values(receiver),
            BuiltinCall::ArrayIteratorNext => self.array_iterator_next(host, receiver),
            BuiltinCall::IteratorIdentity => Ok(Invocation::Value(receiver.clone())),
            BuiltinCall::ReflectOwnKeys => {
                let Some(Value::Object(id)) = args.first() else {
                    return self.throw_type_error("Reflect.ownKeys requires an object");
                };
                let keys = self
                    .heap
                    .get(*id)?
                    .own_keys(false)
                    .map(|key| match key {
                        PropertyKey::String(key) => Value::String(key),
                        PropertyKey::Symbol(id) => Value::Symbol(id),
                    })
                    .collect();
                Ok(Invocation::Value(Value::Object(
                    self.heap.alloc_array(keys),
                )))
            }
            BuiltinCall::QueueMicrotask => self.queue_microtask(args),
            BuiltinCall::SymbolConstructor => self.symbol_constructor(args),
            BuiltinCall::SymbolFor => {
                self.symbol_static(receiver, |vm, args| vm.symbol_for(args), args)
            }
            BuiltinCall::SymbolKeyFor => {
                self.symbol_static(receiver, |vm, args| vm.symbol_key_for(args), args)
            }
            BuiltinCall::Static(name) => {
                if !matches!(receiver, Value::Object(id) if *id == self.builtins.promise) {
                    return self.throw_type_error("incompatible Promise receiver");
                }
                self.call_promise_static(host, &name, args)
            }
            BuiltinCall::Method { name } => {
                let Value::Object(id) = receiver else {
                    return self.throw_type_error("incompatible Promise receiver");
                };
                if self.heap.get(*id)?.as_promise().is_none() {
                    return self.throw_type_error("incompatible Promise receiver");
                }
                self.call_promise_method(*id, &name, args)
            }
            BuiltinCall::Resolution { id } => {
                let value = args.first().cloned().unwrap_or(Value::Undefined);
                self.capability_call(host, id, value)?;
                Ok(Invocation::Value(Value::Undefined))
            }
        }
    }

    fn array_values(&mut self, receiver: &Value) -> Result<Invocation, VmError> {
        let id = match *receiver {
            Value::Object(id) if matches!(self.heap.get(id)?, Object::Array(_)) => id,
            _ => {
                return self.throw_type_error("Array.values requires an array");
            }
        };
        let iterator = self.heap.alloc(Object::ArrayIterator {
            object: PlainObject::new(Some(self.builtins.array_iterator_prototype)),
            array: Some(id),
            next_index: 0,
        });
        Ok(Invocation::Value(Value::Object(iterator)))
    }

    fn array_iterator_next(
        &mut self,
        host: &mut dyn Host,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
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
            let Object::Array(array) = self.heap.get(source)? else {
                return Err(VmError::InvalidState(
                    "array iterator source is not an array".to_owned(),
                ));
            };
            let next = (*next_index < array.length).then_some((source, *next_index));
            let Object::ArrayIterator {
                array, next_index, ..
            } = self.heap.get_mut(id)?
            else {
                unreachable!()
            };
            if next.is_some() {
                *next_index += 1;
            } else {
                *array = None;
            }
            next
        } else {
            None
        };
        let done = next.is_none();
        let value = if let Some((source, index)) = next {
            match self.read_array_index(host, source, index)? {
                Invocation::Value(value) => value,
                thrown => return Ok(thrown),
            }
        } else {
            Value::Undefined
        };
        let result = self.heap.alloc_plain();
        self.heap.set_own_string(result, "value", value)?;
        self.heap
            .set_own_string(result, "done", Value::Bool(done))?;
        Ok(Invocation::Value(Value::Object(result)))
    }

    fn call_function_method(
        &mut self,
        host: &mut dyn Host,
        name: &str,
        args: Vec<Value>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        let target = match *receiver {
            Value::Object(id) if self.heap.function(id).is_some() => id,
            _ => {
                return self.throw_type_error("incompatible Function receiver");
            }
        };
        let mut args = args.into_iter();
        let this_arg = args.next().unwrap_or(Value::Undefined);
        match name {
            "call" => self.invoke(host, receiver, args.collect(), &this_arg),
            "apply" => {
                let args = match args.next() {
                    None | Some(Value::Undefined | Value::Null) => Vec::new(),
                    Some(Value::Object(id)) if let Object::Array(array) = self.heap.get(id)? => {
                        match self.read_array_elements(host, id, array.length)? {
                            Ok(values) => values,
                            Err(error) => return Ok(Invocation::Thrown(error)),
                        }
                    }
                    _ => return self.throw_type_error("createListFromArrayLike"),
                };
                self.invoke(host, receiver, args, &this_arg)
            }
            "bind" => {
                let id = self.heap.alloc(Object::Bound(BoundObject {
                    target,
                    bound_this: this_arg,
                    args: args.collect(),
                    object: PlainObject::new(None),
                }));
                Ok(Invocation::Value(Value::Object(id)))
            }
            _ => Err(VmError::InvalidState(format!(
                "unknown Function method {name}"
            ))),
        }
    }

    pub(crate) fn type_error(&mut self, message: &str) -> Result<Value, VmError> {
        self.alloc_error(
            self.builtins.typeerror_prototype,
            "TypeError",
            Some(message.to_owned()),
        )
    }

    pub(crate) fn throw_type_error(&mut self, message: &str) -> Result<Invocation, VmError> {
        self.type_error(message).map(Invocation::Thrown)
    }

    fn construct_object(&mut self, args: &[Value]) -> Result<Invocation, VmError> {
        match args.first() {
            None | Some(Value::Undefined | Value::Null) => {
                Ok(Invocation::Value(Value::Object(self.heap.alloc_plain())))
            }
            Some(value @ Value::Object(_)) => Ok(Invocation::Value(value.clone())),
            _ => self.throw_type_error("Object does not support primitive wrappers"),
        }
    }

    fn object_keys(&mut self, args: &[Value]) -> Result<Invocation, VmError> {
        let keys = match args.first() {
            None | Some(Value::Undefined | Value::Null) => {
                return self.throw_type_error("Object.keys requires a non-nullish value");
            }
            Some(Value::Object(id)) => self
                .heap
                .get(*id)?
                .own_keys(true)
                .map(|key| {
                    let PropertyKey::String(key) = key else {
                        unreachable!("only string keys")
                    };
                    Value::String(key)
                })
                .collect(),
            Some(Value::String(text)) => (0..text.encode_utf16().count())
                .map(|index| Value::String(index.to_string()))
                .collect(),
            _ => Vec::new(),
        };
        Ok(Invocation::Value(Value::Object(
            self.heap.alloc_array(keys),
        )))
    }

    fn get_own_property_descriptor(&mut self, args: &[Value]) -> Result<Invocation, VmError> {
        let Some(Value::Object(target)) = args.first() else {
            return self
                .throw_type_error("Object.getOwnPropertyDescriptor requires an object in this VM");
        };
        let key = ops::property_key(args.get(1).unwrap_or(&Value::Undefined))?;
        let Some(property) = self.heap.get(*target)?.own_property(key.borrowed()) else {
            return Ok(Invocation::Value(Value::Undefined));
        };
        let attributes = property.attributes();
        let property = property.into_owned();
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
        Ok(Invocation::Value(Value::Object(result)))
    }

    fn object_define_property(
        &mut self,
        host: &mut dyn Host,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let Some(Value::Object(target)) = args.first() else {
            return self.throw_type_error("Object.defineProperty requires an object target");
        };
        let key = ops::property_key(args.get(1).unwrap_or(&Value::Undefined))?;
        let update =
            match self.read_property_descriptor(host, args.get(2).unwrap_or(&Value::Undefined))? {
                Ok(update) => update,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
        self.apply_property_update(*target, key, update)
    }

    fn object_define_properties(
        &mut self,
        host: &mut dyn Host,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let Some(Value::Object(target)) = args.first() else {
            return self.throw_type_error("Object.defineProperties requires an object target");
        };
        let (properties, source) = match args.get(1) {
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
            _ => return Ok(Invocation::Value(Value::Object(*target))),
        };
        let keys: Vec<_> = self.heap.get(source)?.own_keys(false).collect();
        let mut updates = Vec::new();
        for key in keys {
            // Getters can change the enumerability of keys still to be visited.
            let Some(property) = self.heap.get(source)?.own_property(key.borrowed()) else {
                continue;
            };
            if !property.attributes().enumerable {
                continue;
            }
            let descriptor = match self.read_property(host, properties, key.borrowed())? {
                Invocation::Value(value) => value,
                thrown => return Ok(thrown),
            };
            match self.read_property_descriptor(host, &descriptor)? {
                Ok(update) => updates.push((key, update)),
                Err(error) => return Ok(Invocation::Thrown(error)),
            }
        }
        // Conversion errors precede all definitions; application errors keep earlier writes.
        for (key, update) in updates {
            if let thrown @ Invocation::Thrown(_) =
                self.apply_property_update(*target, key, update)?
            {
                return Ok(thrown);
            }
        }
        Ok(Invocation::Value(Value::Object(*target)))
    }

    fn read_property_descriptor(
        &mut self,
        host: &mut dyn Host,
        descriptor: &Value,
    ) -> Result<Result<DescriptorUpdate, Value>, VmError> {
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
            let key = PropertyKeyRef::String(name);
            if !self.heap.has_property(id, key)? {
                continue;
            }
            let value = match self.read_property(host, descriptor, key)? {
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
        target: ObjectId,
        key: PropertyKey,
        mut update: DescriptorUpdate,
    ) -> Result<Invocation, VmError> {
        if key.as_string() == Some("length")
            && matches!(self.heap.get(target)?, Object::Array(_))
            && let Some(value) = &mut update.value
        {
            let length = match ops::to_number(value) {
                Ok(length) => length,
                Err(_) if matches!(value, Value::String(_)) => f64::NAN,
                Err(_) => return self.throw_type_error("cannot convert array length to a number"),
            };
            if !(0.0..=u32::MAX as f64).contains(&length) || length.fract() != 0.0 {
                let error = self.alloc_error(
                    self.builtins.error_prototype,
                    "RangeError",
                    Some("Invalid array length".to_owned()),
                )?;
                let Value::Object(id) = error else {
                    unreachable!()
                };
                self.heap.define_data(
                    id,
                    "name",
                    Value::String("RangeError".to_owned()),
                    Attributes::METHOD,
                )?;
                return Ok(Invocation::Thrown(error));
            }
            *value = Value::Number((length as u32) as f64);
        }
        if self.heap.apply_descriptor(target, key, update)? {
            Ok(Invocation::Value(Value::Object(target)))
        } else {
            self.throw_type_error("Cannot define property")
        }
    }

    fn construct_error(&mut self, name: &str, args: Vec<Value>) -> Result<Invocation, VmError> {
        let prototype = match name {
            "Error" => self.builtins.error_prototype,
            "TypeError" => self.builtins.typeerror_prototype,
            _ => {
                return Err(VmError::InvalidState(format!(
                    "unknown Error constructor {name}"
                )));
            }
        };
        let message = match args.first() {
            Some(value) if *value != Value::Undefined => Some(ops::to_js_string(value)),
            _ => None,
        };
        self.alloc_error(prototype, name, message)
            .map(Invocation::Value)
    }

    fn alloc_error(
        &mut self,
        prototype: ObjectId,
        name: &str,
        message: Option<String>,
    ) -> Result<Value, VmError> {
        let id = self.heap.alloc_plain_with(Some(prototype));
        let stack = format!("{name}: {}", message.as_deref().unwrap_or_default());
        if let Some(message) = message {
            self.heap
                .define_data(id, "message", Value::String(message), Attributes::METHOD)?;
        }
        self.heap
            .define_data(id, "stack", Value::String(stack), Attributes::METHOD)?;
        Ok(Value::Object(id))
    }

    fn construct_promise(
        &mut self,
        host: &mut dyn Host,
        args: Vec<Value>,
    ) -> Result<Invocation, VmError> {
        let executor = args.first().cloned().unwrap_or(Value::Undefined);
        if !self.is_callable(&executor) {
            return self.throw_type_error("Promise executor is not a function");
        }
        let promise = self.heap.alloc_promise();
        let (resolve_id, reject_id) = self.alloc_capability_pair(promise)?;
        let invocation = self.invoke(
            host,
            &executor,
            vec![Value::Object(resolve_id), Value::Object(reject_id)],
            &Value::Undefined,
        )?;
        if let Invocation::Thrown(error) = invocation {
            self.capability_call(host, reject_id, error)?;
        }
        Ok(Invocation::Value(Value::Object(promise)))
    }

    fn symbol_static(
        &mut self,
        receiver: &Value,
        call: impl FnOnce(&mut Self, Vec<Value>) -> Result<Invocation, VmError>,
        args: Vec<Value>,
    ) -> Result<Invocation, VmError> {
        if !matches!(receiver, Value::Object(id) if *id == self.builtins.symbol) {
            return self.throw_type_error("incompatible Symbol receiver");
        }
        call(self, args)
    }

    /// `Symbol(desc)`: every call produces a fresh identity.
    fn symbol_constructor(&mut self, args: Vec<Value>) -> Result<Invocation, VmError> {
        let description = match args.first() {
            None | Some(Value::Undefined) => None,
            Some(Value::Symbol(_)) => {
                return self.throw_type_error(ops::SYMBOL_TO_STRING);
            }
            Some(value) => Some(ops::to_js_string(value)),
        };
        let id = self.heap.alloc_symbol(description)?;
        Ok(Invocation::Value(Value::Symbol(id)))
    }

    fn symbol_for(&mut self, args: Vec<Value>) -> Result<Invocation, VmError> {
        let key = match args.first() {
            None | Some(Value::Undefined) => "undefined".to_owned(),
            Some(Value::Symbol(_)) => {
                return self.throw_type_error(ops::SYMBOL_TO_STRING);
            }
            Some(value) => ops::to_js_string(value),
        };
        let id = self.heap.intern_symbol(key)?;
        Ok(Invocation::Value(Value::Symbol(id)))
    }

    fn symbol_key_for(&mut self, args: Vec<Value>) -> Result<Invocation, VmError> {
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

    fn queue_microtask(&mut self, args: Vec<Value>) -> Result<Invocation, VmError> {
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

    fn call_promise_static(
        &mut self,
        host: &mut dyn Host,
        name: &str,
        args: Vec<Value>,
    ) -> Result<Invocation, VmError> {
        match name {
            "resolve" => {
                let value = args.first().cloned().unwrap_or(Value::Undefined);
                let promise = self.heap.alloc_promise();
                self.resolve_promise(host, promise, value)?;
                Ok(Invocation::Value(Value::Object(promise)))
            }
            "reject" => {
                let reason = args.first().cloned().unwrap_or(Value::Undefined);
                let promise = self.heap.alloc_promise();
                self.settle(promise, Settlement::Rejected(reason))?;
                Ok(Invocation::Value(Value::Object(promise)))
            }
            "all" => self.combine(host, CombineKind::All, args),
            "allSettled" => self.combine(host, CombineKind::AllSettled, args),
            "race" => self.combine(host, CombineKind::Race, args),
            "any" => self.combine(host, CombineKind::Any, args),
            _ => self.throw_type_error(&format!("Promise.{name} is not a function")),
        }
    }

    fn call_promise_method(
        &mut self,
        receiver: ObjectId,
        name: &str,
        args: Vec<Value>,
    ) -> Result<Invocation, VmError> {
        if self.heap.promise(receiver).is_err() {
            return self.throw_type_error("Promise method called on a non-promise");
        }
        match name {
            "then" => self.promise_then(receiver, args),
            "catch" => {
                let rejected = args.first().cloned();
                self.promise_then(
                    receiver,
                    vec![Value::Undefined, rejected.unwrap_or(Value::Undefined)],
                )
            }
            "finally" => self.promise_finally(receiver, args),
            _ => self.throw_type_error(&format!("Promise.{name} is not a function")),
        }
    }

    fn promise_then(
        &mut self,
        receiver: ObjectId,
        args: Vec<Value>,
    ) -> Result<Invocation, VmError> {
        let on_fulfilled = self.handler_arg(args.first());
        let on_rejected = self.handler_arg(args.get(1));
        let result = self.heap.alloc_promise();
        self.react(
            receiver,
            ReactionJob::Script {
                result,
                on_fulfilled,
                on_rejected,
            },
        )?;
        Ok(Invocation::Value(Value::Object(result)))
    }

    fn promise_finally(
        &mut self,
        receiver: ObjectId,
        args: Vec<Value>,
    ) -> Result<Invocation, VmError> {
        let on_finally = args.first().cloned().unwrap_or(Value::Undefined);
        let result = self.heap.alloc_promise();
        self.react(receiver, ReactionJob::Finalize { result, on_finally })?;
        Ok(Invocation::Value(Value::Object(result)))
    }

    fn handler_arg(&self, value: Option<&Value>) -> Option<Value> {
        value.filter(|value| self.is_callable(value)).cloned()
    }

    fn combine(
        &mut self,
        host: &mut dyn Host,
        kind: CombineKind,
        args: Vec<Value>,
    ) -> Result<Invocation, VmError> {
        let iterable = args.first().cloned().unwrap_or(Value::Undefined);
        let Value::Object(id) = iterable else {
            return self.throw_type_error("Promise combinator iterable is not an array");
        };
        let Object::Array(array) = self.heap.get(id)? else {
            return self.throw_type_error("Promise combinator iterable is not an array");
        };
        let elements = self.read_array_elements(host, id, array.length)?;
        let result = self.heap.alloc_promise();
        let elements = match elements {
            Ok(elements) => elements,
            Err(error) => {
                self.settle(result, Settlement::Rejected(error))?;
                return Ok(Invocation::Value(Value::Object(result)));
            }
        };
        if elements.is_empty() {
            match kind {
                CombineKind::All | CombineKind::AllSettled => {
                    let array = self.heap.alloc_array(Vec::new());
                    self.settle(result, Settlement::Fulfilled(Value::Object(array)))?;
                }
                CombineKind::Race => {}
                CombineKind::Any => {
                    self.settle(result, Settlement::Rejected(aggregate_error()))?;
                }
            }
            return Ok(Invocation::Value(Value::Object(result)));
        }
        let count = elements.len();
        self.heap.combines.insert(
            result,
            CombineState {
                kind,
                count,
                remaining: count,
                results: vec![Value::Undefined; count],
                states: if kind == CombineKind::AllSettled {
                    vec![PromiseState::Pending; count]
                } else {
                    Vec::new()
                },
                first: None,
                elements: Vec::with_capacity(count),
            },
        );
        for (index, element) in elements.into_iter().enumerate() {
            let slot = self.heap.alloc_promise();
            self.heap
                .combines
                .get_mut(&result)
                .expect("combine state inserted above")
                .elements
                .push(slot);
            self.react(
                slot,
                ReactionJob::Combine {
                    result,
                    index,
                    kind,
                },
            )?;
            self.resolve_promise(host, slot, element)?;
        }
        Ok(Invocation::Value(Value::Object(result)))
    }

    fn read_array_elements(
        &mut self,
        host: &mut dyn Host,
        id: ObjectId,
        length: usize,
    ) -> Result<Result<Vec<Value>, Value>, VmError> {
        let mut values = Vec::new();
        for index in 0..length {
            match self.read_array_index(host, id, index)? {
                Invocation::Value(value) => values.push(value),
                Invocation::Thrown(error) => return Ok(Err(error)),
            }
        }
        Ok(Ok(values))
    }

    pub(crate) fn alloc_capability_pair(
        &mut self,
        promise: ObjectId,
    ) -> Result<(ObjectId, ObjectId), VmError> {
        let resolve_id = self.heap.alloc(Object::Resolution(Resolution {
            promise,
            pair: 0,
            resolve: true,
            called: false,
            object: PlainObject::new(None),
        }));
        let reject_id = self.heap.alloc(Object::Resolution(Resolution {
            promise,
            pair: resolve_id,
            resolve: false,
            called: false,
            object: PlainObject::new(None),
        }));
        match self.heap.get_mut(resolve_id)? {
            Object::Resolution(resolution) => resolution.pair = reject_id,
            _ => {
                return Err(VmError::InvalidState(
                    "capability allocation did not produce a Resolution".to_owned(),
                ));
            }
        }
        self.heap.resolutions.insert(resolve_id);
        self.heap.resolutions.insert(reject_id);
        Ok((resolve_id, reject_id))
    }

    pub(crate) fn capability_call(
        &mut self,
        host: &mut dyn Host,
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
            self.resolve_promise(host, promise, value)
        } else {
            self.reject_if_open(promise, value)
        }
    }

    fn resolution_called(&self, id: ObjectId) -> Result<bool, VmError> {
        match self.heap.get(id)? {
            Object::Resolution(resolution) => Ok(resolution.called),
            _ => Err(VmError::InvalidState(format!(
                "object {id} is not a resolution function"
            ))),
        }
    }

    fn mark_resolution_called(&mut self, id: ObjectId) -> Result<(), VmError> {
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
        handler: Option<Value>,
        argument: Value,
        result: ObjectId,
        rejected: bool,
    ) -> Result<(), VmError> {
        let handler = handler.filter(|value| self.is_callable(value));
        let Some(handler) = handler else {
            if rejected {
                return self.settle(result, Settlement::Rejected(argument));
            }
            return self.resolve_promise(host, result, argument);
        };
        match self.invoke(host, &handler, vec![argument], &Value::Undefined)? {
            Invocation::Thrown(error) => self.settle(result, Settlement::Rejected(error)),
            Invocation::Value(value) => self.resolve_promise(host, result, value),
        }
    }

    pub(crate) fn run_queue_call(
        &mut self,
        host: &mut dyn Host,
        function: Value,
        args: Vec<Value>,
    ) -> Result<(), VmError> {
        let _ = self.invoke(host, &function, args, &Value::Undefined)?;
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
                    if state.first.is_none() {
                        state.first = Some(reason);
                    }
                    if state.remaining == 0 && pending {
                        self.heap.combines.remove(&result);
                        self.settle(result, Settlement::Rejected(aggregate_error()))?;
                        return Ok(());
                    }
                }
            },
        }
        self.heap.combines.insert(result, state);
        Ok(())
    }

    fn settlement_record(
        &mut self,
        status: &str,
        payload: (&str, Value),
    ) -> Result<Value, VmError> {
        let id = self.heap.alloc_plain();
        self.heap
            .set_own_string(id, "status", Value::String(status.to_owned()))?;
        self.heap.set_own_string(id, payload.0, payload.1)?;
        Ok(Value::Object(id))
    }
}

pub(crate) fn is_callable_object(object: &Object) -> bool {
    match object {
        Object::Function(_) | Object::Resolution(_) | Object::Bound(_) => true,
        Object::BuiltinStatic(function) => static_call(function).is_some(),
        _ => false,
    }
}

fn static_call(function: &BuiltinStatic) -> Option<BuiltinCall> {
    match (function.name.as_str(), function.member.as_deref()) {
        ("Promise", None) => Some(BuiltinCall::Constructor),
        ("queueMicrotask", None) => Some(BuiltinCall::QueueMicrotask),
        (name @ ("Error" | "TypeError"), None) => Some(BuiltinCall::ErrorConstructor {
            name: name.to_owned(),
        }),
        ("Function", Some(name @ ("call" | "apply" | "bind"))) => {
            Some(BuiltinCall::FunctionMethod {
                name: name.to_owned(),
            })
        }
        ("Object", Some(name @ "hasOwnProperty")) => Some(BuiltinCall::ObjectMethod {
            name: name.to_owned(),
        }),
        ("Object", None) => Some(BuiltinCall::ObjectConstructor),
        ("Object", Some("keys")) => Some(BuiltinCall::ObjectKeys),
        ("Object", Some("getOwnPropertyDescriptor")) => {
            Some(BuiltinCall::ObjectGetOwnPropertyDescriptor)
        }
        ("Object", Some("defineProperty")) => Some(BuiltinCall::ObjectDefineProperty),
        ("Object", Some("defineProperties")) => Some(BuiltinCall::ObjectDefineProperties),
        ("Array", Some("values")) => Some(BuiltinCall::ArrayValues),
        ("ArrayIterator", Some("next")) => Some(BuiltinCall::ArrayIteratorNext),
        ("Iterator", Some("identity")) => Some(BuiltinCall::IteratorIdentity),
        ("Reflect", Some("ownKeys")) => Some(BuiltinCall::ReflectOwnKeys),
        ("Promise", Some(member)) if PROMISE_STATICS.contains(&member) => {
            Some(BuiltinCall::Static(member.to_owned()))
        }
        ("Promise", Some(member)) if PROMISE_METHODS.contains(&member) => {
            Some(BuiltinCall::Method {
                name: member.to_owned(),
            })
        }
        ("Symbol", None) => Some(BuiltinCall::SymbolConstructor),
        ("Symbol", Some("for")) => Some(BuiltinCall::SymbolFor),
        ("Symbol", Some("keyFor")) => Some(BuiltinCall::SymbolKeyFor),
        _ => None,
    }
}

fn aggregate_error() -> Value {
    Value::String("AggregateError: All promises were rejected".to_owned())
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
