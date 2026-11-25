//! File API objects: immutable byte storage and independently branded views.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::builtin::binary::text::{Source, source_bytes};
use crate::builtin::binary::{ARRAY_BUFFER_PROTOTYPE, Completion, MAX_BYTE_LENGTH};
use crate::builtin::event::install_accessor;
use crate::builtin::globals::Owner;
use crate::builtin::{
    FUNCTION_PARENT, OBJECT_PARENT, RootKind, RootSpec, define_builtin_data, link_constructor,
};
use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::heap::{
    ArrayBufferObject, Attributes, ElementKind, Object, PlainObject, TO_STRING_TAG_SYMBOL,
    TypedArrayObject,
};
use crate::host::Host;
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::runtime::{Invocation, Settlement};
use crate::value::{ObjectId, PropertyKey, PropertyKeyRef, Value, ValueRef};
use crate::vm::VmCore;

const BLOB: &str = "blob";
pub(crate) const BLOB_PROTOTYPE: &str = "blob.prototype";
const FILE: &str = "file";
pub(crate) const FILE_PROTOTYPE: &str = "file.prototype";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobData {
    pub(crate) bytes: Arc<Vec<u8>>,
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) mime_type: String,
}

impl BlobData {
    pub(crate) fn valid(&self) -> bool {
        self.start <= self.end
            && self.end <= self.bytes.len()
            && self
                .mime_type
                .bytes()
                .all(|b| (0x20..=0x7e).contains(&b) && !b.is_ascii_uppercase())
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.bytes[self.start..self.end]
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileData {
    pub(crate) blob: BlobData,
    pub(crate) name: String,
    pub(crate) last_modified: i64,
}

impl FileData {
    pub(crate) fn valid(&self) -> bool {
        self.blob.valid()
    }
}

pub(crate) fn alloc_file(vm: &mut VmCore, data: FileData) -> ObjectId {
    vm.heap.alloc(Object::File {
        object: PlainObject::new(Some(vm.builtins.get(FILE_PROTOTYPE))),
        data,
    })
}

pub(crate) fn blob_now(clock: &dyn crate::host::TimeService) -> i64 {
    clock.now_ms()
}

macro_rules! converted {
    ($value:expr) => {
        match $value? {
            Ok(value) => value,
            Err(error) => return Ok(Invocation::Thrown(error)),
        }
    };
}

const BLOB_DEFS: &[IntrinsicDef] = &[
    IntrinsicDef::constructor_with_target(
        IntrinsicKey::new("Blob", Slot::Global, ""),
        0,
        |vm, _, roots, _, _| vm.throw_type_error(roots, "Blob constructor requires new"),
        |vm, host, roots, args, target| construct(vm, host, roots, args, target, false),
    ),
    IntrinsicDef::call(
        IntrinsicKey::new("Blob", Slot::PrototypeGetter, "size"),
        0,
        |vm, _, roots, _, this| {
            blob_get(vm, roots, this, |data| {
                Value::number((data.end - data.start) as f64)
            })
        },
    ),
    IntrinsicDef::call(
        IntrinsicKey::new("Blob", Slot::PrototypeGetter, "type"),
        0,
        |vm, _, roots, _, this| {
            blob_get(vm, roots, this, |data| {
                Value::string(data.mime_type.as_str().into())
            })
        },
    ),
    IntrinsicDef::call(
        IntrinsicKey::new("Blob", Slot::Prototype, "slice"),
        0,
        slice,
    ),
    IntrinsicDef::call(
        IntrinsicKey::new("Blob", Slot::Prototype, "stream"),
        0,
        stream,
    ),
    IntrinsicDef::call(
        IntrinsicKey::new("Blob", Slot::Prototype, "arrayBuffer"),
        0,
        |vm, _, roots, _, this| read(vm, roots, this, ReadAs::Buffer),
    ),
    IntrinsicDef::call(
        IntrinsicKey::new("Blob", Slot::Prototype, "bytes"),
        0,
        |vm, _, roots, _, this| read(vm, roots, this, ReadAs::Bytes),
    ),
    IntrinsicDef::call(
        IntrinsicKey::new("Blob", Slot::Prototype, "text"),
        0,
        |vm, _, roots, _, this| read(vm, roots, this, ReadAs::Text),
    ),
];

const FILE_DEFS: &[IntrinsicDef] = &[
    IntrinsicDef::constructor_with_target(
        IntrinsicKey::new("File", Slot::Global, ""),
        2,
        |vm, _, roots, _, _| vm.throw_type_error(roots, "File constructor requires new"),
        |vm, host, roots, args, target| construct(vm, host, roots, args, target, true),
    ),
    IntrinsicDef::call(
        IntrinsicKey::new("File", Slot::PrototypeGetter, "name"),
        0,
        |vm, _, roots, _, this| {
            file_get(vm, roots, this, |data| {
                Value::string(data.name.as_str().into())
            })
        },
    ),
    IntrinsicDef::call(
        IntrinsicKey::new("File", Slot::PrototypeGetter, "lastModified"),
        0,
        |vm, _, roots, _, this| {
            file_get(vm, roots, this, |data| {
                Value::number(data.last_modified as f64)
            })
        },
    ),
];

pub(crate) const BLOB_OWNER: Owner = Owner::new(
    BLOB,
    &[
        RootSpec::new(
            BLOB,
            RootKind::Native(IntrinsicKey::new("Blob", Slot::Global, "")),
            FUNCTION_PARENT,
        )
        .fixed(BLOB_PROTOTYPE)
        .backlink(Some(Attributes::METHOD)),
        RootSpec::new(BLOB_PROTOTYPE, RootKind::Plain, OBJECT_PARENT),
    ],
    &[BLOB_DEFS],
    Some(|vm, _| install(vm, BLOB, BLOB_PROTOTYPE, "Blob", BLOB_DEFS)),
);

pub(crate) const FILE_OWNER: Owner = Owner::new(
    FILE,
    &[
        RootSpec::new(
            FILE,
            RootKind::Native(IntrinsicKey::new("File", Slot::Global, "")),
            Some(BLOB),
        )
        .fixed(FILE_PROTOTYPE)
        .backlink(Some(Attributes::METHOD)),
        RootSpec::new(FILE_PROTOTYPE, RootKind::Plain, Some(BLOB_PROTOTYPE)),
    ],
    &[FILE_DEFS],
    Some(|vm, _| install(vm, FILE, FILE_PROTOTYPE, "File", FILE_DEFS)),
);

fn install(
    vm: &mut VmCore,
    root: &'static str,
    proto: &'static str,
    tag: &str,
    defs: &[IntrinsicDef],
) {
    let prototype = vm.builtins.get(proto);
    link_constructor(vm, vm.builtins.get(root), prototype);
    for def in defs {
        let key = def.key;
        match key.slot {
            Slot::PrototypeGetter => {
                let getter = vm.alloc_intrinsic(key);
                install_accessor(vm, prototype, key.member, getter, None, true)
                    .expect("File API accessor");
            }
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
            _ => {}
        }
    }
    define_builtin_data(
        vm,
        prototype,
        PropertyKey::Symbol(TO_STRING_TAG_SYMBOL),
        Value::string(tag.into()),
        Attributes::FUNCTION_METADATA,
    );
}

fn blob_receiver<'a>(vm: &'a VmCore, this: &Value) -> Result<Option<&'a BlobData>, VmError> {
    let ValueRef::Object(id) = this.view() else {
        return Ok(None);
    };
    Ok(match vm.heap.get(id)? {
        Object::Blob { data, .. } => Some(data),
        Object::File { data, .. } => Some(&data.blob),
        _ => None,
    })
}

