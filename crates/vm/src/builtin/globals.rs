use super::global_functions;
use crate::builtin::array;
use crate::builtin::bigint;
use crate::builtin::binary;
use crate::builtin::boolean::members as boolean_family;
use crate::builtin::collections;
use crate::builtin::date;
use crate::builtin::disposable_stack;
use crate::builtin::json::namespace as json;
use crate::builtin::math::namespace as math;
use crate::builtin::number;
use crate::builtin::object::members as object_members;
use crate::builtin::promise::members as promise_members;
use crate::builtin::proxy;
use crate::builtin::reflect::namespace as reflect_namespace;
use crate::builtin::regexp_runtime as regexp;
use crate::builtin::roots::{RootKind, RootSpec};
use crate::builtin::string;
use crate::builtin::{async_generator, error_family, generator, queue_family, symbol_family};
use crate::error::VmError;
use crate::exec;
use crate::heap::{Attributes, DescriptorUpdate, HAS_INSTANCE_SYMBOL, TO_STRING_TAG_SYMBOL};
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::value::{PropertyKey, Value};
use crate::vm::Vm;

pub(crate) struct Owner {
    root: &'static str,
    roots: &'static [RootSpec],
    groups: &'static [&'static [IntrinsicDef]],
    install: Option<fn(&mut Vm, &Value)>,
}

impl Owner {
    pub(crate) const fn new(
        root: &'static str,
        roots: &'static [RootSpec],
        groups: &'static [&'static [IntrinsicDef]],
        install: Option<fn(&mut Vm, &Value)>,
    ) -> Self {
        Self {
            root,
            roots,
            groups,
            install,
        }
    }
}

