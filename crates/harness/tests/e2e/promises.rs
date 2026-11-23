//! Promise reactions and combination state survive suspension and restore.

use super::support::{RUN, Suspended, json_round_trip};
use yield_harness::{RejectionEvent, TestHost, build};
use yield_vm::heap::PromiseCallback;
use yield_vm::snapshot::OrdinaryTask;
use yield_vm::{
    Completion, JsValue, Object, ObjectId, PromiseState, RestoreOptions, SnapshotValidation, Value,
    ValueRef, Vm, VmError,
};

#[test]
fn script_reaction_survives_restore_and_runs_on_fulfill() {
    let source = r#"
import { wait } from "test";
export async function run() {
  return await wait("a").then(function (value) {
    return value + 1;
  });
}
"#;
    let mut s = Suspended::start(source, vec![]);
    assert_eq!(s.host.pending_labels(), vec!["wait:a".to_owned()]);
    let snapshot = s.snapshot();
    assert!(
        snapshot.objects.iter().any(|record| matches!(
            &record.object,
            Object::Promise(promise) if promise.reactions.iter().any(|job| matches!(job, yield_vm::heap::ReactionJob::Script { .. }))
        )),
        "then() handler is a persisted reaction"
    );
    s.restore(snapshot).expect("restore");
    s.complete("wait:a", Completion::Fulfilled(JsValue::number(4.0)));
    assert_eq!(s.state(), PromiseState::Fulfilled(Value::number(5.0)));
}

#[test]
fn snapshot_rejects_an_unrooted_thenable_capability() {
    let source = r#"
import { wait } from "test";
export async function run() {
  Promise.resolve({
    then: function () {},
  });
  await wait("hold");
  return 1;
}
"#;
    let build = build(source);
    let mut host = TestHost::default();
    let mut vm = Vm::new(build, &mut host, Default::default()).expect("bindings");
    let result = vm.start(&mut host, "run", Vec::new()).expect("start");
    vm.run_until_idle(&mut host).expect("await");
    let error = vm.snapshot(&[result]).expect_err("unrooted resolve");
    assert!(
        matches!(error, VmError::InvalidState(message) if message.contains("thenable assimilation"))
    );
}

#[test]
fn restore_rejects_a_locked_promise_without_an_adopt_job() {
    let mut s = Suspended::start(RUN, vec![]);
    let mut snapshot = s.snapshot();
    let mut locked = false;
    for record in &mut snapshot.objects {
        if let Object::Promise(promise) = &mut record.object
            && matches!(promise.state, PromiseState::Pending)
        {
            promise.locked = true;
            locked = true;
            break;
        }
    }
    assert!(locked, "a pending Promise exists at the first await");
    let error = s
        .restore_validated(snapshot)
        .expect_err("locked without adopt");
    assert!(
        matches!(error, VmError::InvalidSnapshot(message) if message.contains("exactly one Adopt"))
    );
}

#[test]
fn restore_rejects_a_reaction_whose_handler_is_missing() {
    let source = r#"
import { wait } from "test";
export async function run() {
  return await wait("a").then(function (value) {
    return value;
  });
}
"#;
    let mut s = Suspended::start(source, vec![]);
    let mut snapshot = s.snapshot();
    let handler = snapshot
        .objects
        .iter()
        .find_map(|record| match &record.object {
            Object::Function(_) => Some(record.id),
            _ => None,
        })
        .expect("then handler");
    snapshot.objects.retain(|record| record.id != handler);
    let error = s.restore_validated(snapshot).expect_err("missing handler");
    assert!(matches!(error, VmError::InvalidSnapshot(_)));
}

const CUSTOM_ALL: &str = r#"
import { wait } from "test";
function Custom(executor) {
  return new Promise(function (resolve, reject) {
    executor(function (values) {
      resolve(values[0] + 10 * values[1]);
    }, reject);
  });
}
Custom.resolve = function (value) { return value; };
export async function run() {
  return await Promise.all.call(Custom, [wait("a"), wait("b")]);
}
"#;

