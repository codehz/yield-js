//! The always-present Web Crypto singleton; random APIs require host entropy.

use super::globals::Owner;
use super::roots::{RootKind, RootSpec};
use super::{define_builtin_data, install_getter};
use crate::VmError;
use crate::gc::ActiveRoots;
use crate::heap::{Attributes, ElementKind, TO_STRING_TAG_SYMBOL, ViewRange};
use crate::host::{CryptoService, Host, PlatformServices};
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::runtime::Invocation;
use crate::value::{PropertyKey, Value, ValueRef};
use crate::vm::VmCore;

pub(super) mod subtle;
pub(crate) mod worker;

const ROOT: &str = "crypto";
const PROTOTYPE: &str = "crypto.prototype";
const GET_RANDOM_VALUES: IntrinsicKey =
    IntrinsicKey::new("Crypto", Slot::Prototype, "getRandomValues");
const RANDOM_UUID: IntrinsicKey = IntrinsicKey::new("Crypto", Slot::Prototype, "randomUUID");
const SUBTLE: IntrinsicKey = IntrinsicKey::new("Crypto", Slot::PrototypeGetter, "subtle");
const MAX_RANDOM_BYTES: usize = 65_536;

const ROOTS: &[RootSpec] = &[
    RootSpec::new(ROOT, RootKind::Plain, Some(PROTOTYPE)),
    RootSpec::new(PROTOTYPE, RootKind::Plain, Some("object.prototype")),
    RootSpec::new(subtle::INSTANCE, RootKind::Plain, Some(subtle::PROTOTYPE)),
    RootSpec::new(subtle::PROTOTYPE, RootKind::Plain, Some("object.prototype")),
];

impl<P: crate::host::PlatformServices> BuiltinTable<P> {
    const MEMBERS: &[IntrinsicDef<P>] = &[
        IntrinsicDef::call(SUBTLE, 0, |vm, _, roots, _, receiver| {
            if !is_crypto(vm, receiver) {
                return vm.throw_type_error(roots, "Incompatible Crypto receiver");
            }
            Ok(Invocation::Value(Value::object(
                vm.builtins.get(subtle::INSTANCE),
            )))
        }),
        IntrinsicDef::call(GET_RANDOM_VALUES, 1, |vm, host, roots, args, receiver| {
            get_random_values(vm, host, roots, args.into_iter().next(), receiver)
        }),
        IntrinsicDef::call(RANDOM_UUID, 0, |vm, host, roots, _, receiver| {
            random_uuid(vm, host, roots, receiver)
        }),
    ];
}

impl<P: crate::host::PlatformServices> BuiltinTable<P> {
    pub(super) const OWNER: Owner<P> = Owner::new(
        ROOT,
        ROOTS,
        &[Self::MEMBERS, subtle::BuiltinTable::<P>::MEMBERS],
        Some(install),
    );
}

fn install(vm: &mut VmCore, _: &Value) {
    let prototype = vm.builtins.get(PROTOTYPE);
    for key in [GET_RANDOM_VALUES, RANDOM_UUID] {
        let method = vm.alloc_intrinsic(key);
        define_builtin_data(
            vm,
            prototype,
            key.member,
            Value::object(method),
            Attributes::ORDINARY,
        );
    }
    define_builtin_data(
        vm,
        prototype,
        PropertyKey::Symbol(TO_STRING_TAG_SYMBOL),
        Value::string("Crypto".into()),
        Attributes::FUNCTION_METADATA,
    );
    install_getter(vm, prototype, "subtle".into(), SUBTLE);
    subtle::install(vm);
}

fn is_crypto(vm: &VmCore, receiver: &Value) -> bool {
    matches!(receiver.view(), ValueRef::Object(id) if id == vm.builtins.get(ROOT))
}

fn get_random_values<P: PlatformServices>(
    vm: &mut VmCore,
    host: &Host<P>,
    roots: &ActiveRoots<'_>,
    arg: Option<Value>,
    receiver: &Value,
) -> Result<Invocation, VmError> {
    if !is_crypto(vm, receiver) {
        return vm.throw_type_error(roots, "Incompatible Crypto receiver");
    }
    let Some(ValueRef::Object(id)) = arg.as_ref().map(Value::view) else {
        return Ok(type_mismatch(vm));
    };
    let Some(view) = vm.heap.typed_array(id)? else {
        return Ok(type_mismatch(vm));
    };
    if matches!(
        view.kind,
        ElementKind::Float16 | ElementKind::Float32 | ElementKind::Float64
    ) {
        return Ok(type_mismatch(vm));
    }
    let buffer = view.buffer;
    let ViewRange::Bytes { start, len } = vm.heap.typed_range(view)? else {
        return vm.throw_type_error(roots, "TypedArray is detached or out of bounds");
    };
    if len > MAX_RANDOM_BYTES {
        return Ok(dom_error(
            vm,
            "QuotaExceededError",
            "Random byte length exceeds 65536",
        ));
    }
    // Stage only the visible bytes so a failing host cannot partially modify the view.
    let mut bytes = vec![0; len];
    if let Err(error) = fill_random(vm, host, &mut bytes) {
        return Ok(error);
    }
    vm.write_buffer(buffer, start, &bytes)?;
    Ok(Invocation::Value(Value::object(id)))
}

fn random_uuid<P: PlatformServices>(
    vm: &mut VmCore,
    host: &Host<P>,
    roots: &ActiveRoots<'_>,
    receiver: &Value,
) -> Result<Invocation, VmError> {
    if !is_crypto(vm, receiver) {
        return vm.throw_type_error(roots, "Incompatible Crypto receiver");
    }
    let mut bytes = [0; 16];
    if let Err(error) = fill_random(vm, host, &mut bytes) {
        return Ok(error);
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;

    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut uuid = String::with_capacity(36);
    for (i, byte) in bytes.into_iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            uuid.push('-');
        }
        uuid.push(HEX[(byte >> 4) as usize] as char);
        uuid.push(HEX[(byte & 0x0f) as usize] as char);
    }
    Ok(Invocation::Value(Value::string(uuid.into())))
}

fn fill_random<P: PlatformServices>(
    vm: &mut VmCore,
    host: &Host<P>,
    bytes: &mut [u8],
) -> Result<(), Invocation> {
    let Some(service) = host.platform().crypto() else {
        return Err(dom_error(
            vm,
            "NotSupportedError",
            "Host entropy service is not available",
        ));
    };
    service
        .fill_random(bytes)
        .map_err(|_| dom_error(vm, "OperationError", "Host entropy service failed"))
}

fn type_mismatch(vm: &mut VmCore) -> Invocation {
    dom_error(vm, "TypeMismatchError", "Expected an integer TypedArray")
}

fn dom_error(vm: &mut VmCore, name: &str, message: &str) -> Invocation {
    Invocation::Thrown(vm.alloc_dom_exception(name.into(), message.into()))
}

pub(crate) struct BuiltinTable<P>(std::marker::PhantomData<P>);
