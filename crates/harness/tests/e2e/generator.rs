//! Generator snapshot identity, recovery, and rejection.

use yield_harness::TestHost;
use yield_vm::heap::GeneratorStatus;
use yield_vm::{Completion, PromiseState, Value, Vm, VmError};

use super::support::{Suspended, TEST_DECLARATIONS, declared, json_round_trip};

/// A generator suspended inside a `finally` holds a pending `return` completion
/// in its saved slots. Restoring must reproduce the object graph, then finish
/// the pending return with the very object the caller passed in.
const RETURN_THROUGH_FINALLY: &str = r#"
import { wait } from "test";

function* g(x) {
  try {
    return x + (yield 2);
  } finally {
    yield 3;
  }
}

export const state = { it: null, alias: null, mark: null, closed: null };

export function setup() {
  const it = g(5);
  it.next();
  const mark = { tag: "mark" };
  state.it = it;
  state.alias = it;
  state.mark = mark;
  state.closed = it.return(mark);
  return 1;
}

export async function run() {
  setup();
  await wait("pause");
  const last = state.it.next();
  return last.done === true &&
    last.value === state.mark &&
    state.it === state.alias &&
    state.closed.done === false &&
    state.closed.value === 3
    ? "ok"
    : "bad";
}
"#;

fn start(source: &str) -> Suspended {
    let _ = declared(TEST_DECLARATIONS);
    Suspended::start(source, vec![])
}

fn assert_fulfilled(suspended: &Suspended, expected: &str) {
    assert_eq!(
        suspended.state(),
        PromiseState::Fulfilled(Value::string(expected.into()))
    );
}

#[test]
fn generator_resumes_after_restore_with_the_returned_object() {
    let mut suspended = start(RETURN_THROUGH_FINALLY);
    let roots = suspended.roots();
    suspended.vm.collect_garbage(&roots).expect("collect");

    let captured = json_round_trip(&suspended.snapshot());
    assert_eq!(captured.generators.len(), 1);
    let (object, record) = captured.generators.iter().next().expect("generator");
    assert_eq!(record.status, GeneratorStatus::Suspended(1));
    assert_eq!(record.values.len(), 2);
    let object = *object;

    suspended.restore(captured).expect("restore generator");
    let restored = suspended
        .vm
        .snapshot(&roots)
        .expect("capture after restore");
    assert_eq!(
        restored.generators.get(&object).map(|record| record.status),
        Some(GeneratorStatus::Suspended(1)),
        "the generator keeps its identity and resume point"
    );

    suspended.complete(
        "wait:pause",
        Completion::Fulfilled(yield_vm::JsValue::undefined()),
    );
    assert_fulfilled(&suspended, "ok");
}

#[test]
fn generator_start_state_survives_restore() {
    let source = r#"
import { wait } from "test";

function* g(x = 6) {
  yield x;
}

export const state = { it: null };

export function setup() {
  state.it = g();
  return 1;
}

export async function run() {
  setup();
  await wait("pause");
  const step = state.it.next();
  const fresh = g(2).next();
  return step.done === false &&
    step.value === 6 &&
    fresh.done === false &&
    fresh.value === 2
    ? "ok"
    : "bad";
}
"#;
    let mut suspended = start(source);
    let captured = json_round_trip(&suspended.snapshot());
    let record = captured.generators.values().next().expect("generator");
    assert_eq!(record.status, GeneratorStatus::Start);
    suspended
        .restore(captured)
        .expect("restore unstarted generator");
    suspended.complete(
        "wait:pause",
        Completion::Fulfilled(yield_vm::JsValue::undefined()),
    );
    assert_fulfilled(&suspended, "ok");
}

