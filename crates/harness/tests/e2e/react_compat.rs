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
// The preload supplies environment globals; URI functions are VM builtins.

// esm.sh inlines unenv's node:events polyfill, which reads Node's module-private
// `kEvents` symbol without declaring it. Supplying it as an environment global
// keeps the vendor bundle unchanged. `setTimeout` / `clearTimeout` are host
// functions of the `test` module; re-exporting them makes the reconciler's
// unenv fallback resolve without inventing a second timer implementation.
const PRELOAD: &str = include_str!("../../benches/baseline/react-preload.mjs");

fn preload_source(environment_globals: bool) -> String {
    let mut source = String::from(PRELOAD);
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