fn blob_get(
    vm: &mut VmCore,
    roots: &ActiveRoots<'_>,
    this: &Value,
    get: impl FnOnce(&BlobData) -> Value,
) -> Result<Invocation, VmError> {
    match blob_receiver(vm, this)? {
        Some(data) => Ok(Invocation::Value(get(data))),
        None => vm.throw_type_error(roots, "incompatible Blob receiver"),
    }
}

fn file_get(
    vm: &mut VmCore,
    roots: &ActiveRoots<'_>,
    this: &Value,
    get: impl FnOnce(&FileData) -> Value,
) -> Result<Invocation, VmError> {
    if let ValueRef::Object(id) = this.view()
        && let Object::File { data, .. } = vm.heap.get(id)?
    {
        return Ok(Invocation::Value(get(data)));
    }
    vm.throw_type_error(roots, "incompatible File receiver")
}

fn usv(vm: &mut VmCore, host: &Host, roots: &ActiveRoots<'_>, value: &Value) -> Completion<String> {
    Ok(vm.coerce_string(host, roots, value)?.map(|value| {
        value
            .to_utf8()
            .unwrap_or_else(|_| String::from_utf16_lossy(&value.to_utf16()))
    }))
}

fn mime(mut text: String) -> String {
    if text.bytes().all(|b| (0x20..=0x7e).contains(&b)) {
        text.make_ascii_lowercase();
        text
    } else {
        String::new()
    }
}

