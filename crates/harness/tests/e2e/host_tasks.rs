//! Host task ABI and macro-lowered continuations exercised across VM restore.

use std::sync::Arc;

use super::support::{compile_main, declared};
use yield_harness::{LabelService, Wait};
use yield_vm::binding::__macro_support as abi;
use yield_vm::binding::{ArgKind, CallCx, CallSignature, CpsBoundary, CpsStage, ReturnKind};
use yield_vm::{
    Bindings, Completion, GuestOutcome, Host, HostStep, JsResult, JsValue, Value, Vm, VmError,
};

struct TaskHost {
    bindings: Arc<Bindings>,
    labels: Arc<LabelService>,
    errors: Option<Vec<JsValue>>,
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
            errors: None,
        }
    }
}

impl Host for TaskHost {
    fn bindings(&self) -> Arc<Bindings> {
        Arc::clone(&self.bindings)
    }

    fn now_ms(&self) -> i64 {
        0
    }

    fn time_zone(&self) -> &str {
        "Etc/UTC"
    }

    fn report_error(&mut self, error: JsValue) {
        match &mut self.errors {
            Some(errors) => errors.push(error),
            None => panic!("unexpected error: {error}"),
        }
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
    let build = compile_main(
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
        &declared(include_str!("../dts/task.d.ts")),
    )
    .unwrap();
    let mut vm = Vm::new(build.clone(), &mut host).unwrap();
    let result = vm.start(&mut host, "run", vec![]).unwrap();
    let yield_vm::ValueRef::Number(handle) = result.view() else {
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
                Vm::restore_validated(image, build.clone(), &mut host),
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
        assert_eq!(vm.get_export("result").unwrap(), Value::number(0.0));
        if cancel {
            vm.start(&mut host, "clear", vec![]).unwrap();
        }
        vm.run_until_idle(&mut host).unwrap();
        assert_eq!(
            vm.get_export("result").unwrap(),
            Value::number(if cancel { 0.0 } else { 42.0 })
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

    #[function(name = "repeat")]
    fn repeat(cx: &mut CallCx<'_>, cb: JsValue) -> JsResult<JsValue> {
        cx.spawn(async move {
            hit(cx, "repeat-prefix");
            let mut previous = JsValue::undefined();
            loop {
                let next = cx
                    .effect(Wait {
                        label: "repeat".into(),
                    })
                    .await?;
                let seen = std::mem::replace(&mut previous, next);
                if !seen.is_undefined() {
                    cx.call(cb.clone(), JsValue::undefined(), Vec::from([seen]))
                        .await?;
                }
                if previous.as_string().is_some_and(|text| text == "stop") {
                    break;
                }
            }
            hit(cx, "repeat-exit");
            Ok(())
        })
    }

    #[function(name = "countWhile")]
    async fn count_while(cx: &mut CallCx<'_>, limit: JsValue) -> JsResult<HostStep> {
        let mut count = JsValue::number(0.0);
        while {
            hit(cx, "while-check");
            cx.number(&count)? < cx.number(&limit)?
        } {
            cx.gc().await?;
            count = JsValue::number(cx.number(&count)? + 1.0);
        }
        Ok(HostStep::Return(count))
    }

    #[function(name = "countWhileLet")]
    async fn count_while_let(cx: &mut CallCx<'_>) -> JsResult<HostStep> {
        let mut value = JsValue::number(0.0);
        let mut sum = JsValue::number(0.0);
        while let Some(value) = next_match(cx, &mut value)? {
            cx.gc().await?;
            sum = JsValue::number(cx.number(&sum)? + cx.number(&value)?);
        }
        Ok(HostStep::Return(JsValue::number(
            cx.number(&sum)? + cx.number(&value)?,
        )))
    }

    fn next_match(cx: &mut CallCx<'_>, count: &mut JsValue) -> JsResult<Option<JsValue>> {
        hit(cx, "while-let-check");
        let next = cx.number(count)? + 1.0;
        *count = JsValue::number(next);
        Ok((next <= 2.0).then(|| JsValue::number(next * 10.0)))
    }

    #[function(name = "labeled")]
    async fn labeled(cx: &mut CallCx<'_>) -> JsResult<HostStep> {
        let mut round = JsValue::number(0.0);
        let result = 'outer: loop {
            round = JsValue::number(cx.number(&round)? + 1.0);
            'inner: loop {
                if cx.number(&round)? == 1.0 {
                    continue 'outer;
                }
                cx.gc().await?;
                if cx.number(&round)? == 2.0 {
                    break 'outer round;
                }
                break 'inner;
            }
            hit(cx, "after-inner");
        };
        Ok(HostStep::Return(result))
    }

    #[function(name = "nativeFor")]
    async fn native_for(cx: &mut CallCx<'_>) -> JsResult<HostStep> {
        let result = 'outer: loop {
            cx.gc().await?;
            for index in 0..3 {
                if index == 0 {
                    continue;
                }
                hit(cx, "for-break-value");
                break 'outer JsValue::number(f64::from(index));
            }
            hit(cx, "after-for");
        };
        Ok(HostStep::Return(result))
    }

