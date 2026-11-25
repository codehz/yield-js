//! Source module linking, initialization, and snapshot integration.

use super::support::{TEST_DECLARATIONS, declared, json_round_trip};
use yield_compiler::{CompileError, MemoryResolver, Project};
use yield_harness::TestHost;
use yield_vm::bytecode::ModuleTarget;
use yield_vm::heap::Property;
use yield_vm::{
    Build, Completion, JsString, JsValue, Object, PromiseState, RestoreOptions, SnapshotValidation,
    Value, Vm, VmError,
};

fn link<'a>(
    files: impl IntoIterator<Item = (&'a str, &'a str)>,
    preload: Option<&str>,
) -> Result<Build, CompileError> {
    let resolver = MemoryResolver::new(files).expect("valid fixture paths");
    let options = declared(TEST_DECLARATIONS);
    let project = Project::new("main.js", resolver);
    options.compile(match preload {
        Some(preload) => project.with_preload(preload),
        None => project,
    })
}

fn run(files: &[(&str, &str)], expected: f64) {
    let build = link(files.iter().copied(), None).unwrap();
    let mut host = TestHost::default();
    let mut vm = Vm::new(build, &mut host, Default::default()).unwrap();
    assert_eq!(
        vm.start(&mut host, "run", vec![]).unwrap(),
        Value::number(expected)
    );
}

#[test]
fn cycle_can_call_hoisted_function_before_the_exporter_evaluates() {
    run(
        &[
            (
                "main.js",
                "import { answer } from './b.js';
                 globalThis.mainEvaluated = true;
                 export function hoisted() { return globalThis.mainEvaluated ? -1 : 7; }
                 export function run() { return answer; }",
            ),
            (
                "b.js",
                "import { hoisted } from './main.js'; export const answer = hoisted();",
            ),
        ],
        7.0,
    );
}

#[test]
fn cycle_throws_catchable_reference_error_for_uninitialized_import() {
    run(
        &[
            (
                "main.js",
                "import { caught } from './b.js';
                 export let late;
                 export function run() {
                     if (!caught || late !== undefined) throw 'wrong TDZ behavior';
                     return 7;
                 }",
            ),
            (
                "b.js",
                "import { late } from './main.js';
                 export let caught = false;
                 try { caught = late === 7; } catch (e) { caught = e instanceof ReferenceError; }",
            ),
        ],
        7.0,
    );
}

#[test]
fn missing_source_export_fails_during_compilation() {
    let error = link(
        [
            ("main.js", "import { missing } from './dep.js';"),
            ("dep.js", "export const present = 1;"),
        ],
        None,
    )
    .unwrap_err();
    assert_eq!(error.module(), "main.js");
    assert!(
        error.message().contains("has no export \"missing\""),
        "{error}"
    );
}

const COUNTER: &str = r#"
globalThis.counterInitializations = (globalThis.counterInitializations || 0) + 1;
export let count = 0;
export function inc() { return ++count; }
export default 99;
"#;

#[test]
fn diamond_evaluates_once_and_same_target_stars_remain_live() {
    run(
        &[
            (
                "main.js",
                r#"
                import { inc } from './left.js';
                import { count } from './join.js';
                import * as ns from './join.js';
                export function run() {
                    const { get } = Object.getOwnPropertyDescriptor(ns, 'count');
                    if (globalThis.counterInitializations !== 1 || count !== 0 || ns.count !== 0) {
                        throw 'bad initialization';
                    }
                    if ('default' in ns) throw 'star leaked default';
                    inc();
                    if (count !== 1 || ns.count !== 1 || get() !== 1) throw 'stale export';
                    return count;
                }
                "#,
            ),
            (
                "join.js",
                "export * from './left.js'; export * from './right.js';",
            ),
            ("left.js", "export * from './counter.js';"),
            ("right.js", "export * from './counter.js';"),
            ("counter.js", COUNTER),
        ],
        1.0,
    );
}

const CONFLICT: &[(&str, &str)] = &[
    ("left.js", "export const same = 7; export const left = 3;"),
    ("right.js", "export const same = 8; export const right = 4;"),
    (
        "join.js",
        "export * from './left.js'; export * from './right.js';",
    ),
    (
        "explicit.js",
        "export * from './left.js'; export * from './right.js';
         export { same } from './left.js'; export const privateToEntry = 11;",
    ),
];

