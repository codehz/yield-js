//! Host-declared error subclasses: per-module identity, Rust-side roots,
//! restore re-linking, and undeclared-class faults.

use super::support::{compile_main, declared};
use yield_vm::binding::CallCx;
use yield_vm::intrinsics::{DOMException, Error, TypeError};
use yield_vm::{
    Decode, Encode, Host, Instance, JsResult, JsString, JsValue, RestoreOptions, Snapshot,
    SnapshotValidation, This, Value, ValueRef, Vm, VmError, VmOptions, host_class, host_module,
    methods, module_exports,
};

mod classes {
    use super::*;

    #[host_class(tag = "other:Missing", parent = yield_vm::intrinsics::Error)]
    struct Missing;

    macro_rules! error_module {
        ($module:ident, $name:tt, $tag:tt, $parent:ident, $source:literal) => {
            pub mod $module {
                use super::*;

                #[host_class(tag = $tag, parent = $parent, methods, prototype_name = "ParseError")]
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
                        Ok(JsValue::number(f64::from(
                            this.instance().payload(cx)?.source,
                        )))
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
                    fn parse_error(
                        cx: &mut yield_vm::ModuleCx<'_, Self>,
                        message: Option<JsValue>,
                    ) -> JsResult<Instance<ParseError>> {
                        let message = message.unwrap_or_else(JsValue::undefined);
                        let options = cx.args().get(1).cloned().unwrap_or_else(JsValue::undefined);
                        let instance = cx.alloc_with_super(
                            ParseError { source: $source },
                            &[message.clone(), options],
                        )?;
                        instance.set_detail(cx, message)?;
                        Ok(instance)
                    }

                    #[function(name = "make")]
                    fn make(
                        cx: &mut yield_vm::ModuleCx<'_, Self>,
                    ) -> JsResult<Instance<ParseError>> {
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

    error_module!(a, "errsA", "errsA:ParseError", Error, 1);
    error_module!(b, "errsB", "errsB:ParseError", TypeError, 2);

    pub mod dom {
        use super::*;

        #[host_class(tag = "errsDOM:HostDOMException", parent = DOMException)]
        struct HostDOMException;

        #[host_module(name = "errsDOM", classes = [HostDOMException])]
        pub struct Module;

        #[module_exports]
        impl Module {
            #[constructor(name = "HostDOMException", class = HostDOMException)]
            fn dom_exception(
                cx: &mut yield_vm::ModuleCx<'_, Self>,
                message: JsValue,
                name: JsValue,
            ) -> JsResult<Instance<HostDOMException>> {
                cx.alloc_with_super(HostDOMException, &[message, name])
            }
        }
    }
}

use classes::{a::Module as ModuleA, b::Module as ModuleB, dom::Module as DomModule};

fn cross_host() -> Host<impl yield_vm::PlatformServices> {
    yield_vm::host! {
        dts: "../dts/errsA-errsB.d.ts",
        modules: [ModuleA, ModuleB, DomModule],
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
import { HostDOMException } from 'errsDOM';
export const originalError = Error;
export const originalTypeError = TypeError;
export const originalDOMException = DOMException;
globalThis.TypeError = function ReplacementTypeError() {};
globalThis.DOMException = function ReplacementDOMException() {};
class Derived extends B {}
const a = makeA();
const b = new B('original B');
const derived = new Derived('derived B', { cause: b });
export const dom = new HostDOMException('host DOM failure', 'AbortError');
const detachedDOM = new HostDOMException('detached DOM failure', 'DataCloneError');
const stack = a.stack;
const detail = a.detail;
const source = a.source;
Object.setPrototypeOf(a, null);
Object.setPrototypeOf(detachedDOM, null);
export function make() { return makeB(); }
export function check() {
    return globalThis.TypeError !== originalTypeError
        && globalThis.DOMException !== originalDOMException
        && Object.getPrototypeOf(B) === originalTypeError
        && Object.getPrototypeOf(HostDOMException) === originalDOMException
        && Object.getPrototypeOf(originalTypeError) === originalError
        && Object.getPrototypeOf(originalDOMException) === originalError
        && Object.getPrototypeOf(a) === null && !(a instanceof A)
        && Error.isError(a) && a.stack === stack
        && detail.call(a) === 'declared failure' && source.call(a) === 1
        && b instanceof B && !(b instanceof A) && b instanceof originalTypeError
        && b instanceof Error && Error.isError(b)
        && b.source() === 2 && b.detail() === 'original B'
        && derived instanceof Derived && derived instanceof B
        && derived instanceof originalTypeError && derived instanceof Error
        && Error.isError(derived) && derived.message === 'derived B' && derived.cause === b
        && derived.source() === 2 && derived.detail() === 'derived B'
        && dom instanceof HostDOMException && dom instanceof originalDOMException
        && dom instanceof Error && Error.isError(dom)
        && dom.name === 'AbortError' && dom.message === 'host DOM failure' && dom.code === 20
        && Object.getPrototypeOf(detachedDOM) === null && !(detachedDOM instanceof HostDOMException)
        && Error.isError(detachedDOM)
        && Reflect.get(originalDOMException.prototype, 'name', detachedDOM) === 'DataCloneError'
        && Reflect.get(originalDOMException.prototype, 'message', detachedDOM) === 'detached DOM failure';
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
    old.format_version = yield_vm::SNAPSHOT_FORMAT_VERSION - 1;
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

    // A valid DOMException object shape is not enough if its ErrorData slot is gone.
    let dom = vm.get_export("dom").expect("host DOMException export");
    let ValueRef::Object(dom_id) = dom.view() else {
        panic!("dom must be an object")
    };
    let dom_id = serde_json::to_value(dom_id).expect("serialize DOMException object ID");
    let mut json = serde_json::to_value(&snapshot).expect("serialize snapshot");
    let record = json["objects"]
        .as_array_mut()
        .expect("object records")
        .iter_mut()
        .find(|record| record["id"] == dom_id)
        .expect("host DOMException in snapshot");
    let brand = &mut record["object"]["dom_exception"]["object"]["is_error"];
    assert_eq!(brand.as_bool(), Some(true));
    *brand = serde_json::Value::Bool(false);
    let tampered = Snapshot::from_json(&json.to_string()).expect("parse tampered snapshot");
    assert!(matches!(
        Vm::restore(
            tampered,
            verified.clone(),
            cross_host(),
            RestoreOptions {
                validation: SnapshotValidation::Full,
                ..Default::default()
            },
        ),
        Err(VmError::InvalidSnapshot(message))
            if message.contains("a DOMException") || message.contains("ErrorData slot")
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

    assert_eq!(
        vm.constructor::<Error>()
            .expect("restored Error constructor"),
        vm.get_export("originalError")
            .expect("saved Error constructor"),
    );
    assert_eq!(
        vm.constructor::<TypeError>()
            .expect("restored TypeError constructor"),
        vm.get_export("originalTypeError")
            .expect("saved TypeError constructor"),
    );
    assert_eq!(
        vm.constructor::<DOMException>()
            .expect("restored DOMException constructor"),
        vm.get_export("originalDOMException")
            .expect("saved DOMException constructor"),
    );

    let dom = vm
        .construct::<DOMException>(&[
            Value::string(JsString::from("constructed after restore")),
            Value::string(JsString::from("InvalidStateError")),
        ])
        .expect("construct intrinsic DOMException despite replaced global");
    let ValueRef::Object(dom_id) = dom.view() else {
        panic!("construct must return a DOMException object")
    };
    let snapshot = vm
        .snapshot(std::slice::from_ref(&dom))
        .expect("snapshot constructed DOMException");
    let record = snapshot
        .objects
        .iter()
        .find(|record| record.id == dom_id)
        .expect("constructed DOMException in snapshot");
    let yield_vm::Object::DomException { name, message, .. } = &record.object else {
        panic!("construct must initialize DOMException slots")
    };
    assert_eq!(message, &JsString::from("constructed after restore"));
    assert_eq!(name, &JsString::from("InvalidStateError"));
}