    #[function(name = "selectorJump")]
    async fn selector_jump(cx: &mut CallCx<'_>) -> JsResult<HostStep> {
        let mut count = JsValue::number(0.0);
        'outer: while {
            count = JsValue::number(cx.number(&count)? + 1.0);
            if cx.number(&count)? == 1.0 {
                continue 'outer;
            }
            cx.number(&count)? <= 3.0
        } {
            match cx.number(&count)? {
                n if {
                    if n == 3.0 {
                        break 'outer;
                    }
                    true
                } =>
                {
                    cx.gc().await?
                }
                _ => JsValue::undefined(),
            };
            hit(cx, "after-guard");
        }
        Ok(HostStep::Return(count))
    }

    fn hit(cx: &CallCx<'_>, name: &str) {
        cx.service::<Arc<yield_harness::Counters>>("counters")
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
    let counters = Arc::new(yield_harness::Counters::default());
    let bindings = yield_vm::host_bindings! {
        dts: "tests/dts/branch.d.ts",
        modules: [branch], classes: [], states: [], operations: [Wait],
        services: [("labels", 1, Arc::clone(&labels)), ("counters", 1, Arc::clone(&counters))],
    }
    .unwrap();
    let mut host = TaskHost {
        bindings: Arc::new(bindings),
        labels,
        errors: None,
    };
    let build = compile_main(
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
        &declared(include_str!("../dts/branch.d.ts")),
    )
    .unwrap();
    let mut vm = Vm::new(build.clone(), &mut host).unwrap();
    assert_eq!(
        vm.start(&mut host, "immediate", vec![JsValue::number(2.0)])
            .unwrap(),
        Value::number(-1.0)
    );
    let failed = vm.start(&mut host, "immediate", vec![JsValue::number(1.0)]);
    assert!(matches!(
        failed,
        Err(VmError::Thrown(value)) if value == Value::string("prefix failed".into())
    ));
    let handle = vm
        .start(&mut host, "immediate", vec![JsValue::number(0.0)])
        .unwrap();
    assert!(matches!(handle.view(), yield_vm::ValueRef::Number(id) if id > 0.0));
    assert!(vm.snapshot(&[]).unwrap().host_tasks.is_empty());
    assert_eq!(
        vm.start(&mut host, "sync", vec![JsValue::number(0.0)])
            .unwrap(),
        Value::number(99.0)
    );
    assert_eq!(
        vm.start(&mut host, "sync", vec![JsValue::number(1.0)])
            .unwrap(),
        Value::number(10.0)
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
    assert_eq!(vm.get_export("result").unwrap(), Value::number(12.0));
    assert_eq!(host.labels.pending_labels(), ["wait:again"]);
    assert!(matches!(
        vm.complete_operation(first, Completion::Fulfilled(JsValue::undefined())),
        Err(VmError::UnknownOperation(_))
    ));
    let snapshot = super::support::json_round_trip(&vm.snapshot(&[]).unwrap());
    assert_eq!(Value::number(snapshot.host_tasks[0].id as f64), handle);
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
            Value::number(if cancel { 12.0 } else { 32.0 })
        );
    }
    assert_eq!(counters.count("prefix"), 1);
    assert_eq!(counters.count("one"), 1);
    assert_eq!(counters.count("other"), 0);
    assert_eq!(counters.count("tail"), 1);
}

