//! Web Crypto interface objects and VM-side argument normalization.

use super::worker::Output;
use crate::VmError;
use crate::builtin::binary::ARRAY_BUFFER_PROTOTYPE;
use crate::builtin::globals::Owner;
use crate::builtin::roots::{RootKind, RootSpec};
use crate::builtin::{
    FUNCTION_PARENT, OBJECT_PARENT, define_builtin_data, install_getter, link_constructor,
};
use crate::gc::ActiveRoots;
use crate::heap::ArrayBufferObject;
use crate::heap::{
    Attributes, CryptoKey, CryptoKeyAlgorithm, CryptoKeyUsage, MAX_CRYPTO_KEY_BYTES, Object,
    PlainObject, TO_STRING_TAG_SYMBOL, ViewRange,
};
use crate::host::{Host, SubtleCryptoKey, SubtleCryptoRequest};
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::runtime::{Invocation, Settlement};
use crate::value::{ObjectId, PropertyKey, PropertyKeyRef, Value, ValueRef};
use crate::vm::Vm;

pub(super) const INSTANCE: &str = "subtleCrypto.instance";
pub(super) const PROTOTYPE: &str = "subtleCrypto.prototype";
const SUBTLE: &str = "subtleCrypto";
const KEY: &str = "cryptoKey";
const KEY_PROTO: &str = "cryptoKey.prototype";
const SUBTLE_CTOR: IntrinsicKey = IntrinsicKey::new("SubtleCrypto", Slot::Global, "");
const KEY_CTOR: IntrinsicKey = IntrinsicKey::new("CryptoKey", Slot::Global, "");
const DIGEST: IntrinsicKey = method("digest");
const IMPORT: IntrinsicKey = method("importKey");
const EXPORT: IntrinsicKey = method("exportKey");
const DERIVE: IntrinsicKey = method("deriveKey");
const ENCRYPT: IntrinsicKey = method("encrypt");
const DECRYPT: IntrinsicKey = method("decrypt");
const SIGN: IntrinsicKey = method("sign");
const VERIFY: IntrinsicKey = method("verify");
const TYPE: IntrinsicKey = getter("type");
const EXTRACTABLE: IntrinsicKey = getter("extractable");
const ALGORITHM: IntrinsicKey = getter("algorithm");
const USAGES: IntrinsicKey = getter("usages");

const fn method(name: &'static str) -> IntrinsicKey {
    IntrinsicKey::new("SubtleCrypto", Slot::Prototype, name)
}

const fn getter(name: &'static str) -> IntrinsicKey {
    IntrinsicKey::new("CryptoKey", Slot::PrototypeGetter, name)
}

pub(in crate::builtin) const SUBTLE_OWNER: Owner = Owner::new(
    SUBTLE,
    &[
        RootSpec::new(SUBTLE, RootKind::Native(SUBTLE_CTOR), FUNCTION_PARENT)
            .fixed(PROTOTYPE)
            .backlink(Some(Attributes::METHOD)),
    ],
    &[&[illegal_constructor(SUBTLE_CTOR)]],
    Some(|vm, _| link_constructor(vm, vm.builtins.get(SUBTLE), vm.builtins.get(PROTOTYPE))),
);

pub(in crate::builtin) const KEY_OWNER: Owner = Owner::new(
    KEY,
    &[
        RootSpec::new(KEY, RootKind::Native(KEY_CTOR), FUNCTION_PARENT)
            .fixed(KEY_PROTO)
            .backlink(Some(Attributes::METHOD)),
        RootSpec::new(KEY_PROTO, RootKind::Plain, OBJECT_PARENT),
    ],
    &[&[
        illegal_constructor(KEY_CTOR),
        key_getter(TYPE),
        key_getter(EXTRACTABLE),
        key_getter(ALGORITHM),
        key_getter(USAGES),
    ]],
    Some(install_key),
);

const fn illegal_constructor(key: IntrinsicKey) -> IntrinsicDef {
    IntrinsicDef::constructor_with_target(
        key,
        0,
        |vm, _, roots, _, _| vm.throw_type_error(roots, "Illegal constructor"),
        |vm, _, roots, _, _| vm.throw_type_error(roots, "Illegal constructor"),
    )
}

