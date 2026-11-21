use std::sync::Arc;

use super::{StrategyHost, options};
use yield_harness::{LabelService, Wait};
use yield_vm::binding::{CallCx, ExportDecl};
use yield_vm::{
    Bindings, Completion, HostStep, Instance, JsResult, JsValue, PromiseState, This, Value, Vm,
    VmError, host_bindings, host_class, host_module, methods,
};

mod original {
    use super::*;

    #[host_class(tag = "Entity", methods)]
    struct Entity;

    #[host_class(tag = "Auxiliary")]
    struct Auxiliary;

    #[host_module(name = "auxiliary")]
    mod extra {}

    #[methods(Entity)]
    impl Entity {
        #[get(name = "value")]
        fn view(_: &mut CallCx<'_>, _: This<Entity>) -> JsResult<JsValue> {
            Ok(JsValue::undefined())
        }
    }

    #[host_module(name = "contract")]
    mod api {
        use super::*;

        #[function(name = "probe")]
        fn probe(_: &mut CallCx<'_>, value: JsValue) -> JsResult<JsValue> {
            Ok(value)
        }

        #[function(name = "wait")]
        fn wait(cx: &mut CallCx<'_>) -> JsResult<JsValue> {
            cx.promise(Wait {
                label: "pending".into(),
            })
        }
    }

    pub(super) fn bindings(labels: &Arc<LabelService>) -> Result<Bindings, String> {
        host_bindings! {
            modules: [api, extra], classes: [Entity, Auxiliary], states: [], operations: [Wait],
            services: [("labels", 1, Arc::clone(labels))],
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

            #[host_module(name = "auxiliary")]
            mod extra {}


            #[methods(Entity)]
            impl Entity {
                #[get(name = "value")]
                fn renamed_getter($getter_cx: &mut CallCx<'_>, _: This<Entity>) -> JsResult<$($getter)+> $view
            }

            #[host_module(name = "contract")]
            mod alternate_api {
                use super::*;

                #[function(name = "wait")]
                fn schedule(cx: &mut CallCx<'_>) -> JsResult<JsValue> {
                    cx.promise(Wait { label: "pending".into() })
                }

                #[function(name = "probe")]
                fn renamed_probe($probe_cx: &mut CallCx<'_>, $value: $($arg)+) -> JsResult<$($result)+> $probe
            }

            pub(super) fn bindings(labels: &Arc<LabelService>) -> Result<Bindings, String> {
                host_bindings! {
                    modules: [extra, alternate_api], classes: [Extra, Entity], states: [], operations: [Wait],
                    services: [("labels", 1, Arc::clone(labels))],
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

fn assert_probe_arity(bindings: &Bindings) {
    let (_, ExportDecl::Function(probe)) =
        bindings.export("contract", "probe").expect("probe export")
    else {
        panic!("probe must be a function");
    };
    assert_eq!(probe.arity, 1);
}

#[test]
fn snapshot_restore_checks_signatures_without_hashing_rust_names_or_registration_order() {
    let labels = Arc::new(LabelService::default());
    let bindings = original::bindings(&labels).expect("original bindings");
    assert_probe_arity(&bindings);
    let build = yield_compiler::compile(
        r#"
        import { probe, wait } from 'contract';
        export async function run() {
          await wait();
          return probe(42);
        }
        "#,
        &options(&bindings),
    )
    .expect("compile");
    let mut host = StrategyHost::new(bindings);
    let mut vm = Vm::new(build.clone(), &mut host).expect("create VM");
    let result = vm.start(&mut host, "run", vec![]).expect("start");
    vm.run_until_idle(&mut host).expect("suspend");
    assert!(labels.token("wait:pending").is_some());
    let snapshot = vm
        .snapshot(std::slice::from_ref(&result))
        .expect("snapshot");
    drop(vm);

    for (change, bind) in [
        (
            "function return kind",
            instance_result::bindings as fn(&Arc<LabelService>) -> Result<Bindings, String>,
        ),
        ("step return kind", step_result::bindings),
        ("function parameter kind", optional_argument::bindings),
        ("getter return kind", instance_getter::bindings),
    ] {
        let labels = Arc::new(LabelService::default());
        let bindings = bind(&labels).expect("compatible export shape");
        assert_probe_arity(&bindings);
        let mut changed_host = StrategyHost::new(bindings);
        let error = Vm::restore(snapshot.clone(), build.clone(), &mut changed_host)
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

    let labels = Arc::new(LabelService::default());
    let mut reordered_host =
        StrategyHost::new(reordered::bindings(&labels).expect("reordered bindings"));
    let mut restored = Vm::restore(snapshot, build, &mut reordered_host)
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
    restored
        .run_until_idle(&mut reordered_host)
        .expect("resume");
    assert_eq!(
        restored.promise_state(&result).unwrap(),
        PromiseState::Fulfilled(Value::Number(42.0))
    );
}