fn branch_host() -> (TaskHost, Arc<yield_harness::Counters>) {
    let labels = Arc::new(LabelService::default());
    let counters = Arc::new(yield_harness::Counters::default());
    let bindings = yield_vm::host_bindings! {
        dts: "tests/dts/branch.d.ts",
        modules: [branch], classes: [], states: [], operations: [Wait],
        services: [("labels", 1, Arc::clone(&labels)), ("counters", 1, Arc::clone(&counters))],
    }
    .unwrap();
    (
        TaskHost {
            bindings: Arc::new(bindings),
            labels,
            errors: None,
        },
        counters,
    )
}

fn branch_build(source: &str, imports: &[&str]) -> yield_vm::Build {
    let mut options = declared(include_str!("../dts/branch.d.ts"));
    options
        .imports
        .get_mut("branch")
        .expect("branch declarations")
        .retain(|name| imports.contains(&name.as_str()));
    compile_main(source, &options).unwrap()
}

#[test]
fn loop_reuses_wait_stage_and_restores_previous_iteration_value() {
    let (mut host, counters) = branch_host();
    let build = branch_build(
        r#"
        import { repeat } from 'branch';
        export let observed = '';
        function record(value) { observed += value + ';'; }
        export function run() { return repeat(record); }
    "#,
        &["repeat"],
    );
    let mut vm = Vm::new(build.clone(), &mut host).unwrap();
    let handle = vm.start(&mut host, "run", vec![]).unwrap();
    let first = host.labels.token("wait:repeat").unwrap();
    let first_snapshot = vm.snapshot(&[]).unwrap();
    let [first_task] = first_snapshot.host_tasks.as_slice() else {
        panic!("expected one task")
    };
    assert_eq!(Value::number(first_task.id as f64), handle);
    vm.complete_operation(first, Completion::Fulfilled(JsValue::string("first")))
        .unwrap();
    vm.run_until_idle(&mut host).unwrap();
    assert_eq!(vm.get_export("observed").unwrap(), Value::string("".into()));
    let second = host.labels.token("wait:repeat").unwrap();
    let second_snapshot = super::support::json_round_trip(&vm.snapshot(&[]).unwrap());
    let [second_task] = second_snapshot.host_tasks.as_slice() else {
        panic!("expected one task")
    };
    assert_eq!(
        (second_task.id, second_task.stage),
        (first_task.id, first_task.stage)
    );
    assert_ne!(second_task.waiting_on, first_task.waiting_on);
    assert!(matches!(
        vm.complete_operation(first, Completion::Fulfilled(JsValue::undefined())),
        Err(VmError::UnknownOperation(_))
    ));

    vm = Vm::restore(second_snapshot, build, &mut host).unwrap();
    vm.start_recovered_operations().unwrap();
    let recovered = host.labels.token("wait:repeat").unwrap();
    assert!(matches!(
        vm.complete_operation(second, Completion::Fulfilled(JsValue::undefined())),
        Err(VmError::UnknownOperation(_))
    ));
    vm.complete_operation(recovered, Completion::Fulfilled(JsValue::string("second")))
        .unwrap();
    vm.run_until_idle(&mut host).unwrap();
    assert_eq!(
        vm.get_export("observed").unwrap(),
        Value::string("first;".into())
    );
    let third = host.labels.token("wait:repeat").unwrap();
    vm.complete_operation(third, Completion::Fulfilled(JsValue::string("stop")))
        .unwrap();
    vm.run_until_idle(&mut host).unwrap();
    assert_eq!(
        vm.get_export("observed").unwrap(),
        Value::string("first;second;".into())
    );
    assert!(vm.snapshot(&[]).unwrap().host_tasks.is_empty());
    assert!(host.labels.pending_labels().is_empty());
    assert_eq!(counters.count("repeat-prefix"), 1);
    assert_eq!(counters.count("repeat-exit"), 1);

    host.errors = Some(Vec::new());
    vm.start(&mut host, "run", vec![]).unwrap();
    let token = host.labels.token("wait:repeat").unwrap();
    vm.complete_operation(token, Completion::Rejected(JsValue::string("rejected")))
        .unwrap();
    vm.run_until_idle(&mut host).unwrap();
    let errors = host.errors.unwrap();
    let [error] = errors.as_slice() else {
        panic!("expected one rejection")
    };
    assert!(error.as_string().is_some_and(|text| text == "rejected"));
    assert!(host.labels.pending_labels().is_empty());
    assert!(vm.snapshot(&[]).unwrap().host_tasks.is_empty());
    assert_eq!(counters.count("repeat-exit"), 1);
}