const fn key_getter(key: IntrinsicKey) -> IntrinsicDef {
    // Intrinsic callbacks cannot capture which member they implement.
    match key.member.as_bytes() {
        b"type" => IntrinsicDef::call(key, 0, |vm, _, roots, _, recv| {
            key_property(vm, roots, recv, TYPE)
        }),
        b"extractable" => IntrinsicDef::call(key, 0, |vm, _, roots, _, recv| {
            key_property(vm, roots, recv, EXTRACTABLE)
        }),
        b"algorithm" => IntrinsicDef::call(key, 0, |vm, _, roots, _, recv| {
            key_property(vm, roots, recv, ALGORITHM)
        }),
        _ => IntrinsicDef::call(key, 0, |vm, _, roots, _, recv| {
            key_property(vm, roots, recv, USAGES)
        }),
    }
}

macro_rules! promise_method {
    ($key:expr, $arity:expr, $prepare:expr) => {
        IntrinsicDef::call($key, $arity, |vm, host, roots, args, receiver| {
            call(vm, host, roots, &args, receiver, $arity, $prepare)
        })
    };
}

pub(super) const MEMBERS: &[IntrinsicDef] = &[
    promise_method!(DIGEST, 2, digest),
    promise_method!(IMPORT, 5, import_key),
    promise_method!(EXPORT, 2, export_key),
    promise_method!(DERIVE, 5, derive_key),
    promise_method!(ENCRYPT, 3, |vm, host, roots, args| gcm(
        vm, host, roots, args, true
    )),
    promise_method!(DECRYPT, 3, |vm, host, roots, args| gcm(
        vm, host, roots, args, false
    )),
    promise_method!(SIGN, 3, |vm, host, roots, args| hmac(
        vm, host, roots, args, false
    )),
    promise_method!(VERIFY, 4, |vm, host, roots, args| hmac(
        vm, host, roots, args, true
    )),
];

pub(super) fn install(vm: &mut Vm) {
    let proto = vm.builtins.get(PROTOTYPE);
    tag(vm, proto, "SubtleCrypto");
    for key in [
        DIGEST, IMPORT, EXPORT, DERIVE, ENCRYPT, DECRYPT, SIGN, VERIFY,
    ] {
        let method = vm.alloc_intrinsic(key);
        define_builtin_data(
            vm,
            proto,
            key.member,
            Value::object(method),
            Attributes::METHOD,
        );
    }
}

fn install_key(vm: &mut Vm, _: &Value) {
    let proto = vm.builtins.get(KEY_PROTO);
    link_constructor(vm, vm.builtins.get(KEY), proto);
    tag(vm, proto, "CryptoKey");
    for key in [TYPE, EXTRACTABLE, ALGORITHM, USAGES] {
        install_getter(vm, proto, key.member.into(), key);
    }
}

fn tag(vm: &mut Vm, proto: ObjectId, name: &str) {
    define_builtin_data(
        vm,
        proto,
        PropertyKey::Symbol(TO_STRING_TAG_SYMBOL),
        Value::string(name.into()),
        Attributes::FUNCTION_METADATA,
    );
}

fn key_property(
    vm: &mut Vm,
    roots: &ActiveRoots<'_>,
    receiver: &Value,
    member: IntrinsicKey,
) -> Result<Invocation, VmError> {
    let ValueRef::Object(id) = receiver.view() else {
        return vm.throw_type_error(roots, "Incompatible CryptoKey receiver");
    };
    let Object::CryptoKey(key) = vm.heap.get(id)? else {
        return vm.throw_type_error(roots, "Incompatible CryptoKey receiver");
    };
    let value = match member.member {
        "type" => Value::string("secret".into()),
        "extractable" => Value::boolean(true),
        "usages" => {
            let values = key
                .usages
                .iter()
                .map(|usage| Value::string(usage_name(*usage).into()))
                .collect();
            Value::object(vm.heap.alloc_array(values))
        }
        _ => {
            let algorithm = key.algorithm;
            let length = key.bytes.len() * 8;
            let object = vm.heap.alloc_plain();
            define_builtin_data(
                vm,
                object,
                "name",
                Value::string(key_name(algorithm).into()),
                Attributes::ORDINARY,
            );
            if algorithm != CryptoKeyAlgorithm::Pbkdf2 {
                define_builtin_data(
                    vm,
                    object,
                    "length",
                    Value::number(length as f64),
                    Attributes::ORDINARY,
                );
            }
            if algorithm == CryptoKeyAlgorithm::HmacSha256 {
                let hash = vm.heap.alloc_plain();
                define_builtin_data(
                    vm,
                    hash,
                    "name",
                    Value::string("SHA-256".into()),
                    Attributes::ORDINARY,
                );
                define_builtin_data(
                    vm,
                    object,
                    "hash",
                    Value::object(hash),
                    Attributes::ORDINARY,
                );
            }
            Value::object(object)
        }
    };
    Ok(Invocation::Value(value))
}