#[test]
fn distinct_targets_are_omitted_unless_explicitly_shadowed() {
    let main = r#"
        import * as ns from './join.js';
        import { same } from './explicit.js';
        export * from './join.js';
        export function run() {
            if ('same' in ns || ns.left !== 3 || ns.right !== 4) throw 'wrong namespace';
            return same;
        }
    "#;
    let build = link(
        [("main.js", main)]
            .into_iter()
            .chain(CONFLICT.iter().copied()),
        None,
    )
    .unwrap();
    let mut host = TestHost::default();
    let mut vm = Vm::new(build, &mut host, Default::default()).unwrap();
    assert!(vm.get_export("same").is_err());
    assert_eq!(vm.get_export("left").unwrap(), Value::number(3.0));
    assert_eq!(
        vm.start(&mut host, "run", vec![]).unwrap(),
        Value::number(7.0)
    );
    assert!(vm.get_export("privateToEntry").is_err());

    let error = link(
        [("main.js", "import { same } from './join.js';")]
            .into_iter()
            .chain(CONFLICT.iter().copied()),
        None,
    )
    .unwrap_err();
    assert_eq!(error.module(), "main.js");
    assert!(error.message().contains("ambiguous"), "{error}");
}

#[test]
fn pure_alias_cycle_is_rejected_even_without_an_importer() {
    let error = link(
        [
            ("main.js", "import './a.js';"),
            ("a.js", "export { y as x } from './b.js';"),
            ("b.js", "export { x as y } from './a.js';"),
        ],
        None,
    )
    .unwrap_err();
    assert!(matches!(error.module(), "a.js" | "b.js"), "{error}");
    assert!(error.message().contains("circular export alias"), "{error}");
}

