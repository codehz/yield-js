//! Preload initialization and the live bindings rooted by its getters.

use super::support::{TEST_DECLARATIONS, compile_preloaded, declared, json_round_trip};
use yield_compiler::Compiler;
use yield_harness::TestHost;
use yield_vm::heap::Property;
use yield_vm::{JsString, Object, RestoreOptions, SnapshotValidation, Value, Vm, VmError};

#[test]
fn preload_import_errors_identify_the_source_module() {
    let error = compile_preloaded("", "import 'missing-host';", &options()).unwrap_err();
    assert_eq!(error.module(), "preload.js");
    assert!(
        error
            .message()
            .contains("source file not found: \"missing-host\""),
        "{error}"
    );

    let error = compile_preloaded("", "import { missing } from 'test';", &options()).unwrap_err();
    assert_eq!(error.module(), "preload.js");
    assert!(error.message().contains("has no export"), "{error}");
}

#[test]
fn preload_getters_survive_restore() {
    let build = compile_preloaded(
        r#"
export function run() {
    const n = Object.getOwnPropertyDescriptor(globalThis, 'n');
    const alias = Object.getOwnPropertyDescriptor(globalThis, 'alias');
    if (!n.enumerable || n.configurable || n.set !== undefined || n.get !== alias.get) {
        throw 'bad descriptor';
    }
    globalThis.bump();
    return globalThis.n;
}
"#,
        "export let n = 1; export {n as alias}; export function bump() { n++; }",
        &Compiler::default(),
    )
    .unwrap();
    let mut host = TestHost::default();
    let mut vm = Vm::new(build.clone(), &mut host, Default::default()).unwrap();
    assert_eq!(
        vm.start(&mut host, "run", vec![]).unwrap(),
        Value::number(2.0)
    );
    let snapshot = json_round_trip(&vm.snapshot(&[]).unwrap());
    let mut vm = Vm::restore(
        snapshot.clone(),
        build.clone(),
        &mut host,
        Default::default(),
    )
    .unwrap();
    assert_eq!(
        vm.start(&mut host, "run", vec![]).unwrap(),
        Value::number(3.0)
    );

    let mut corrupt = snapshot;
    let global = corrupt.roots["globalThis"];
    let object = &mut corrupt
        .objects
        .iter_mut()
        .find(|record| record.id == global)
        .unwrap()
        .object;
    let Object::EventTarget { object: global, .. } = object else {
        panic!("expected event target global")
    };
    let alias = global.props.get(&JsString::from("bump")).unwrap().clone();
    let n = global.props.get_mut(&JsString::from("n")).unwrap();
    assert!(matches!(n, Property::Accessor { .. }));
    *n = alias;
    assert!(matches!(Vm::restore(
        corrupt,
        build,
        &mut host,
        RestoreOptions {
            validation: SnapshotValidation::Full,
            ..Default::default()
        },
    ),
        Err(VmError::InvalidSnapshot(message)) if message.contains("wrong getter function")));
}

const MAIN: &str = r#"
import { seed as shadow, inspect } from 'test';
inspect();
function local(n) { return typeof n === 'string' && n === 'local' && globalThis.n > 0; }
export function run(expected) {
    if (bump() !== undefined) throw 'wrong receiver';
    const pair = [n, globalThis.n];
    if (pair[0] !== expected || pair[1] !== expected || typeof n !== 'number') throw 'not live';
    if (document !== globalThis.document || document.title !== 'ok') throw 'identity';
    if (readSeed() !== 10 || Math !== 42 || globalThis.Math !== 42) throw 'wrong global';
    if (shadow() !== 10 || globalThis.shadow !== 90 || !local('local')) throw 'shadowing';
    let hits = 0;
    try { n = ++hits; } catch (e) { if (!(e instanceof TypeError)) throw e; hits += 10; }
    try { globalThis.n = 9; } catch (e) { if (!(e instanceof TypeError)) throw e; hits += 10; }
    try { n += ++hits; } catch (e) { if (!(e instanceof TypeError)) throw e; hits += 10; }
    try { n++; } catch (e) { if (!(e instanceof TypeError)) throw e; hits += 10; }
    try { [n] = [0]; } catch (e) { if (!(e instanceof TypeError)) throw e; hits += 10; }
    if ((n ||= ++hits) !== expected || (n ??= ++hits) !== expected) throw 'short circuit';
    try { n &&= ++hits; } catch (e) { if (!(e instanceof TypeError)) throw e; hits += 10; }
    if (hits !== 63 || n !== expected) throw 'wrong write timing';
    return n;
}
"#;