fn key_name(algorithm: CryptoKeyAlgorithm) -> &'static str {
    match algorithm {
        CryptoKeyAlgorithm::AesGcm => "AES-GCM",
        CryptoKeyAlgorithm::Pbkdf2 => "PBKDF2",
        CryptoKeyAlgorithm::HmacSha256 => "HMAC",
    }
}

fn usage_name(usage: CryptoKeyUsage) -> &'static str {
    match usage {
        CryptoKeyUsage::DeriveKey => "deriveKey",
        CryptoKeyUsage::Encrypt => "encrypt",
        CryptoKeyUsage::Decrypt => "decrypt",
        CryptoKeyUsage::Sign => "sign",
        CryptoKeyUsage::Verify => "verify",
    }
}

pub(super) fn alloc_key(
    vm: &mut Vm,
    algorithm: CryptoKeyAlgorithm,
    usages: Vec<CryptoKeyUsage>,
    bytes: Vec<u8>,
) -> Value {
    let key = CryptoKey {
        object: PlainObject::new(Some(vm.builtins.get(KEY_PROTO))),
        algorithm,
        usages,
        bytes,
    };
    debug_assert!(key.is_valid());
    Value::object(vm.heap.alloc(Object::CryptoKey(key)))
}

enum Failure {
    Fault(VmError),
    Thrown(Value),
    Type(&'static str),
    Dom(&'static str, &'static str),
}

impl From<VmError> for Failure {
    fn from(value: VmError) -> Self {
        Self::Fault(value)
    }
}

type Js<T> = Result<T, Failure>;
type Prepare = fn(&mut Vm, &mut dyn Host, &ActiveRoots<'_>, &[Value]) -> Js<Work>;

enum Work {
    Primitive(SubtleCryptoRequest, Output),
    Local(Value),
}

fn call(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    args: &[Value],
    receiver: &Value,
    required: usize,
    prepare: Prepare,
) -> Result<Invocation, VmError> {
    let result = if !matches!(receiver.view(), ValueRef::Object(id) if id == vm.builtins.get(INSTANCE))
    {
        Err(Failure::Type("Incompatible SubtleCrypto receiver"))
    } else if vm.bindings.service::<SubtleCryptoKey>().is_none() {
        Err(Failure::Dom(
            "NotSupportedError",
            "Host SubtleCrypto service is not available",
        ))
    } else if args.len() < required {
        Err(Failure::Type("Missing required SubtleCrypto argument"))
    } else {
        prepare(vm, host, roots, args)
    };
    let promise = vm.heap.alloc_promise();
    match result {
        Ok(Work::Primitive(request, output)) => {
            vm.start_subtle_request(promise, request, output)?
        }
        Ok(Work::Local(value)) => vm.queue_adopt(promise, Settlement::Fulfilled(value)),
        Err(Failure::Fault(fault)) => return Err(fault),
        Err(error) => {
            let reason = match error {
                Failure::Type(message) => vm.type_error(roots, message)?,
                Failure::Dom(name, message) => vm.alloc_dom_exception(name.into(), message.into()),
                Failure::Thrown(value) => value,
                Failure::Fault(_) => unreachable!(),
            };
            vm.queue_adopt(promise, Settlement::Rejected(reason));
        }
    }
    Ok(Invocation::Value(Value::object(promise)))
}

fn digest(vm: &mut Vm, host: &mut dyn Host, roots: &ActiveRoots<'_>, args: &[Value]) -> Js<Work> {
    let data = bytes(vm, &args[1])?;
    require_algorithm(vm, host, roots, &args[0], "SHA-256")?;
    Ok(Work::Primitive(
        SubtleCryptoRequest::DigestSha256 { data },
        Output::Bytes(32),
    ))
}

fn require_raw(format: &Value) -> Js<()> {
    match format.view() {
        ValueRef::String(name) if name == "raw" => Ok(()),
        ValueRef::String(_) => Err(Failure::Dom(
            "NotSupportedError",
            "Only the raw key format is supported",
        )),
        _ => Err(Failure::Type("Expected key format string")),
    }
}

fn require_extractable(value: &Value) -> Js<()> {
    if !value.truthy() {
        return Err(Failure::Dom(
            "NotSupportedError",
            "Nonextractable keys are not supported",
        ));
    }
    Ok(())
}

fn read_key<'a>(vm: &'a Vm, value: &Value) -> Js<&'a CryptoKey> {
    if let ValueRef::Object(id) = value.view()
        && let Object::CryptoKey(key) = vm.heap.get(id)?
    {
        return Ok(key);
    }
    Err(Failure::Type("Expected a CryptoKey"))
}