enum Global {
    Owner(Owner),
    Function(Owner),
    Number(f64),
    /// A global whose object is an already allocated builtin root, used for a
    /// family of constructors that share one owner.
    Root(&'static str),
    AccessorRoot(&'static str),
}

macro_rules! builtin_globals {
    ($($name:expr => $global:expr),+ $(,)?) => {
        const GLOBALS: &[Global] = &[$($global),+];

        /// VM-provided globals in bytecode operand order. Append; never reorder.
        pub const BUILTINS: [&str; GLOBALS.len()] = [$($name),+];
    };
}

// Foundational roots and non-global functions are not bytecode globals.
const ITERATOR_IDENTITY: IntrinsicKey =
    IntrinsicKey::new("Iterator", Slot::PrototypeSymbol, "iterator");
const ITERATOR_INTRINSICS: &[IntrinsicDef] = &[IntrinsicDef::call(
    ITERATOR_IDENTITY,
    0,
    |_, _, _, _, receiver| Ok(crate::runtime::Invocation::Value(receiver.clone())),
)];
const GLOBAL_THIS: &str = "globalThis";
const GLOBAL_THIS_ROOTS: &[RootSpec] = &[RootSpec::new(
    GLOBAL_THIS,
    RootKind::EventTarget,
    Some(super::event::TARGET_PROTOTYPE),
)];

builtin_globals! {
    promise_members::NAME => Global::Owner(promise_members::OWNER),
    "queueMicrotask" => Global::Owner(queue_family::OWNER),
    "Error" => Global::Owner(error_family::ERROR),
    "TypeError" => Global::Owner(error_family::TYPE_ERROR),
    "Symbol" => Global::Owner(symbol_family::OWNER),
    reflect_namespace::NAME => Global::Owner(reflect_namespace::OWNER),
    object_members::NAME => Global::Owner(object_members::OWNER),
    "NaN" => Global::Number(f64::NAN),
    "Infinity" => Global::Number(f64::INFINITY),
    "Array" => Global::Owner(array::OWNER),
    string::members::NAME => Global::Owner(string::OWNER),
    "RangeError" => Global::Owner(error_family::RANGE_ERROR),
    "ReferenceError" => Global::Owner(error_family::REFERENCE_ERROR),
    "SyntaxError" => Global::Owner(error_family::SYNTAX_ERROR),
    "EvalError" => Global::Owner(error_family::EVAL_ERROR),
    "URIError" => Global::Owner(error_family::URI_ERROR),
    "AggregateError" => Global::Owner(error_family::AGGREGATE_ERROR),
    number::members::NAME => Global::Owner(number::OWNER),
    math::NAME => Global::Owner(math::OWNER),
    json::NAME => Global::Owner(json::OWNER),
    "Map" => Global::Owner(collections::MAP_OWNER),
    "Set" => Global::Owner(collections::SET_OWNER),
    "WeakMap" => Global::Owner(collections::WEAK_MAP_OWNER),
    "Proxy" => Global::Owner(proxy::OWNER),
    "Boolean" => Global::Owner(boolean_family::OWNER),
    GLOBAL_THIS => Global::Owner(Owner::new(GLOBAL_THIS, GLOBAL_THIS_ROOTS, &[], None)),
    "RegExp" => Global::Owner(regexp::OWNER),
    "WeakSet" => Global::Owner(collections::WEAK_SET_OWNER),
    "Function" => Global::Owner(Owner::new("function", &[], &[], None)),
    "Date" => Global::Owner(date::OWNER),
    bigint::members::NAME => Global::Owner(bigint::OWNER),
    "ArrayBuffer" => Global::Owner(binary::OWNER),
    "DataView" => Global::Root("dataView"),
    "Int8Array" => Global::Root("int8Array"),
    "Uint8Array" => Global::Root("uint8Array"),
    "Uint8ClampedArray" => Global::Root("uint8ClampedArray"),
    "Int16Array" => Global::Root("int16Array"),
    "Uint16Array" => Global::Root("uint16Array"),
    "Int32Array" => Global::Root("int32Array"),
    "Uint32Array" => Global::Root("uint32Array"),
    "Float16Array" => Global::Root("float16Array"),
    "Float32Array" => Global::Root("float32Array"),
    "Float64Array" => Global::Root("float64Array"),
    "BigInt64Array" => Global::Root("bigInt64Array"),
    "BigUint64Array" => Global::Root("bigUint64Array"),
    "TextEncoder" => Global::Owner(binary::text::ENCODER_OWNER),
    "TextDecoder" => Global::Owner(binary::text::DECODER_OWNER),
    "TextEncoderStream" => Global::Owner(super::streams::text::ENCODER_OWNER),
    "TextDecoderStream" => Global::Owner(super::streams::text::DECODER_OWNER),
    "CompressionStream" => Global::Owner(super::streams::compression::ENCODER_OWNER),
    "DecompressionStream" => Global::Owner(super::streams::compression::DECODER_OWNER),
    "URL" => Global::Owner(super::url_api::URL_OWNER),
    "URLSearchParams" => Global::Owner(super::url_api::params::PARAMS_OWNER),
    "URLPattern" => Global::Owner(super::url_api::pattern::PATTERN_OWNER),
    "Blob" => Global::Owner(super::fetch::blob::BLOB_OWNER),
    "File" => Global::Owner(super::fetch::blob::FILE_OWNER),
    "Headers" => Global::Owner(super::fetch::headers::HEADERS_OWNER),
    "FormData" => Global::Owner(super::fetch::form_data::FORM_DATA_OWNER),
    "Request" => Global::Owner(super::fetch::request::REQUEST_OWNER),
    "Response" => Global::Owner(super::fetch::response::RESPONSE_OWNER),
    "SuppressedError" => Global::Owner(error_family::SUPPRESSED_ERROR),
    "DisposableStack" => Global::Owner(disposable_stack::SYNC_OWNER),
    "AsyncDisposableStack" => Global::Owner(disposable_stack::ASYNC_OWNER),
    "parseInt" => Global::Function(global_functions::PARSE_INT),
    "parseFloat" => Global::Function(global_functions::PARSE_FLOAT),
    "isNaN" => Global::Function(global_functions::IS_NAN),
    "isFinite" => Global::Function(global_functions::IS_FINITE),
    "encodeURI" => Global::Function(global_functions::ENCODE_URI),
    "decodeURI" => Global::Function(global_functions::DECODE_URI),
    "encodeURIComponent" => Global::Function(global_functions::ENCODE_URI_COMPONENT),
    "decodeURIComponent" => Global::Function(global_functions::DECODE_URI_COMPONENT),
    "eval" => Global::Function(global_functions::EVAL),
    "Iterator" => Global::Function(super::iterator::OWNER),
    "DOMException" => Global::Owner(super::dom_exception::OWNER),
    "structuredClone" => Global::Function(super::structured_clone::OWNER),
    "Event" => Global::Owner(super::event::OWNER),
    "CustomEvent" => Global::Root(super::event::CUSTOM_ROOT),
    "EventTarget" => Global::Root(super::event::TARGET_ROOT),
    "AbortController" => Global::Owner(super::abort::CONTROLLER_OWNER),
    "AbortSignal" => Global::Owner(super::abort::SIGNAL_OWNER),
    "ErrorEvent" => Global::Root(super::event::ERROR_ROOT),
    "PromiseRejectionEvent" => Global::Root(super::event::PROMISE_REJECTION_ROOT),
    "reportError" => Global::Function(global_functions::REPORT_ERROR),
    "Performance" => Global::Owner(super::performance::OWNER),
    "performance" => Global::AccessorRoot(super::performance::INSTANCE),
    "setTimeout" => Global::Owner(super::timers::SET_TIMEOUT),
    "setInterval" => Global::Owner(super::timers::SET_INTERVAL),
    "clearTimeout" => Global::Owner(super::timers::CLEAR_TIMEOUT),
    "clearInterval" => Global::Owner(super::timers::CLEAR_INTERVAL),
    "atob" => Global::Function(global_functions::ATOB),
    "btoa" => Global::Function(global_functions::BTOA),
    "CountQueuingStrategy" => Global::Owner(super::streams::COUNT_OWNER),
    "ByteLengthQueuingStrategy" => Global::Owner(super::streams::BYTE_LENGTH_OWNER),
    "ReadableStream" => Global::Owner(super::streams::STREAM_OWNER),
    "ReadableStreamDefaultReader" => Global::Owner(super::streams::READER_OWNER),
    "ReadableStreamDefaultController" => Global::Owner(super::streams::CONTROLLER_OWNER),
    "ReadableByteStreamController" => Global::Owner(super::streams::BYTE_CONTROLLER_OWNER),
    "ReadableStreamBYOBRequest" => Global::Owner(super::streams::BYOB_REQUEST_OWNER),
    "ReadableStreamBYOBReader" => Global::Owner(super::streams::BYOB_READER_OWNER),
    "WritableStream" => Global::Owner(super::streams::WRITABLE_OWNER),
    "WritableStreamDefaultWriter" => Global::Owner(super::streams::WRITER_OWNER),
    "WritableStreamDefaultController" => Global::Owner(super::streams::WRITABLE_CONTROLLER_OWNER),
    "TransformStream" => Global::Owner(super::streams::TRANSFORM_OWNER),
    "TransformStreamDefaultController" => Global::Owner(super::streams::TRANSFORM_CONTROLLER_OWNER),
    "fetch" => Global::Function(super::fetch::controller::OWNER),
    "console" => Global::Owner(super::console::OWNER),
}

fn owners() -> impl Iterator<Item = &'static Owner> {
    GLOBALS.iter().filter_map(|global| match global {
        Global::Owner(owner) | Global::Function(owner) => Some(owner),
        Global::Number(_) | Global::Root(_) | Global::AccessorRoot(_) => None,
    })
}

pub(crate) fn intrinsic_groups() -> impl Iterator<Item = &'static [IntrinsicDef]> {
    owners()
        .flat_map(|owner| owner.groups.iter().copied())
        .chain([
            exec::FUNCTION_INTRINSICS,
            ITERATOR_INTRINSICS,
            super::iterator::INTRINSICS,
            generator::INTRINSICS,
            async_generator::INTRINSICS,
            super::disposal::SCOPE_INTRINSICS.as_slice(),
            super::global_handlers::INTRINSICS,
        ])
}