#[test]
fn while_evaluates_once_per_test_and_while_let_preserves_both_bindings() {
    let (mut host, counters) = branch_host();
    let build = branch_build(
        r#"
        import { countWhile, countWhileLet } from 'branch';
        export function run(n) { return countWhile(n); }
        export function runLet() { return countWhileLet(); }
    "#,
        &["countWhile", "countWhileLet"],
    );
    let mut vm = Vm::new(build, &mut host).unwrap();
    assert_eq!(
        vm.start(&mut host, "run", vec![JsValue::number(0.0)])
            .unwrap(),
        Value::number(0.0)
    );
    assert_eq!(counters.count("while-check"), 1);
    assert_eq!(
        vm.start(&mut host, "run", vec![JsValue::number(5.0)])
            .unwrap(),
        Value::number(5.0)
    );
    assert_eq!(counters.count("while-check") - 1, 6);
    assert_eq!(
        vm.start(&mut host, "runLet", vec![]).unwrap(),
        Value::number(33.0)
    );
    assert_eq!(counters.count("while-let-check"), 3);
}

#[test]
fn labeled_jumps_skip_the_tail_and_native_for_can_exit_the_graph_loop() {
    let (mut host, counters) = branch_host();
    let build = branch_build(
        r#"
        import { labeled, nativeFor, selectorJump, countWhile } from 'branch';
        export function run() { return labeled(); }
        export function fromFor() { return nativeFor(); }
        export function selector() { return selectorJump(); }
        export function many() { return countWhile(2048); }
    "#,
        &["labeled", "nativeFor", "selectorJump", "countWhile"],
    );
    let mut vm = Vm::new(build, &mut host).unwrap();
    assert_eq!(
        vm.start(&mut host, "run", vec![]).unwrap(),
        Value::number(2.0)
    );
    assert_eq!(
        vm.start(&mut host, "fromFor", vec![]).unwrap(),
        Value::number(1.0)
    );
    assert_eq!(counters.count("for-break-value"), 1);
    assert_eq!(counters.count("after-for"), 0);
    assert_eq!(counters.count("after-inner"), 0);
    assert_eq!(
        vm.start(&mut host, "selector", vec![]).unwrap(),
        Value::number(3.0)
    );
    assert_eq!(counters.count("after-guard"), 1);
    assert_eq!(
        vm.start(&mut host, "many", vec![]).unwrap(),
        Value::number(2048.0)
    );
    assert_eq!(counters.count("while-check"), 2049);
}
