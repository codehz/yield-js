//! Compatibility probe: real React 19 source compiled and run by the strict
//! ESM subset.
//!
//! The probe supplies the environment globals React expects through the
//! preload module, then renders one element with the vendored custom renderer
//! and asserts the commit reached the host container.
//!
//! Host-provided environment globals are supplied by the preload module; the
//! remaining framework-owned gaps are recorded in `docs/gaps.md` §8.
use super::support::{TEST_DECLARATIONS, compile_preloaded, declared};
use yield_compiler::{MemoryResolver, compile};
use yield_harness::TestHost;
use yield_vm::{Value, Vm};

const REACT_19_BUNDLE: &str = include_str!("vendor/react-19.2.0-esm-sh.mjs");
const REACT_RECONCILER: &str = include_str!("vendor/react-reconciler-0.31.0.mjs");
const CUSTOM_RENDERER: &str = include_str!("vendor/react-renderer.mjs");
// Host-provided environment globals and the small standards polyfills the
// vendor sources call are supplied by the preload module. Framework-owned
// gaps stay recorded in `docs/gaps.md` §8.
//
// esm.sh inlines unenv's node:events polyfill, which reads Node's module-private
// `kEvents` symbol without declaring it. Supplying it as an environment global
// keeps the vendor bundle unchanged. `setTimeout` / `clearTimeout` are host
// functions of the `test` module; re-exporting them makes the reconciler's
// unenv fallback resolve without inventing a second timer implementation.
// The URI family lives here because the VM has no standard URI globals yet.
const PRELOAD_POLYFILLS: &str = r#"
import { clearTimeout, setTimeout } from "test";
import * as console from "console";
export { clearTimeout, setTimeout, console };
export function reportError(error) {}
export const kEvents = Symbol("kEvents");

const uriHexDigits = "0123456789ABCDEF";
const uriReserved = ";/?:@&=+$,#";
const uriUnescaped = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_.!~*'()";
const uriDecoder = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true });

function uriByte(byte) {
    return "%" + uriHexDigits[byte >> 4] + uriHexDigits[byte & 15];
}

function uriHexValue(code) {
    if (code >= 0x30 && code <= 0x39) return code - 0x30;
    if (code >= 0x41 && code <= 0x46) return code - 0x41 + 10;
    if (code >= 0x61 && code <= 0x66) return code - 0x61 + 10;
    return -1;
}

// One percent triplet starting at `at`, or -1 when the triplet is malformed.
function uriHex(text, at) {
    if (at + 2 >= text.length || text[at] !== "%") return -1;
    const hi = uriHexValue(text.charCodeAt(at + 1));
    const lo = uriHexValue(text.charCodeAt(at + 2));
    return hi < 0 || lo < 0 ? -1 : hi * 16 + lo;
}

// ToString plus UTF-8 percent encoding; `extra` is the code-unit set the
// caller leaves unescaped. Lone surrogates are URI errors, not U+FFFD.
function uriEncode(text, extra) {
    let out = "";
    for (let index = 0; index < text.length; index++) {
        const unit = text.charCodeAt(index);
        if (unit < 0x80 && extra.indexOf(text[index]) >= 0) {
            out += text[index];
            continue;
        }
        let point = unit;
        if (unit >= 0xd800 && unit <= 0xdbff) {
            const next = index + 1 < text.length ? text.charCodeAt(index + 1) : 0;
            if (next < 0xdc00 || next > 0xdfff) throw new URIError("URI malformed");
            point = (unit - 0xd800) * 0x400 + (next - 0xdc00) + 0x10000;
            index += 1;
        } else if (unit >= 0xdc00 && unit <= 0xdfff) {
            throw new URIError("URI malformed");
        }
        if (point < 0x80) {
            out += uriByte(point);
        } else if (point < 0x800) {
            out += uriByte(0xc0 | (point >> 6));
            out += uriByte(0x80 | (point & 0x3f));
        } else if (point < 0x10000) {
            out += uriByte(0xe0 | (point >> 12));
            out += uriByte(0x80 | ((point >> 6) & 0x3f));
            out += uriByte(0x80 | (point & 0x3f));
        } else {
            out += uriByte(0xf0 | (point >> 18));
            out += uriByte(0x80 | ((point >> 12) & 0x3f));
            out += uriByte(0x80 | ((point >> 6) & 0x3f));
            out += uriByte(0x80 | (point & 0x3f));
        }
    }
    return out;
}

// Percent decoding of one UTF-8 scalar at a time. A fatal decoder rejects
// overlong, surrogate and out-of-range sequences as URI errors. When
// `reserved` is set (decodeURI), reserved characters keep their original
// percent spelling.
function uriDecode(text, reserved) {
    let out = "";
    for (let index = 0; index < text.length; index++) {
        if (text[index] !== "%") {
            out += text[index];
            continue;
        }
        const first = uriHex(text, index);
        if (first < 0) throw new URIError("URI malformed");
        let count;
        if (first < 0x80) count = 1;
        else if (first >= 0xc2 && first <= 0xdf) count = 2;
        else if (first >= 0xe0 && first <= 0xef) count = 3;
        else if (first >= 0xf0 && first <= 0xf4) count = 4;
        else throw new URIError("URI malformed");
        const bytes = [first];
        let cursor = index + 3;
        for (let offset = 1; offset < count; offset++) {
            const byte = uriHex(text, cursor);
            if (byte < 0) throw new URIError("URI malformed");
            bytes.push(byte);
            cursor += 3;
        }
        let decoded;
        try {
            decoded = uriDecoder.decode(new Uint8Array(bytes));
        } catch (error) {
            throw new URIError("URI malformed");
        }
        if (reserved && uriReserved.indexOf(decoded) >= 0) {
            out += text.slice(index, cursor);
        } else {
            out += decoded;
        }
        index = cursor - 1;
    }
    return out;
}

