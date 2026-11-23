//! Global functions with stable native identities shared by bare names and globalThis.

use super::FUNCTION_PARENT;
use super::binary::codec::{decode_html_base64, encode_html_base64};
use super::globals::Owner;
use super::roots::{RootKind, RootSpec};
use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::host::Host;
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::runtime::Invocation;
use crate::value::{JsString, Value};
use crate::vm::Vm;

macro_rules! function {
    ($owner:ident, $name:literal, $length:literal, $call:expr) => {
        pub(super) const $owner: Owner = {
            const KEY: IntrinsicKey = IntrinsicKey::new($name, Slot::Global, "");
            Owner::new(
                $name,
                &[RootSpec::new($name, RootKind::Native(KEY), FUNCTION_PARENT)],
                &[&[IntrinsicDef::call(KEY, $length, $call)]],
                None,
            )
        };
    };
}

function!(PARSE_INT, "parseInt", 2, |vm, host, roots, args, _| {
    vm.call_number_static(host, roots, "parseInt", &args)
});
function!(PARSE_FLOAT, "parseFloat", 1, |vm, host, roots, args, _| {
    vm.call_number_static(host, roots, "parseFloat", &args)
});
function!(IS_NAN, "isNaN", 1, |vm, host, roots, args, _| {
    number_predicate(vm, host, roots, &args, f64::is_nan)
});
function!(IS_FINITE, "isFinite", 1, |vm, host, roots, args, _| {
    number_predicate(vm, host, roots, &args, f64::is_finite)
});
function!(EVAL, "eval", 1, |vm, _, roots, _, _| {
    vm.alloc_error(
        roots,
        vm.builtins.get("evalerror.prototype"),
        "EvalError",
        Some("Dynamic code evaluation is disabled".into()),
    )
    .map(Invocation::Thrown)
});
function!(
    REPORT_ERROR,
    "reportError",
    1,
    |vm, host, roots, args, _| {
        let Some(error) = args.into_iter().next() else {
            return vm.throw_type_error(roots, "reportError requires one argument");
        };
        vm.report_exception(host, roots, error)?;
        Ok(Invocation::Value(Value::UNDEFINED))
    }
);

function!(ATOB, "atob", 1, |vm, host, roots, args, _| {
    call_base64(vm, host, roots, &args, decode_html_base64)
});
function!(BTOA, "btoa", 1, |vm, host, roots, args, _| {
    call_base64(vm, host, roots, &args, encode_html_base64)
});

fn call_base64(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    args: &[Value],
    transform: fn(&JsString) -> Option<JsString>,
) -> Result<Invocation, VmError> {
    let Some(input) = args.first() else {
        return vm.throw_type_error(roots, "Base64 function requires one argument");
    };
    let roots = &roots.with(&args);
    let text = match vm.coerce_string(host, roots, input)? {
        Ok(text) => text,
        Err(error) => return Ok(Invocation::Thrown(error)),
    };
    Ok(match transform(&text) {
        Some(text) => Invocation::Value(Value::string(text)),
        None => Invocation::Thrown(vm.alloc_dom_exception(
            "InvalidCharacterError".into(),
            "Invalid Base64 input".into(),
        )),
    })
}

fn number_predicate(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    args: &[Value],
    predicate: fn(f64) -> bool,
) -> Result<Invocation, VmError> {
    let roots = &roots.with(&args);
    let undefined = Value::UNDEFINED;
    let value = args.first().unwrap_or(&undefined);
    Ok(match vm.coerce_number(host, roots, value)? {
        Ok(number) => Invocation::Value(Value::boolean(predicate(number))),
        Err(error) => Invocation::Thrown(error),
    })
}

const RESERVED: &[u8] = b";/?:@&=+$,#";
const HEX: &[u8; 16] = b"0123456789ABCDEF";