const PRELOAD: &str = r#"
import { seed } from 'test';
export { seed as readSeed };
export let n = 1;
export function bump() { n++; return this; }
export const document = {title: 'ok'};
const replacement = 42;
export { replacement as Math };
export const shadow = 90;
"#;

fn options() -> Compiler {
    declared(TEST_DECLARATIONS)
}

#[test]
fn preload_globals_are_live_and_read_only() {
    let mut host = TestHost::default();
    let build = compile_preloaded(MAIN, PRELOAD, &options()).unwrap();
    let mut vm = Vm::new(build.clone(), &mut host, Default::default()).unwrap();
    assert!(vm.get_export("bump").is_err());
    assert!(vm.start(&mut host, "bump", vec![]).is_err());
    assert_eq!(
        vm.start(&mut host, "run", vec![yield_harness::number(2.0)])
            .unwrap(),
        Value::number(2.0)
    );
    let snapshot = json_round_trip(&vm.snapshot(&[]).unwrap());
    let changed = compile_preloaded(MAIN, &PRELOAD.replace("n = 1", "n = 5"), &options()).unwrap();
    assert!(matches!(
        Vm::restore(snapshot.clone(), changed, &mut host, Default::default()),
        Err(VmError::InvalidSnapshot(_))
    ));
    let mut vm = Vm::restore(snapshot, build, &mut host, Default::default()).unwrap();
    assert_eq!(
        vm.start(&mut host, "run", vec![yield_harness::number(3.0)])
            .unwrap(),
        Value::number(3.0)
    );
    assert_eq!(host.count("inspect"), 1);
}

#[test]
fn preload_failure_prevents_main_initialization() {
    let main = "import { inspect } from 'test'; inspect();";
    let mut host = TestHost::default();
    let build = compile_preloaded(main, "throw 'boom';", &options()).unwrap();
    assert!(matches!(
        Vm::new(build, &mut host, Default::default()),
        Err(VmError::Thrown(value)) if value == Value::string("boom".into())
    ));
    let build = compile_preloaded(
        main,
        "Object.defineProperty(globalThis, 'x', {value: 0}); export let x = 1;",
        &options(),
    )
    .unwrap();
    assert!(matches!(Vm::new(
            build,
            &mut host,
            Default::default(),
        ), Err(VmError::InvalidState(message)) if message.contains("cannot install preload global")));
    assert_eq!(host.count("inspect"), 0);
}

#[test]
fn preload_can_shadow_global_this_without_recursive_lookup() {
    let build = compile_preloaded(
        "export const result = globalThis + undefined;",
        "const g = 8; const u = 9; export {g as globalThis, u as undefined};",
        &Compiler::default(),
    )
    .unwrap();
    let vm = Vm::new(build, &mut TestHost::default(), Default::default()).unwrap();
    assert_eq!(vm.get_export("result").unwrap(), Value::number(17.0));
}

#[test]
fn preload_initialization_does_not_wait_and_drains_microtasks_after_main() {
    let build = compile_preloaded(
        "export const observed = n; export function current() { return n; }",
        "import { wait } from 'test'; wait('preload'); export let n = 1; queueMicrotask(() => { n = 2; });",
        &options(),
    ).unwrap();
    let mut host = TestHost::default();
    let mut vm = Vm::new(build, &mut host, Default::default()).unwrap();
    assert_eq!(host.pending_labels(), ["wait:preload"]);
    assert_eq!(vm.get_export("observed").unwrap(), Value::number(1.0));
    assert_eq!(
        vm.start(&mut host, "current", vec![]).unwrap(),
        Value::number(2.0)
    );
}