enum Part {
    Text(String),
    Blob(BlobData),
    Bytes(Vec<u8>),
}

fn part(vm: &mut VmCore, host: &Host, roots: &ActiveRoots<'_>, value: &Value) -> Completion<Part> {
    if let Some(data) = blob_receiver(vm, value)? {
        return Ok(Ok(Part::Blob(data.clone())));
    }
    if let ValueRef::Object(id) = value.view() {
        let source = match vm.heap.get(id)? {
            Object::ArrayBuffer(_) => Some(Source::Buffer(id)),
            Object::TypedArray(_) | Object::DataView(_) => Some(Source::View(id)),
            _ => None,
        };
        if let Some(source) = source {
            return Ok(Ok(Part::Bytes(source_bytes(vm, source)?.to_vec())));
        }
    }
    Ok(usv(vm, host, roots, value)?.map(Part::Text))
}

fn dictionary_member(
    vm: &mut VmCore,
    host: &Host,
    roots: &ActiveRoots<'_>,
    options: &Value,
    member: &str,
) -> Completion<Value> {
    Ok(match options.view() {
        ValueRef::Undefined | ValueRef::Null => Ok(Value::UNDEFINED),
        ValueRef::Object(_) => {
            match vm.read_property(host, roots, options, PropertyKeyRef::String(member))? {
                Invocation::Value(value) => Ok(value),
                Invocation::Thrown(error) => Err(error),
            }
        }
        _ => Err(vm.type_error(roots, "File API options must be a dictionary")?),
    })
}

// Web IDL signed long long (without Clamp or EnforceRange), reduced modulo 2^64.
fn signed_long_long(number: f64) -> i64 {
    let modulus = 18446744073709551616.0;
    let half = modulus / 2.0;
    if !number.is_finite() {
        return 0;
    }
    let reduced = number.trunc() % modulus;
    if reduced >= half {
        (reduced - modulus) as i64
    } else if reduced < -half {
        (reduced + modulus) as i64
    } else {
        reduced as i64
    }
}

