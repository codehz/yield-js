use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use yield_compiler::{Compiler, NodeResolver, Project};
use yield_harness::{TestHost, string};
use yield_vm::{Build, Completion, JsValue, PromiseState, Snapshot, Value, ValueRef, Vm};

use super::support::{TEST_DECLARATIONS, declared};

const ENTRY: &str = "import { relay } from 'vendor';\n\
     import { callback } from './callback.js';\n\
     import { wait } from 'test';\n\
     export function capture() {\n  return relay(callback).stack;\n}\n\
     export async function run() {\n  const error = relay(callback);\n  await wait('stack');\n  return error.stack;\n}";

const FILTERED: &str =
    "Error: created\n    at callback (callback.js:2:10)\n    at capture (entry.js:5:10)";

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        loop {
            let path = std::env::temp_dir().join(format!(
                "yield-stack-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("creating {path:?}: {error}"),
            }
        }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write(root: &Path, relative: &str, source: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, source).unwrap();
}

/// A project module and a callback reachable only through an installed package.
fn fixture() -> TempDir {
    let root = TempDir::new();
    write(&root.0, "entry.js", ENTRY);
    write(
        &root.0,
        "callback.js",
        "export function callback() {\n  return new Error('created');\n}",
    );
    write(
        &root.0,
        "node_modules/vendor/package.json",
        r#"{"name":"vendor","type":"module","exports":{".":"./index.js"}}"#,
    );
    write(
        &root.0,
        "node_modules/vendor/index.js",
        "export function relay(callback) {\n  return callback();\n}",
    );
    root
}

fn build_stack(options: &Compiler) -> Build {
    let root = fixture();
    let resolver = NodeResolver::new(&root.0).unwrap();
    options.compile(Project::new("entry.js", resolver)).unwrap()
}

fn stack_text(build: Build, entry: &str) -> String {
    let host = TestHost::default();
    let mut vm = Vm::new(build, host.host(), Default::default()).unwrap();
    let stack = vm.start(entry, vec![]).unwrap();
    text(&stack)
}

fn text(value: &Value) -> String {
    let ValueRef::String(value) = value.view() else {
        panic!("expected stack string: {value:?}")
    };
    value.to_utf8().unwrap()
}

#[test]
fn filters_node_modules_modules_and_round_trips_build() {
    let build = build_stack(&declared(TEST_DECLARATIONS));
    let framework: Vec<_> = build
        .functions
        .iter()
        .filter(|function| {
            function
                .module
                .is_some_and(|index| build.modules[index as usize].id.starts_with("npm:"))
        })
        .collect();
    assert!(!framework.is_empty(), "the fixture links the package");
    assert!(
        framework
            .iter()
            .all(|function| function.stack_sites.is_empty()),
        "node_modules definitions carry no stack sites"
    );
    assert!(
        build.functions.iter().any(|function| {
            function
                .module
                .is_some_and(|index| build.modules[index as usize].id == "callback.js")
                && !function.stack_sites.is_empty()
        }),
        "project modules keep their stack sites"
    );
    let decoded = Build::from_bytes(&build.to_bytes()).unwrap();
    assert_eq!(decoded.build_id, build.build_id);
    assert_eq!(stack_text(build, "capture"), FILTERED);
    assert_eq!(stack_text(decoded, "capture"), FILTERED);
}

#[test]
fn stack_traces_flag_disables_every_site() {
    let mut options = declared(TEST_DECLARATIONS);
    options.stack_traces = false;
    let build = build_stack(&options);
    assert!(build.functions.iter().all(|f| f.stack_sites.is_empty()));
    assert_eq!(stack_text(build, "capture"), "Error: created");
}

#[test]
fn retains_creation_stack_across_snapshot_restore() {
    let build = build_stack(&declared(TEST_DECLARATIONS));
    let mut host = TestHost::default();
    let mut vm = Vm::new(build.clone(), host.host(), Default::default()).unwrap();
    let result = vm.start("run", Vec::<JsValue>::new()).unwrap();
    vm.run_until_idle().unwrap();
    let snapshot = vm.snapshot(std::slice::from_ref(&result)).unwrap();
    let decoded = Snapshot::from_bytes(&snapshot.to_bytes().unwrap()).unwrap();
    let finish = |mut vm: Vm, host: &mut TestHost| {
        host.complete(&mut vm, "wait:stack", Completion::Fulfilled(string("done")))
            .unwrap();
        vm.run_until_idle().unwrap();
        let PromiseState::Fulfilled(value) = vm.promise_state(&result).unwrap() else {
            panic!("expected fulfillment")
        };
        text(&value)
    };
    let original = finish(vm, &mut host);
    let mut restored_host = TestHost::default();
    let mut restored =
        Vm::restore(decoded, build, restored_host.host(), Default::default()).unwrap();
    restored.start_recovered_operations().unwrap();
    assert_eq!(finish(restored, &mut restored_host), original);
    assert_eq!(
        original,
        "Error: created\n    at callback (callback.js:2:10)\n    at run (entry.js:8:17)"
    );
}

#[test]
fn rejects_rehashed_invalid_stack_sites() {
    let build = build_stack(&declared(TEST_DECLARATIONS));
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
