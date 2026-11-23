//! Compatibility probe: real React 19 source compiled and run by the strict
//! ESM subset.
//!
//! The probe supplies the environment globals React expects through the
//! preload module, then renders one element with the vendored custom renderer
//! and asserts the commit reached the host container.
//!
//! Host-provided environment globals are supplied by the preload module; the
//! remaining framework-owned gaps are recorded in `docs/gaps.md` §8.
use super::support::{TEST_DECLARATIONS, compile_main, compile_preloaded, declared};
use yield_compiler::{MemoryResolver, compile};
use yield_harness::TestHost;
use yield_vm::{RestoreOptions, Value, Vm, VmOptions};

const REACT_19_BUNDLE: &str = include_str!("vendor/react-19.2.0-esm-sh.mjs");
// esm.sh's inlined node:events polyfill references a missing private symbol.
const REACT_RECONCILER: &str = concat!(
    "const kEvents = Symbol('kEvents');\n",
    include_str!("vendor/react-reconciler-0.31.0.mjs"),
);
const CUSTOM_RENDERER: &str = include_str!("vendor/react-renderer.mjs");
const PRELOAD: &str = include_str!("../../benches/baseline/react-preload.mjs");

#[test]
fn real_react_19_bundle_compiles_with_environment_polyfills() {
    compile_preloaded(REACT_19_BUNDLE, PRELOAD, &declared(TEST_DECLARATIONS))
        .unwrap_or_else(|error| panic!("React 19 compile diagnostic changed: {error}"));
}

#[test]
fn react_bundle_compiles_without_window_or_process_preload_exports() {
    compile_main(REACT_19_BUNDLE, &declared(TEST_DECLARATIONS)).unwrap_or_else(|error| {
        panic!("React bundle depended on a removed environment export: {error}")
    });
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
    let resolver = MemoryResolver::new([
        ("main.js", main),
        ("react.mjs", REACT_19_BUNDLE),
        ("react-reconciler-0.31.0.mjs", REACT_RECONCILER),
        ("renderer.mjs", CUSTOM_RENDERER),
    ])
    .expect("valid React probe module paths");
    compile("main.js", None, &resolver, &declared(TEST_DECLARATIONS)).unwrap_or_else(|error| {
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
    let resolver = MemoryResolver::new([
        ("main.js", main),
        ("preload.js", PRELOAD),
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
    let clock = host.clock().clone();
    let mut vm = Vm::new(
        build,
        &mut host,
        VmOptions {
            test_clock: Some(clock),
            ..Default::default()
        },
    )
    .expect("React headless VM initialization");
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

#[test]
fn react_scheduler_delayed_callback_survives_restore_once_unless_canceled() {
    // Expose the scheduler already bundled by esm.sh, without replacing its
    // implementation or the platform globals it reads.
    let reconciler = format!("{REACT_RECONCILER}\nexport const scheduler = Tf();");
    let main = r#"
        import { scheduler } from 'reconciler.mjs';
        let calls = 0;
        let scheduled;
        export function run() {
            scheduled = scheduler.unstable_scheduleCallback(
                scheduler.unstable_NormalPriority, () => { ++calls; }, { delay: 5 });
        }
        export function cancel() { scheduler.unstable_cancelCallback(scheduled); }
        export function observe() { return calls; }
    "#;
    let resolver = MemoryResolver::new([
        ("main.js", main),
        ("reconciler.mjs", &reconciler),
        ("react.mjs", REACT_19_BUNDLE),
        ("preload.js", PRELOAD),
    ])
    .unwrap();
    let build = compile(
        "main.js",
        Some("preload.js"),
        &resolver,
        &declared(TEST_DECLARATIONS),
    )
    .unwrap();
    let mut host = TestHost::new();
    let clock = host.clock().clone();
    let mut source = Vm::new(
        build.clone(),
        &mut host,
        VmOptions {
            test_clock: Some(clock),
            ..Default::default()
        },
    )
    .unwrap();
    source.start(&mut host, "run", vec![]).unwrap();
    let image = source.snapshot(&[]).unwrap();
    assert_eq!(host.clock().registrations().len(), 1);
    assert_eq!(image.script_timers.len(), 1);
    for cancel in [false, true] {
        let mut host = TestHost::new();
        host.clock().set_time(1000).unwrap();
        let clock = host.clock().clone();
        let mut vm = Vm::restore(
            image.clone(),
            build.clone(),
            &mut host,
            RestoreOptions {
                test_clock: Some(clock),
                ..Default::default()
            },
        )
        .unwrap();
        vm.start_recovered_operations().unwrap();
        if cancel {
            vm.start(&mut host, "cancel", vec![]).unwrap();
        }
        host.advance(&mut vm, 4).unwrap();
        assert_eq!(
            vm.start(&mut host, "observe", vec![]).unwrap(),
            Value::number(0.0)
        );
        host.advance(&mut vm, 1).unwrap();
        host.advance(&mut vm, 20).unwrap();
        assert_eq!(
            vm.start(&mut host, "observe", vec![]).unwrap(),
            Value::number(if cancel { 0.0 } else { 1.0 })
        );
        assert!(host.errors().is_empty());
    }
}