fn construct(
    vm: &mut VmCore,
    host: &Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    target: &Value,
    file: bool,
) -> Result<Invocation, VmError> {
    if file && args.len() < 2 {
        return vm.throw_type_error(roots, "File requires a sequence and a name");
    }
    let rooted = (&args, target);
    let roots = &roots.with(&rooted);
    let undefined = Value::UNDEFINED;
    let input = args.first().unwrap_or(&undefined);
    let mut parts = Vec::new();
    if file || !matches!(input.view(), ValueRef::Undefined) {
        if !matches!(input.view(), ValueRef::Object(_)) {
            return vm.throw_type_error(roots, "Blob parts must be an iterable object");
        }
        let (iterator, next) = converted!(vm.get_iterator(host, roots, input));
        let rooted = (&iterator, &next);
        let roots = &roots.with(&rooted);
        while let Some(value) = converted!(vm.iterator_step(host, roots, &iterator, &next)) {
            let value_roots = &roots.with(&value);
            let converted = match part(vm, host, value_roots, &value)? {
                Ok(part) => part,
                Err(error) => {
                    let rooted = (&iterator, &error);
                    let _ = vm.close_iterator(host, &roots.with(&rooted), &iterator)?;
                    return Ok(Invocation::Thrown(error));
                }
            };
            parts.push(converted);
        }
    }
    let name = if file {
        converted!(usv(vm, host, roots, &args[1]))
    } else {
        String::new()
    };
    let options = args.get(if file { 2 } else { 1 }).unwrap_or(&undefined);
    let content_type = converted!(dictionary_member(vm, host, roots, options, "type"));
    let mime_type = if matches!(content_type.view(), ValueRef::Undefined) {
        String::new()
    } else {
        mime(converted!(usv(vm, host, roots, &content_type)))
    };
    let endings = converted!(dictionary_member(vm, host, roots, options, "endings"));
    let converted_endings = if matches!(endings.view(), ValueRef::Undefined) {
        String::from("transparent")
    } else {
        converted!(usv(vm, host, roots, &endings))
    };
    let native_endings = match converted_endings.as_str() {
        "transparent" => false,
        "native" => true,
        _ => return vm.throw_type_error(roots, "invalid Blob endings"),
    };
    let last_modified = if file {
        let value = converted!(dictionary_member(vm, host, roots, options, "lastModified"));
        if matches!(value.view(), ValueRef::Undefined) {
            None
        } else {
            Some(signed_long_long(converted!(
                vm.coerce_number(host, roots, &value)
            )))
        }
    } else {
        None
    };

    let mut bytes = Vec::new();
    for part in parts {
        match part {
            Part::Text(text) if native_endings => {
                let newline = if cfg!(windows) { "\r\n" } else { "\n" };
                let mut chunks = text.split_inclusive(['\r', '\n']).peekable();
                while let Some(chunk) = chunks.next() {
                    if let Some(prefix) = chunk
                        .strip_suffix('\r')
                        .or_else(|| chunk.strip_suffix('\n'))
                    {
                        bytes.extend_from_slice(prefix.as_bytes());
                        bytes.extend_from_slice(newline.as_bytes());
                        if chunk.ends_with('\r') && chunks.peek() == Some(&"\n") {
                            chunks.next();
                        }
                    } else {
                        bytes.extend_from_slice(chunk.as_bytes());
                    }
                }
            }
            Part::Text(text) => bytes.extend_from_slice(text.as_bytes()),
            Part::Blob(data) => bytes.extend_from_slice(data.as_bytes()),
            Part::Bytes(data) => bytes.extend_from_slice(&data),
        }
    }
    let end = bytes.len();
    let blob = BlobData {
        bytes: Arc::new(bytes),
        start: 0,
        end,
        mime_type,
    };
    let prototype = converted!(vm.prototype_from(
        host,
        roots,
        target,
        if file { FILE_PROTOTYPE } else { BLOB_PROTOTYPE }
    ));
    let object = PlainObject::new(Some(prototype));
    let entry = if file {
        Object::File {
            object,
            data: FileData {
                blob,
                name,
                last_modified: last_modified.unwrap_or_else(|| blob_now(vm.time())),
            },
        }
    } else {
        Object::Blob { object, data: blob }
    };
    Ok(Invocation::Value(Value::object(vm.heap.alloc(entry))))
}