#[test]
fn promise_all_combine_state_survives_a_partial_fulfill() {
    let mut s = Suspended::start(CUSTOM_ALL, vec![]);
    s.complete("wait:a", Completion::Fulfilled(JsValue::number(2.0)));
    s.vm.collect_garbage(&[]).expect("collect while suspended");
    let snapshot = s.snapshot();
    assert_eq!(snapshot.combines.len(), 1);
    let state = snapshot.combines.values().next().expect("combine state");
    assert_eq!(state.called.as_slice(), &[true, false]);
    assert_eq!(state.remaining, 1);
    s.restore(snapshot).expect("restore");
    s.complete("wait:b", Completion::Fulfilled(JsValue::number(3.0)));
    assert_eq!(s.state(), PromiseState::Fulfilled(Value::number(32.0)));
}

#[test]
fn restore_rejects_a_combine_element_index_out_of_bounds() {
    let mut s = Suspended::start(CUSTOM_ALL, vec![]);
    let mut snapshot = s.snapshot();
    let index = snapshot
        .objects
        .iter_mut()
        .find_map(|record| match &mut record.object {
            Object::PromiseCallback {
                callback: PromiseCallback::CombineElement { index, .. },
                ..
            } => Some(index),
            _ => None,
        })
        .expect("combine element callback");
    *index = 2;
    let error = s
        .restore_validated(snapshot)
        .expect_err("out-of-bounds element index");
    assert!(
        matches!(error, VmError::InvalidSnapshot(message) if message.contains("index 2 is out of bounds"))
    );
}

#[test]
fn restore_rejects_a_non_callable_combine_capability() {
    let mut s = Suspended::start(CUSTOM_ALL, vec![]);
    let mut snapshot = s.snapshot();
    let (id, state) = snapshot.combines.iter_mut().next().expect("combine state");
    state.capability = Value::object(*id);
    let error = s
        .restore_validated(snapshot)
        .expect_err("non-callable combine capability");
    assert!(
        matches!(error, VmError::InvalidSnapshot(message) if message.contains("capability is not callable"))
    );
}

fn object_id(value: &Value) -> ObjectId {
    let ValueRef::Object(id) = value.view() else {
        panic!("expected an object, got {value:?}");
    };
    id
}

fn assert_host_identity(handle: &JsValue, value: &Value) {
    // JsValue keeps the raw object ID private but displays it for diagnostics.
    assert_eq!(handle.to_string(), format!("object({})", object_id(value)));
}

#[test]
fn passthrough_reports_only_the_child_and_late_catches_notify_once() {
    let source = r#"
let child;
export let handled;
export let handledCount = 0;
onrejectionhandled = e => { handled = e.promise; handledCount++; };
export function run() {
  child = Promise.reject('pass through').then();
  return child;
}
export function catchTwice() {
  child.catch(() => {});
  child.catch(() => {});
}
"#;
    let mut host = TestHost::default();
    let mut vm = Vm::new(build(source), &mut host, Default::default()).expect("create VM");
    let child = vm.start(&mut host, "run", vec![]).expect("start");
    assert!(host.rejection_events().is_empty());
    vm.run_until_idle(&mut host).expect("first checkpoint");
    let [RejectionEvent::Unhandled { promise, reason }] = host.rejection_events() else {
        panic!(
            "expected only the child rejection: {:?}",
            host.rejection_events()
        );
    };
    assert_host_identity(promise, &child);
    assert_eq!(reason, &JsValue::string("pass through"));
    assert!(host.errors().is_empty());

    for _ in 0..2 {
        vm.run_until_idle(&mut host).expect("empty checkpoint");
        assert_eq!(host.rejection_events().len(), 1);
    }

    vm.start(&mut host, "catchTwice", vec![])
        .expect("attach catches");
    assert_eq!(host.rejection_events().len(), 1);
    vm.run_until_idle(&mut host).expect("late catch checkpoint");
    vm.run_until_idle(&mut host).expect("empty checkpoint");
    assert_eq!(host.rejection_events().len(), 1);
    assert_eq!(vm.get_export("handled").unwrap(), child);
    assert_eq!(vm.get_export("handledCount").unwrap(), Value::number(1.0));
}