fn key_bytes(
    vm: &Vm,
    value: &Value,
    algorithm: CryptoKeyAlgorithm,
    usage: CryptoKeyUsage,
) -> Js<Vec<u8>> {
    let key = read_key(vm, value)?;
    if key.algorithm != algorithm || !key.usages.contains(&usage) {
        return Err(Failure::Dom(
            "InvalidAccessError",
            "CryptoKey algorithm or usage does not permit this operation",
        ));
    }
    Ok(key.bytes.clone())
}

fn key_usages(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    value: &Value,
    allowed: &[CryptoKeyUsage],
) -> Js<Vec<CryptoKeyUsage>> {
    let (iterator, next) = vm
        .get_iterator(host, roots, value)?
        .map_err(Failure::Thrown)?;
    let state = (&iterator, &next);
    let roots = &roots.with(&state);
    let mut usages = Vec::new();
    while let Some(value) = vm
        .iterator_step(host, roots, &iterator, &next)?
        .map_err(Failure::Thrown)?
    {
        let ValueRef::String(name) = value.view() else {
            return Err(Failure::Type("CryptoKey usage must be a string"));
        };
        let Some(usage) = [
            CryptoKeyUsage::DeriveKey,
            CryptoKeyUsage::Encrypt,
            CryptoKeyUsage::Decrypt,
            CryptoKeyUsage::Sign,
            CryptoKeyUsage::Verify,
        ]
        .into_iter()
        .find(|usage| name == usage_name(*usage)) else {
            if name == "deriveBits" || name == "wrapKey" || name == "unwrapKey" {
                return Err(Failure::Dom("SyntaxError", "Unsupported CryptoKey usage"));
            }
            return Err(Failure::Type("Unknown CryptoKey usage"));
        };
        if !allowed.contains(&usage) {
            return Err(Failure::Dom(
                "SyntaxError",
                "CryptoKey usage is incompatible with the algorithm",
            ));
        }
        if !usages.contains(&usage) {
            usages.push(usage);
        }
    }
    if usages.is_empty() {
        return Err(Failure::Dom(
            "SyntaxError",
            "Secret keys require at least one usage",
        ));
    }
    Ok(usages)
}

const AES_USAGES: &[CryptoKeyUsage] = &[CryptoKeyUsage::Encrypt, CryptoKeyUsage::Decrypt];
const HMAC_USAGES: &[CryptoKeyUsage] = &[CryptoKeyUsage::Sign, CryptoKeyUsage::Verify];
const PBKDF2_USAGES: &[CryptoKeyUsage] = &[CryptoKeyUsage::DeriveKey];

fn import_key(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    args: &[Value],
) -> Js<Work> {
    require_raw(&args[0])?;
    let data = bytes(vm, &args[1])?;
    let params = &args[2];
    let name = algorithm_name(vm, host, roots, params)?;
    require_extractable(&args[3])?;
    let (algorithm, allowed) = if named(&name, "AES-GCM") {
        if !matches!(data.len(), 16 | 24 | 32) {
            return Err(Failure::Dom(
                "DataError",
                "AES keys must be 128, 192 or 256 bits",
            ));
        }
        (CryptoKeyAlgorithm::AesGcm, AES_USAGES)
    } else if named(&name, "HMAC") {
        require_hash(vm, host, roots, params)?;
        if data.is_empty() {
            return Err(Failure::Dom("DataError", "HMAC keys must not be empty"));
        }
        let length = prop(vm, host, roots, params, "length")?;
        if !matches!(length.view(), ValueRef::Undefined)
            && unsigned(vm, host, roots, &length, u32::MAX)? as usize != data.len() * 8
        {
            return Err(Failure::Dom(
                "DataError",
                "This HMAC key length must equal the raw key length",
            ));
        }
        (CryptoKeyAlgorithm::HmacSha256, HMAC_USAGES)
    } else if named(&name, "PBKDF2") {
        (CryptoKeyAlgorithm::Pbkdf2, PBKDF2_USAGES)
    } else {
        return Err(Failure::Dom(
            "NotSupportedError",
            "Unsupported key algorithm",
        ));
    };
    let usages = key_usages(vm, host, roots, &args[4], allowed)?;
    Ok(Work::Local(alloc_key(vm, algorithm, usages, data)))
}

