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
    ITERATOR_SYMBOL, IntegrityLevel, Object, PlainObject, PromiseState, Property, ReactionJob,
    Resolution, TO_PRIMITIVE_SYMBOL,
};
use crate::host::Host;
use crate::number::{NUMBER_METHODS, NUMBER_STATICS};
use crate::ops;
use crate::runtime::{Invocation, Microtask, Settlement};
use crate::string::{StringMethod, StringStatic};
use crate::value::{ObjectId, PropertyKey, PropertyKeyRef, Value};
use crate::vm::Vm;

pub(crate) const ROOT_PROMISE: &str = "promise";
pub(crate) const ROOT_PROMISE_PROTOTYPE: &str = "promise.prototype";
pub(crate) const ROOT_QUEUE_MICROTASK: &str = "queueMicrotask";
pub(crate) const ROOT_OBJECT_PROTOTYPE: &str = "object.prototype";
pub(crate) const ROOT_ARRAY: &str = "array";
pub(crate) const ROOT_ARRAY_PROTOTYPE: &str = "array.prototype";
pub(crate) const ROOT_ARRAY_VALUES: &str = "array.values";
pub(crate) const ROOT_STRING: &str = "string";
pub(crate) const ROOT_STRING_PROTOTYPE: &str = "string.prototype";
pub(crate) const ROOT_NUMBER: &str = "number";
pub(crate) const ROOT_NUMBER_PROTOTYPE: &str = "number.prototype";
pub(crate) const ROOT_FUNCTION_PROTOTYPE: &str = "function.prototype";
pub(crate) const ROOT_ARRAY_ITERATOR_PROTOTYPE: &str = "arrayIterator.prototype";
pub(crate) const ROOT_STRING_ITERATOR: &str = "string.iterator";
pub(crate) const ROOT_STRING_ITERATOR_PROTOTYPE: &str = "stringIterator.prototype";
pub(crate) const ROOT_ERROR_PROTOTYPE: &str = "error.prototype";
pub(crate) const ROOT_TYPEERROR_PROTOTYPE: &str = "typeerror.prototype";
pub(crate) const ROOT_ERROR: &str = "error";
pub(crate) const ROOT_TYPEERROR: &str = "typeerror";
pub(crate) const ROOT_RANGEERROR: &str = "rangeerror";
pub(crate) const ROOT_RANGEERROR_PROTOTYPE: &str = "rangeerror.prototype";
pub(crate) const ROOT_REFERENCEERROR: &str = "referenceerror";
pub(crate) const ROOT_REFERENCEERROR_PROTOTYPE: &str = "referenceerror.prototype";
pub(crate) const ROOT_SYNTAXERROR: &str = "syntaxerror";
pub(crate) const ROOT_SYNTAXERROR_PROTOTYPE: &str = "syntaxerror.prototype";
pub(crate) const ROOT_EVALERROR: &str = "evalerror";
pub(crate) const ROOT_EVALERROR_PROTOTYPE: &str = "evalerror.prototype";
pub(crate) const ROOT_URIERROR: &str = "urierror";
pub(crate) const ROOT_URIERROR_PROTOTYPE: &str = "urierror.prototype";
pub(crate) const ROOT_AGGREGATEERROR: &str = "aggregateerror";
pub(crate) const ROOT_AGGREGATEERROR_PROTOTYPE: &str = "aggregateerror.prototype";
pub(crate) const ROOT_SYMBOL: &str = "symbol";
pub(crate) const ROOT_REFLECT: &str = "reflect";
pub(crate) const ROOT_OBJECT: &str = "object";

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct BuiltinRoots {
    pub promise: ObjectId,
    pub promise_prototype: ObjectId,
    pub queue_microtask: ObjectId,
    pub object_prototype: ObjectId,
    pub array: ObjectId,
    pub array_prototype: ObjectId,
    pub array_values: ObjectId,
    pub string: ObjectId,
    pub string_prototype: ObjectId,
    pub number: ObjectId,
    pub number_prototype: ObjectId,
    pub function_prototype: ObjectId,
    pub array_iterator_prototype: ObjectId,
    pub string_iterator: ObjectId,
    pub string_iterator_prototype: ObjectId,
    pub error_prototype: ObjectId,
    pub typeerror_prototype: ObjectId,
    pub error: ObjectId,
    pub typeerror: ObjectId,
    pub rangeerror_prototype: ObjectId,
    pub referenceerror_prototype: ObjectId,
    pub syntaxerror_prototype: ObjectId,
    pub evalerror_prototype: ObjectId,
    pub urierror_prototype: ObjectId,
    pub aggregateerror_prototype: ObjectId,
    pub rangeerror: ObjectId,
    pub referenceerror: ObjectId,
    pub syntaxerror: ObjectId,
    pub evalerror: ObjectId,
    pub urierror: ObjectId,
    pub aggregateerror: ObjectId,
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
            (ROOT_ARRAY.to_owned(), self.array),
            (ROOT_ARRAY_PROTOTYPE.to_owned(), self.array_prototype),
            (ROOT_ARRAY_VALUES.to_owned(), self.array_values),
            (ROOT_STRING.to_owned(), self.string),
            (ROOT_STRING_PROTOTYPE.to_owned(), self.string_prototype),
            (ROOT_NUMBER.to_owned(), self.number),
            (ROOT_NUMBER_PROTOTYPE.to_owned(), self.number_prototype),
            (ROOT_FUNCTION_PROTOTYPE.to_owned(), self.function_prototype),
            (
                ROOT_ARRAY_ITERATOR_PROTOTYPE.to_owned(),
                self.array_iterator_prototype,
            ),
            (ROOT_STRING_ITERATOR.to_owned(), self.string_iterator),
            (
                ROOT_STRING_ITERATOR_PROTOTYPE.to_owned(),
                self.string_iterator_prototype,
            ),
            (ROOT_ERROR_PROTOTYPE.to_owned(), self.error_prototype),
            (
                ROOT_TYPEERROR_PROTOTYPE.to_owned(),
                self.typeerror_prototype,
            ),
            (ROOT_ERROR.to_owned(), self.error),
            (ROOT_TYPEERROR.to_owned(), self.typeerror),
            (ROOT_RANGEERROR.to_owned(), self.rangeerror),
            (
                ROOT_RANGEERROR_PROTOTYPE.to_owned(),
                self.rangeerror_prototype,
            ),
            (ROOT_REFERENCEERROR.to_owned(), self.referenceerror),
            (
                ROOT_REFERENCEERROR_PROTOTYPE.to_owned(),
                self.referenceerror_prototype,
            ),
            (ROOT_SYNTAXERROR.to_owned(), self.syntaxerror),
            (
                ROOT_SYNTAXERROR_PROTOTYPE.to_owned(),
                self.syntaxerror_prototype,
            ),
            (ROOT_EVALERROR.to_owned(), self.evalerror),
            (
                ROOT_EVALERROR_PROTOTYPE.to_owned(),
                self.evalerror_prototype,
            ),
            (ROOT_URIERROR.to_owned(), self.urierror),
            (ROOT_URIERROR_PROTOTYPE.to_owned(), self.urierror_prototype),
            (ROOT_AGGREGATEERROR.to_owned(), self.aggregateerror),
            (
                ROOT_AGGREGATEERROR_PROTOTYPE.to_owned(),
                self.aggregateerror_prototype,
            ),
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
    ArrayConstructor,
    ArrayOperation(String),
    ArrayValues,
    ArrayIteratorNext,
    StringConstructor,
    StringMethod(StringMethod),
    StringStatic(StringStatic),
    NumberConstructor,
    NumberMethod(&'static str),
    NumberStatic(&'static str),
    StringIterator,
    StringIteratorNext,
    ArrayJoin,
    ArrayToString,
    IteratorIdentity,
    ReflectApply,
    ReflectConstruct,
    ReflectOwnKeys,
    ReflectDeleteProperty,
    ReflectGet,
    ReflectSet,
    ReflectHas,
    ReflectGetOwnPropertyDescriptor,
    ObjectConstructor,
    ObjectCreate,
    GetPrototypeOf { reflect: bool },
    SetPrototypeOf { reflect: bool },
    ObjectKeys,
    ObjectGetOwnPropertyDescriptor,
    ObjectGetOwnPropertyDescriptors,
    DefineProperty { reflect: bool },
    ObjectDefineProperties,
    PreventExtensions { reflect: bool },
    IsExtensible { reflect: bool },
    SetIntegrityLevel(IntegrityLevel),
    TestIntegrityLevel(IntegrityLevel),
    ObjectAssign,
    ObjectIs,
}

enum Dispatch<'a> {
    Script { func_id: u32, env: ObjectId },
    Builtin(BuiltinCall),
    Bound(&'a BoundObject),
}

enum Constructor {
    Script { func_id: u32, env: ObjectId },
    Promise,
    Object,
    Array,
    Error { name: String },
}

const PROMISE_STATICS: [&str; 6] = ["resolve", "reject", "all", "allSettled", "race", "any"];
const PROMISE_METHODS: [&str; 3] = ["then", "catch", "finally"];

const STRING_METHODS: &[(&str, StringMethod)] = &[
    ("charAt", StringMethod::CharAt),
    ("charCodeAt", StringMethod::CharCodeAt),
    ("codePointAt", StringMethod::CodePointAt),
    ("at", StringMethod::At),
    ("indexOf", StringMethod::IndexOf),
    ("lastIndexOf", StringMethod::LastIndexOf),
    ("includes", StringMethod::Includes),
    ("startsWith", StringMethod::StartsWith),
    ("endsWith", StringMethod::EndsWith),
    ("slice", StringMethod::Slice),
    ("substring", StringMethod::Substring),
    ("toUpperCase", StringMethod::ToUpperCase),
    ("toLowerCase", StringMethod::ToLowerCase),
    ("trim", StringMethod::Trim),
    ("trimStart", StringMethod::TrimStart),
    ("trimEnd", StringMethod::TrimEnd),
    ("split", StringMethod::Split),
    ("concat", StringMethod::Concat),
    ("repeat", StringMethod::Repeat),
    ("padStart", StringMethod::PadStart),
    ("padEnd", StringMethod::PadEnd),
    ("replace", StringMethod::Replace),
    ("toString", StringMethod::ToString),
    ("valueOf", StringMethod::ValueOf),
];
const STRING_STATICS: &[(&str, StringStatic)] = &[
    ("fromCharCode", StringStatic::FromCharCode),
    ("fromCodePoint", StringStatic::FromCodePoint),
    ("raw", StringStatic::Raw),
];

impl Vm {
    fn init_string_constructor(&mut self, prototype: ObjectId, iterator: ObjectId) -> ObjectId {
        let constructor = self.heap.alloc_builtin("String", None);
        self.heap
            .define_data(
                constructor,
                "prototype",
                Value::Object(prototype),
                Attributes::FIXED,
            )
            .expect("String stores its prototype");
        self.heap
            .define_data(
                prototype,
                "constructor",
                Value::Object(constructor),
                Attributes::METHOD,
            )
            .expect("String.prototype stores its constructor");
        self.heap
            .define_data(
                prototype,
                PropertyKey::Symbol(ITERATOR_SYMBOL),
                Value::Object(iterator),
                Attributes::METHOD,
            )
            .expect("String.prototype stores Symbol.iterator");
        for name in STRING_METHODS.iter().map(|(name, _)| *name) {
            let method = self.heap.alloc_builtin("String", Some(name));
            self.heap
                .define_data(prototype, name, Value::Object(method), Attributes::METHOD)
                .expect("String.prototype stores methods");
        }
        for name in STRING_STATICS.iter().map(|(name, _)| *name) {
            let method = self.heap.alloc_builtin("String", Some(name));
            self.heap
                .define_data(constructor, name, Value::Object(method), Attributes::METHOD)
                .expect("String stores statics");
        }
        constructor
    }

    pub(crate) fn init_builtins(&mut self) {
        let object_prototype = self.heap.alloc_plain();
        self.heap.object_prototype = Some(object_prototype);
        let function_prototype = self.heap.alloc_plain();
        self.heap.function_prototype = Some(function_prototype);
        let to_string = Value::Object(self.heap.alloc_builtin("Function", Some("toString")));
        self.heap
            .define_data(
                function_prototype,
                "toString",
                to_string,
                Attributes::METHOD,
            )
            .expect("Function.prototype stores toString");
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
            "assign",
            "is",
            "keys",
            "create",
            "getPrototypeOf",
            "setPrototypeOf",
            "getOwnPropertyDescriptor",
            "getOwnPropertyDescriptors",
            "defineProperty",
            "defineProperties",
            "preventExtensions",
            "isExtensible",
            "seal",
            "isSealed",
            "freeze",
            "isFrozen",
        ] {
            let method = self.heap.alloc_builtin("Object", Some(name));
            self.heap
                .define_data(object, name, Value::Object(method), Attributes::METHOD)
                .expect("Object stores statics");
        }
        for name in ["hasOwnProperty", "valueOf", "toString"] {
            let method = self.heap.alloc_builtin("Object", Some(name));
            self.heap
                .define_data(
                    object_prototype,
                    name,
                    Value::Object(method),
                    Attributes::METHOD,
                )
                .expect("Object.prototype stores methods");
        }

        let array_prototype = self.heap.alloc_array(Vec::new());
        self.heap
            .set_prototype(array_prototype, Some(object_prototype))
            .expect("Array.prototype inherits Object.prototype");
        let array = self.init_array_constructor(array_prototype);
        let array_iterator_prototype = self.heap.alloc_plain();
        self.heap.array_prototype = Some(array_prototype);
        let join = Value::Object(self.heap.alloc_builtin("Array", Some("join")));
        let to_string = Value::Object(self.heap.alloc_builtin("Array", Some("toString")));
        self.heap
            .define_data(array_prototype, "join", join, Attributes::METHOD)
            .expect("Array.prototype stores join");
        self.heap
            .define_data(array_prototype, "toString", to_string, Attributes::METHOD)
            .expect("Array.prototype stores toString");
        let array_values = self.heap.alloc_builtin("Array", Some("values"));
        let values = Value::Object(array_values);
        let string_iterator_prototype = self.heap.alloc_plain();
        let string_iterator = self.heap.alloc_builtin("String", Some("iterator"));
        let string_prototype = self.heap.alloc_plain();
        let string = self.init_string_constructor(string_prototype, string_iterator);
        let number_prototype = self.heap.alloc_plain();
        let number = self.init_number_constructor(number_prototype);
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
        for (prototype, kind) in [
            (array_iterator_prototype, "ArrayIterator"),
            (string_iterator_prototype, "StringIterator"),
        ] {
            let next = Value::Object(self.heap.alloc_builtin(kind, Some("next")));
            self.heap
                .define_data(prototype, "next", next, Attributes::METHOD)
                .expect("iterator prototype stores next");
            self.heap
                .define_data(
                    prototype,
                    PropertyKey::Symbol(ITERATOR_SYMBOL),
                    identity.clone(),
                    Attributes::METHOD,
                )
                .expect("iterator prototype stores Symbol.iterator");
        }

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
        let rangeerror_prototype = self.heap.alloc_plain_with(Some(error_prototype));
        let referenceerror_prototype = self.heap.alloc_plain_with(Some(error_prototype));
        let syntaxerror_prototype = self.heap.alloc_plain_with(Some(error_prototype));
        let evalerror_prototype = self.heap.alloc_plain_with(Some(error_prototype));
        let urierror_prototype = self.heap.alloc_plain_with(Some(error_prototype));
        let aggregateerror_prototype = self.heap.alloc_plain_with(Some(error_prototype));
        let constructors = [
            ("Error", error_prototype),
            ("TypeError", typeerror_prototype),
            ("RangeError", rangeerror_prototype),
            ("ReferenceError", referenceerror_prototype),
            ("SyntaxError", syntaxerror_prototype),
            ("EvalError", evalerror_prototype),
            ("URIError", urierror_prototype),
            ("AggregateError", aggregateerror_prototype),
        ];
        let mut error_constructors = BTreeMap::new();
        for (name, prototype) in constructors {
            self.heap
                .define_data(
                    prototype,
                    "name",
                    Value::String(name.to_owned()),
                    Attributes::METHOD,
                )
                .expect("Error prototype stores name");
            if name == "Error" {
                self.heap
                    .define_data(
                        prototype,
                        "message",
                        Value::String(String::new()),
                        Attributes::METHOD,
                    )
                    .expect("Error.prototype stores message");
            }
            let constructor = self.heap.alloc_builtin(name, None);
            self.heap
                .define_data(
                    constructor,
                    "prototype",
                    Value::Object(prototype),
                    Attributes::FIXED,
                )
                .expect("error constructor stores its prototype");
            self.heap
                .define_data(
                    prototype,
                    "constructor",
                    Value::Object(constructor),
                    Attributes::METHOD,
                )
                .expect("Error prototype stores its constructor");
            error_constructors.insert(name, constructor);
        }
        let error = error_constructors["Error"];
        let typeerror = error_constructors["TypeError"];
        let rangeerror = error_constructors["RangeError"];
        let referenceerror = error_constructors["ReferenceError"];
        let syntaxerror = error_constructors["SyntaxError"];
        let evalerror = error_constructors["EvalError"];
        let urierror = error_constructors["URIError"];
        let aggregateerror = error_constructors["AggregateError"];
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
        self.heap
            .define_data(
                symbol,
                "toPrimitive",
                Value::Symbol(TO_PRIMITIVE_SYMBOL),
                Attributes::FIXED,
            )
            .expect("Symbol constructor stores the toPrimitive symbol");
        let reflect = self.heap.alloc_plain();
        for name in [
            "ownKeys",
            "deleteProperty",
            "defineProperty",
            "preventExtensions",
            "isExtensible",
            "getPrototypeOf",
            "setPrototypeOf",
            "get",
            "set",
            "has",
            "getOwnPropertyDescriptor",
            "apply",
            "construct",
        ] {
            let method = self.heap.alloc_builtin("Reflect", Some(name));
            self.heap
                .define_data(reflect, name, Value::Object(method), Attributes::METHOD)
                .expect("Reflect stores methods");
        }
        self.builtins = BuiltinRoots {
            promise,
            promise_prototype: prototype,
            queue_microtask: queue,
            object_prototype,
            array,
            array_prototype,
            array_values,
            string,
            string_prototype,
            number,
            number_prototype,
            function_prototype,
            array_iterator_prototype,
            string_iterator,
            string_iterator_prototype,
            error_prototype,
            typeerror_prototype,
            error,
            typeerror,
            rangeerror_prototype,
            referenceerror_prototype,
            syntaxerror_prototype,
            evalerror_prototype,
            urierror_prototype,
            aggregateerror_prototype,
            rangeerror,
            referenceerror,
            syntaxerror,
            evalerror,
            urierror,
            aggregateerror,
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

    pub(crate) fn builtin_value(&self, builtin: u32) -> Result<Value, VmError> {
        match builtin {
            0 => Ok(Value::Object(self.builtins.promise)),
            1 => Ok(Value::Object(self.builtins.queue_microtask)),
            2 => Ok(Value::Object(self.builtins.error)),
            3 => Ok(Value::Object(self.builtins.typeerror)),
            4 => Ok(Value::Object(self.builtins.symbol)),
            5 => Ok(Value::Object(self.builtins.reflect)),
            6 => Ok(Value::Object(self.builtins.object)),
            7 => Ok(Value::Number(f64::NAN)),
            8 => Ok(Value::Number(f64::INFINITY)),
            9 => Ok(Value::Object(self.builtins.array)),
            10 => Ok(Value::Object(self.builtins.string)),
            11 => Ok(Value::Object(self.builtins.rangeerror)),
            12 => Ok(Value::Object(self.builtins.referenceerror)),
            13 => Ok(Value::Object(self.builtins.syntaxerror)),
            14 => Ok(Value::Object(self.builtins.evalerror)),
            15 => Ok(Value::Object(self.builtins.urierror)),
            16 => Ok(Value::Object(self.builtins.aggregateerror)),
            17 => Ok(Value::Object(self.builtins.number)),
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

    fn constructor(&self, value: &Value) -> Result<Option<Constructor>, VmError> {
        Ok(match self.classify(value)? {
            Some(Dispatch::Script { func_id, env })
                if func_id != self.build.script
                    && self.build.functions[self.function_index(func_id)?].is_constructable() =>
            {
                Some(Constructor::Script { func_id, env })
            }
            Some(Dispatch::Builtin(BuiltinCall::Constructor)) => Some(Constructor::Promise),
            Some(Dispatch::Builtin(BuiltinCall::ObjectConstructor)) => Some(Constructor::Object),
            Some(Dispatch::Builtin(BuiltinCall::ArrayConstructor)) => Some(Constructor::Array),
            Some(Dispatch::Builtin(BuiltinCall::ErrorConstructor { name })) => {
                Some(Constructor::Error { name })
            }
            _ => None,
        })
    }

    pub(crate) fn construct_with(
        &mut self,
        host: &mut dyn Host,
        callee: &Value,
        args: Vec<Value>,
        new_target: &Value,
    ) -> Result<Invocation, VmError> {
        let Some(constructor) = self.constructor(callee)? else {
            return match self.classify(callee)? {
                Some(Dispatch::Builtin(BuiltinCall::StringConstructor)) => {
                    self.call_string(host, &args, true)
                }
                Some(Dispatch::Builtin(BuiltinCall::NumberConstructor)) => {
                    self.throw_type_error("Number does not support primitive boxing")
                }
                Some(Dispatch::Builtin(BuiltinCall::SymbolConstructor)) => {
                    self.throw_type_error("Symbol is not a constructor")
                }
                _ => self.throw_type_error("value is not a constructor"),
            };
        };
        let prototype = if !ops::strict_equal(callee, new_target)
            || matches!(constructor, Constructor::Script { .. })
        {
            match self.read_property(host, new_target, PropertyKeyRef::String("prototype"))? {
                Invocation::Value(Value::Object(id)) => Some(id),
                Invocation::Value(_) => None,
                thrown @ Invocation::Thrown(_) => return Ok(thrown),
            }
        } else {
            None
        };
        let result = match constructor {
            Constructor::Script { func_id, env } => {
                let instance = Value::Object(self.heap.alloc_plain_with(prototype));
                return match self.call_function(host, func_id, Some(env), args, &instance)? {
                    Invocation::Value(value) if !matches!(value, Value::Object(_)) => {
                        Ok(Invocation::Value(instance))
                    }
                    result => Ok(result),
                };
            }
            Constructor::Promise => {
                return self.construct_promise(host, args, prototype);
            }
            Constructor::Object => {
                // Do not change the prototype of a supplied argument object.
                if let Some(prototype) = prototype {
                    return Ok(Invocation::Value(Value::Object(
                        self.heap.alloc_plain_with(Some(prototype)),
                    )));
                }
                return self.construct_object(&args);
            }
            Constructor::Array => self.construct_array(args)?,
            Constructor::Error { name } => self.construct_error(host, &name, args)?,
        };
        if let (Some(prototype), Invocation::Value(Value::Object(id))) = (prototype, &result)
            && !self.heap.set_prototype(*id, Some(prototype))?
        {
            return self.throw_type_error("cannot set constructor result prototype");
        }
        Ok(result)
    }

    fn reflect_call(
        &mut self,
        host: &mut dyn Host,
        args: &[Value],
        construct: bool,
    ) -> Result<Invocation, VmError> {
        let target = args.first().unwrap_or(&Value::Undefined);
        let new_target = args.get(2).unwrap_or(target);
        let list = if construct {
            if self.constructor(target)?.is_none() || self.constructor(new_target)?.is_none() {
                return self.throw_type_error("value is not a constructor");
            }
            args.get(1)
        } else {
            if !self.is_callable(target) {
                return self.throw_type_error("value is not a function");
            }
            args.get(2)
        };
        let values =
            match self.arguments_from_array_like(host, list.unwrap_or(&Value::Undefined))? {
                Ok(values) => values,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
        if construct {
            self.construct_with(host, target, values, new_target)
        } else {
            self.invoke(
                host,
                target,
                values,
                args.get(1).unwrap_or(&Value::Undefined),
            )
        }
    }

    fn arguments_from_array_like(
        &mut self,
        host: &mut dyn Host,
        list: &Value,
    ) -> Result<Result<Vec<Value>, Value>, VmError> {
        if !matches!(list, Value::Object(_)) {
            return Ok(Err(self.type_error("argumentsList must be an object")?));
        }
        let length = match self.read_property(host, list, PropertyKeyRef::String("length"))? {
            Invocation::Value(value) => value,
            Invocation::Thrown(error) => return Ok(Err(error)),
        };
        let length = match self.coerce_number(host, &length)? {
            // Casting NaN to an integer yields zero, as required by ToLength.
            Ok(number) => number.trunc().clamp(0.0, 9_007_199_254_740_991.0) as u64,
            Err(error) => return Ok(Err(error)),
        };
        let mut values = Vec::new();
        for index in 0..length {
            match self.read_property(host, list, PropertyKeyRef::String(&index.to_string()))? {
                Invocation::Value(value) => values.push(value),
                Invocation::Thrown(error) => return Ok(Err(error)),
            }
        }
        Ok(Ok(values))
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
            BuiltinCall::ObjectCreate => self.object_create(host, &args),
            BuiltinCall::GetPrototypeOf { reflect } => self.get_prototype_of(&args, reflect),
            BuiltinCall::SetPrototypeOf { reflect } => self.set_prototype_of(&args, reflect),
            BuiltinCall::ObjectKeys => self.object_keys(&args),
            BuiltinCall::ObjectGetOwnPropertyDescriptors => {
                self.get_own_property_descriptors(&args)
            }
            BuiltinCall::ObjectAssign => self.object_assign(host, &args),
            BuiltinCall::ObjectIs => Ok(Invocation::Value(Value::Bool(ops::same_value(
                args.first().unwrap_or(&Value::Undefined),
                args.get(1).unwrap_or(&Value::Undefined),
            )))),
            BuiltinCall::ObjectGetOwnPropertyDescriptor => {
                self.get_own_property_descriptor(host, &args)
            }
            BuiltinCall::ReflectGetOwnPropertyDescriptor => {
                self.get_own_property_descriptor(host, &args)
            }
            BuiltinCall::DefineProperty { reflect } => {
                self.define_property_builtin(host, &args, reflect)
            }
            BuiltinCall::ObjectDefineProperties => {
                let Some(Value::Object(target)) = args.first() else {
                    return self
                        .throw_type_error("Object.defineProperties requires an object target");
                };
                self.define_properties(host, *target, args.get(1))
            }
            BuiltinCall::PreventExtensions { reflect } => {
                let target = args.first().unwrap_or(&Value::Undefined);
                if let Value::Object(id) = target {
                    self.heap.get_mut(*id)?.prevent_extensions();
                } else if reflect {
                    return self.throw_type_error("Reflect.preventExtensions requires an object");
                }
                Ok(Invocation::Value(if reflect {
                    Value::Bool(true)
                } else {
                    target.clone()
                }))
            }
            BuiltinCall::IsExtensible { reflect } => {
                let extensible = match args.first() {
                    Some(Value::Object(id)) => self.heap.get(*id)?.is_extensible(),
                    _ if reflect => {
                        return self.throw_type_error("Reflect.isExtensible requires an object");
                    }
                    _ => false,
                };
                Ok(Invocation::Value(Value::Bool(extensible)))
            }
            BuiltinCall::SetIntegrityLevel(level) => {
                let target = args.first().unwrap_or(&Value::Undefined);
                if let Value::Object(id) = target {
                    self.heap.get_mut(*id)?.set_integrity_level(level);
                }
                Ok(Invocation::Value(target.clone()))
            }
            BuiltinCall::TestIntegrityLevel(level) => {
                let satisfies = match args.first() {
                    Some(Value::Object(id)) => self.heap.get(*id)?.test_integrity_level(level),
                    _ => true,
                };
                Ok(Invocation::Value(Value::Bool(satisfies)))
            }
            BuiltinCall::ErrorConstructor { name } => self.construct_error(host, &name, args),
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
                    let key = match self
                        .coerce_property_key(host, args.first().unwrap_or(&Value::Undefined))?
                    {
                        Ok(key) => key,
                        Err(thrown) => return Ok(Invocation::Thrown(thrown)),
                    };
                    Ok(Invocation::Value(Value::Bool(self.heap.has_own(id, &key)?)))
                }
                "valueOf" => match *receiver {
                    Value::Object(id) if self.heap.is_environment(id) == Some(false) => {
                        Ok(Invocation::Value(Value::Object(id)))
                    }
                    _ => self.throw_type_error("incompatible Object.prototype receiver"),
                },
                "toString" => match receiver {
                    Value::Object(id) if self.heap.is_environment(*id) == Some(true) => {
                        self.throw_type_error("incompatible Object.prototype receiver")
                    }
                    Value::Object(id) => Ok(Invocation::Value(Value::String(format!(
                        "[object {}]",
                        self.object_tag(*id)?
                    )))),
                    _ => Ok(Invocation::Value(Value::String(
                        "[object Undefined]".to_owned(),
                    ))),
                },
                _ => Err(VmError::InvalidState(format!(
                    "unknown Object method {name}"
                ))),
            },
            BuiltinCall::ArrayValues => self.array_values(receiver),
            BuiltinCall::ArrayConstructor => self.construct_array(args),
            BuiltinCall::ArrayOperation(name) => self.array_operation(host, &name, receiver, args),
            BuiltinCall::ArrayIteratorNext => self.array_iterator_next(host, receiver),
            BuiltinCall::StringConstructor => self.call_string(host, &args, false),
            BuiltinCall::StringMethod(method) => {
                self.call_string_method(host, method, receiver, &args)
            }
            BuiltinCall::StringStatic(method) => self.call_string_static(host, method, &args),
            BuiltinCall::NumberConstructor => self.call_number(host, &args),
            BuiltinCall::NumberMethod(method) => {
                self.call_number_method(host, method, receiver, &args)
            }
            BuiltinCall::NumberStatic(method) => self.call_number_static(host, method, &args),
            BuiltinCall::StringIterator => self.string_iterator(host, receiver),
            BuiltinCall::StringIteratorNext => self.string_iterator_next(receiver),
            BuiltinCall::ArrayJoin => self.array_join(host, receiver, &args),
            BuiltinCall::ArrayToString => self.array_to_string(host, receiver),
            BuiltinCall::IteratorIdentity => Ok(Invocation::Value(receiver.clone())),
            BuiltinCall::ReflectApply => self.reflect_call(host, &args, false),
            BuiltinCall::ReflectConstruct => self.reflect_call(host, &args, true),
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
            BuiltinCall::ReflectDeleteProperty => {
                let Some(target @ Value::Object(_)) = args.first() else {
                    return self.throw_type_error("Reflect.deleteProperty requires an object");
                };
                self.delete_property(host, target, args.get(1).unwrap_or(&Value::Undefined))
            }
            BuiltinCall::ReflectGet => {
                let Some(Value::Object(target)) = args.first() else {
                    return self.throw_type_error("Reflect.get requires an object");
                };
                let key = match self
                    .coerce_property_key(host, args.get(1).unwrap_or(&Value::Undefined))?
                {
                    Ok(key) => key,
                    Err(thrown) => return Ok(Invocation::Thrown(thrown)),
                };
                self.read_property_with_receiver(
                    host,
                    *target,
                    args.get(2).unwrap_or(&args[0]),
                    key.borrowed(),
                )
            }
            BuiltinCall::ReflectHas => {
                let Some(Value::Object(target)) = args.first() else {
                    return self.throw_type_error("Reflect.has requires an object");
                };
                let key = match self
                    .coerce_property_key(host, args.get(1).unwrap_or(&Value::Undefined))?
                {
                    Ok(key) => key,
                    Err(thrown) => return Ok(Invocation::Thrown(thrown)),
                };
                Ok(Invocation::Value(Value::Bool(
                    self.has_property_key(*target, key.borrowed())?,
                )))
            }
            BuiltinCall::ReflectSet => {
                let Some(Value::Object(target)) = args.first() else {
                    return self.throw_type_error("Reflect.set requires an object");
                };
                let key = match self
                    .coerce_property_key(host, args.get(1).unwrap_or(&Value::Undefined))?
                {
                    Ok(key) => key,
                    Err(thrown) => return Ok(Invocation::Thrown(thrown)),
                };
                let value = args.get(2).cloned().unwrap_or(Value::Undefined);
                let receiver = args.get(3).unwrap_or(&args[0]);
                self.set_property(host, *target, receiver, key, value, false)
            }
            BuiltinCall::QueueMicrotask => self.queue_microtask(args),
            BuiltinCall::SymbolConstructor => self.symbol_constructor(host, args),
            BuiltinCall::SymbolFor => {
                self.symbol_static(receiver, |vm, args| vm.symbol_for(host, args), args)
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

    fn array_to_string(
        &mut self,
        host: &mut dyn Host,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        let join = match self.read_property(host, receiver, PropertyKeyRef::String("join"))? {
            Invocation::Value(value) => value,
            thrown => return Ok(thrown),
        };
        if self.is_callable(&join) {
            self.invoke(host, &join, Vec::new(), receiver)
        } else {
            let tag = match receiver {
                Value::Object(id) => self.object_tag(*id)?,
                _ => "Object",
            };
            Ok(Invocation::Value(Value::String(format!("[object {tag}]"))))
        }
    }

    fn object_tag(&self, id: ObjectId) -> Result<&'static str, VmError> {
        Ok(match self.heap.get(id)? {
            Object::Array(_) => "Array",
            Object::Function(_)
            | Object::BuiltinStatic(_)
            | Object::Bound(_)
            | Object::Resolution(_) => "Function",
            Object::Promise(_) => "Promise",
            _ => "Object",
        })
    }

    fn array_join(
        &mut self,
        host: &mut dyn Host,
        receiver: &Value,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let Value::Object(id) = receiver else {
            return self.throw_type_error("Array.prototype.join called on null or undefined");
        };
        let length = match self.read_property(host, receiver, PropertyKeyRef::String("length"))? {
            Invocation::Value(value) => value,
            thrown => return Ok(thrown),
        };
        let length = match self.coerce_number(host, &length)? {
            Ok(number) => number.trunc().clamp(0.0, 9_007_199_254_740_991.0) as u64,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let separator = match args.first() {
            None | Some(Value::Undefined) => ",".to_owned(),
            Some(value) => match self.coerce_string(host, value)? {
                Ok(string) => string,
                Err(error) => return Ok(Invocation::Thrown(error)),
            },
        };
        let array = matches!(self.heap.get(*id)?, Object::Array(_));
        if array && self.active_array_joins.contains(id) {
            return Ok(Invocation::Value(Value::String(String::new())));
        }
        if array {
            self.active_array_joins.insert(*id);
        }
        let result = (|| {
            let mut output = String::new();
            for index in 0..length {
                if index > 0 {
                    output.push_str(&separator);
                }
                let item = if array {
                    self.read_array_index(host, *id, index as usize)?
                } else {
                    self.read_property(host, receiver, PropertyKeyRef::String(&index.to_string()))?
                };
                let value = match item {
                    Invocation::Value(value) => value,
                    thrown => return Ok(thrown),
                };
                if matches!(value, Value::Undefined | Value::Null) {
                    continue;
                }
                match self.coerce_string(host, &value)? {
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
                PropertyKey::String(index.to_string()),
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
            Value::Object(self.builtins.array_values),
            Attributes::METHOD,
        )?;
        Ok(Value::Object(id))
    }

    fn array_values(&mut self, receiver: &Value) -> Result<Invocation, VmError> {
        let Value::Object(id) = *receiver else {
            return self.throw_type_error("Array.values requires an object");
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
            let index = *next_index;
            let length = match self.heap.get(source)? {
                Object::Array(array) => array.length,
                _ => {
                    let value = match self.read_property(
                        host,
                        &Value::Object(source),
                        PropertyKeyRef::String("length"),
                    )? {
                        Invocation::Value(value) => value,
                        thrown => return Ok(thrown),
                    };
                    match self.coerce_number(host, &value)? {
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
                self.read_array_index(host, source, index)?
            } else {
                self.read_property(
                    host,
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

    fn string_iterator(
        &mut self,
        host: &mut dyn Host,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        if matches!(receiver, Value::Null | Value::Undefined) {
            return self.throw_type_error("String[Symbol.iterator] requires a non-null receiver");
        }
        let string = match self.string_value(host, receiver)? {
            Invocation::Value(Value::String(string)) => string,
            Invocation::Value(_) => unreachable!(),
            thrown => return Ok(thrown),
        };
        let iterator = self.heap.alloc(Object::StringIterator {
            object: PlainObject::new(Some(self.builtins.string_iterator_prototype)),
            string,
            byte_offset: 0,
        });
        Ok(Invocation::Value(Value::Object(iterator)))
    }

    fn string_iterator_next(&mut self, receiver: &Value) -> Result<Invocation, VmError> {
        let invalid_receiver = "StringIterator.next requires a string iterator";
        let Value::Object(id) = *receiver else {
            return self.throw_type_error(invalid_receiver);
        };
        let Object::StringIterator {
            string,
            byte_offset,
            ..
        } = self.heap.get_mut(id)?
        else {
            return self.throw_type_error(invalid_receiver);
        };
        let Some(code_point) = string[*byte_offset..].chars().next() else {
            return self.iterator_result(Value::Undefined, true);
        };
        *byte_offset += code_point.len_utf8();
        if *byte_offset == string.len() {
            *string = String::new();
            *byte_offset = 0;
        }
        self.iterator_result(Value::String(code_point.to_string()), false)
    }

    fn iterator_result(&mut self, value: Value, done: bool) -> Result<Invocation, VmError> {
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
        if name == "toString" {
            return if self.is_callable(receiver) {
                Ok(Invocation::Value(Value::String(
                    "function () { [native code] }".to_owned(),
                )))
            } else {
                self.throw_type_error("incompatible Function.prototype.toString receiver")
            };
        }
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
                    object: PlainObject::new(self.heap.function_prototype),
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
    pub(crate) fn make_range_error(&mut self, message: &str) -> Result<Value, VmError> {
        self.alloc_error(
            self.builtins.rangeerror_prototype,
            "RangeError",
            Some(message.to_owned()),
        )
    }

    pub(crate) fn throw_range_error(&mut self, message: &str) -> Result<Invocation, VmError> {
        self.make_range_error(message).map(Invocation::Thrown)
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

    fn object_create(
        &mut self,
        host: &mut dyn Host,
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
            properties => self.define_properties(host, object, properties),
        }
    }

    fn get_prototype_of(&mut self, args: &[Value], reflect: bool) -> Result<Invocation, VmError> {
        let Some(Value::Object(target)) = args.first() else {
            return self.throw_type_error(if reflect {
                "Reflect.getPrototypeOf requires an object"
            } else {
                "Object.getPrototypeOf requires an object in this VM"
            });
        };
        Ok(Invocation::Value(
            self.heap
                .get(*target)?
                .prototype()
                .map_or(Value::Null, Value::Object),
        ))
    }

    fn set_prototype_of(&mut self, args: &[Value], reflect: bool) -> Result<Invocation, VmError> {
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

    fn get_own_property_descriptor(
        &mut self,
        host: &mut dyn Host,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let Some(Value::Object(target)) = args.first() else {
            return self
                .throw_type_error("Object.getOwnPropertyDescriptor requires an object in this VM");
        };
        let key = match self.coerce_property_key(host, args.get(1).unwrap_or(&Value::Undefined))? {
            Ok(key) => key,
            Err(thrown) => return Ok(Invocation::Thrown(thrown)),
        };
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

    fn get_own_property_descriptors(&mut self, args: &[Value]) -> Result<Invocation, VmError> {
        let Some(Value::Object(target)) = args.first() else {
            return self.throw_type_error(
                "Object.getOwnPropertyDescriptors requires an object in this VM",
            );
        };
        let keys = self.heap.get(*target)?.own_keys(false).collect::<Vec<_>>();
        let result = self.heap.alloc_plain();
        for key in keys {
            let Some(property) = self.heap.get(*target)?.own_property(key.borrowed()) else {
                continue;
            };
            let attributes = property.attributes();
            let property = property.into_owned();
            let descriptor = self.heap.alloc_plain();
            match property {
                Property::Data(value) => {
                    self.heap.set_own_string(descriptor, "value", value)?;
                    self.heap.set_own_string(
                        descriptor,
                        "writable",
                        Value::Bool(attributes.writable),
                    )?;
                }
                Property::Accessor { get, set } => {
                    self.heap.set_own_string(
                        descriptor,
                        "get",
                        get.map_or(Value::Undefined, Value::Object),
                    )?;
                    self.heap.set_own_string(
                        descriptor,
                        "set",
                        set.map_or(Value::Undefined, Value::Object),
                    )?;
                }
            }
            self.heap.set_own_string(
                descriptor,
                "enumerable",
                Value::Bool(attributes.enumerable),
            )?;
            self.heap.set_own_string(
                descriptor,
                "configurable",
                Value::Bool(attributes.configurable),
            )?;
            self.heap.set_own(result, key, Value::Object(descriptor))?;
        }
        Ok(Invocation::Value(Value::Object(result)))
    }

    fn define_property_builtin(
        &mut self,
        host: &mut dyn Host,
        args: &[Value],
        reflect: bool,
    ) -> Result<Invocation, VmError> {
        let Some(Value::Object(target)) = args.first() else {
            return self.throw_type_error(if reflect {
                "Reflect.defineProperty requires an object target"
            } else {
                "Object.defineProperty requires an object target"
            });
        };
        let key = match self.coerce_property_key(host, args.get(1).unwrap_or(&Value::Undefined))? {
            Ok(key) => key,
            Err(thrown) => return Ok(Invocation::Thrown(thrown)),
        };
        let update =
            match self.read_property_descriptor(host, args.get(2).unwrap_or(&Value::Undefined))? {
                Ok(update) => update,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
        if reflect {
            return Ok(
                match self.try_apply_property_update(host, *target, key, update)? {
                    Ok(applied) => Invocation::Value(Value::Bool(applied)),
                    Err(error) => Invocation::Thrown(error),
                },
            );
        }
        self.apply_property_update(host, *target, key, update)
    }
    fn object_assign(
        &mut self,
        host: &mut dyn Host,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let Some(Value::Object(target)) = args.first() else {
            return self.throw_type_error("Object.assign requires an object target");
        };
        let target = *target;
        for source in args.iter().skip(1) {
            let Value::Object(source_id) = source else {
                // Primitive sources currently have no enumerable own keys.
                continue;
            };
            let keys: Vec<_> = self.heap.get(*source_id)?.own_keys(false).collect();
            for key in keys {
                let Some(property) = self.heap.get(*source_id)?.own_property(key.borrowed()) else {
                    continue;
                };
                if !property.attributes().enumerable {
                    continue;
                }
                let value = match self.read_property(host, source, key.borrowed())? {
                    Invocation::Value(value) => value,
                    thrown @ Invocation::Thrown(_) => return Ok(thrown),
                };
                let invocation =
                    self.set_property(host, target, &Value::Object(target), key, value, true)?;
                if let thrown @ Invocation::Thrown(_) = invocation {
                    return Ok(thrown);
                }
            }
        }
        Ok(Invocation::Value(Value::Object(target)))
    }

    fn define_properties(
        &mut self,
        host: &mut dyn Host,
        target: ObjectId,
        properties: Option<&Value>,
    ) -> Result<Invocation, VmError> {
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
                self.apply_property_update(host, target, key, update)?
            {
                return Ok(thrown);
            }
        }
        Ok(Invocation::Value(Value::Object(target)))
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

    fn apply_property_update(
        &mut self,
        host: &mut dyn Host,
        target: ObjectId,
        key: PropertyKey,
        update: DescriptorUpdate,
    ) -> Result<Invocation, VmError> {
        match self.try_apply_property_update(host, target, key, update)? {
            Ok(true) => Ok(Invocation::Value(Value::Object(target))),
            Ok(false) => self.throw_type_error("Cannot define property"),
            Err(error) => Ok(Invocation::Thrown(error)),
        }
    }

    pub(crate) fn try_apply_property_update(
        &mut self,
        host: &mut dyn Host,
        target: ObjectId,
        key: PropertyKey,
        mut update: DescriptorUpdate,
    ) -> Result<Result<bool, Value>, VmError> {
        if key.as_string() == Some("length")
            && matches!(self.heap.get(target)?, Object::Array(_))
            && let Some(value) = &mut update.value
        {
            // ArraySetLength performs ToUint32 and then a separate ToNumber.
            // Each may call script and observe changes made by the previous one.
            let length = match self.coerce_number(host, value)? {
                Ok(number) => ops::number_to_uint32(number),
                Err(error) => return Ok(Err(error)),
            };
            let number = match self.coerce_number(host, value)? {
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

    fn construct_error(
        &mut self,
        host: &mut dyn Host,
        name: &str,
        args: Vec<Value>,
    ) -> Result<Invocation, VmError> {
        let prototype = match name {
            "Error" => self.builtins.error_prototype,
            "TypeError" => self.builtins.typeerror_prototype,
            "RangeError" => self.builtins.rangeerror_prototype,
            "ReferenceError" => self.builtins.referenceerror_prototype,
            "SyntaxError" => self.builtins.syntaxerror_prototype,
            "EvalError" => self.builtins.evalerror_prototype,
            "URIError" => self.builtins.urierror_prototype,
            "AggregateError" => self.builtins.aggregateerror_prototype,
            _ => {
                return Err(VmError::InvalidState(format!(
                    "unknown Error constructor {name}"
                )));
            }
        };
        let (errors, message_arg) = if name == "AggregateError" {
            let iterable = args.first().unwrap_or(&Value::Undefined);
            let mut errors = Vec::new();
            if !matches!(iterable, Value::Undefined) {
                let (iterator, next) = match self.get_iterator(host, iterable)? {
                    Ok(iterator) => iterator,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                };
                loop {
                    match self.iterator_step(host, &iterator, &next)? {
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
        let message = match message_arg {
            None | Some(Value::Undefined) => None,
            Some(value) => match self.coerce_string(host, value)? {
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
    fn alloc_aggregate_error(&mut self, errors: Vec<Value>) -> Result<Value, VmError> {
        let error = self.alloc_error(
            self.builtins.aggregateerror_prototype,
            "AggregateError",
            Some("All promises were rejected".to_owned()),
        )?;
        let Value::Object(id) = error else {
            unreachable!()
        };
        let errors = self.heap.alloc_array(errors);
        self.heap
            .define_data(id, "errors", Value::Object(errors), Attributes::METHOD)?;
        Ok(Value::Object(id))
    }

    fn construct_promise(
        &mut self,
        host: &mut dyn Host,
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
    fn symbol_constructor(
        &mut self,
        host: &mut dyn Host,
        args: Vec<Value>,
    ) -> Result<Invocation, VmError> {
        let description = match args.first() {
            None | Some(Value::Undefined) => None,
            Some(value) => match self.coerce_string(host, value)? {
                Ok(description) => Some(description),
                Err(error) => return Ok(Invocation::Thrown(error)),
            },
        };
        let id = self.heap.alloc_symbol(description)?;
        Ok(Invocation::Value(Value::Symbol(id)))
    }

    fn symbol_for(&mut self, host: &mut dyn Host, args: Vec<Value>) -> Result<Invocation, VmError> {
        let key = match self.coerce_string(host, args.first().unwrap_or(&Value::Undefined))? {
            Ok(key) => key,
            Err(error) => return Ok(Invocation::Thrown(error)),
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

    pub(crate) fn is_intrinsic_promise_then(&self, value: &Value) -> bool {
        matches!(value, Value::Object(id) if matches!(self.heap.get(*id),
            Ok(Object::BuiltinStatic(function))
                if function.name == "Promise" && function.member.as_deref() == Some("then")))
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
                match self.promise_identity(host, &value)? {
                    Ok(Some(id)) => return Ok(Invocation::Value(Value::Object(id))),
                    Ok(None) => {}
                    Err(error) => return Ok(Invocation::Thrown(error)),
                }
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
        let iterable = args.first().unwrap_or(&Value::Undefined);
        let result = self.heap.alloc_promise();
        let (iterator, next) = match self.get_iterator(host, iterable)? {
            Ok(record) => record,
            Err(error) => {
                self.settle(result, Settlement::Rejected(error))?;
                return Ok(Invocation::Value(Value::Object(result)));
            }
        };
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
            let element = match self.iterator_step(host, &iterator, &next)? {
                Ok(Some(element)) => element,
                Ok(None) => break,
                Err(error) => {
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
            // Resolve before advancing: reading an element's `then` may run script.
            self.resolve_promise(host, slot, element)?;
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
            object: PlainObject::new(self.heap.function_prototype),
        }));
        let reject_id = self.heap.alloc(Object::Resolution(Resolution {
            promise,
            pair: resolve_id,
            resolve: false,
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
        (
            name @ ("Error" | "TypeError" | "RangeError" | "ReferenceError" | "SyntaxError"
            | "EvalError" | "URIError" | "AggregateError"),
            None,
        ) => Some(BuiltinCall::ErrorConstructor {
            name: name.to_owned(),
        }),
        ("Function", Some(name @ ("call" | "apply" | "bind" | "toString"))) => {
            Some(BuiltinCall::FunctionMethod {
                name: name.to_owned(),
            })
        }
        ("Object", Some(name @ ("hasOwnProperty" | "valueOf" | "toString"))) => {
            Some(BuiltinCall::ObjectMethod {
                name: name.to_owned(),
            })
        }
        ("Object", None) => Some(BuiltinCall::ObjectConstructor),
        ("Object", Some("assign")) => Some(BuiltinCall::ObjectAssign),
        ("Object", Some("is")) => Some(BuiltinCall::ObjectIs),

        ("Object", Some("keys")) => Some(BuiltinCall::ObjectKeys),
        ("Object", Some("create")) => Some(BuiltinCall::ObjectCreate),
        (owner @ ("Object" | "Reflect"), Some("getPrototypeOf")) => {
            Some(BuiltinCall::GetPrototypeOf {
                reflect: owner == "Reflect",
            })
        }
        (owner @ ("Object" | "Reflect"), Some("setPrototypeOf")) => {
            Some(BuiltinCall::SetPrototypeOf {
                reflect: owner == "Reflect",
            })
        }
        (owner @ ("Object" | "Reflect"), Some("getOwnPropertyDescriptor")) => {
            Some(if owner == "Reflect" {
                BuiltinCall::ReflectGetOwnPropertyDescriptor
            } else {
                BuiltinCall::ObjectGetOwnPropertyDescriptor
            })
        }
        ("Object", Some("getOwnPropertyDescriptors")) => {
            Some(BuiltinCall::ObjectGetOwnPropertyDescriptors)
        }
        (owner @ ("Object" | "Reflect"), Some("defineProperty")) => {
            Some(BuiltinCall::DefineProperty {
                reflect: owner == "Reflect",
            })
        }
        ("Object", Some("defineProperties")) => Some(BuiltinCall::ObjectDefineProperties),
        (owner @ ("Object" | "Reflect"), Some("preventExtensions")) => {
            Some(BuiltinCall::PreventExtensions {
                reflect: owner == "Reflect",
            })
        }
        (owner @ ("Object" | "Reflect"), Some("isExtensible")) => Some(BuiltinCall::IsExtensible {
            reflect: owner == "Reflect",
        }),
        ("Object", Some("seal")) => Some(BuiltinCall::SetIntegrityLevel(IntegrityLevel::Sealed)),
        ("Object", Some("freeze")) => Some(BuiltinCall::SetIntegrityLevel(IntegrityLevel::Frozen)),
        ("Object", Some("isSealed")) => {
            Some(BuiltinCall::TestIntegrityLevel(IntegrityLevel::Sealed))
        }
        ("Object", Some("isFrozen")) => {
            Some(BuiltinCall::TestIntegrityLevel(IntegrityLevel::Frozen))
        }
        ("Array", Some("join")) => Some(BuiltinCall::ArrayJoin),
        ("Array", Some("toString")) => Some(BuiltinCall::ArrayToString),
        ("Array", Some("values")) => Some(BuiltinCall::ArrayValues),
        ("Array", None) => Some(BuiltinCall::ArrayConstructor),
        ("Array", Some(name)) if crate::array::is_array_operation(name) => {
            Some(BuiltinCall::ArrayOperation(name.to_owned()))
        }
        ("ArrayIterator", Some("next")) => Some(BuiltinCall::ArrayIteratorNext),
        ("Number", None) => Some(BuiltinCall::NumberConstructor),
        ("Number", Some(name)) => NUMBER_METHODS
            .iter()
            .find(|&&key| key == name)
            .map(|&key| BuiltinCall::NumberMethod(key))
            .or_else(|| {
                NUMBER_STATICS
                    .iter()
                    .find(|&&key| key == name)
                    .map(|&key| BuiltinCall::NumberStatic(key))
            }),
        ("String", None) => Some(BuiltinCall::StringConstructor),
        ("String", Some("iterator")) => Some(BuiltinCall::StringIterator),
        ("String", Some(name)) => STRING_METHODS
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, method)| BuiltinCall::StringMethod(*method))
            .or_else(|| {
                STRING_STATICS
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, method)| BuiltinCall::StringStatic(*method))
            }),
        ("StringIterator", Some("next")) => Some(BuiltinCall::StringIteratorNext),
        ("Iterator", Some("identity")) => Some(BuiltinCall::IteratorIdentity),
        ("Reflect", Some("apply")) => Some(BuiltinCall::ReflectApply),
        ("Reflect", Some("construct")) => Some(BuiltinCall::ReflectConstruct),
        ("Reflect", Some("ownKeys")) => Some(BuiltinCall::ReflectOwnKeys),
        ("Reflect", Some("deleteProperty")) => Some(BuiltinCall::ReflectDeleteProperty),
        ("Reflect", Some("get")) => Some(BuiltinCall::ReflectGet),
        ("Reflect", Some("set")) => Some(BuiltinCall::ReflectSet),
        ("Reflect", Some("has")) => Some(BuiltinCall::ReflectHas),
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