pub(crate) fn root_groups() -> impl Iterator<Item = &'static [RootSpec]> {
    let (object, object_rest) = object_members::ROOTS.split_at(1);
    let (function, function_rest) = exec::FUNCTION_ROOTS.split_at(1);
    [
        object,
        function,
        function_rest,
        super::iterator::ROOTS,
        generator::ROOTS,
        async_generator::ROOTS,
        super::disposal::SCOPE_ROOTS,
        super::event::ROOTS,
    ]
    .into_iter()
    .chain(owners().map(move |owner| {
        if owner.root == object_members::ROOT {
            object_rest
        } else if owner.root == super::event::EVENT_ROOT {
            &[]
        } else {
            owner.roots
        }
    }))
}

impl Vm {
    pub(crate) fn builtin_value(&self, index: u32) -> Result<Value, VmError> {
        if let Some(offset) = crate::bytecode::DisposeBuiltin::offset(index) {
            let key = super::disposal::SCOPE_INTRINSICS[offset].key;
            return Ok(Value::object(self.builtins.get(key.member)));
        }
        match GLOBALS.get(index as usize) {
            Some(Global::Owner(owner) | Global::Function(owner)) => {
                Ok(Value::object(self.builtins.get(owner.root)))
            }
            Some(Global::Number(number)) => Ok(Value::number(*number)),
            Some(Global::Root(root) | Global::AccessorRoot(root)) => {
                Ok(Value::object(self.builtins.get(root)))
            }
            None => Err(VmError::InvalidState(format!(
                "builtin {index} does not exist"
            ))),
        }
    }