#[test]
fn catch_attached_in_a_microtask_suppresses_rejection_notification() {
    let source = r#"
export function run() {
  const promise = Promise.reject('caught');
  queueMicrotask(() => { promise.catch(() => {}); });
  queueMicrotask(() => { throw 'callback error'; });
}
"#;
    let s = Suspended::start(source, vec![]);
    assert!(s.host.rejection_events().is_empty());
    assert_eq!(s.host.errors(), &[JsValue::string("callback error")]);
}

#[test]
fn rejection_queue_survives_gc_and_json_restore_without_replaying_reports() {
    let source = r#"
export function enqueue() { return Promise.reject({}); }
export function catchReported() { reported.catch(() => {}); }
let reported;
export let handled;
export let handledCount = 0;
onrejectionhandled = e => { handled = e.promise; handledCount++; };
export function run() {
  reported = Promise.reject('already reported');
  return reported;
}
"#;
    let mut host = TestHost::default();
    let mut vm = Vm::new(build(source), &mut host, Default::default()).expect("create VM");
    let reported = vm.start(&mut host, "run", vec![]).expect("start");
    vm.run_until_idle(&mut host)
        .expect("report the first rejection");
    let [RejectionEvent::Unhandled { promise, reason }] = host.rejection_events() else {
        panic!("expected the initial report: {:?}", host.rejection_events());
    };
    assert_host_identity(promise, &reported);
    assert_eq!(reason, &JsValue::string("already reported"));

    let queued = [(); 2].map(|()| {
        let promise = vm.start(&mut host, "enqueue", vec![]).expect("enqueue");
        let PromiseState::Rejected(reason) = vm.promise_state(&promise).expect("read state") else {
            panic!("expected a rejected promise");
        };
        (promise, reason)
    });
    assert_ne!(queued[0].1, queued[1].1);
    assert_eq!(host.rejection_events().len(), 1);

    // Neither queued promise nor either reason is otherwise rooted in the VM.
    vm.collect_garbage(&[])
        .expect("collect with a pending queue");
    let snapshot = json_round_trip(&vm.snapshot(&[]).expect("snapshot pending reports"));
    assert_eq!(
        snapshot.pending_rejection_notifications,
        queued.each_ref().map(|(promise, _)| object_id(promise)),
    );
    let mut restored_host = TestHost::default();
    let mut vm = Vm::restore(
        snapshot,
        vm.build().clone(),
        &mut restored_host,
        Default::default(),
    )
    .expect("restore");
    assert!(restored_host.rejection_events().is_empty());
    vm.run_until_idle(&mut restored_host)
        .expect("first restored checkpoint");
    assert_eq!(restored_host.rejection_events().len(), queued.len());
    for (event, (expected_promise, expected_reason)) in
        restored_host.rejection_events().iter().zip(&queued)
    {
        let RejectionEvent::Unhandled { promise, reason } = event;
        assert_host_identity(promise, expected_promise);
        assert_host_identity(reason, expected_reason);
    }

    vm.start(&mut restored_host, "catchReported", vec![])
        .expect("attach late catch");
    vm.run_until_idle(&mut restored_host)
        .expect("late catch checkpoint");
    assert_eq!(restored_host.rejection_events().len(), queued.len());
    assert_eq!(vm.get_export("handled").unwrap(), reported);
    assert_eq!(vm.get_export("handledCount").unwrap(), Value::number(1.0));
}

