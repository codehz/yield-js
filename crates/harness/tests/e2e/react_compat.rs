//! Compatibility probes: real React 19 and scheduler source compiled and run
//! by the strict ESM subset.
//!
//! React DOM rendering is covered by `yield-blitz`; these probes keep the
//! compile contract and the scheduler restore contract in the harness.
use super::support::{TEST_DECLARATIONS, compile_main, compile_preloaded, declared};
use yield_compiler::{MemoryResolver, Project};
use yield_harness::TestHost;
use yield_vm::{RestoreOptions, Value, Vm, VmOptions};

const REACT_19_BUNDLE: &str = include_str!("vendor/react-19.2.0-esm-sh.mjs");
const SCHEDULER: &str = include_str!("vendor/scheduler-0.27.0.mjs");
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
fn react_scheduler_delayed_callback_survives_restore_once_unless_canceled() {
    // The production scheduler reads the platform globals supplied by the VM;
    // the preload only provides `window` and `process` as `undefined`.
    let main = r#"
        import scheduler from 'scheduler.mjs';
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
        ("scheduler.mjs", SCHEDULER),
        ("preload.js", PRELOAD),
    ])
    .unwrap();
    let build = declared(TEST_DECLARATIONS)
        .compile(Project::new("main.js", resolver).with_preload("preload.js"))
        .unwrap();
    let host = TestHost::new();
    let clock = host.clock().clone();
    let mut source = Vm::new(
        build.clone(),
        host.host(),
        VmOptions {
            test_clock: Some(clock),
            ..Default::default()
        },
    )
    .unwrap();
    source.start("run", vec![]).unwrap();
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
            host.host(),
            RestoreOptions {
                test_clock: Some(clock),
                ..Default::default()
            },
        )
        .unwrap();
        vm.start_recovered_operations().unwrap();
        if cancel {
            vm.start("cancel", vec![]).unwrap();
        }
        host.advance(&mut vm, 4).unwrap();
        assert_eq!(vm.start("observe", vec![]).unwrap(), Value::number(0.0));
        host.advance(&mut vm, 1).unwrap();
        host.advance(&mut vm, 20).unwrap();
        assert_eq!(
            vm.start("observe", vec![]).unwrap(),
            Value::number(if cancel { 0.0 } else { 1.0 })
        );
        assert!(host.errors().is_empty());
    }
}