    pub(crate) fn init_builtins(&mut self) {
        self.allocate_builtin_roots();
        let function_prototype = self.builtins.get("function.prototype");
        super::link_constructor(self, self.builtins.get("function"), function_prototype);
        for key in exec::FUNCTION_METHODS {
            let method = self.alloc_intrinsic(key);
            super::define_builtin_data(
                self,
                function_prototype,
                key.member,
                Value::object(method),
                Attributes::METHOD,
            );
        }
        let poison = self.alloc_intrinsic(exec::THROW_TYPE_ERROR);
        for (key, value) in [
            ("length", Value::number(0.0)),
            ("name", Value::string("".into())),
        ] {
            super::define_builtin_data(self, poison, key, value, Attributes::FIXED);
        }
        self.heap
            .get_mut(poison)
            .expect("poison function")
            .prevent_extensions();
        for key in ["caller", "arguments"] {
            assert!(
                self.heap
                    .apply_descriptor(
                        function_prototype,
                        key.into(),
                        DescriptorUpdate {
                            get: Some(Some(poison)),
                            set: Some(Some(poison)),
                            configurable: Some(true),
                            ..Default::default()
                        }
                    )
                    .expect("restricted function accessor")
            );
        }
        let has_instance = self.alloc_intrinsic(exec::FUNCTION_HAS_INSTANCE);
        super::define_builtin_data(
            self,
            function_prototype,
            PropertyKey::Symbol(HAS_INSTANCE_SYMBOL),
            Value::object(has_instance),
            Attributes::FIXED,
        );
        let identity = Value::object(self.alloc_intrinsic(ITERATOR_IDENTITY));
        super::iterator::install(self, &identity);
        for owner in owners() {
            if let Some(install) = owner.install {
                install(self, &identity);
            }
        }
        for (root, tag) in [
            (math::OWNER.root, "Math"),
            (json::OWNER.root, "JSON"),
            (reflect_namespace::OWNER.root, "Reflect"),
            ("promise.prototype", "Promise"),
        ] {
            super::define_builtin_data(
                self,
                self.builtins.get(root),
                PropertyKey::Symbol(TO_STRING_TAG_SYMBOL),
                Value::string(tag.into()),
                Attributes::FUNCTION_METADATA,
            );
        }
        self.install_generator_methods();
        self.install_async_generator_methods();
        let global_this = self.builtins.get(GLOBAL_THIS);
        for (index, name) in BUILTINS.iter().enumerate() {
            if matches!(GLOBALS[index], Global::AccessorRoot(_)) {
                continue;
            }
            let name: &str = name;
            let value = self.builtin_value(index as u32).expect("global builtin");
            let attributes = if matches!(GLOBALS[index], Global::Function(_)) || name == "console" {
                Attributes::METHOD
            } else {
                Attributes::ORDINARY
            };
            self.heap
                .define_data(global_this, name, value, attributes)
                .expect("global builtin property");
        }
        self.heap
            .define_data(
                global_this,
                "undefined",
                Value::UNDEFINED,
                Attributes::ORDINARY,
            )
            .expect("global undefined property");
        super::global_handlers::install(self);
    }
}
