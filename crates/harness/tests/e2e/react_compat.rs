//! Compatibility probe: real React 19 source compiled by the strict-ESM subset.
//!
//! Environment globals absent from the VM are supplied by the preload module.
//! This probes compilation only, not runtime behavior.
use super::support::{TEST_DECLARATIONS, compile_main, compile_preloaded, declared};
use yield_compiler::{CompileError, CompileOptions, MemoryResolver, compile};
use yield_harness::TestHost;
use yield_vm::Vm;

const REACT_19_BUNDLE: &str = include_str!("vendor/react-19.2.0-esm-sh.mjs");
const REACT_RECONCILER: &str = include_str!("vendor/react-reconciler-0.31.0.mjs");
const CUSTOM_RENDERER: &str = include_str!("vendor/react-renderer.mjs");
// esm.sh inlines unenv's node:events polyfill, which reads Node's module-private
// `kEvents` symbol without declaring it. Supplying it as an environment global
// keeps the vendor bundle unchanged.
const PRELOAD: &str = "import * as console from 'console'; export { console }; export function reportError(error) {} export const window = undefined; export const process = undefined; export const kEvents = Symbol('kEvents');";

#[test]
fn real_react_19_bundle_compiles_with_environment_polyfills() {
    compile_preloaded(REACT_19_BUNDLE, PRELOAD, &declared(TEST_DECLARATIONS))
        .unwrap_or_else(|error| panic!("React 19 compile diagnostic changed: {error}"));
}

#[test]
fn react_reconciler_custom_renderer_attempts_headless_host_execution() {
    let main = r#"
        import React from "react.mjs";
        import { render } from "renderer.mjs";
        export function run() {
            const container = { children: [] };
            render(React.createElement("scene", { id: "root" }, "hello"), container);
            return container.children.length;
        }
    "#;
    let resolver = MemoryResolver::new([
        ("main.js", main),
        ("preload.js", PRELOAD),
        ("react.mjs", REACT_19_BUNDLE),
        ("react-reconciler-0.31.0.mjs", REACT_RECONCILER),
        ("renderer.mjs", CUSTOM_RENDERER),
    ])
    .expect("valid React probe module paths");
    match compile(
        "main.js",
        Some("preload.js"),
        &resolver,
        &declared(TEST_DECLARATIONS),
    ) {
        Ok(build) => {
            let mut host = TestHost::default();
            let mut vm = Vm::new(build, &mut host).expect("headless host bindings");
            match vm.start(&mut host, "run", Vec::new()) {
                Ok(result) => panic!("unexpected React headless result: {result:?}"),
                Err(error) => eprintln!("React reconciler headless runtime failure: {error}"),
            }
        }
        Err(error) => eprintln!("React reconciler headless compile failure: {error}"),
    }
}

#[test]
fn minimal_input_confirms_the_unknown_global_cause() {
    let error: CompileError = compile_main(
        "function probe(){ reportError('x'); }",
        &CompileOptions {
            imports: Default::default(),
        },
    )
    .unwrap_err();
    assert_eq!(error.message(), "unknown callee \"reportError\"", "{error}");
    assert_eq!(error.module(), "main.js");
}