#[test]
fn completed_generator_does_not_rerun_after_restore() {
    let source = r#"
import { wait } from "test";

let bodyRuns = 0;

function* g() {
  bodyRuns = bodyRuns + 1;
  yield 1;
  return 2;
}

export const state = { it: null };

export function setup() {
  const it = g();
  it.next();
  it.next();
  state.it = it;
  return bodyRuns;
}

export async function run() {
  setup();
  await wait("pause");
  const last = state.it.next();
  const closed = state.it.return(9);
  return last.done === true &&
    last.value === undefined &&
    closed.done === true &&
    closed.value === 9 &&
    bodyRuns === 1
    ? "ok"
    : "bad";
}
"#;
    let mut suspended = start(source);
    let captured = json_round_trip(&suspended.snapshot());
    let record = captured.generators.values().next().expect("generator");
    assert_eq!(record.status, GeneratorStatus::Completed);
    suspended
        .restore(captured)
        .expect("restore completed generator");
    suspended.complete(
        "wait:pause",
        Completion::Fulfilled(yield_vm::JsValue::undefined()),
    );
    assert_fulfilled(&suspended, "ok");
}

/// A delegation suspension keeps the delegate, the cached `next`, and the read
/// state in saved slots, so recovery continues the same iterator.
#[test]
fn delegated_generator_resumes_without_regaining_the_iterator() {
    let source = r#"
import { wait } from "test";

let iteratorCalls = 0;
let nextCalls = 0;

const delegate = {
  next() {
    nextCalls = nextCalls + 1;
    return { done: false, value: nextCalls };
  },
};
delegate[Symbol.iterator] = function () {
  iteratorCalls = iteratorCalls + 1;
  return delegate;
};

function* outer() {
  yield* delegate;
}

export const state = { it: null };

export async function run() {
  const it = outer();
  state.it = it;
  const first = it.next();
  await wait("pause");
  const second = it.next();
  return iteratorCalls === 1 &&
    nextCalls === 2 &&
    first.value === 1 &&
    second.value === 2 &&
    second.done === false
    ? "ok"
    : "bad";
}
"#;
    let mut suspended = start(source);
    let mut roots = suspended.roots();
    suspended.vm.collect_garbage(&roots).expect("collect");
    let captured = json_round_trip(&suspended.snapshot());
    assert_eq!(
        captured
            .generators
            .values()
            .next()
            .map(|record| record.status),
        Some(GeneratorStatus::Suspended(0))
    );
    suspended.restore(captured).expect("restore delegation");
    roots = suspended.roots();
    suspended
        .vm
        .collect_garbage(&roots)
        .expect("collect after restore");
    suspended.complete(
        "wait:pause",
        Completion::Fulfilled(yield_vm::JsValue::undefined()),
    );
    assert_fulfilled(&suspended, "ok");
}

#[test]
fn restore_rejects_an_out_of_range_generator_resume() {
    let suspended = start(RETURN_THROUGH_FINALLY);
    let mut captured = suspended.snapshot();
    let record = captured
        .generators
        .values_mut()
        .next()
        .expect("generator frame");
    record.status = GeneratorStatus::Suspended(7);
    assert!(matches!(
        Vm::restore(captured, suspended.vm.build().clone(), &mut TestHost::default()),
        Err(VmError::InvalidSnapshot(message)) if message.contains("invalid resume index")
    ));
}

#[test]
fn restore_rejects_a_generator_missing_a_saved_value() {
    let suspended = start(RETURN_THROUGH_FINALLY);
    let mut captured = suspended.snapshot();
    let record = captured
        .generators
        .values_mut()
        .next()
        .expect("generator frame");
    record.values.pop();
    assert!(matches!(
        Vm::restore(captured, suspended.vm.build().clone(), &mut TestHost::default()),
        Err(VmError::InvalidSnapshot(message)) if message.contains("wrong number of saved values")
    ));
}

#[test]
fn restore_rejects_a_generator_that_was_captured_while_executing() {
    let suspended = start(RETURN_THROUGH_FINALLY);
    let mut captured = suspended.snapshot();
    let record = captured
        .generators
        .values_mut()
        .next()
        .expect("generator frame");
    record.status = GeneratorStatus::Executing;
    assert!(matches!(
        Vm::restore(captured, suspended.vm.build().clone(), &mut TestHost::default()),
        Err(VmError::InvalidSnapshot(message)) if message.contains("captured while executing")
    ));
}