#[test]
fn dynamic_import_fulfills_with_the_eagerly_initialized_namespace() {
    let build = link([
        ("main.js", r#"
            export const beforeEntry = globalThis.dynamicInitializations;
            export async function run() {
                const NativePromise = Promise;
                return check(NativePromise, null);
            }
            async function check(NativePromise, Promise) {
                const first = import('./target.js');
                const second = import('./target.js');
                if (!(first instanceof NativePromise) || first === second) throw 'not fresh';
                const namespace = await first;
                if (namespace !== await second || namespace.initializations !== 1) throw 'wrong namespace';
                if (beforeEntry !== 1 || globalThis.dynamicInitializations !== 1) throw 'wrong order';
                return namespace.initializations;
            }
        "#),
        ("target.js", r#"
            globalThis.dynamicInitializations = (globalThis.dynamicInitializations || 0) + 1;
            export const initializations = globalThis.dynamicInitializations;
            export function then() { throw 'namespace must not be assimilated'; }
        "#),
    ], None).unwrap();
    let mut host = TestHost::default();
    let mut vm = Vm::new(build, &mut host, Default::default()).unwrap();
    assert_eq!(vm.get_export("beforeEntry").unwrap(), Value::number(1.0));
    let result = vm.start(&mut host, "run", vec![]).unwrap();
    vm.run_until_idle(&mut host).unwrap();
    assert!(
        matches!(vm.promise_state(&result).unwrap(), PromiseState::Fulfilled(value) if value == Value::number(1.0))
    );
}

const PRELOADED: [(&str, &str); 4] = [
    (
        "main.js",
        "import { count } from './counter.js';
         import { seen } from './entry-only.js';
         export function run() {
             if (seen !== 1 || globalThis.counterInitializations !== 1) throw 'wrong preload order';
             return count;
         }",
    ),
    (
        "preload.js",
        "import { inc } from './counter.js';
         export { count as preloadCount } from './counter.js';
         inc();",
    ),
    ("entry-only.js", "export const seen = preloadCount;"),
    ("counter.js", COUNTER),
];

#[test]
fn preload_and_entry_share_state_and_only_entry_exports_are_public() {
    let build = link(PRELOADED, Some("preload.js")).unwrap();
    let reversed = link(PRELOADED.into_iter().rev(), Some("preload.js")).unwrap();
    assert_eq!(build.build_id, reversed.build_id);

    let mut host = TestHost::default();
    let mut vm = Vm::new(build, &mut host, Default::default()).unwrap();
    assert_eq!(
        vm.start(&mut host, "run", vec![]).unwrap(),
        Value::number(1.0)
    );
    for hidden in ["preloadCount", "count", "inc", "seen", "default"] {
        assert!(vm.get_export(hidden).is_err(), "exposed {hidden}");
    }
}

#[test]
fn shared_dependency_cannot_use_bare_preload_export() {
    let mut files = PRELOADED;
    files[3].1 = "export let count = 0;
                  export function inc() { return ++count; }
                  export const illegal = preloadCount;";
    let error = link(files, Some("preload.js")).unwrap_err();
    assert_eq!(error.module(), "counter.js");
    assert!(error.message().contains("preloadCount"), "{error}");
}

#[test]
fn suspended_graph_restores_live_bindings_and_namespace_identity() {
    let build = link(
        [
            (
                "main.js",
                r#"
                import { wait } from 'test';
                import { count, inc } from './counter.js';
                import * as ns from './counter.js';
                import { counterNamespace } from './barrel.js';
                export async function run() {
                    inc();
                    const before = ns;
                    await wait('counter');
                    if (count !== 1 || before.count !== 1 || globalThis.counterInitializations !== 1) {
                        throw 'counter lost or module re-evaluated';
                    }
                    if (before !== ns || before !== counterNamespace) throw 'namespace identity lost';
                    inc();
                    if (before.count !== 2) throw 'stale namespace getter';
                    return count;
                }
                "#,
            ),
            ("counter.js", COUNTER),
            (
                "barrel.js",
                "export * as counterNamespace from './counter.js';",
            ),
        ],
        None,
    )
    .unwrap();
    let counter = build
        .modules
        .iter()
        .position(|module| module.id == "counter.js")
        .unwrap();
    let mut host = TestHost::default();
    let mut vm = Vm::new(build.clone(), &mut host, Default::default()).unwrap();
    let result = vm.start(&mut host, "run", vec![]).unwrap();
    vm.run_until_idle(&mut host).unwrap();
    assert_eq!(vm.promise_state(&result).unwrap(), PromiseState::Pending);
    assert!(host.token("wait:counter").is_some());
    let roots = std::slice::from_ref(&result);
    vm.collect_garbage(roots).unwrap();
    let snapshot = json_round_trip(&vm.snapshot(roots).unwrap());
    drop(vm);

    let mut bad_env = snapshot.clone();
    assert!(bad_env.module_envs[counter].is_some());
    bad_env.module_envs[counter] = Some(bad_env.module_namespaces[counter]);
    assert!(matches!(
        Vm::restore(
            bad_env,
            build.clone(),
            &mut host,
            RestoreOptions {
                validation: SnapshotValidation::Full,
                ..Default::default()
            },
        ),
        Err(VmError::InvalidSnapshot(_))
    ));

    let mut bad_getter = snapshot.clone();
    let namespace = bad_getter.module_namespaces[counter];
    let record = bad_getter
        .objects
        .iter_mut()
        .find(|record| record.id == namespace)
        .unwrap();
    let Object::Plain(namespace) = &mut record.object else {
        panic!("expected source namespace object");
    };
    let Property::Accessor {
        get: Some(wrong_getter),
        ..
    } = *namespace.props.get(&JsString::from("inc")).unwrap()
    else {
        panic!("expected inc getter");
    };
    let Property::Accessor { get, .. } = namespace.props.get_mut(&JsString::from("count")).unwrap()
    else {
        panic!("expected count getter");
    };
    assert!(get.is_some());
    assert_ne!(*get, Some(wrong_getter));
    *get = Some(wrong_getter);
    assert!(matches!(
        Vm::restore(
            bad_getter,
            build.clone(),
            &mut host,
            RestoreOptions {
                validation: SnapshotValidation::Full,
                ..Default::default()
            },
        ),
        Err(VmError::InvalidSnapshot(_))
    ));

    let mut bad_build = build.clone();
    let target = &mut bad_build.modules[counter]
        .exports
        .get_mut("count")
        .unwrap()
        .target;
    let ModuleTarget::Binding { module, .. } = target else {
        panic!("expected count binding target");
    };
    *module = u32::MAX;
    bad_build.finalize();
    assert!(matches!(
        Vm::new(bad_build, &mut host, Default::default()),
        Err(VmError::InvalidBuild(_))
    ));

    let mut vm = Vm::restore(snapshot, build, &mut host, Default::default()).unwrap();
    vm.start_recovered_operations().unwrap();
    host.complete(
        &mut vm,
        "wait:counter",
        Completion::Fulfilled(JsValue::undefined()),
    )
    .unwrap();
    vm.run_until_idle(&mut host).unwrap();
    assert_eq!(
        vm.promise_state(&result).unwrap(),
        PromiseState::Fulfilled(Value::number(2.0))
    );
}

#[test]
fn shared_root_is_evaluated_once_before_installing_its_getters() {
    let build = link([("main.js", r#"
        export const installedEarly = 'count' in globalThis;
        export const count = (globalThis.executions = (globalThis.executions || 0) + 1);
        export function run() { return !installedEarly && globalThis.count === 1 && globalThis.executions === 1; }
    "#)], Some("./main.js")).unwrap();
    let mut host = TestHost::default();
    let mut vm = Vm::new(build, &mut host, Default::default()).unwrap();
    assert_eq!(
        vm.start(&mut host, "run", vec![]).unwrap(),
        Value::boolean(true)
    );
}

#[test]
fn restores_captured_block_bindings_created_during_evaluation() {
    let build = link(
        [(
            "main.js",
            r#"
        export let run;
        { let captured = 7; run = () => captured; }
    "#,
        )],
        None,
    )
    .unwrap();
    let mut host = TestHost::default();
    let vm = Vm::new(build.clone(), &mut host, Default::default()).unwrap();
    let mut vm = Vm::restore(
        json_round_trip(&vm.snapshot(&[]).unwrap()),
        build,
        &mut host,
        Default::default(),
    )
    .unwrap();
    assert_eq!(
        vm.start(&mut host, "run", vec![]).unwrap(),
        Value::number(7.0)
    );
}