fn export_key(vm: &mut Vm, _: &mut dyn Host, _: &ActiveRoots<'_>, args: &[Value]) -> Js<Work> {
    require_raw(&args[0])?;
    let data = read_key(vm, &args[1])?.bytes.clone();
    let object = PlainObject::new(Some(vm.builtins.get(ARRAY_BUFFER_PROTOTYPE)));
    Ok(Work::Local(Value::object(vm.heap.alloc(
        Object::ArrayBuffer(ArrayBufferObject {
            object,
            bytes: Some(data),
            max_byte_length: None,
        }),
    ))))
}

fn derive_key(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    args: &[Value],
) -> Js<Work> {
    let params = &args[0];
    require_algorithm(vm, host, roots, params, "PBKDF2")?;
    let salt = dictionary_prop(vm, host, roots, params, "salt")?;
    let salt = bytes(vm, &salt)?;
    let iterations = prop(vm, host, roots, params, "iterations")?;
    let iterations = unsigned(vm, host, roots, &iterations, u32::MAX)?;
    require_hash(vm, host, roots, params)?;
    if iterations == 0 {
        return Err(Failure::Dom(
            "OperationError",
            "PBKDF2 iterations must be positive",
        ));
    }
    let target = &args[2];
    require_algorithm(vm, host, roots, target, "AES-GCM")?;
    let length = dictionary_prop(vm, host, roots, target, "length")?;
    let length = unsigned(vm, host, roots, &length, u16::MAX.into())?;
    if !matches!(length, 128 | 192 | 256) {
        return Err(Failure::Dom(
            "OperationError",
            "AES keys must be 128, 192 or 256 bits",
        ));
    }
    require_extractable(&args[3])?;
    let usages = key_usages(vm, host, roots, &args[4], AES_USAGES)?;
    let password = key_bytes(
        vm,
        &args[1],
        CryptoKeyAlgorithm::Pbkdf2,
        CryptoKeyUsage::DeriveKey,
    )?;
    Ok(Work::Primitive(
        SubtleCryptoRequest::Pbkdf2Sha256 {
            password,
            salt,
            iterations,
            length,
        },
        Output::AesKey {
            usages,
            length: length as usize / 8,
        },
    ))
}

fn gcm(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    args: &[Value],
    encrypt: bool,
) -> Js<Work> {
    let data = bytes(vm, &args[2])?;
    let params = &args[0];
    require_algorithm(vm, host, roots, params, "AES-GCM")?;
    let iv = dictionary_prop(vm, host, roots, params, "iv")?;
    let iv = bytes(vm, &iv)?;
    let aad = prop(vm, host, roots, params, "additionalData")?;
    let additional_data = if matches!(aad.view(), ValueRef::Undefined) {
        Vec::new()
    } else {
        bytes(vm, &aad)?
    };
    let length = prop(vm, host, roots, params, "tagLength")?;
    let tag_length = if matches!(length.view(), ValueRef::Undefined) {
        128
    } else {
        unsigned(vm, host, roots, &length, u8::MAX.into())? as u16
    };
    if iv.is_empty() || !matches!(tag_length, 32 | 64 | 96 | 104 | 112 | 120 | 128) {
        return Err(Failure::Dom(
            "OperationError",
            "Invalid AES-GCM IV or tag length",
        ));
    }
    let usage = if encrypt {
        CryptoKeyUsage::Encrypt
    } else {
        CryptoKeyUsage::Decrypt
    };
    let key = key_bytes(vm, &args[1], CryptoKeyAlgorithm::AesGcm, usage)?;
    let tag_bytes = usize::from(tag_length / 8);
    let (request, output_length) = if encrypt {
        let length = data.len() + tag_bytes;
        if length > MAX_CRYPTO_KEY_BYTES {
            return Err(Failure::Dom(
                "OperationError",
                "AES-GCM ciphertext exceeds the SubtleCrypto byte limit",
            ));
        }
        (
            SubtleCryptoRequest::AesGcmEncrypt {
                key,
                iv,
                additional_data,
                tag_length,
                data,
            },
            length,
        )
    } else {
        let length = data.len().checked_sub(tag_bytes).ok_or(Failure::Dom(
            "OperationError",
            "AES-GCM ciphertext is shorter than its tag",
        ))?;
        (
            SubtleCryptoRequest::AesGcmDecrypt {
                key,
                iv,
                additional_data,
                tag_length,
                data,
            },
            length,
        )
    };
    Ok(Work::Primitive(request, Output::Bytes(output_length)))
}

