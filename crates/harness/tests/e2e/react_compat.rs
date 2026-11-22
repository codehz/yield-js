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
const PRELOAD_POLYFILLS: &str = include_str!("../../benches/baseline/react-preload.mjs");

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
