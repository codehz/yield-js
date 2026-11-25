//! Default exports: statement-position values, live aliases, and preload names.

use super::support::{
    TEST_DECLARATIONS, compile_main, compile_preloaded, declared, json_round_trip,
};
use yield_compiler::Compiler;
use yield_harness::TestHost;
use yield_vm::{Value, Vm, VmError};

fn vm_of(source: &str, options: &Compiler) -> Vm {
    let build = compile_main(source, options).unwrap();
    Vm::new(build, &mut TestHost::default(), Default::default()).unwrap()
}

#[test]
fn default_value_is_taken_at_the_statement() {
    let vm = vm_of(
        "let x = 1; export default x++; export {x}; x++;",
        &Compiler::default(),
    );
    assert_eq!(vm.get_export("default").unwrap(), Value::number(1.0));
    assert_eq!(vm.get_export("x").unwrap(), Value::number(3.0));
}

#[test]
fn default_alias_keeps_the_live_binding() {
    let vm = vm_of(
        "let x = 1; export {x as default}; x = 3;",
        &Compiler::default(),
    );
    assert_eq!(vm.get_export("default").unwrap(), Value::number(3.0));
}

#[test]
fn hoisted_default_function_initializes_earlier_exports() {
    let mut vm = vm_of(
        "export const early = f(); export default function f(){ return 7 }",
        &Compiler::default(),
    );
    assert_eq!(vm.get_export("early").unwrap(), Value::number(7.0));
    assert!(matches!(
        vm.get_export("f"),
        Err(VmError::InvalidState(message)) if message == "unknown export \"f\""
    ));
    assert_eq!(
        vm.start(&mut TestHost::default(), "default", vec![])
            .unwrap(),
        Value::number(7.0)
    );

    // A later assignment to the local name rebinds the default as well.
    let mut vm = vm_of(
        "export const early = f(); export default function f(){ return 7 }; f = () => 8;",
        &Compiler::default(),
    );
    assert_eq!(
        vm.start(&mut TestHost::default(), "default", vec![])
            .unwrap(),
        Value::number(8.0)
    );
}

#[test]
fn default_export_of_a_host_import_is_callable() {
    let mut vm = vm_of(
        "import { seed as x } from 'test'; export { x as default };",
        &declared(TEST_DECLARATIONS),
    );
    assert_eq!(
        vm.start(&mut TestHost::default(), "default", vec![])
            .unwrap(),
        Value::number(10.0)
    );
}

#[test]
fn preload_default_name_is_default() {
    for preload in [
        "export default async function(){ return 4 }",
        "export default class {}",
        "export default (() => 4)",
    ] {
        let build = compile_preloaded(
            "export function run(){ return globalThis.default.name }",
            preload,
            &Compiler::default(),
        )
        .unwrap();
        let mut host = TestHost::default();
        let mut vm = Vm::new(build.clone(), &mut host, Default::default()).unwrap();
        assert_eq!(
            vm.start(&mut host, "run", vec![]).unwrap(),
            Value::string("default".into()),
            "{preload}"
        );
        // The host reads the main module's own default, not the preload's.
        let build = compile_preloaded("export default 9;", preload, &Compiler::default()).unwrap();
        let vm = Vm::new(build, &mut TestHost::default(), Default::default()).unwrap();
        assert_eq!(vm.get_export("default").unwrap(), Value::number(9.0));
    }
}

#[test]
fn preload_default_alias_survives_restore() {
    let build = compile_preloaded(
        "export function run(){ bump(); return globalThis.default; }",
        "let x = 1; export { x as default }; export function bump(){ x++ }",
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
    let mut vm = Vm::restore(snapshot, build, &mut host, Default::default()).unwrap();
    assert_eq!(
        vm.start(&mut host, "run", vec![]).unwrap(),
        Value::number(3.0)
    );
    // The alias is still a getter-only preload global.
    let build = compile_preloaded(
        r#"
export function run(){
  try { globalThis.default = 0; return 'no-throw'; }
  catch (e) { return e instanceof TypeError; }
}"#,
        "let x = 1; export { x as default };",
        &Compiler::default(),
    )
    .unwrap();
    let mut host = TestHost::default();
    let mut vm = Vm::new(build, &mut host, Default::default()).unwrap();
    assert_eq!(
        vm.start(&mut host, "run", vec![]).unwrap(),
        Value::boolean(true)
    );
}

#[test]
fn named_default_class_shares_one_object_with_its_exported_name() {
    let vm = vm_of(
        "export default class C {}; export { C };",
        &Compiler::default(),
    );
    let default = vm.get_export("default").unwrap();
    assert_eq!(default, vm.get_export("C").unwrap());
    let mut host = TestHost::default();
    let mut vm2 = vm_of(
        "export default class C {}; export function run(){ return C.name }",
        &Compiler::default(),
    );
    assert_eq!(
        vm2.start(&mut host, "run", vec![]).unwrap(),
        Value::string("C".into())
    );
}
