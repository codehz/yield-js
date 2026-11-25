//! Host task ABI and macro-lowered continuations exercised across VM restore.

use std::cell::RefCell;
use std::rc::Rc;

use super::support::{compile_main, declared};
use yield_harness::{Counters, LabelService, Wait};
use yield_vm::__macro_support as abi;
use yield_vm::binding::{ArgKind, CallCx, CallSignature, CpsBoundary, CpsStage, ReturnKind};
use yield_vm::{
    Completion, Diagnostic, Diagnostics, GuestOutcome, Host, HostStep, JsResult, JsValue,
    RestoreOptions, SnapshotValidation, Value, Vm, VmError,
};

#[derive(Default)]
struct TaskDiagnostics(RefCell<Option<Vec<JsValue>>>);

impl Diagnostics for TaskDiagnostics {
    fn report(&self, event: Diagnostic<'_>) {
        if let Diagnostic::Uncaught { error, .. } = event {
            match &mut *self.0.borrow_mut() {
                Some(errors) => errors.push(error),
                None => panic!("unexpected error: {error}"),
            }
        }
    }
}

fn task_host(labels: &Rc<LabelService>) -> Host {
    let module = abi::raw_module(
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
    struct TaskServices {
        labels: Rc<LabelService>,
        diagnostics: Rc<dyn Diagnostics>,
    }
    let layout = TaskServices {
        labels: Rc::clone(labels),
        diagnostics: Rc::new(TaskDiagnostics::default()),
    };
    // SAFETY: These descriptors use the exact types and offsets of the layout's fields.
    let storage = unsafe {
        abi::ServiceStorage::from_layout(
            layout,
            [
                abi::ServiceField::of::<Rc<LabelService>>(std::mem::offset_of!(
                    TaskServices,
                    labels
                )),
                abi::ServiceField::of::<Rc<dyn Diagnostics>>(std::mem::offset_of!(
                    TaskServices,
                    diagnostics
                )),
            ],
        )
    }
    .unwrap();
    let mut deps = abi::ModuleDependencies::default();
    deps.operation::<Wait>();
    abi::link([module], deps, storage).unwrap()
}

static STAGES: [CpsStage; 2] = [
    CpsStage {
        resume: abi::ResumeCall::Plain(fire),
        kind: CpsBoundary::External,
        slots: &[0],
        receiver: None,
    },
    CpsStage {
        resume: abi::ResumeCall::Plain(finish),
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
    let labels = Rc::new(LabelService::default());
    let make_host = || task_host(&labels);
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
    let mut vm = Vm::new(build.clone(), make_host(), Default::default()).unwrap();
    let result = vm.start("run", vec![]).unwrap();
    let yield_vm::ValueRef::Number(handle) = result.view() else {
        panic!("expected numeric handle");
    };
    let old = labels.token("wait:task").unwrap();
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
    assert_eq!(labels.token("wait:task"), None);

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
                Vm::restore(
                    image,
                    build.clone(),
                    make_host(),
                    RestoreOptions {
                        validation: SnapshotValidation::Full,
                        ..Default::default()
                    },
                ),
                Err(VmError::InvalidSnapshot(_))
            ),
            "accepted {label}"
        );
        assert!(labels.pending_labels().is_empty());
    }

    for cancel in [true, false] {
        let mut vm = Vm::restore(
            snapshot.clone(),
            build.clone(),
            make_host(),
            Default::default(),
        )
        .unwrap();
        assert_eq!(labels.token("wait:task"), None);
        vm.start_recovered_operations().unwrap();
        let token = labels.token("wait:task").unwrap();
        assert!(matches!(
            vm.complete_operation(old, Completion::Fulfilled(JsValue::undefined())),
            Err(VmError::UnknownOperation(_))
        ));
        vm.complete_operation(token, Completion::Fulfilled(JsValue::undefined()))
            .unwrap();
        assert_eq!(vm.get_export("result").unwrap(), Value::number(0.0));
        if cancel {
            vm.start("clear", vec![]).unwrap();
        }
        vm.run_until_idle().unwrap();
        assert_eq!(
            vm.get_export("result").unwrap(),
            Value::number(if cancel { 0.0 } else { 42.0 })
        );
        assert!(vm.snapshot(&[]).unwrap().host_tasks.is_empty());
    }
}

yield_vm::service_scope! {
    BranchServices { counters: Rc<Counters> }
}

