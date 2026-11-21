//! Host task ABI exercised without relying on CPS macro lowering.

use std::sync::Arc;

use yield_compiler::{CompileOptions, compile};
use yield_poc::{LabelService, Wait};
use yield_vm::binding::__macro_support as abi;
use yield_vm::binding::{ArgKind, CallCx, CallSignature, CpsBoundary, CpsStage, ReturnKind};
use yield_vm::{
    Bindings, Completion, GuestOutcome, Host, HostStep, JsResult, JsValue, Value, Vm, VmError,
};

struct TaskHost {
    bindings: Arc<Bindings>,
    labels: Arc<LabelService>,
}

impl TaskHost {
    fn new() -> Self {
        let labels = Arc::new(LabelService::default());
        let module = abi::module(
            "task",
            vec![
                abi::function_cps(
                    "spawn",
                    false,
                    None,
                    spawn,
                    CallSignature {
                        args: &[ArgKind::Value],
                        result: ReturnKind::Value,
                    },
                    &STAGES,
                ),
                abi::function(
                    "cancel",
                    false,
                    None,
                    cancel,
                    CallSignature {
                        args: &[ArgKind::Value],
                        result: ReturnKind::Void,
                    },
                ),
            ],
        );
        let bindings = abi::link(
            [module],
            [],
            [],
            [abi::operation::<Wait>()],
            [abi::service("labels", 1, Arc::clone(&labels))],
        )
        .unwrap();
        Self {
            bindings: Arc::new(bindings),
            labels,
        }
    }
}

impl Host for TaskHost {
    fn bindings(&self) -> Arc<Bindings> {
        Arc::clone(&self.bindings)
    }
    fn report_error(&mut self, error: JsValue) {
        panic!("unexpected error: {error}");
    }
}

static STAGES: [CpsStage; 2] = [
    CpsStage {
        resume: fire,
        kind: CpsBoundary::External,
        slots: &[0],
        receiver: None,
    },
    CpsStage {
        resume: finish,
        kind: CpsBoundary::Call,
        slots: &[],
        receiver: None,
    },
];

fn spawn(cx: &mut CallCx<'_>) -> JsResult<HostStep> {
    let cb = cx.arg(0)?;
    let first = cx.__linked_effect(
        Wait {
            label: "task".into(),
        },
        vec![cb],
        0,
    )?;
    cx.__spawn(first)
}

fn cancel(cx: &mut CallCx<'_>) -> JsResult<HostStep> {
    let handle = cx.arg(0)?;
    cx.cancel_task(&handle)?;
    Ok(HostStep::Return(JsValue::undefined()))
}

fn fire(cx: &mut CallCx<'_>, outcome: GuestOutcome) -> JsResult<HostStep> {
    cx.__outcome(outcome)?;
    let cb = cx.arg(0)?;
    cx.__linked_call(cb, JsValue::undefined(), Vec::new(), Vec::new(), 1)
}

fn finish(cx: &mut CallCx<'_>, outcome: GuestOutcome) -> JsResult<HostStep> {
    cx.__outcome(outcome)?;
    Ok(HostStep::Return(JsValue::undefined()))
}

