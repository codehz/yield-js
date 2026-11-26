//! BigInt constructor, static helpers, and prototype methods.

use num_bigint::BigInt;
use num_traits::{Signed, Zero};

use crate::builtin::globals::Owner;
use crate::error::{ErrorKind, VmError};
use crate::gc::ActiveRoots;
use crate::heap::{BoxedPrimitive, Object};
use crate::host::{Host, PlatformServices};
use crate::intrinsic::{IntrinsicKey, Slot};
use crate::runtime::Invocation;
use crate::value::{JsBigInt, Value, ValueRef};
use crate::vm::VmCore;

const CONSTRUCTOR: IntrinsicKey = IntrinsicKey::new("BigInt", Slot::Global, "");
const PROTOTYPE: &str = "bigint.prototype";

#[yield_binding_macros::builtin_global(name = "BigInt", root = "bigint", kind = "constructor")]
pub(crate) mod members {
    pub(crate) struct BuiltinTable<P>(std::marker::PhantomData<P>);

    use super::{CONSTRUCTOR, PROTOTYPE};
    use crate::builtin::globals::Owner;
    use crate::builtin::roots::{RootKind, RootSpec};
    use crate::builtin::{FUNCTION_PARENT, OBJECT_PARENT, link_constructor};
    use crate::intrinsic::IntrinsicDef;
    use crate::value::{PropertyKey, Value};
    use crate::vm::VmCore;

    impl<P: crate::host::PlatformServices> BuiltinTable<P> {
        pub(crate) const OWNER: Owner<P> = Owner::new(
            ROOT,
            &[
                RootSpec::new(ROOT, RootKind::Native(CONSTRUCTOR), FUNCTION_PARENT)
                    .fixed(PROTOTYPE),
                RootSpec::new(PROTOTYPE, RootKind::Plain, OBJECT_PARENT),
            ],
            &[Self::BIGINT_INTRINSICS, Globals::<P>::INTRINSICS],
            Some(install),
        );
    }

    impl<P: crate::host::PlatformServices> BuiltinTable<P> {
        pub(crate) const BIGINT_INTRINSICS: &[IntrinsicDef<P>] = &[IntrinsicDef::call(
            CONSTRUCTOR,
            1,
            |vm, host, roots, args, _| vm.call_bigint(host, roots, &args),
        )];
    }

    fn install(vm: &mut VmCore, _: &Value) {
        let constructor = vm.builtins.get(ROOT);
        let prototype = vm.builtins.get(PROTOTYPE);
        link_constructor(vm, constructor, prototype);
        install_members(vm, constructor, prototype);
        vm.heap
            .define_data(
                prototype,
                PropertyKey::Symbol(crate::heap::TO_STRING_TAG_SYMBOL),
                Value::string("BigInt".into()),
                crate::heap::Attributes::FUNCTION_METADATA,
            )
            .expect("BigInt.prototype[Symbol.toStringTag]");
    }

    static_calls! {
        asIntN: 2 => |vm, host, roots, args, _| vm.bigint_as_n(host, roots, &args, true);
        asUintN: 2 => |vm, host, roots, args, _| vm.bigint_as_n(host, roots, &args, false);
    }

    prototype_calls! {
        toString: 1 => |vm, host, roots, args, receiver| vm.bigint_to_string(host, roots, receiver, &args);
        toLocaleString: 0 => |vm, _, roots, _, receiver| vm.bigint_to_locale_string(roots, receiver);
        valueOf: 0 => |vm, _, roots, _, receiver| vm.bigint_value_of(roots, receiver);
    }
}

impl<P: crate::host::PlatformServices> BuiltinTable<P> {
    pub(crate) const OWNER: Owner<P> = members::BuiltinTable::<P>::OWNER;
}

