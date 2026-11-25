//! Synchronous execution checks do not require a suspension harness.

use yield_harness::{TestHost, build};
use yield_vm::{JsValue, RestoreOptions, SnapshotValidation, Value, Vm, VmError};

#[test]
fn finally_completion_overrides_return_and_throw() {
    let source = r#"
export function run() {
  let log = 0;
  try {
    return 1;
  } finally {
    log = 2;
    return log + 3;
  }
}
"#;
    let build_return = build(source);
    let host = TestHost::default();
    let mut vm = Vm::new(build_return, host.host(), Default::default()).expect("bindings");
    let result = vm.start("run", Vec::new()).expect("start");
    assert_eq!(result, Value::number(5.0));

    let source = r#"
export function run() {
  try {
    throw 1;
  } finally {
    return 2;
  }
}
"#;
    let build_override = build(source);
    let mut vm = Vm::new(build_override, host.host(), Default::default()).expect("bindings");
    let result = vm.start("run", Vec::new()).expect("start");
    assert_eq!(result, Value::number(2.0));

    let source = r#"
export function run() {
  try {
    return 1;
  } finally {
    throw 3;
  }
}
"#;
    let build_throw = build(source);
    let mut vm = Vm::new(build_throw, host.host(), Default::default()).expect("bindings");
    let error = vm.start("run", Vec::new()).expect_err("finally throw wins");
    assert!(matches!(error, VmError::Thrown(value) if value == Value::number(3.0)));
}

#[test]
fn sync_control_flow_executes_before_any_await() {
    let source = r#"
export function run(n) {
  let sum = 0;
  for (let i = 0; i < n; i += 1) {
    if (i % 2 === 0) {
      sum += i;
    }
  }
  return sum;
}
"#;
    let build = build(source);
    let host = TestHost::default();
    let mut vm = Vm::new(build, host.host(), Default::default()).expect("bindings");
    let result = vm.start("run", vec![JsValue::number(5.0)]).expect("start");
    assert!(matches!(
        vm.promise_state(&result),
        Err(VmError::InvalidState(message)) if message == "value is not an object"
    ));
    assert_eq!(result, Value::number(6.0));
}

#[test]
fn math_random_continues_after_snapshot_restore() {
    let build = build("export function run() { return Math.random(); }");
    let host = TestHost::default();
    let mut vm = Vm::new(build.clone(), host.host(), Default::default()).unwrap();
    let first = vm.start("run", vec![]).unwrap();
    let snapshot = vm.snapshot(&[]).unwrap();
    let expected = vm.start("run", vec![]).unwrap();
    assert_ne!(first, expected);
    let decoded = yield_vm::Snapshot::from_json(&snapshot.to_json().unwrap()).unwrap();
    let mut restored =
        Vm::restore(decoded, build.clone(), host.host(), Default::default()).unwrap();
    assert_eq!(restored.start("run", vec![]).unwrap(), expected);
    assert_eq!(
        restored.start("run", vec![]).unwrap(),
        vm.start("run", vec![]).unwrap()
    );

    let mut missing_root = snapshot.clone();
    missing_root.roots.remove("math");
    assert!(matches!(
        Vm::restore(
            missing_root,
            build.clone(),
            host.host(),
            RestoreOptions {
                validation: SnapshotValidation::Full,
                ..Default::default()
            },
        ),
        Err(VmError::InvalidSnapshot(_))
    ));
    let mut zero_state = snapshot;
    zero_state.random_state = 0;
    assert!(matches!(
        Vm::restore(
            zero_state,
            build,
            host.host(),
            RestoreOptions {
                validation: SnapshotValidation::Full,
                ..Default::default()
            },
        ),
        Err(VmError::InvalidSnapshot(_))
    ));
}