#[test]
fn linked_task_survives_restore_and_can_be_cancelled_while_ready() {
    let mut host = TaskHost::new();
    let build = compile(
        r#"
        import { spawn, cancel } from 'task';
        export let result = 0;
        let handle;
        export function run() {
            const box = { n: 42 };
            handle = spawn(() => { result = box.n; });
            return handle;
        }
        export function clear() { cancel(handle); }
    "#,
        &CompileOptions {
            imports: [("task".into(), ["spawn".into(), "cancel".into()].into())].into(),
        },
    )
    .unwrap();
    let mut vm = Vm::new(build.clone(), &mut host).unwrap();
    let Value::Number(handle) = vm.start(&mut host, "run", vec![]).unwrap() else {
        panic!("expected numeric handle");
    };
    let old = host.labels.token("wait:task").unwrap();
    vm.collect_garbage(&[]).unwrap();
    let snapshot = vm.snapshot(&[]).unwrap();
    let [task] = snapshot.host_tasks.as_slice() else {
        panic!("expected one waiting task");
    };
    assert_eq!(task.id as f64, handle);
    assert_eq!(task.saved.len(), 1);
    assert_eq!(
        snapshot.operations[0].target,
        yield_vm::snapshot::DeliveryTarget::Task(task.id)
    );
    drop(vm);
    assert_eq!(host.labels.token("wait:task"), None);

    for corrupt in [
        (
            "non-external stage",
            (|image: &mut yield_vm::Snapshot| image.host_tasks[0].stage = 1)
                as fn(&mut yield_vm::Snapshot),
        ),
        ("missing slot", |image| image.host_tasks[0].saved.clear()),
        ("missing operation", |image| {
            image.host_tasks[0].waiting_on = 0
        }),
        ("invalid high-water mark", |image| {
            image.next_host_task_id = image.host_tasks[0].id
        }),
    ] {
        let (label, mutate) = corrupt;
        let mut image = snapshot.clone();
        mutate(&mut image);
        assert!(
            matches!(
                Vm::restore(image, build.clone(), &mut host),
                Err(VmError::InvalidSnapshot(_))
            ),
            "accepted {label}"
        );
        assert!(host.labels.pending_labels().is_empty());
    }

    for cancel in [true, false] {
        let mut vm = Vm::restore(snapshot.clone(), build.clone(), &mut host).unwrap();
        assert_eq!(host.labels.token("wait:task"), None);
        vm.start_recovered_operations().unwrap();
        let token = host.labels.token("wait:task").unwrap();
        assert!(matches!(
            vm.complete_operation(old, Completion::Fulfilled(JsValue::undefined())),
            Err(VmError::UnknownOperation(_))
        ));
        vm.complete_operation(token, Completion::Fulfilled(JsValue::undefined()))
            .unwrap();
        assert_eq!(vm.get_export("result").unwrap(), Value::Number(0.0));
        if cancel {
            vm.start(&mut host, "clear", vec![]).unwrap();
        }
        vm.run_until_idle(&mut host).unwrap();
        assert_eq!(
            vm.get_export("result").unwrap(),
            Value::Number(if cancel { 0.0 } else { 42.0 })
        );
        assert!(vm.snapshot(&[]).unwrap().host_tasks.is_empty());
    }
}

#[yield_vm::host_module(name = "branch")]
mod branch {
    use super::*;

    #[function(name = "choose")]
    async fn choose(cx: &mut CallCx<'_>, cb: JsValue, mode: JsValue) -> JsResult<HostStep> {
        let result = if cx.number(&mode)? == 1.0 {
            cx.call(cb.clone(), JsValue::undefined(), vec![JsValue::number(7.0)])
                .await?
        } else {
            return Ok(HostStep::Return(JsValue::number(99.0)));
        };
        let shadowed = match Some((result.clone(), JsValue::number(3.0))) {
            Some((_, result)) => {
                cx.gc().await?;
                result
            }
            None => JsValue::undefined(),
        };
        Ok(HostStep::Return(JsValue::number(
            cx.number(&result)? + cx.number(&shadowed)?,
        )))
    }

    #[function(name = "spawn")]
    fn spawn(cx: &mut CallCx<'_>, cb: JsValue, mode: JsValue) -> JsResult<JsValue> {
        cx.spawn(async move {
            hit(cx, "prefix");
            let selected = match cx.number(&mode)? {
                1.0 => {
                    hit(cx, "one");
                    cx.effect(Wait {
                        label: "one".into(),
                    })
                    .await?
                }
                _ => {
                    hit(cx, "other");
                    cx.effect(Wait {
                        label: "other".into(),
                    })
                    .await?
                }
            };
            cx.call(cb.clone(), JsValue::undefined(), Vec::from([selected]))
                .await?;
            cx.effect(Wait {
                label: "again".into(),
            })
            .await?;
            cx.call(cb, JsValue::undefined(), Vec::from([JsValue::number(20.0)]))
                .await?;
            hit(cx, "tail");
            Ok(())
        })
    }

    fn hit(cx: &CallCx<'_>, name: &str) {
        cx.service::<Arc<yield_poc::Counters>>("counters")
            .unwrap()
            .hit(name);
    }

    #[function(name = "cancel")]
    fn cancel(cx: &mut CallCx<'_>, handle: JsValue) -> JsResult<()> {
        cx.cancel_task(&handle)
    }

    #[function(name = "early")]
    fn early(cx: &mut CallCx<'_>, mode: JsValue) -> JsResult<JsValue> {
        if cx.number(&mode)? == 2.0 {
            return Ok(JsValue::number(-1.0));
        }
        cx.spawn(async move {
            if cx.number(&mode)? == 1.0 {
                return Err(cx.throw_string("prefix failed"));
            }
            return Ok(());
        })
    }
}

