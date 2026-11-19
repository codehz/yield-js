//! Promise reactions and combination state survive suspension and restore.

use super::support::{RUN, Suspended};
use yield_poc::{TestHost, build};
use yield_vm::{Completion, HostValue, Object, PromiseState, Value, Vm, VmError};

#[test]
fn script_reaction_survives_restore_and_runs_on_fulfill() {
    let source = r#"
async function run() {
  return await wait("a").then(function (value) {
    return value + 1;
  });
}
"#;
    let mut s = Suspended::start(source, vec![]);
    assert_eq!(s.vm.pending_operation_ids(), vec!["wait:a".to_owned()]);
    let snapshot = s.snapshot();
    assert!(
        snapshot.objects.iter().any(|record| matches!(
            &record.object,
            Object::Promise(promise) if promise.reactions.iter().any(|job| matches!(job, yield_vm::heap::ReactionJob::Script { .. }))
        )),
        "then() handler is a persisted reaction"
    );
    s.restore(snapshot).expect("restore");
    s.complete("wait:a", Completion::Fulfilled(HostValue::Number(4.0)));
    assert_eq!(s.state(), PromiseState::Fulfilled(Value::Number(5.0)));
}

#[test]
fn snapshot_rejects_an_unrooted_thenable_capability() {
    let source = r#"
async function run() {
  Promise.resolve({
    then: function () {},
  });
  await wait("hold");
  return 1;
}
"#;
    let build = build(source);
    let mut host = TestHost::default();
    let mut vm = Vm::new(build, &host).expect("bindings");
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
async function run() {
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

#[test]
fn promise_all_combine_state_survives_a_partial_fulfill() {
    let source = r#"
async function run() {
  const values = await Promise.all([wait("a"), wait("b")]);
  return values[0] + values[1];
}
"#;
    let mut s = Suspended::start(source, vec![]);
    s.complete("wait:a", Completion::Fulfilled(HostValue::Number(2.0)));
    let snapshot = s.snapshot();
    assert_eq!(snapshot.combines.len(), 1);
    s.restore(snapshot).expect("restore");
    s.complete("wait:b", Completion::Fulfilled(HostValue::Number(3.0)));
    assert_eq!(s.state(), PromiseState::Fulfilled(Value::Number(5.0)));
}