fn hmac(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    args: &[Value],
    verify: bool,
) -> Js<Work> {
    let (data, output) = if verify {
        let signature = bytes(vm, &args[2])?;
        (bytes(vm, &args[3])?, Output::Verify(signature))
    } else {
        (bytes(vm, &args[2])?, Output::Bytes(32))
    };
    require_algorithm(vm, host, roots, &args[0], "HMAC")?;
    let usage = if verify {
        CryptoKeyUsage::Verify
    } else {
        CryptoKeyUsage::Sign
    };
    let key = key_bytes(vm, &args[1], CryptoKeyAlgorithm::HmacSha256, usage)?;
    Ok(Work::Primitive(
        SubtleCryptoRequest::HmacSha256 { key, data },
        output,
    ))
}

fn dictionary_prop(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    params: &Value,
    name: &str,
) -> Js<Value> {
    if !matches!(params.view(), ValueRef::Object(_)) {
        return Err(Failure::Type("Expected an algorithm dictionary"));
    }
    prop(vm, host, roots, params, name)
}

fn require_hash(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    params: &Value,
) -> Js<()> {
    let hash = dictionary_prop(vm, host, roots, params, "hash")?;
    require_algorithm(vm, host, roots, &hash, "SHA-256")
}

fn unsigned(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    input: &Value,
    max: u32,
) -> Js<u32> {
    let value = vm
        .coerce_number(host, roots, input)?
        .map_err(Failure::Thrown)?
        .trunc();
    if !(0.0..=f64::from(max)).contains(&value) {
        return Err(Failure::Type("Algorithm number is out of range"));
    }
    Ok(value as u32)
}

fn prop(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    object: &Value,
    name: &str,
) -> Js<Value> {
    match vm.read_property(host, roots, object, PropertyKeyRef::String(name))? {
        Invocation::Value(value) => Ok(value),
        Invocation::Thrown(value) => Err(Failure::Thrown(value)),
    }
}

fn algorithm_name(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    algorithm: &Value,
) -> Js<Value> {
    match algorithm.view() {
        ValueRef::String(_) => Ok(algorithm.clone()),
        ValueRef::Object(_) => {
            let name = prop(vm, host, roots, algorithm, "name")?;
            if !matches!(name.view(), ValueRef::String(_)) {
                return Err(Failure::Type("Algorithm name must be a string"));
            }
            Ok(name)
        }
        _ => Err(Failure::Type("Expected an algorithm string or dictionary")),
    }
}

fn named(name: &Value, expected: &str) -> bool {
    matches!(name.view(), ValueRef::String(name) if name.units().len() == expected.len()
        && name.units().zip(expected.bytes()).all(|(a, b)| u8::try_from(a).is_ok_and(|a| a.eq_ignore_ascii_case(&b))))
}

fn require_algorithm(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    algorithm: &Value,
    expected: &str,
) -> Js<()> {
    if named(&algorithm_name(vm, host, roots, algorithm)?, expected) {
        Ok(())
    } else {
        Err(Failure::Dom("NotSupportedError", "Unsupported algorithm"))
    }
}

fn bytes(vm: &Vm, value: &Value) -> Js<Vec<u8>> {
    let ValueRef::Object(id) = value.view() else {
        return Err(Failure::Type("Expected BufferSource"));
    };
    let invalid = Failure::Type("BufferSource is detached or out of bounds");
    let source = if let Some(buffer) = vm.heap.buffer(id)? {
        buffer.bytes.as_deref().ok_or(invalid)?
    } else {
        let (buffer, range) = if let Some(view) = vm.heap.typed_array(id)? {
            (view.buffer, vm.heap.typed_range(view)?)
        } else if let Some(view) = vm.heap.data_view(id)? {
            (view.buffer, vm.heap.data_view_range(view)?)
        } else {
            return Err(Failure::Type("Expected BufferSource"));
        };
        let ViewRange::Bytes { start, len } = range else {
            return Err(invalid);
        };
        &vm.heap.view_bytes(buffer)?.ok_or(invalid)?[start..start + len]
    };
    if source.len() > MAX_CRYPTO_KEY_BYTES {
        return Err(Failure::Dom(
            "OperationError",
            "BufferSource exceeds the SubtleCrypto byte limit",
        ));
    }
    Ok(source.to_vec())
}
