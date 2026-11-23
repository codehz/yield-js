use yield_compiler::{CompileOptions, MemoryResolver, compile};
use yield_harness::{TestHost, string};
use yield_vm::{Build, Completion, JsValue, PromiseState, Snapshot, Value, ValueRef, Vm};

use super::support::{TEST_DECLARATIONS, declared};

const FILTERED: &str =
    "Error: created\n    at callback (callback.js:2:10)\n    at capture (entry.js:5:10)";

fn build_stack(options: &CompileOptions, import: &str, alias: &str) -> Build {
    let entry = format!(
        "import {{ relay as {alias} }} from '{import}';\n\
         import {{ callback }} from './callback.js';\n\
         import {{ wait }} from 'test';\n\
         export function capture() {{\n  return {alias}(callback).stack;\n}}\n\
         export async function run() {{\n  const error = {alias}(callback);\n  await wait('stack');\n  return error.stack;\n}}",
    );
    let resolver = MemoryResolver::new([
        ("entry.js", entry.as_str()),
        (
            "lib/vendor.js",
            "export function relay(callback) {\n  return callback();\n}",
        ),
        (
            "callback.js",
            "export function callback() {\n  return new Error('created');\n}",
        ),
    ])
    .unwrap();
    compile("entry.js", None, &resolver, options).unwrap()
}

fn stack_text(build: Build, entry: &str) -> String {
    let mut host = TestHost::default();
    let mut vm = Vm::new(build, &mut host, Default::default()).unwrap();
    let stack = vm.start(&mut host, entry, vec![]).unwrap();
    text(&stack)
}

fn text(value: &Value) -> String {
    let ValueRef::String(value) = value.view() else {
        panic!("expected stack string: {value:?}")
    };
    value.to_utf8().unwrap()
}

#[test]
fn filters_defining_module_and_round_trips_build() {
    let mut options = declared(TEST_DECLARATIONS);
    // A specifier alias and a host name are not canonical source IDs.
    options
        .framework_modules
        .extend(["./lib/vendor.js".into(), "test".into()]);
    let visible = build_stack(&options, "./lib/vendor.js", "relay");
    assert_eq!(
        stack_text(visible, "capture"),
        "Error: created\n    at callback (callback.js:2:10)\n    at relay (lib/vendor.js:2:10)\n    at capture (entry.js:5:10)",
    );
    options.framework_modules.insert("lib/vendor.js".into());
    for (import, alias) in [
        ("./lib/vendor.js", "relay"),
        ("./lib/../lib/vendor.js", "alias"),
    ] {
        let build = build_stack(&options, import, alias);
        for function in build.functions.iter().filter(|f| {
            f.module
                .is_some_and(|i| build.modules[i as usize].id == "lib/vendor.js")
        }) {
            assert!(function.stack_sites.is_empty());
        }
        let decoded = Build::from_bytes(&build.to_bytes()).unwrap();
        assert_eq!(decoded.build_id, build.build_id);
        assert_eq!(stack_text(build, "capture"), FILTERED);
        assert_eq!(stack_text(decoded, "capture"), FILTERED);
    }
    options
        .framework_modules
        .extend(["entry.js".into(), "callback.js".into()]);
    let hidden = build_stack(&options, "./lib/vendor.js", "relay");
    assert_eq!(stack_text(hidden, "capture"), "Error: created");
    options.framework_modules.clear();
    options.stack_traces = false;
    let disabled = build_stack(&options, "./lib/vendor.js", "relay");
    assert!(disabled.functions.iter().all(|f| f.stack_sites.is_empty()));
    assert_eq!(stack_text(disabled, "capture"), "Error: created");
}

#[test]
fn retains_creation_stack_across_snapshot_restore() {
    let mut options = declared(TEST_DECLARATIONS);
    options.framework_modules.insert("lib/vendor.js".into());
    let build = build_stack(&options, "./lib/vendor.js", "relay");
    let mut host = TestHost::default();
    let mut vm = Vm::new(build.clone(), &mut host, Default::default()).unwrap();
    let result = vm.start(&mut host, "run", Vec::<JsValue>::new()).unwrap();
    vm.run_until_idle(&mut host).unwrap();
    let snapshot = vm.snapshot(std::slice::from_ref(&result)).unwrap();
    let decoded = Snapshot::from_bytes(&snapshot.to_bytes().unwrap()).unwrap();
    let finish = |mut vm: Vm, host: &mut TestHost| {
        host.complete(&mut vm, "wait:stack", Completion::Fulfilled(string("done")))
            .unwrap();
        vm.run_until_idle(host).unwrap();
        let PromiseState::Fulfilled(value) = vm.promise_state(&result).unwrap() else {
            panic!("expected fulfillment")
        };
        text(&value)
    };
    let original = finish(vm, &mut host);
    let mut restored_host = TestHost::default();
    let mut restored = Vm::restore(decoded, build, &mut restored_host, Default::default()).unwrap();
    restored.start_recovered_operations().unwrap();
    assert_eq!(finish(restored, &mut restored_host), original);
    assert_eq!(
        original,
        "Error: created\n    at callback (callback.js:2:10)\n    at run (entry.js:8:17)"
    );
}

#[test]
fn rejects_rehashed_invalid_stack_sites() {
    let build = build_stack(&declared(TEST_DECLARATIONS), "./lib/vendor.js", "relay");
    let index = build
        .functions
        .iter()
        .position(|f| f.initial_name == "callback")
        .unwrap();
    assert!(build.functions[index].stack_sites.len() >= 2);
    for (case, expected) in [
        (0, "invalid stack site PC"),
        (1, "invalid stack site PC"),
        (2, "invalid stack site coordinates"),
        (3, "invalid defining module index"),
        (4, "stack sites without a defining module"),
    ] {
        let mut invalid = build.clone();
        let function = &mut invalid.functions[index];
        match case {
            0 => function.stack_sites.swap(0, 1),
            1 => function.stack_sites.last_mut().unwrap().pc = function.code.len() as u32,
            2 => function.stack_sites[0].column = 0,
            3 => function.module = Some(invalid.modules.len() as u32),
            _ => function.module = None,
        }
        invalid.finalize();
        assert_ne!(invalid.build_id, build.build_id);
        let error = invalid.validate().unwrap_err();
        assert!(error.contains(expected), "{error}");
    }
}