export function encodeURIComponent(value) {
    return uriEncode("" + value, uriUnescaped);
}

export function encodeURI(value) {
    return uriEncode("" + value, uriUnescaped + uriReserved);
}

export function decodeURIComponent(value) {
    return uriDecode("" + value, false);
}

export function decodeURI(value) {
    return uriDecode("" + value, true);
}
"#;

fn preload_source(environment_globals: bool) -> String {
    let mut source = String::from(PRELOAD_POLYFILLS);
    if environment_globals {
        source.push_str("export const window = undefined; export const process = undefined;");
    }
    source
}

#[test]
fn real_react_19_bundle_compiles_with_environment_polyfills() {
    let preload = preload_source(true);
    compile_preloaded(REACT_19_BUNDLE, &preload, &declared(TEST_DECLARATIONS))
        .unwrap_or_else(|error| panic!("React 19 compile diagnostic changed: {error}"));
}

#[test]
fn react_bundle_compiles_without_window_or_process_preload_exports() {
    let preload = preload_source(false);
    compile_preloaded(REACT_19_BUNDLE, &preload, &declared(TEST_DECLARATIONS)).unwrap_or_else(
        |error| panic!("React bundle depended on a removed environment export: {error}"),
    );
}

#[test]
fn preload_uri_polyfills_match_standard_encoding() {
    let main = r#"
        function throws(fn) {
            try { fn(); return false; } catch (error) { return error instanceof URIError; }
        }
        export function run() {
            return [
                encodeURIComponent("a b&c/中文"),
                encodeURIComponent("!'()*-._~"),
                encodeURI("http://x/y?z=1#f"),
                decodeURIComponent("%E4%B8%AD%20a"),
                decodeURI("%3F%23%41"),
                throws(() => encodeURIComponent("\uD800")),
                throws(() => decodeURIComponent("%E4%B8")),
                throws(() => decodeURIComponent("%ZZ")),
            ].join("|");
        }
    "#;
    let preload = preload_source(false);
    let resolver = MemoryResolver::new([("main.js", main), ("preload.js", &preload)])
        .expect("valid URI probe module paths");
    let build = compile(
        "main.js",
        Some("preload.js"),
        &resolver,
        &declared(TEST_DECLARATIONS),
    )
    .expect("URI polyfill probe links");
    let mut host = TestHost::default();
    let mut vm = Vm::new(build, &mut host).expect("URI polyfill VM initialization");
    let result = vm
        .start(&mut host, "run", Vec::new())
        .expect("URI polyfill probe run");
    assert_eq!(
        result,
        Value::string(
            "a%20b%26c%2F%E4%B8%AD%E6%96%87|!'()*-._~|http://x/y?z=1#f|中 a|%3F%23A|true|true|true"
                .into()
        )
    );
}

#[test]
fn react_reconciler_compiles_without_window_or_process_preload_exports() {
    let main = r#"
        import React from "react.mjs";
        import { render } from "renderer.mjs";
        export function run() {
            const container = { children: [] };
            render(React.createElement("scene", { id: "root" }, "hello"), container);
            return container.children.length;
        }
    "#;
    let preload = preload_source(false);
    let resolver = MemoryResolver::new([
        ("main.js", main),
        ("preload.js", &preload),
        ("react.mjs", REACT_19_BUNDLE),
        ("react-reconciler-0.31.0.mjs", REACT_RECONCILER),
        ("renderer.mjs", CUSTOM_RENDERER),
    ])
    .expect("valid React probe module paths");
    compile(
        "main.js",
        Some("preload.js"),
        &resolver,
        &declared(TEST_DECLARATIONS),
    )
    .unwrap_or_else(|error| {
        panic!("React reconciler depended on a removed environment export: {error}")
    });
}

#[test]
fn react_reconciler_custom_renderer_headless_smoke() {
    let main = r#"
        import React from "react.mjs";
        import { render } from "renderer.mjs";
        const container = { children: [] };
        export function run() {
            render(React.createElement("scene", { id: "root" }, "hello"), container);
            return container.children.length;
        }
        export function committed() {
            return container.children.length;
        }
    "#;
    let preload = preload_source(true);
    let resolver = MemoryResolver::new([
        ("main.js", main),
        ("preload.js", &preload),
        ("react.mjs", REACT_19_BUNDLE),
        ("react-reconciler-0.31.0.mjs", REACT_RECONCILER),
        ("renderer.mjs", CUSTOM_RENDERER),
    ])
    .expect("valid React probe module paths");
    let build = compile(
        "main.js",
        Some("preload.js"),
        &resolver,
        &declared(TEST_DECLARATIONS),
    )
    .unwrap_or_else(|error| panic!("React probe stopped compiling: {error}"));

    let mut host = TestHost::default();
    let mut vm = Vm::new(build, &mut host).expect("React headless VM initialization");
    vm.start(&mut host, "run", Vec::new())
        .expect("React headless smoke run");
    host.advance(&mut vm, 10)
        .expect("deliver React scheduler work");
    assert!(
        host.errors().is_empty(),
        "scheduler reported an error: {:?}",
        host.errors()
    );
    let children = vm
        .start(&mut host, "committed", Vec::new())
        .expect("read committed children");
    assert_eq!(
        children,
        Value::number(1.0),
        "custom renderer committed one child"
    );
}