#[yield_vm::host_module(
    name = "branch",
    operations = [Wait],
    services = BranchServices,
)]
#[derive(Default, yield_vm::Encode, yield_vm::Decode)]
struct Branch {
    count: u8,
}

#[yield_vm::host_module(name = "mirror")]
struct Mirror {
    #[js_slot]
    count: JsValue,
}

#[yield_vm::module_exports]
impl Mirror {
    #[function(name = "choose")]
    fn choose(cx: &mut yield_vm::ModuleCx<'_, Self>) -> JsResult<JsValue> {
        let count = cx.state_slot(Self::SLOT_COUNT)?.as_number().unwrap_or(0.0);
        let next = JsValue::number(count + 100.0);
        cx.set_state_slot(Self::SLOT_COUNT, next.clone())?;
        Ok(next)
    }
}

#[yield_vm::module_exports]
impl Branch {
    fn increment(cx: &mut yield_vm::ModuleCx<'_, Self>) {
        cx.state().count += 1;
    }

    #[function(name = "choose")]
    async fn choose(
        cx: &mut yield_vm::ModuleCx<'_, Self>,
        cb: JsValue,
        mode: JsValue,
    ) -> JsResult<HostStep> {
        let result = if cx.number(&mode)? == 1.0 {
            Self::increment(cx);
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
            cx.number(&result)? + cx.number(&shadowed)? + f64::from(cx.state().count),
        )))
    }

    #[function(name = "spawn")]
    fn spawn(
        cx: &mut yield_vm::ModuleCx<'_, Self>,
        cb: JsValue,
        mode: JsValue,
    ) -> JsResult<JsValue> {
        Self::increment(cx);
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
            let count = JsValue::number(f64::from(cx.state().count));
            cx.call(cb, JsValue::undefined(), Vec::from([count]))
                .await?;
            hit(cx, "tail");
            Ok(())
        })
    }

    #[function(name = "repeat")]
    fn repeat(cx: &mut yield_vm::ModuleCx<'_, Self>, cb: JsValue) -> JsResult<JsValue> {
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
    async fn count_while(
        cx: &mut yield_vm::ModuleCx<'_, Self>,
        limit: JsValue,
    ) -> JsResult<HostStep> {
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
    async fn count_while_let(cx: &mut yield_vm::ModuleCx<'_, Self>) -> JsResult<HostStep> {
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

    #[function(name = "labeled")]
    async fn labeled(cx: &mut yield_vm::ModuleCx<'_, Self>) -> JsResult<HostStep> {
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
    async fn native_for(cx: &mut yield_vm::ModuleCx<'_, Self>) -> JsResult<HostStep> {
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
    async fn selector_jump(cx: &mut yield_vm::ModuleCx<'_, Self>) -> JsResult<HostStep> {
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

    #[function(name = "cancel")]
    fn cancel(cx: &mut yield_vm::ModuleCx<'_, Self>, handle: JsValue) -> JsResult<()> {
        cx.cancel_task(&handle)
    }

    #[function(name = "early")]
    fn early(cx: &mut yield_vm::ModuleCx<'_, Self>, mode: JsValue) -> JsResult<JsValue> {
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

fn next_match(
    cx: &mut yield_vm::ModuleCx<'_, Branch>,
    count: &mut JsValue,
) -> JsResult<Option<JsValue>> {
    hit(cx, "while-let-check");
    let next = cx.number(count)? + 1.0;
    *count = JsValue::number(next);
    Ok((next <= 2.0).then(|| JsValue::number(next * 10.0)))
}

fn hit(cx: &yield_vm::ModuleCx<'_, Branch>, name: &str) {
    BranchServices::bind(cx.services()).counters().hit(name);
}

#[test]
fn structured_cps_resumes_selected_arm_and_keeps_the_task_id() {
    let (make_host, labels, counters, _) = branch_fixture();
    let build = compile_main(
        r#"
        import { choose, spawn, cancel, early } from 'branch';
        import { choose as mirrorChoose } from 'mirror';
        export function other() { return mirrorChoose(); }
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
    let mut vm = Vm::new(build.clone(), make_host(), Default::default()).unwrap();
    assert_eq!(
        vm.start("immediate", vec![JsValue::number(2.0)]).unwrap(),
        Value::number(-1.0)
    );
    let failed = vm.start("immediate", vec![JsValue::number(1.0)]);
    assert!(matches!(
        failed,
        Err(VmError::Thrown(value)) if value == Value::string("prefix failed".into())
    ));
    let handle = vm.start("immediate", vec![JsValue::number(0.0)]).unwrap();
    assert!(matches!(handle.view(), yield_vm::ValueRef::Number(id) if id > 0.0));
    assert!(vm.snapshot(&[]).unwrap().host_tasks.is_empty());
    assert_eq!(
        vm.start("sync", vec![JsValue::number(0.0)]).unwrap(),
        Value::number(99.0)
    );
    assert_eq!(vm.start("other", vec![]).unwrap(), Value::number(100.0));
    assert_eq!(
        vm.start("sync", vec![JsValue::number(1.0)]).unwrap(),
        Value::number(11.0)
    );
    let handle = vm.start("run", vec![]).unwrap();
    assert_eq!(labels.pending_labels(), ["wait:one"]);
    let snapshot = super::support::json_round_trip(&vm.snapshot(&[]).unwrap());
    assert_eq!(snapshot.host_tasks[0].saved.len(), 1);
    vm = Vm::restore(snapshot, build.clone(), make_host(), Default::default()).unwrap();
    assert_eq!(vm.start("other", vec![]).unwrap(), Value::number(200.0));
    vm.start_recovered_operations().unwrap();
    let first = labels.token("wait:one").unwrap();
    vm.complete_operation(first, Completion::Fulfilled(JsValue::number(5.0)))
        .unwrap();
    vm.run_until_idle().unwrap();
    assert_eq!(vm.get_export("result").unwrap(), Value::number(12.0));
    assert_eq!(labels.pending_labels(), ["wait:again"]);
    assert!(matches!(
        vm.complete_operation(first, Completion::Fulfilled(JsValue::undefined())),
        Err(VmError::UnknownOperation(_))
    ));
    let snapshot = super::support::json_round_trip(&vm.snapshot(&[]).unwrap());
    assert_eq!(Value::number(snapshot.host_tasks[0].id as f64), handle);
    for cancel in [true, false] {
        let mut vm = Vm::restore(
            snapshot.clone(),
            build.clone(),
            make_host(),
            Default::default(),
        )
        .unwrap();
        vm.start_recovered_operations().unwrap();
        let token = labels.token("wait:again").unwrap();
        if cancel {
            vm.start("clear", vec![]).unwrap();
            assert_eq!(labels.token("wait:again"), None);
            assert!(matches!(
                vm.complete_operation(token, Completion::Fulfilled(JsValue::undefined())),
                Err(VmError::UnknownOperation(_))
            ));
        } else {
            vm.complete_operation(token, Completion::Fulfilled(JsValue::undefined()))
                .unwrap();
        }
        vm.run_until_idle().unwrap();
        assert_eq!(
            vm.get_export("result").unwrap(),
            Value::number(if cancel { 12.0 } else { 14.0 })
        );
    }
    assert_eq!(counters.count("prefix"), 1);
    assert_eq!(counters.count("one"), 1);
    assert_eq!(counters.count("other"), 0);
    assert_eq!(counters.count("tail"), 1);
}

fn branch_fixture() -> (
    impl Fn() -> Host,
    Rc<LabelService>,
    Rc<yield_harness::Counters>,
    Rc<TaskDiagnostics>,
) {
    let labels = Rc::new(LabelService::default());
    let counters = Rc::new(yield_harness::Counters::default());
    let diagnostics = Rc::new(TaskDiagnostics::default());
    let make_host = {
        let (labels, counters, diagnostics) =
            (labels.clone(), counters.clone(), diagnostics.clone());
        move || {
            yield_vm::host! {
                dts: "../dts/branch.d.ts",
                modules: [Branch, Mirror],
                services: {
                    Rc<LabelService> => labels.clone(),
                    Rc<Counters> => counters.clone(),
                    Rc<dyn Diagnostics> => diagnostics.clone(),
                },
            }
            .unwrap()
        }
    };
    (make_host, labels, counters, diagnostics)
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
    let (make_host, labels, counters, diagnostics) = branch_fixture();
    let build = branch_build(
        r#"
        import { repeat } from 'branch';
        export let observed = '';
        function record(value) { observed += value + ';'; }
        export function run() { return repeat(record); }
    "#,
        &["repeat"],
    );
    let mut vm = Vm::new(build.clone(), make_host(), Default::default()).unwrap();
    let handle = vm.start("run", vec![]).unwrap();
    let first = labels.token("wait:repeat").unwrap();
    let first_snapshot = vm.snapshot(&[]).unwrap();
    let [first_task] = first_snapshot.host_tasks.as_slice() else {
        panic!("expected one task")
    };
    assert_eq!(Value::number(first_task.id as f64), handle);
    vm.complete_operation(first, Completion::Fulfilled(JsValue::string("first")))
        .unwrap();
    vm.run_until_idle().unwrap();
    assert_eq!(vm.get_export("observed").unwrap(), Value::string("".into()));
    let second = labels.token("wait:repeat").unwrap();
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

    vm = Vm::restore(second_snapshot, build, make_host(), Default::default()).unwrap();
    vm.start_recovered_operations().unwrap();
    let recovered = labels.token("wait:repeat").unwrap();
    assert!(matches!(
        vm.complete_operation(second, Completion::Fulfilled(JsValue::undefined())),
        Err(VmError::UnknownOperation(_))
    ));
    vm.complete_operation(recovered, Completion::Fulfilled(JsValue::string("second")))
        .unwrap();
    vm.run_until_idle().unwrap();
    assert_eq!(
        vm.get_export("observed").unwrap(),
        Value::string("first;".into())
    );
    let third = labels.token("wait:repeat").unwrap();
    vm.complete_operation(third, Completion::Fulfilled(JsValue::string("stop")))
        .unwrap();
    vm.run_until_idle().unwrap();
    assert_eq!(
        vm.get_export("observed").unwrap(),
        Value::string("first;second;".into())
    );
    assert!(vm.snapshot(&[]).unwrap().host_tasks.is_empty());
    assert!(labels.pending_labels().is_empty());
    assert_eq!(counters.count("repeat-prefix"), 1);
    assert_eq!(counters.count("repeat-exit"), 1);

    *diagnostics.0.borrow_mut() = Some(Vec::new());
    vm.start("run", vec![]).unwrap();
    let token = labels.token("wait:repeat").unwrap();
    vm.complete_operation(token, Completion::Rejected(JsValue::string("rejected")))
        .unwrap();
    vm.run_until_idle().unwrap();
    let errors = diagnostics.0.borrow_mut().take().unwrap();
    let [error] = errors.as_slice() else {
        panic!("expected one rejection")
    };
    assert!(error.as_string().is_some_and(|text| text == "rejected"));
    assert!(labels.pending_labels().is_empty());
    assert!(vm.snapshot(&[]).unwrap().host_tasks.is_empty());
    assert_eq!(counters.count("repeat-exit"), 1);
}

#[test]
fn while_evaluates_once_per_test_and_while_let_preserves_both_bindings() {
    let (make_host, _, counters, _) = branch_fixture();
    let build = branch_build(
        r#"
        import { countWhile, countWhileLet } from 'branch';
        export function run(n) { return countWhile(n); }
        export function runLet() { return countWhileLet(); }
    "#,
        &["countWhile", "countWhileLet"],
    );
    let mut vm = Vm::new(build, make_host(), Default::default()).unwrap();
    assert_eq!(
        vm.start("run", vec![JsValue::number(0.0)]).unwrap(),
        Value::number(0.0)
    );
    assert_eq!(counters.count("while-check"), 1);
    assert_eq!(
        vm.start("run", vec![JsValue::number(5.0)]).unwrap(),
        Value::number(5.0)
    );
    assert_eq!(counters.count("while-check") - 1, 6);
    assert_eq!(vm.start("runLet", vec![]).unwrap(), Value::number(33.0));
    assert_eq!(counters.count("while-let-check"), 3);
}

#[test]
fn labeled_jumps_skip_the_tail_and_native_for_can_exit_the_graph_loop() {
    let (make_host, _, counters, _) = branch_fixture();
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
    let mut vm = Vm::new(build, make_host(), Default::default()).unwrap();
    assert_eq!(vm.start("run", vec![]).unwrap(), Value::number(2.0));
    assert_eq!(vm.start("fromFor", vec![]).unwrap(), Value::number(1.0));
    assert_eq!(counters.count("for-break-value"), 1);
    assert_eq!(counters.count("after-for"), 0);
    assert_eq!(counters.count("after-inner"), 0);
    assert_eq!(vm.start("selector", vec![]).unwrap(), Value::number(3.0));
    assert_eq!(counters.count("after-guard"), 1);
    assert_eq!(vm.start("many", vec![]).unwrap(), Value::number(2048.0));
    assert_eq!(counters.count("while-check"), 2049);
}