impl VmCore {
    fn call_bigint<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let Some(value) = args.first() else {
            return Ok(Invocation::Value(Value::bigint(JsBigInt::from_i64(0))));
        };
        match self.coerce_bigint(host, roots, value)? {
            Ok(value) => Ok(Invocation::Value(Value::bigint(value))),
            Err(error) => Ok(Invocation::Thrown(error)),
        }
    }

    fn coerce_bigint<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        value: &Value,
    ) -> Result<Result<JsBigInt, Value>, VmError> {
        let primitive =
            match self.coerce_primitive(host, roots, value, crate::exec::Hint::Number)? {
                Ok(value) => value,
                Err(error) => return Ok(Err(error)),
            };
        let result = match primitive.as_ref().view() {
            ValueRef::Undefined => {
                Err(self.type_error(roots, "Cannot convert undefined to a BigInt")?)
            }
            ValueRef::Null => Err(self.type_error(roots, "Cannot convert null to a BigInt")?),
            ValueRef::Bool(value) => Ok(JsBigInt::from_i64(i64::from(value))),
            ValueRef::Number(value) => match JsBigInt::from_f64(value) {
                Some(value) => Ok(value),
                None => Err(self.make_range_error(
                    roots,
                    "The number cannot be converted to a BigInt because it is not an integer",
                )?),
            },
            ValueRef::BigInt(value) => Ok(value.clone()),
            ValueRef::String(value) => {
                let parsed = value
                    .to_utf8()
                    .ok()
                    .and_then(|text| JsBigInt::from_js_string(&text));
                match parsed {
                    Some(value) => Ok(value),
                    None => Err(self.alloc_class_error(
                        roots,
                        ErrorKind::SyntaxError,
                        Some("Cannot convert string to a BigInt".into()),
                    )?),
                }
            }
            ValueRef::Symbol(_) => {
                Err(self.type_error(roots, "Cannot convert a Symbol value to a BigInt")?)
            }
            ValueRef::Object(_) => unreachable!("ToPrimitive returned an object"),
        };
        Ok(result)
    }

    fn bigint_as_n<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        args: &[Value],
        signed: bool,
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let undefined = Value::UNDEFINED;
        let bits = args.first().unwrap_or(&undefined);
        let bits = match self.coerce_number(host, roots, bits)? {
            Ok(bits) => bits,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let bits = match self.bigint_index(roots, bits)? {
            Ok(bits) => bits,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let value = args.get(1).unwrap_or(&undefined);
        let value = match self.coerce_bigint(host, roots, value)? {
            Ok(value) => value,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let result = as_int_n(value.as_bigint(), bits, signed);
        Ok(Invocation::Value(Value::bigint(JsBigInt::from_bigint(
            result,
        ))))
    }

    fn bigint_index(
        &mut self,
        roots: &ActiveRoots<'_>,
        number: f64,
    ) -> Result<Result<u64, Value>, VmError> {
        if number.is_nan() || number == 0.0 {
            return Ok(Ok(0));
        }
        let integer = number.trunc();
        if integer < 0.0 {
            return Ok(Err(self.make_range_error(roots, "Invalid BigInt length")?));
        }
        let clamped = integer.min(9_007_199_254_740_991.0);
        Ok(Ok(clamped as u64))
    }

    fn bigint_receiver(
        &mut self,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
    ) -> Result<Result<JsBigInt, Value>, VmError> {
        Ok(match receiver.view() {
            ValueRef::BigInt(value) => Ok(value.clone()),
            ValueRef::Object(id) => match self.heap.get(id)? {
                Object::Boxed {
                    primitive: BoxedPrimitive::BigInt(value),
                    ..
                } => Ok(value.clone()),
                _ => Err(self.type_error(roots, "BigInt method called on incompatible receiver")?),
            },
            _ => Err(self.type_error(roots, "BigInt method called on incompatible receiver")?),
        })
    }

    fn bigint_to_string<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let value = match self.bigint_receiver(roots, receiver)? {
            Ok(value) => value,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let roots = &roots.with(&args);
        let radix = match args.first() {
            None => 10.0,
            Some(arg) => match self.coerce_number(host, roots, arg)? {
                Ok(number) if number.is_nan() => 0.0,
                Ok(number) => number.trunc(),
                Err(error) => return Ok(Invocation::Thrown(error)),
            },
        };
        if !(2.0..=36.0).contains(&radix) {
            return self.throw_range_error(roots, "BigInt radix must be between 2 and 36");
        }
        Ok(Invocation::Value(Value::string(
            value.to_radix(radix as u32).into(),
        )))
    }

    fn bigint_to_locale_string(
        &mut self,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        let value = match self.bigint_receiver(roots, receiver)? {
            Ok(value) => value,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        Ok(Invocation::Value(Value::string(value.to_decimal().into())))
    }

    fn bigint_value_of(
        &mut self,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        let value = match self.bigint_receiver(roots, receiver)? {
            Ok(value) => value,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        Ok(Invocation::Value(Value::bigint(value)))
    }
}

fn as_int_n(value: &BigInt, bits: u64, signed: bool) -> BigInt {
    if bits == 0 {
        return BigInt::zero();
    }
    let magnitude_bits = value.bits();
    if !value.is_negative() {
        if if signed {
            magnitude_bits < bits
        } else {
            magnitude_bits <= bits
        } {
            return value.clone();
        }
    } else if signed && magnitude_bits < bits {
        return value.clone();
    }
    let bits = match usize::try_from(bits) {
        Ok(bits) => bits,
        Err(_) => return BigInt::zero(),
    };
    let modulus = BigInt::from(1u8) << bits;
    let mut remainder = value % &modulus;
    if remainder.is_negative() {
        remainder += &modulus;
    }
    if signed {
        let half = BigInt::from(1u8) << (bits - 1);
        if remainder >= half {
            remainder -= modulus;
        }
    }
    remainder
}

pub(crate) struct BuiltinTable<P>(std::marker::PhantomData<P>);