function!(ENCODE_URI, "encodeURI", 1, |vm, host, roots, args, _| {
    call_uri(vm, host, roots, &args, |text| encode(text, true))
});
function!(
    ENCODE_URI_COMPONENT,
    "encodeURIComponent",
    1,
    |vm, host, roots, args, _| { call_uri(vm, host, roots, &args, |text| encode(text, false)) }
);
function!(DECODE_URI, "decodeURI", 1, |vm, host, roots, args, _| {
    call_uri(vm, host, roots, &args, |text| decode(text, true))
});
function!(
    DECODE_URI_COMPONENT,
    "decodeURIComponent",
    1,
    |vm, host, roots, args, _| { call_uri(vm, host, roots, &args, |text| decode(text, false)) }
);

fn call_uri(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    args: &[Value],
    transform: impl FnOnce(JsString) -> Option<JsString>,
) -> Result<Invocation, VmError> {
    let roots = &roots.with(&args);
    let undefined = Value::UNDEFINED;
    let value = args.first().unwrap_or(&undefined);
    let text = match vm.coerce_string(host, roots, value)? {
        Ok(text) => text,
        Err(error) => return Ok(Invocation::Thrown(error)),
    };
    match transform(text) {
        Some(text) => Ok(Invocation::Value(Value::string(text))),
        None => vm
            .alloc_error(
                roots,
                vm.builtins.get("urierror.prototype"),
                "URIError",
                Some("URI malformed".into()),
            )
            .map(Invocation::Thrown),
    }
}

fn unescaped(byte: u8, preserve_reserved: bool) -> bool {
    byte.is_ascii_alphanumeric()
        || b"-_.!~*'()".contains(&byte)
        || (preserve_reserved && RESERVED.contains(&byte))
}

fn encode(text: JsString, preserve_reserved: bool) -> Option<JsString> {
    if text
        .latin1_bytes()
        .is_some_and(|bytes| bytes.iter().all(|&byte| unescaped(byte, preserve_reserved)))
    {
        return Some(text);
    }
    let mut output = Vec::with_capacity(text.unit_len());
    for scalar in char::decode_utf16(text.units()) {
        let scalar = scalar.ok()?;
        if scalar.is_ascii() && unescaped(scalar as u8, preserve_reserved) {
            output.push(scalar as u8);
        } else {
            for &byte in scalar.encode_utf8(&mut [0; 4]).as_bytes() {
                output.extend([b'%', HEX[(byte >> 4) as usize], HEX[(byte & 15) as usize]]);
            }
        }
    }
    Some(JsString::from_latin1(output))
}

fn hex(unit: u16) -> Option<u8> {
    (u8::try_from(unit).ok()? as char)
        .to_digit(16)
        .map(|digit| digit as u8)
}

fn decode(text: JsString, preserve_reserved: bool) -> Option<JsString> {
    let mut input = text.units();
    let Some(prefix_len) = input.position(|unit| unit == u16::from(b'%')) else {
        return Some(text);
    };
    let mut output = Vec::with_capacity(text.unit_len());
    output.extend(text.units().take(prefix_len));
    'escaped: loop {
        let hi = input.next()?;
        let lo = input.next()?;
        let first = hex(hi)? << 4 | hex(lo)?;
        if first.is_ascii() {
            if preserve_reserved && RESERVED.contains(&first) {
                // Keep the original spelling, including hex digit case.
                output.extend([u16::from(b'%'), hi, lo]);
            } else {
                output.push(u16::from(first));
            }
        } else {
            let width = match first {
                0xc2..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf4 => 4,
                _ => return None,
            };
            let mut bytes = [first, 0, 0, 0];
            for byte in &mut bytes[1..width] {
                if input.next()? != u16::from(b'%') {
                    return None;
                }
                *byte = hex(input.next()?)? << 4 | hex(input.next()?)?;
            }
            // Reject invalid UTF-8 (including overlong and surrogate encodings).
            output.extend(std::str::from_utf8(&bytes[..width]).ok()?.encode_utf16());
        }
        // Non-escaped code units pass through unchanged, even lone surrogates.
        for unit in input.by_ref() {
            if unit == u16::from(b'%') {
                continue 'escaped;
            }
            output.push(unit);
        }
        return Some(JsString::from_units(output));
    }
}
