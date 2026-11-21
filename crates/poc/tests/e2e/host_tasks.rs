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

static STAGES: [CpsStage; 3] = [
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
    CpsStage {
        resume: begin,
        kind: CpsBoundary::Spawn,
        slots: &[0],
        receiver: None,
    },
];

fn spawn(cx: &mut CallCx<'_>) -> JsResult<HostStep> {
    let cb = cx.arg(0)?;
    cx.__spawn(vec![cb], 2)
}

fn cancel(cx: &mut CallCx<'_>) -> JsResult<HostStep> {
    let handle = cx.arg(0)?;
    cx.cancel_task(&handle)?;
    Ok(HostStep::Return(JsValue::undefined()))
}

fn begin(cx: &mut CallCx<'_>, _: GuestOutcome) -> JsResult<HostStep> {
    let cb = cx.arg(0)?;
    cx.__linked_effect(
        Wait {
            label: "task".into(),
        },
        vec![cb],
        0,
    )
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
