//! Compatibility probe: real React 19 source compiled by the strict-ESM subset.
//!
//! The probe stages a headless custom-renderer smoke test: compile the
//! reconciler, then run one render once the framework gaps in `docs/gaps.md`
//! are closed. Until then it pins the first compile blocker, so a moved
//! frontier fails instead of passing silently.
//!
//! Host-provided environment globals are supplied by the preload module; the
//! framework-owned gaps are recorded in `docs/gaps.md` §8.
use super::support::{TEST_DECLARATIONS, compile_preloaded, declared};
use yield_compiler::{MemoryResolver, compile};
use yield_harness::TestHost;
use yield_vm::{Value, Vm};

const REACT_19_BUNDLE: &str = include_str!("vendor/react-19.2.0-esm-sh.mjs");
const REACT_RECONCILER: &str = include_str!("vendor/react-reconciler-0.31.0.mjs");
const CUSTOM_RENDERER: &str = include_str!("vendor/react-renderer.mjs");
// esm.sh inlines unenv's node:events polyfill, which reads Node's module-private
// `kEvents` symbol without declaring it. Supplying it as an environment global
// keeps the vendor bundle unchanged.
const PRELOAD: &str = "import * as console from 'console'; export { console }; export function reportError(error) {} export const window = undefined; export const process = undefined; export const kEvents = Symbol('kEvents');";
const PRELOAD_WITHOUT_ENV_GLOBALS: &str = "import * as console from 'console'; export { console }; export function reportError(error) {} export const kEvents = Symbol('kEvents');";

#[test]
fn real_react_19_bundle_compiles_with_environment_polyfills() {
    compile_preloaded(REACT_19_BUNDLE, PRELOAD, &declared(TEST_DECLARATIONS))
        .unwrap_or_else(|error| panic!("React 19 compile diagnostic changed: {error}"));
}

#[test]
fn react_bundle_compiles_without_window_or_process_preload_exports() {
    compile_preloaded(
        REACT_19_BUNDLE,
        PRELOAD_WITHOUT_ENV_GLOBALS,
        &declared(TEST_DECLARATIONS),
    )
    .unwrap_or_else(|error| {
        panic!("React bundle depended on a removed environment export: {error}")
    });
}

#[test]
fn react_reconciler_without_window_or_process_still_stops_at_encode_uri_component() {
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
        ("preload.js", PRELOAD_WITHOUT_ENV_GLOBALS),
        ("react.mjs", REACT_19_BUNDLE),
        ("react-reconciler-0.31.0.mjs", REACT_RECONCILER),
        ("renderer.mjs", CUSTOM_RENDERER),
    ])
    .expect("valid React probe module paths");
    let error = compile(
        "main.js",
        Some("preload.js"),
        &resolver,
        &declared(TEST_DECLARATIONS),
    )
    .expect_err("the reachable framework dependency remains a compile blocker");
    assert_eq!(error.module(), "react-reconciler-0.31.0.mjs", "{error}");
    assert_eq!(
        error.message(),
        "unknown callee \"encodeURIComponent\"",
        "{error}"
    );
    assert_eq!((error.line(), error.column()), (6, 13754), "{error}");
}

#[test]
fn react_reconciler_custom_renderer_headless_smoke() {
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
    let build = match compile(
        "main.js",
        Some("preload.js"),
        &resolver,
        &declared(TEST_DECLARATIONS),
    ) {
        Ok(build) => build,
        Err(error) => {
            // The current frontier and the complete blocker chain, including
            // framework/host ownership, are recorded in `docs/gaps.md` §8.
            // Move this assertion when the framework gap is fixed.
            assert_eq!(error.module(), "react-reconciler-0.31.0.mjs", "{error}");
            assert_eq!(
                error.message(),
                "unknown callee \"encodeURIComponent\"",
                "{error}"
            );
            assert_eq!((error.line(), error.column()), (6, 13754), "{error}");
            return;
        }
    };

    let mut host = TestHost::default();
    let mut vm = Vm::new(build, &mut host).expect("React headless VM initialization");
    let children = vm
        .start(&mut host, "run", Vec::new())
        .expect("React headless smoke run");
    assert_eq!(
        children,
        Value::number(1.0),
        "custom renderer committed one child"
    );
}
