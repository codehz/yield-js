//! Promise reactions and combination state survive suspension and restore.

use super::support::{RUN, Suspended};
use yield_harness::{TestHost, build};
use yield_vm::heap::PromiseCallback;
use yield_vm::{Completion, JsValue, Object, PromiseState, Value, Vm, VmError};

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
    let mut vm = Vm::new(build, &mut host).expect("bindings");
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
    let error = s.restore(snapshot).expect_err("locked without adopt");
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
    let error = s.restore(snapshot).expect_err("missing handler");
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
        .restore(snapshot)
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
        .restore(snapshot)
        .expect_err("non-callable combine capability");
    assert!(
        matches!(error, VmError::InvalidSnapshot(message) if message.contains("capability is not callable"))
    );
}
