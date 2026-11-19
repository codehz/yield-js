//! Suspension preserves state across await and finally boundaries.

use super::support::{RUN, Suspended, json_round_trip};
use yield_poc::number;
use yield_vm::{Completion, HostValue, PromiseState, Value};

#[test]
fn snapshot_and_restore_across_two_awaits() {
    let mut s = Suspended::start(RUN, vec![HostValue::String("input".into())]);
    assert_eq!(s.host.count("inspect"), 1);
    assert_eq!(s.host.count("seed"), 1);
    assert_eq!(s.vm.pending_operation_ids(), vec!["wait:a".to_owned()]);

    let snapshot = s.snapshot();
    assert_eq!(snapshot.continuations.len(), 1);
    assert_eq!(snapshot.operations.len(), 1);
    assert_eq!(
        snapshot.continuations[0].values,
        vec![Value::Number(10.0)],
        "base is the only value live across the first await"
    );
    assert!(
        snapshot.continuations[0].env.is_none(),
        "uncaptured locals do not keep a call environment"
    );
    s.restore(json_round_trip(&snapshot))
        .expect("restore validates bindings");
    assert_eq!(s.vm.pending_operation_ids(), vec!["wait:a".to_owned()]);

    s.complete("wait:a", Completion::Fulfilled(number(2.0)));
    assert_eq!(
        s.host.count("inspect"),
        1,
        "restore must not re-run side effects"
    );
    assert_eq!(s.host.count("seed"), 1);
    assert_eq!(s.host.count("wait"), 2);
    assert_eq!(s.vm.pending_operation_ids(), vec!["wait:b".to_owned()]);

    let snapshot = s.snapshot();
    assert_eq!(
        snapshot.continuations[0].values,
        vec![Value::Number(12.0)],
        "sum is the only value live across the second await"
    );
    assert!(snapshot.continuations[0].env.is_none());
    s.restore(snapshot).expect("restore at second await");
    s.complete("wait:b", Completion::Fulfilled(number(3.0)));
    assert_eq!(s.state(), PromiseState::Fulfilled(Value::Number(15.0)));
    assert_eq!(s.host.count("inspect"), 1);
    assert_eq!(s.host.count("seed"), 1);
    assert_eq!(s.host.count("wait"), 2);
}

#[test]
fn await_inside_finally_preserves_pending_return() {
    let source = r#"
async function run() {
  try {
    return 7;
  } finally {
    await wait("fin");
    setLabel("done");
  }
}
"#;
    let mut s = Suspended::start(source, vec![]);
    assert_eq!(s.vm.pending_operation_ids(), vec!["wait:fin".to_owned()]);
    assert_eq!(s.host.count("setLabel"), 0);
    let snapshot = s.snapshot();
    assert!(
        snapshot.continuations[0]
            .values
            .contains(&Value::Number(1.0)),
        "pending return kind is saved"
    );
    assert!(
        snapshot.continuations[0]
            .values
            .contains(&Value::Number(7.0)),
        "pending return payload is saved"
    );
    s.restore(snapshot).expect("restore");
    s.complete("wait:fin", Completion::Fulfilled(number(0.0)));
    assert_eq!(s.host.count("setLabel"), 1);
    assert_eq!(s.state(), PromiseState::Fulfilled(Value::Number(7.0)));
}

#[test]
fn await_inside_finally_preserves_pending_throw() {
    let source = r#"
async function run() {
  try {
    throw 5;
  } finally {
    await wait("fin");
  }
}
"#;
    let mut s = Suspended::start(source, vec![]);
    let snapshot = s.snapshot();
    assert!(
        snapshot.continuations[0]
            .values
            .contains(&Value::Number(2.0)),
        "pending throw kind is saved"
    );
    assert!(
        snapshot.continuations[0]
            .values
            .contains(&Value::Number(5.0)),
        "pending throw payload is saved"
    );
    s.restore(snapshot).expect("restore");
    s.complete("wait:fin", Completion::Fulfilled(number(0.0)));
    assert_eq!(s.state(), PromiseState::Rejected(Value::Number(5.0)));
}

#[test]
fn temporary_await_results_are_still_saved_across_resume_points() {
    let source = r#"
async function run() {
  return (await wait("a")) + (await wait("b"));
}
"#;
    let mut s = Suspended::start(source, vec![]);
    s.complete("wait:a", Completion::Fulfilled(number(4.0)));
    let snapshot = s.snapshot();
    assert_eq!(
        snapshot.continuations[0].values,
        vec![Value::Number(4.0)],
        "the first await result is live across the second await"
    );
    s.restore(snapshot).expect("restore");
    s.complete("wait:b", Completion::Fulfilled(number(5.0)));
    assert_eq!(s.state(), PromiseState::Fulfilled(Value::Number(9.0)));
}
