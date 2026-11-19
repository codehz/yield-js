//! Synchronous execution checks do not require a suspension harness.

use yield_poc::{TestHost, build};
use yield_vm::{HostValue, Value, Vm, VmError};

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
    let mut host = TestHost::default();
    let mut vm = Vm::new(build_return, &mut host).expect("bindings");
    let result = vm.start(&mut host, "run", Vec::new()).expect("start");
    assert_eq!(result, Value::Number(5.0));

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
    let mut vm = Vm::new(build_override, &mut host).expect("bindings");
    let result = vm.start(&mut host, "run", Vec::new()).expect("start");
    assert_eq!(result, Value::Number(2.0));

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
    let mut vm = Vm::new(build_throw, &mut host).expect("bindings");
    let error = vm
        .start(&mut host, "run", Vec::new())
        .expect_err("finally throw wins");
    assert!(matches!(error, VmError::Thrown(Value::Number(3.0))));
}

#[test]
fn hoisted_functions_can_capture_later_bindings() {
    let source = r#"
export function run() {
  return read();
  function read() {
    return x;
  }
  let x = 4;
}
"#;
    let build = build(source);
    let mut host = TestHost::default();
    let mut vm = Vm::new(build, &mut host).expect("bindings");
    // Scope entry initializes `x`; the later initializer never runs.
    let result = vm
        .start(&mut host, "run", Vec::new())
        .expect("captured read");
    assert_eq!(result, Value::Undefined);
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
    let mut host = TestHost::default();
    let mut vm = Vm::new(build, &mut host).expect("bindings");
    let result = vm
        .start(&mut host, "run", vec![HostValue::Number(5.0)])
        .expect("start");
    assert!(matches!(
        vm.promise_state(&result),
        Err(VmError::InvalidState(message)) if message == "value is not an object"
    ));
    assert_eq!(result, Value::Number(6.0));
}