fn slice(
    vm: &mut VmCore,
    host: &Host,
    roots: &ActiveRoots<'_>,
    args: Vec<Value>,
    this: &Value,
) -> Result<Invocation, VmError> {
    let Some(data) = blob_receiver(vm, this)?.cloned() else {
        return vm.throw_type_error(roots, "incompatible Blob receiver");
    };
    let roots = &roots.with(&args);
    let size = (data.end - data.start) as i64;
    fn clamp(n: f64) -> i64 {
        if n.is_nan() {
            0
        } else if n >= 9_223_372_036_854_775_807.0 {
            i64::MAX
        } else if n <= -9_223_372_036_854_775_808.0 {
            i64::MIN
        } else {
            n.trunc() as i64
        }
    }
    fn relative(n: f64, size: i64) -> usize {
        let n = clamp(n) as i128;
        let size = size as i128;
        let position = if n < 0 {
            (size + n).max(0)
        } else {
            n.min(size)
        };
        position as usize
    }
    let start = match args.first() {
        Some(value) => relative(converted!(vm.coerce_number(host, roots, value)), size),
        None => 0,
    };
    let end = match args.get(1) {
        Some(value) if !matches!(value.view(), ValueRef::Undefined) => {
            relative(converted!(vm.coerce_number(host, roots, value)), size)
        }
        _ => size as usize,
    };
    let mime_type = match args.get(2) {
        Some(value) if !matches!(value.view(), ValueRef::Undefined) => {
            mime(converted!(usv(vm, host, roots, value)))
        }
        _ => String::new(),
    };
    Ok(Invocation::Value(Value::object(vm.heap.alloc(
        Object::Blob {
            object: PlainObject::new(Some(vm.builtins.get(BLOB_PROTOTYPE))),
            data: BlobData {
                bytes: data.bytes,
                start: data.start + start,
                end: data.start + end.max(start),
                mime_type,
            },
        },
    ))))
}

fn stream(
    vm: &mut VmCore,
    _: &Host,
    roots: &ActiveRoots<'_>,
    _: Vec<Value>,
    this: &Value,
) -> Result<Invocation, VmError> {
    let Some(data) = blob_receiver(vm, this)? else {
        return vm.throw_type_error(roots, "incompatible Blob receiver");
    };
    let bytes = data.as_bytes().to_vec();
    Ok(Invocation::Value(Value::object(
        vm.readable_from_bytes(bytes)?,
    )))
}

enum ReadAs {
    Buffer,
    Bytes,
    Text,
}

fn read(
    vm: &mut VmCore,
    roots: &ActiveRoots<'_>,
    this: &Value,
    mode: ReadAs,
) -> Result<Invocation, VmError> {
    let settlement = match blob_receiver(vm, this)? {
        None => Settlement::Rejected(vm.type_error(roots, "incompatible Blob receiver")?),
        Some(data) => match mode {
            ReadAs::Text => {
                let bytes = data.as_bytes();
                let text =
                    String::from_utf8_lossy(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes));
                Settlement::Fulfilled(Value::string(text.as_ref().into()))
            }
            ReadAs::Buffer | ReadAs::Bytes => {
                let bytes = data.as_bytes();
                let len = bytes.len();
                let mut copy = Vec::new();
                if len > MAX_BYTE_LENGTH || copy.try_reserve_exact(len).is_err() {
                    Settlement::Rejected(vm.make_range_error(roots, "Blob byte allocation failed")?)
                } else {
                    copy.extend_from_slice(bytes);
                    let buffer = vm.heap.alloc(Object::ArrayBuffer(ArrayBufferObject {
                        object: PlainObject::new(Some(vm.builtins.get(ARRAY_BUFFER_PROTOTYPE))),
                        bytes: Some(copy),
                        max_byte_length: None,
                    }));
                    let result = if matches!(mode, ReadAs::Bytes) {
                        vm.heap.alloc(Object::TypedArray(TypedArrayObject {
                            object: PlainObject::new(Some(vm.builtins.get("uint8Array.prototype"))),
                            buffer,
                            byte_offset: 0,
                            byte_length: Some(len),
                            kind: ElementKind::Uint8,
                        }))
                    } else {
                        buffer
                    };
                    Settlement::Fulfilled(Value::object(result))
                }
            }
        },
    };
    let promise = vm.heap.alloc_promise();
    vm.settle(promise, settlement)?;
    Ok(Invocation::Value(Value::object(promise)))
}