#[test]
fn restore_operation_rejection_notifies_only_at_the_next_checkpoint() {
    let source = r#"
import { rejectAfterRestore } from 'test';
export function run() { return rejectAfterRestore('a'); }
"#;
    let mut host = TestHost::default();
    let mut vm = Vm::new(build(source), &mut host, Default::default()).expect("create VM");
    let promise = vm.start(&mut host, "run", vec![]).expect("start operation");
    vm.run_until_idle(&mut host).expect("drain before snapshot");
    assert_eq!(
        vm.promise_state(&promise).expect("read state"),
        PromiseState::Pending
    );
    let snapshot = vm
        .snapshot(std::slice::from_ref(&promise))
        .expect("snapshot operation");
    let mut restored_host = TestHost::default();
    let mut vm = Vm::restore(
        snapshot,
        vm.build().clone(),
        &mut restored_host,
        Default::default(),
    )
    .expect("restore");
    vm.start_recovered_operations()
        .expect("start recovered operations");
    assert!(restored_host.rejection_events().is_empty());
    vm.run_until_idle(&mut restored_host)
        .expect("first restored checkpoint");
    let [
        RejectionEvent::Unhandled {
            promise: notified,
            reason,
        },
    ] = restored_host.rejection_events()
    else {
        panic!(
            "expected the recovery rejection: {:?}",
            restored_host.rejection_events()
        );
    };
    assert_host_identity(notified, &promise);
    assert_eq!(reason, &JsValue::string("reject rejected during restore"));
    assert!(restored_host.errors().is_empty());
}

#[test]
fn restore_rejects_duplicate_omitted_and_dangling_pending_notification_ids() {
    let mut host = TestHost::default();
    let mut vm = Vm::new(
        build("export function run() { return Promise.reject('pending report'); }"),
        &mut host,
        Default::default(),
    )
    .expect("create VM");
    let promise = vm.start(&mut host, "run", vec![]).expect("reject");
    let id = object_id(&promise);

    for corruption in ["duplicate ID", "omitted ID", "dangling ID"] {
        let mut snapshot = vm.snapshot(&[]).expect("capture queued rejection");
        assert_eq!(snapshot.pending_rejection_notifications, [id]);
        match corruption {
            "duplicate ID" => snapshot.pending_rejection_notifications.push(id),
            "omitted ID" => snapshot.pending_rejection_notifications.clear(),
            "dangling ID" => snapshot.objects.retain(|record| record.id != id),
            _ => unreachable!(),
        }
        let result = Vm::restore(
            snapshot,
            vm.build().clone(),
            &mut host,
            RestoreOptions {
                validation: SnapshotValidation::Full,
                ..Default::default()
            },
        );
        assert!(
            matches!(result, Err(VmError::InvalidSnapshot(_))),
            "accepted {corruption}"
        );
    }
}

