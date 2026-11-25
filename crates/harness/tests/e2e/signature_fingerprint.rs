use std::rc::Rc;

use super::super::support::{compile_main, declared};
use yield_harness::{LabelService, Wait};
use yield_vm::binding::CallCx;
use yield_vm::{
    Completion, Host, HostStep, Instance, JsResult, JsValue, PromiseState, This, Value, Vm,
    VmError, host, host_class, host_module, methods, module_exports,
};

mod original {
    use super::*;

    #[host_class(tag = "Entity", methods)]
    struct Entity;

    #[host_class(tag = "Auxiliary")]
    struct Auxiliary;

    #[host_module(name = "auxiliary", classes = [Auxiliary])]
    struct ExtraModule;

    #[module_exports]
    impl ExtraModule {}

    #[methods(Entity)]
    impl Entity {
        #[get(name = "value")]
        fn view(_: &mut CallCx<'_>, _: This<Entity>) -> JsResult<JsValue> {
            Ok(JsValue::undefined())
        }
    }

    #[host_module(
        name = "contract",
        classes = [Entity],
        operations = [Wait],
    )]
    struct Api;

    #[module_exports]
    impl Api {
        #[function(name = "probe")]
        fn probe(_: &mut yield_vm::ModuleCx<'_, Self>, value: JsValue) -> JsResult<JsValue> {
            Ok(value)
        }

        #[function(name = "wait")]
        fn wait(cx: &mut yield_vm::ModuleCx<'_, Self>) -> JsResult<JsValue> {
            cx.promise(Wait {
                label: "pending".into(),
            })
        }
    }

    pub(super) fn make_host(labels: &Rc<LabelService>) -> Result<Host, String> {
        host! {
            dts: "../dts/auxiliary-contract.d.ts",
            modules: [Api, ExtraModule],
            services: {
                Rc<LabelService> => Rc::clone(labels),
            },
        }
    }
}

macro_rules! independent_contract {
    ($fixture:ident, probe($probe_cx:ident, $value:ident): [$($arg:tt)+] -> [$($result:tt)+] $probe:block,
        getter($getter_cx:ident) -> [$($getter:tt)+] $view:block) => {
        mod $fixture {
            use super::*;

            #[host_class(tag = "Entity", methods)]
            struct Entity;
            #[host_class(tag = "Auxiliary")]
            struct Extra;

            #[host_module(name = "auxiliary", classes = [Extra])]
            struct ExtraModule;

            #[module_exports]
            impl ExtraModule {}


            #[methods(Entity)]
            impl Entity {
                #[get(name = "value")]
                fn renamed_getter($getter_cx: &mut CallCx<'_>, _: This<Entity>) -> JsResult<$($getter)+> $view
            }

            #[host_module(
                name = "contract",
                classes = [Entity],
                operations = [Wait],
            )]
            struct AlternateApi;

            #[module_exports]
            impl AlternateApi {
                #[function(name = "wait")]
                fn schedule(cx: &mut yield_vm::ModuleCx<'_, Self>) -> JsResult<JsValue> {
                    cx.promise(Wait { label: "pending".into() })
                }

                #[function(name = "probe")]
                fn renamed_probe($probe_cx: &mut yield_vm::ModuleCx<'_, Self>, $value: $($arg)+) -> JsResult<$($result)+> $probe
            }

            pub(super) fn make_host(labels: &Rc<LabelService>) -> Result<Host, String> {
                host! {
                    dts: "../dts/auxiliary-contract.d.ts",
                    modules: [ExtraModule, AlternateApi],
                    services: {
                        Rc<LabelService> => Rc::clone(labels),
                    },
                }
            }
        }
    };
}

independent_contract!(reordered, probe(_cx, value): [JsValue] -> [JsValue] { Ok(value) },
    getter(_cx) -> [JsValue] { Ok(JsValue::undefined()) });
independent_contract!(instance_result, probe(cx, _value): [JsValue] -> [Instance<Entity>] { cx.alloc(Entity) },
    getter(_cx) -> [JsValue] { Ok(JsValue::undefined()) });
independent_contract!(step_result, probe(_cx, value): [JsValue] -> [HostStep] { Ok(HostStep::Return(value)) },
    getter(_cx) -> [JsValue] { Ok(JsValue::undefined()) });
independent_contract!(optional_argument, probe(_cx, value): [Option<JsValue>] -> [JsValue] { Ok(value.unwrap_or_else(JsValue::undefined)) },
    getter(_cx) -> [JsValue] { Ok(JsValue::undefined()) });
independent_contract!(instance_getter, probe(_cx, value): [JsValue] -> [JsValue] { Ok(value) },
    getter(cx) -> [Instance<Entity>] { cx.alloc(Entity) });

#[test]
fn snapshot_restore_checks_signatures_without_hashing_rust_names_or_registration_order() {
    let labels = Rc::new(LabelService::default());
    let host = original::make_host(&labels).expect("original bindings");
    let build = compile_main(
        r#"
        import { probe, wait } from 'contract';
        export function arity() { return probe.length; }
        export async function run() {
          await wait();
          return probe(42);
        }
        "#,
        &declared(include_str!("../dts/auxiliary-contract.d.ts")),
    )
    .expect("compile");
    let mut vm = Vm::new(build.clone(), host, Default::default()).expect("create VM");
    assert_eq!(vm.start("arity", vec![]).unwrap(), Value::number(1.0));
    let result = vm.start("run", vec![]).expect("start");
    vm.run_until_idle().expect("suspend");
    assert!(labels.token("wait:pending").is_some());
    let snapshot = vm
        .snapshot(std::slice::from_ref(&result))
        .expect("snapshot");
    drop(vm);

    for (change, bind) in [
        (
            "function return kind",
            instance_result::make_host as fn(&Rc<LabelService>) -> Result<Host, String>,
        ),
        ("step return kind", step_result::make_host),
        ("function parameter kind", optional_argument::make_host),
        ("getter return kind", instance_getter::make_host),
    ] {
        let labels = Rc::new(LabelService::default());
        let host = bind(&labels).expect("compatible export shape");
        let mut probe_vm =
            Vm::new(build.clone(), bind(&labels).unwrap(), Default::default()).unwrap();
        assert_eq!(probe_vm.start("arity", vec![]).unwrap(), Value::number(1.0));
        let error = Vm::restore(snapshot.clone(), build.clone(), host, Default::default())
            .expect_err("signature change must invalidate the snapshot");
        assert!(
            matches!(&error, VmError::InvalidSnapshot(message) if message.contains("binding fingerprint")),
            "{change}: {error:?}"
        );
        assert!(
            labels.pending_labels().is_empty(),
            "{change}: operation started"
        );
        assert!(
            labels.resume_log().is_empty(),
            "{change}: operation resumed"
        );
    }

    let labels = Rc::new(LabelService::default());
    let reordered = reordered::make_host(&labels).expect("reordered bindings");
    let mut restored = Vm::restore(snapshot, build, reordered, Default::default())
        .expect("restore with renamed and reordered declarations");
    assert!(labels.pending_labels().is_empty());
    restored
        .start_recovered_operations()
        .expect("start recovered operation");
    assert_eq!(labels.resume_log(), ["wait:pending"]);
    restored
        .complete_operation(
            labels.token("wait:pending").expect("restored wait token"),
            Completion::Fulfilled(JsValue::undefined()),
        )
        .expect("complete wait");
    restored.run_until_idle().expect("resume");
    assert_eq!(
        restored.promise_state(&result).unwrap(),
        PromiseState::Fulfilled(Value::number(42.0))
    );
}