#[test]
fn structured_cps_resumes_selected_arm_and_keeps_the_task_id() {
    let labels = Arc::new(LabelService::default());
    let counters = Arc::new(yield_poc::Counters::default());
    let bindings = yield_vm::host_bindings! {
        modules: [branch], classes: [], states: [], operations: [Wait],
        services: [("labels", 1, Arc::clone(&labels)), ("counters", 1, Arc::clone(&counters))],
    }
    .unwrap();
    let mut host = TaskHost {
        bindings: Arc::new(bindings),
        labels,
    };
    let build = compile(
        r#"
        import { choose, spawn, cancel, early } from 'branch';
        export let result = 0;
        let handle;
        function add(n) { result += n; return result; }
        export function sync(mode) { return choose(add, mode); }
        export function run() { handle = spawn(add, 1); return handle; }
        export function immediate(mode) { return early(mode); }
        export function clear() { cancel(handle); }
    "#,
        &CompileOptions {
            imports: [(
                "branch".into(),
                [
                    "choose".into(),
                    "spawn".into(),
                    "cancel".into(),
                    "early".into(),
                ]
                .into(),
            )]
            .into(),
        },
    )
    .unwrap();
    let mut vm = Vm::new(build.clone(), &mut host).unwrap();
    assert_eq!(
        vm.start(&mut host, "immediate", vec![JsValue::number(2.0)])
            .unwrap(),
        Value::Number(-1.0)
    );
    assert!(
        matches!(vm.start(&mut host, "immediate", vec![JsValue::number(1.0)]), Err(VmError::Thrown(Value::String(message))) if message == "prefix failed")
    );
    assert!(
        matches!(vm.start(&mut host, "immediate", vec![JsValue::number(0.0)]), Ok(Value::Number(id)) if id > 0.0)
    );
    assert!(vm.snapshot(&[]).unwrap().host_tasks.is_empty());
    assert_eq!(
        vm.start(&mut host, "sync", vec![JsValue::number(0.0)])
            .unwrap(),
        Value::Number(99.0)
    );
    assert_eq!(
        vm.start(&mut host, "sync", vec![JsValue::number(1.0)])
            .unwrap(),
        Value::Number(10.0)
    );
    let handle = vm.start(&mut host, "run", vec![]).unwrap();
    assert_eq!(host.labels.pending_labels(), ["wait:one"]);
    let snapshot = super::support::json_round_trip(&vm.snapshot(&[]).unwrap());
    assert_eq!(snapshot.host_tasks[0].saved.len(), 1);
    vm = Vm::restore(snapshot, build.clone(), &mut host).unwrap();
    vm.start_recovered_operations().unwrap();
    let first = host.labels.token("wait:one").unwrap();
    vm.complete_operation(first, Completion::Fulfilled(JsValue::number(5.0)))
        .unwrap();
    vm.run_until_idle(&mut host).unwrap();
    assert_eq!(vm.get_export("result").unwrap(), Value::Number(12.0));
    assert_eq!(host.labels.pending_labels(), ["wait:again"]);
    assert!(matches!(
        vm.complete_operation(first, Completion::Fulfilled(JsValue::undefined())),
        Err(VmError::UnknownOperation(_))
    ));
    let snapshot = super::support::json_round_trip(&vm.snapshot(&[]).unwrap());
    assert_eq!(Value::Number(snapshot.host_tasks[0].id as f64), handle);
    for cancel in [true, false] {
        let mut vm = Vm::restore(snapshot.clone(), build.clone(), &mut host).unwrap();
        vm.start_recovered_operations().unwrap();
        let token = host.labels.token("wait:again").unwrap();
        if cancel {
            vm.start(&mut host, "clear", vec![]).unwrap();
            assert_eq!(host.labels.token("wait:again"), None);
            assert!(matches!(
                vm.complete_operation(token, Completion::Fulfilled(JsValue::undefined())),
                Err(VmError::UnknownOperation(_))
            ));
        } else {
            vm.complete_operation(token, Completion::Fulfilled(JsValue::undefined()))
                .unwrap();
        }
        vm.run_until_idle(&mut host).unwrap();
        assert_eq!(
            vm.get_export("result").unwrap(),
            Value::Number(if cancel { 12.0 } else { 32.0 })
        );
    }
    assert_eq!(counters.count("prefix"), 1);
    assert_eq!(counters.count("one"), 1);
    assert_eq!(counters.count("other"), 0);
    assert_eq!(counters.count("tail"), 1);
}