#[test]
fn rejection_tasks_survive_fault_cut_points_without_replay() {
    let build = build(
        r#"
        let reported;
        export let handled;
        export let handledCount = 0;
        export let seen = '';
        onunhandledrejection = e => {
          seen += e.reason;
          if (e.reason === 'A') AbortSignal.timeout(1);
        };
        onrejectionhandled = e => { handled = e.promise; handledCount++; };
        export function run() { return reported = Promise.reject('R'); }
        export function batch() {
          Promise.reject('A');
          return Promise.reject('B');
        }
        export function observe() {
          reported.catch(() => {});
          reported = null;
          queueMicrotask(() => AbortSignal.timeout(1));
        }
    "#,
    );
    let mut host = TestHost::default();
    let mut vm = Vm::new(build.clone(), &mut host, Default::default()).unwrap();
    let reported = vm.start(&mut host, "run", vec![]).unwrap();
    vm.run_until_idle(&mut host).unwrap();
    assert_eq!(host.rejection_events().len(), 1);
    let image = json_round_trip(&vm.snapshot(&[]).unwrap());
    host = TestHost::default();
    vm = Vm::restore(
        image,
        build.clone(),
        &mut host,
        RestoreOptions {
            validation: SnapshotValidation::Full,
            ..Default::default()
        },
    )
    .unwrap();
    vm.run_until_idle(&mut host).unwrap();
    assert!(host.rejection_events().is_empty());

    host.clock().set_fail_start(true);
    let b = vm.start(&mut host, "batch", vec![]).unwrap();
    assert!(matches!(
        vm.run_until_idle(&mut host),
        Err(VmError::HostFault(_))
    ));
    let b_id = object_id(&b);
    vm.collect_garbage(&[]).unwrap();
    let image = vm.snapshot(&[]).unwrap();
    assert!(image.pending_rejection_notifications.is_empty());
    assert_eq!(
        image.ordinary_tasks,
        [OrdinaryTask::UnhandledBatch(vec![b_id])]
    );
    for task in [
        OrdinaryTask::UnhandledBatch(vec![b_id, b_id]),
        OrdinaryTask::UnhandledBatch(vec![]),
        OrdinaryTask::RejectionHandled(b_id),
    ] {
        let mut invalid = image.clone();
        invalid.ordinary_tasks[0] = task;
        assert!(matches!(
            Vm::restore(
                invalid,
                build.clone(),
                &mut host,
                RestoreOptions {
                    validation: SnapshotValidation::Full,
                    ..Default::default()
                },
            ),
            Err(VmError::InvalidSnapshot(_))
        ));
    }
    vm = Vm::restore(
        json_round_trip(&image),
        build.clone(),
        &mut host,
        RestoreOptions {
            validation: SnapshotValidation::Full,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(vm.get_export("seen").unwrap(), Value::string("RA".into()));
    assert!(host.rejection_events().is_empty());
    vm.run_until_idle(&mut host).unwrap();
    assert_eq!(vm.get_export("seen").unwrap(), Value::string("RAB".into()));
    let [RejectionEvent::Unhandled { promise, reason }] = host.rejection_events() else {
        panic!("expected only B's default report");
    };
    assert_host_identity(promise, &b);
    assert_eq!(*reason, JsValue::string("B"));

    vm.start(&mut host, "observe", vec![]).unwrap();
    assert!(
        vm.snapshot(&[]).is_err(),
        "a queued reaction still forbids snapshotting"
    );
    assert!(matches!(
        vm.run_until_idle(&mut host),
        Err(VmError::HostFault(_))
    ));
    vm.collect_garbage(&[]).unwrap();
    let image = vm.snapshot(&[]).unwrap();
    assert_eq!(
        image.ordinary_tasks,
        [OrdinaryTask::RejectionHandled(object_id(&reported))]
    );
    let mut host = TestHost::default();
    let mut vm = Vm::restore(
        json_round_trip(&image),
        build,
        &mut host,
        RestoreOptions {
            validation: SnapshotValidation::Full,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(vm.get_export("handledCount").unwrap(), Value::number(0.0));
    for _ in 0..2 {
        vm.run_until_idle(&mut host).unwrap();
    }
    assert_eq!(vm.get_export("handled").unwrap(), reported);
    assert_eq!(vm.get_export("handledCount").unwrap(), Value::number(1.0));
    assert!(host.rejection_events().is_empty());
    assert!(host.errors().is_empty());
}

#[test]
fn canceled_unhandled_report_still_delivers_handled_once() {
    let source = r#"
        const reason = {};
        let promise;
        export let checked = 0;
        onunhandledrejection = () => false;
        onrejectionhandled = e => {
          if (e.promise === promise && e.reason === reason && !e.cancelable) checked++;
        };
        export function run() { promise = Promise.reject(reason); }
        export function observe() { promise.catch(() => {}); promise.catch(() => {}); }
    "#;
    let mut host = TestHost::default();
    let mut vm = Vm::new(build(source), &mut host, Default::default()).unwrap();
    vm.start(&mut host, "run", vec![]).unwrap();
    vm.run_until_idle(&mut host).unwrap();
    assert!(host.rejection_events().is_empty());
    vm.start(&mut host, "observe", vec![]).unwrap();
    vm.run_until_idle(&mut host).unwrap();
    assert_eq!(vm.get_export("checked").unwrap(), Value::number(1.0));
    assert!(host.rejection_events().is_empty());
}
