//! Suspension preserves state across await and finally boundaries.

use super::support::{
    RUN, Suspended, baseline_at_first_await, baseline_at_second_await, count_delta,
    json_round_trip, values_beyond,
};
use yield_poc::number;
use yield_vm::{Completion, HostValue, PromiseState, Value};

#[test]
fn snapshot_and_restore_across_two_awaits() {
    let mut s = Suspended::start(RUN, vec![HostValue::String("input".into())]);
    assert_eq!(s.host.count("inspect"), 1);
    assert_eq!(s.host.count("seed"), 1);
    assert_eq!(s.vm.pending_operation_ids(), vec!["wait:a".to_owned()]);

    let baseline = baseline_at_first_await();
    let snapshot = s.snapshot();
    assert_eq!(
        count_delta(snapshot.continuations.len(), baseline.continuations.len()),
        0
    );
    assert_eq!(
        count_delta(snapshot.operations.len(), baseline.operations.len()),
        0
    );
    assert_eq!(
        values_beyond(
            &snapshot.continuations[0].values,
            &baseline.continuations[0].values,
        ),
        vec![Value::Number(10.0)],
        "base is the only extra live value"
    );
    assert!(
        snapshot.continuations[0].env.is_none(),
        "uncaptured locals do not keep a call environment"
    );
    s.restore(json_round_trip(&snapshot))
        .expect("restore validates bindings");
    assert_eq!(s.vm.pending_operation_ids(), vec!["wait:a".to_owned()]);

    let inspect = s.host.count("inspect");
    let seed = s.host.count("seed");
    let wait = s.host.count("wait");
    s.complete("wait:a", Completion::Fulfilled(number(2.0)));
    assert_eq!(
        count_delta(s.host.count("inspect"), inspect),
        0,
        "restore must not re-run side effects"
    );
    assert_eq!(count_delta(s.host.count("seed"), seed), 0);
    assert_eq!(count_delta(s.host.count("wait"), wait), 1);
    assert_eq!(s.vm.pending_operation_ids(), vec!["wait:b".to_owned()]);

    let snapshot = s.snapshot();
    assert_eq!(
        values_beyond(
            &snapshot.continuations[0].values,
            &baseline_at_second_await().continuations[0].values,
        ),
        vec![Value::Number(12.0)],
        "sum is the only value live across the second await"
    );
    assert!(snapshot.continuations[0].env.is_none());
    s.restore(snapshot).expect("restore at second await");
    let inspect = s.host.count("inspect");
    let seed = s.host.count("seed");
    let wait = s.host.count("wait");
    s.complete("wait:b", Completion::Fulfilled(number(3.0)));
    assert_eq!(s.state(), PromiseState::Fulfilled(Value::Number(15.0)));
    assert_eq!(count_delta(s.host.count("inspect"), inspect), 0);
    assert_eq!(count_delta(s.host.count("seed"), seed), 0);
    assert_eq!(count_delta(s.host.count("wait"), wait), 0);
}

#[test]
fn await_inside_finally_preserves_pending_return() {
    let source = r#"
import { seed, setLabel, wait } from "test";
export async function run() {
  try {
    seed();
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
            .contains(&Value::Number(7.0)),
        "pending return payload is saved"
    );
    s.restore(snapshot).expect("restore");
    let seed = s.host.count("seed");
    let set_label = s.host.count("setLabel");
    s.complete("wait:fin", Completion::Fulfilled(number(0.0)));
    assert_eq!(count_delta(s.host.count("seed"), seed), 0);
    assert_eq!(count_delta(s.host.count("setLabel"), set_label), 1);
    assert_eq!(s.state(), PromiseState::Fulfilled(Value::Number(7.0)));
}

#[test]
fn await_inside_finally_preserves_pending_throw() {
    let source = r#"
import { seed, wait } from "test";
export async function run() {
  try {
    seed();
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
            .contains(&Value::Number(5.0)),
        "pending throw payload is saved"
    );
    s.restore(snapshot).expect("restore");
    let seed = s.host.count("seed");
    s.complete("wait:fin", Completion::Fulfilled(number(0.0)));
    assert_eq!(count_delta(s.host.count("seed"), seed), 0);
    assert_eq!(s.state(), PromiseState::Rejected(Value::Number(5.0)));
}

#[test]
fn temporary_await_results_are_still_saved_across_resume_points() {
    let source = r#"
import { wait } from "test";
export async function run() {
  return (await wait("a")) + (await wait("b"));
}
"#;
    let mut s = Suspended::start(source, vec![]);
    s.complete("wait:a", Completion::Fulfilled(number(4.0)));
    let snapshot = s.snapshot();
    assert_eq!(
        values_beyond(
            &snapshot.continuations[0].values,
            &baseline_at_second_await().continuations[0].values,
        ),
        vec![Value::Number(4.0)],
        "the first await result is live across the second await"
    );
    s.restore(snapshot).expect("restore");
    s.complete("wait:b", Completion::Fulfilled(number(5.0)));
    assert_eq!(s.state(), PromiseState::Fulfilled(Value::Number(9.0)));
}
