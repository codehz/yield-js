//! Host-declared error subclasses: per-module identity, Rust-side roots,
//! restore re-linking, and undeclared-class faults.

use super::support::{compile_main, declared};
use yield_vm::binding::CallCx;
use yield_vm::{
    Decode, Encode, Host, Instance, JsResult, JsValue, RestoreOptions, SnapshotValidation, This,
    Value, ValueRef, Vm, VmError, VmOptions, host_class, host_module, methods, module_exports,
};

mod classes {
    use super::*;

    #[host_class(tag = "other:Missing", builtin_parent = Error)]
    struct Missing;

    macro_rules! error_module {
        ($module:ident, $name:tt, $tag:tt, $source:literal) => {
            pub mod $module {
                use super::*;

                #[host_class(tag = $tag, builtin_parent = Error, methods, prototype_name = "ParseError")]
                #[derive(Encode, Decode)]
                struct ParseError {
                    source: u8,
                    #[js_slot]
                    detail: JsValue,
                }

                #[methods(ParseError)]
                impl ParseError {
                    #[method(name = "source")]
                    fn source(cx: &mut CallCx<'_>, this: This<ParseError>) -> JsResult<JsValue> {
                        Ok(JsValue::number(f64::from(this.instance().payload(cx)?.source)))
                    }

                    #[method(name = "detail")]
                    fn detail(cx: &mut CallCx<'_>, this: This<ParseError>) -> JsResult<JsValue> {
                        this.detail(cx)
                    }
                }

                #[host_module(name = $name, classes = [ParseError])]
                pub struct Module;

                #[module_exports]
                impl Module {
                    #[constructor(name = "ParseError", class = ParseError)]
                    fn parse_error(cx: &mut yield_vm::ModuleCx<'_, Self>, message: Option<JsValue>) -> JsResult<Instance<ParseError>> {
                        let message = message.unwrap_or_else(JsValue::undefined);
                        let options = cx.args().get(1).cloned().unwrap_or_else(JsValue::undefined);
                        let instance = cx.alloc_with_super(ParseError { source: $source }, &[message.clone(), options])?;
                        instance.set_detail(cx, message)?;
                        Ok(instance)
                    }

                    #[function(name = "make")]
                    fn make(cx: &mut yield_vm::ModuleCx<'_, Self>) -> JsResult<Instance<ParseError>> {
                        Self::parse_error(cx, Some(JsValue::string("declared failure")))
                    }

                    #[function(name = "bad")]
                    fn bad(cx: &mut yield_vm::ModuleCx<'_, Self>) -> JsResult<JsValue> {
                        Ok(cx.alloc(Missing)?.value(cx))
                    }
                }
            }
        };
    }

    error_module!(a, "errsA", "errsA:ParseError", 1);
    error_module!(b, "errsB", "errsB:ParseError", 2);
}

use classes::{a::Module as ModuleA, b::Module as ModuleB};

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
        matches!(bad, Err(VmError::HostFault(message)) if message.contains("unregistered class other:Missing"))
    );
}

#[test]
fn alloc_error_roots_survive_gc_and_full_restore() {
    let source = r#"
import { make as makeA, ParseError as A } from 'errsA';
import { make as makeB, ParseError as B } from 'errsB';
const a = makeA();
const b = new B('original B');
const stack = a.stack;
const detail = a.detail;
const source = a.source;
Object.setPrototypeOf(a, null);
export function make() { return makeB(); }
export function check() {
    return Object.getPrototypeOf(a) === null && !(a instanceof A)
        && Error.isError(a) && a.stack === stack
        && detail.call(a) === 'declared failure' && source.call(a) === 1
        && b instanceof B && !(b instanceof A) && Error.isError(b)
        && b.source() === 2 && b.detail() === 'original B';
}
"#;
    let build =
        compile_main(source, &declared(include_str!("../dts/errsA-errsB.d.ts"))).expect("compile");
    let verified = yield_vm::VerifiedBuild::new(build).expect("verified build");
    let mut vm = Vm::new(verified.clone(), cross_host(), VmOptions::default()).expect("create VM");

    let root = vm.start("make", vec![]).expect("allocate rooted error");
    // The explicit root keeps its object alive across collection.
    vm.collect_garbage(std::slice::from_ref(&root))
        .expect("collect garbage");
    assert_eq!(
        vm.start("check", vec![]).expect("check after GC"),
        Value::boolean(true)
    );

    let snapshot = vm
        .snapshot(std::slice::from_ref(&root))
        .expect("snapshot with explicit root");
    let mut old = snapshot.clone();
    old.format_version = 93;
    assert!(matches!(
        Vm::restore(
            old,
            verified.clone(),
            cross_host(),
            RestoreOptions::default()
        ),
        Err(VmError::InvalidSnapshot(_))
    ));
    let mut tampered = snapshot.clone();
    let ValueRef::Object(root_id) = root.view() else {
        panic!("make must return an object")
    };
    let prototype_id = tampered.class_prototypes["errsB:ParseError"];
    let mut unbranded = tampered
        .objects
        .iter()
        .find(|record| record.id == prototype_id)
        .expect("class prototype in snapshot")
        .object
        .clone();
    let record = tampered
        .objects
        .iter_mut()
        .find(|record| record.id == root_id)
        .expect("explicit root in snapshot");
    let (yield_vm::Object::Plain(replacement), yield_vm::Object::Plain(original)) =
        (&mut unbranded, &record.object)
    else {
        panic!("expected plain class prototype and Error instance");
    };
    replacement.prototype = original.prototype;
    replacement.props = original.props.clone();
    record.object = unbranded;
    let error = Vm::restore(
        tampered,
        verified.clone(),
        cross_host(),
        RestoreOptions {
            validation: SnapshotValidation::Full,
            ..Default::default()
        },
    )
    .expect_err("unbranded host Error instance");
    assert!(matches!(
        error,
        VmError::InvalidSnapshot(message) if message.contains("branded Error")
    ));

    let mut vm = Vm::restore(
        snapshot,
        verified,
        cross_host(),
        RestoreOptions {
            validation: SnapshotValidation::Full,
            ..Default::default()
        },
    )
    .expect("full restore");
    let result = vm.start("check", vec![]).expect("check restored class");
    assert_eq!(result, yield_vm::Value::boolean(true));
}
