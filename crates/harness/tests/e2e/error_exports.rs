//! Host-declared error subclasses: per-module identity, Rust-side roots,
//! restore re-linking, and undeclared-class faults.

use super::support::{compile_main, declared};
use yield_harness::TestHost;
use yield_vm::{
    ErrorClass, Host, JsResult, JsValue, RestoreOptions, SnapshotValidation, Vm, VmError,
    VmOptions, host_module, module_exports,
};

mod classes {
    use super::*;

    #[host_module(name = "errsA")]
    pub struct ModuleA;

    #[module_exports]
    impl ModuleA {
        #[error_class()]
        #[allow(non_upper_case_globals)]
        const ParseError: ErrorClass = ErrorClass::Host {
            module: "errsA",
            name: "ParseError",
        };

        #[function(name = "make")]
        fn make(cx: &mut yield_vm::ModuleCx<'_, Self>) -> JsResult<JsValue> {
            cx.alloc_error(Self::ParseError, "declared failure")
        }

        #[function(name = "bad")]
        fn bad(cx: &mut yield_vm::ModuleCx<'_, Self>) -> JsResult<JsValue> {
            cx.alloc_error(
                ErrorClass::Host {
                    module: "other",
                    name: "Missing",
                },
                "undeclared",
            )
        }
    }

    #[host_module(name = "errsB")]
    pub struct ModuleB;

    #[module_exports]
    impl ModuleB {
        #[error_class()]
        #[allow(non_upper_case_globals)]
        const ParseError: ErrorClass = ErrorClass::Host {
            module: "errsB",
            name: "ParseError",
        };

        #[function(name = "make")]
        fn make(cx: &mut yield_vm::ModuleCx<'_, Self>) -> JsResult<JsValue> {
            cx.alloc_error(Self::ParseError, "declared failure")
        }

        #[function(name = "bad")]
        fn bad(cx: &mut yield_vm::ModuleCx<'_, Self>) -> JsResult<JsValue> {
            cx.alloc_error(
                ErrorClass::Host {
                    module: "other",
                    name: "Missing",
                },
                "undeclared",
            )
        }
    }
}

use classes::{ModuleA, ModuleB};

fn cross_host() -> Host<impl yield_vm::PlatformServices> {
    yield_vm::host! {
        dts: "../dts/errsA-errsB.d.ts",
        modules: [ModuleA, ModuleB],
        services: {},
    }
    .expect("error class bindings")
}

#[test]
fn error_classes_are_identified_by_their_declaring_module() {
    let source = r#"
import { make as makeA, bad as badA, ParseError as ParseErrorA } from 'errsA';
import { make as makeB, ParseError as ParseErrorB } from 'errsB';
export function bad() { return badA(); }
export function run() {
    const a = makeA();
    const b = makeB();
    return a instanceof ParseErrorA
        && b instanceof ParseErrorB
        && !(a instanceof ParseErrorB)
        && !(b instanceof ParseErrorA)
        && a instanceof Error
        && b instanceof Error
        && Error.isError(a)
        && Error.isError(b)
        && ParseErrorA !== ParseErrorB;
}
"#;
    let build =
        compile_main(source, &declared(include_str!("../dts/errsA-errsB.d.ts"))).expect("compile");
    let mut vm = Vm::new(build, cross_host(), VmOptions::default()).expect("create VM");
    let result = vm.start("run", vec![]).expect("start");
    assert_eq!(result, yield_vm::Value::boolean(true));

    // An undeclared error class faults the host contract instead of becoming
    // a catchable script error.
    let bad = vm.start("bad", vec![]);
    assert!(
        matches!(bad, Err(VmError::HostFault(message)) if message.contains("undeclared error class"))
    );
}

#[test]
fn alloc_error_roots_survive_gc_and_full_restore() {
    let source = r#"
import { ParseError } from 'test';
export function check() {
    const e = new ParseError('again');
    return e instanceof ParseError && e instanceof Error && Error.isError(e);
}
"#;
    let build = yield_harness::build(source);
    let verified = yield_vm::VerifiedBuild::new(build.clone()).expect("verified build");
    let host = TestHost::default();
    let mut vm = Vm::new(verified.clone(), host.host(), VmOptions::default()).expect("create VM");

    let root = vm
        .alloc_error(
            ErrorClass::Host {
                module: "test",
                name: "ParseError",
            },
            "explicit root",
        )
        .expect("allocate rooted error");
    // The explicit root keeps its object alive across collection.
    vm.collect_garbage(std::slice::from_ref(&root))
        .expect("collect garbage");

    let snapshot = vm
        .snapshot(std::slice::from_ref(&root))
        .expect("snapshot with explicit root");
    let mut tampered = snapshot.clone();
    tampered.error_exports[0].constructor = tampered.error_exports[0].prototype;
    let error = Vm::restore(
        tampered,
        verified.clone(),
        TestHost::default().host(),
        RestoreOptions {
            validation: SnapshotValidation::Full,
            ..Default::default()
        },
    )
    .expect_err("tampered constructor id");
    assert!(matches!(
        error,
        VmError::InvalidSnapshot(message) if message.contains("not the declared constructor")
    ));

    let mut vm = Vm::restore(
        snapshot,
        verified,
        TestHost::default().host(),
        RestoreOptions {
            validation: SnapshotValidation::Full,
            ..Default::default()
        },
    )
    .expect("full restore");
    let result = vm.start("check", vec![]).expect("check restored class");
    assert_eq!(result, yield_vm::Value::boolean(true));
}
