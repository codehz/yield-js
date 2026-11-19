use std::collections::{BTreeMap, BTreeSet};

use yield_compiler::{CompileOptions, compile};
use yield_vm::bytecode::{Build, Instr, Slot};
use yield_vm::{
    Completion, Host, HostReply, HostValue, NativeKey, Object, PendingOperationMeta, PromiseState,
    Value, Vm, VmError,
};

const RUN: &str = r#"
async function run(input) {
  inspect(input);
  const base = seed();
  const x = await wait("a");
  const sum = base + x;
  const y = await wait("b");
  return sum + y;
}
"#;

#[derive(Default)]
struct TestHost {
    calls: Vec<(String, Vec<HostValue>)>,
    operations: BTreeMap<String, PendingOperationMeta>,
    natives: BTreeSet<NativeKey>,
    reject_restore_operation: bool,
    reject_native: bool,
    fail_import: bool,
}

impl TestHost {
    fn imported(name: &str) -> bool {
        matches!(name, "inspect" | "seed" | "wait" | "bind" | "setLabel")
    }

    fn count(&self, name: &str) -> usize {
        self.calls.iter().filter(|(call, _)| call == name).count()
    }
}

impl Host for TestHost {
    fn has_import(&self, name: &str) -> bool {
        Self::imported(name) && !self.fail_import
    }

    fn call(&mut self, import: &str, args: &[HostValue]) -> Result<HostReply, String> {
        self.calls.push((import.to_owned(), args.to_vec()));
        match import {
            "inspect" => Ok(HostReply::Value(HostValue::Undefined)),
            "seed" => Ok(HostReply::Value(HostValue::Number(10.0))),
            "wait" => {
                let label = match args {
                    [HostValue::String(label)] => label.clone(),
                    _ => return Err("wait expects one string".to_owned()),
                };
                let id = format!("wait:{label}");
                self.operations.insert(
                    id.clone(),
                    PendingOperationMeta {
                        id: id.clone(),
                        import: import.to_owned(),
                    },
                );
                Ok(HostReply::Operation { operation_id: id })
            }
            "bind" => {
                let id = match args {
                    [HostValue::String(id)] => id.clone(),
                    _ => return Err("bind expects one string".to_owned()),
                };
                let key = NativeKey {
                    type_name: "Node".to_owned(),
                    id,
                };
                self.natives.insert(key.clone());
                Ok(HostReply::Value(HostValue::Native(key)))
            }
            "setLabel" => Ok(HostReply::Value(HostValue::Undefined)),
            _ => Err(format!("unknown import {import}")),
        }
    }

    fn has_native(&self, key: &NativeKey) -> bool {
        !self.reject_native && self.natives.contains(key)
    }

    fn rebuild_operation(&mut self, operation: &PendingOperationMeta) -> bool {
        !self.reject_restore_operation
            && self
                .operations
                .get(&operation.id)
                .is_some_and(|known| known == operation)
    }
}

fn build(source: &str) -> Build {
    compile(
        source,
        &CompileOptions {
            imports: ["inspect", "seed", "wait", "bind", "setLabel"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
        },
    )
    .expect("source compiles")
}

fn number(value: f64) -> HostValue {
    HostValue::Number(value)
}

#[test]
fn snapshot_and_restore_across_two_awaits() {
    let build = build(RUN);
    let mut host = TestHost::default();
    let mut vm = Vm::new(build.clone(), &host).expect("bindings satisfy build");
    let result = vm
        .start(&mut host, "run", vec![HostValue::String("input".into())])
        .expect("start");
    vm.run_until_idle(&mut host).expect("drain to first await");
    assert_eq!(host.count("inspect"), 1);
    assert_eq!(host.count("seed"), 1);
    assert_eq!(vm.pending_operation_ids(), vec!["wait:a".to_owned()]);

    let snapshot = vm
        .snapshot(std::slice::from_ref(&result))
        .expect("quiescent snapshot");
    assert_eq!(snapshot.continuations.len(), 1);
    assert_eq!(snapshot.operations.len(), 1);
    assert!(
        snapshot.continuations[0].values.is_empty(),
        "variables live in the captured environment, not frame slots"
    );
    assert!(
        snapshot.continuations[0].env.is_some(),
        "the captured environment is part of the continuation"
    );
    let json = snapshot.to_json().expect("snapshot serializes");
    let snapshot = yield_vm::Snapshot::from_json(&json).expect("snapshot deserializes");
    let mut restored =
        Vm::restore(snapshot, build.clone(), &mut host).expect("restore validates bindings");
    assert_eq!(restored.pending_operation_ids(), vec!["wait:a".to_owned()]);

    restored
        .complete_operation("wait:a", Completion::Fulfilled(number(2.0)))
        .expect("complete a");
    restored
        .run_until_idle(&mut host)
        .expect("resume to second await");
    assert_eq!(
        host.count("inspect"),
        1,
        "restore must not re-run side effects"
    );
    assert_eq!(host.count("seed"), 1);
    assert_eq!(host.count("wait"), 2);
    assert_eq!(restored.pending_operation_ids(), vec!["wait:b".to_owned()]);

    let snapshot = restored
        .snapshot(std::slice::from_ref(&result))
        .expect("snapshot at second await");
    assert!(
        snapshot.continuations[0].values.is_empty(),
        "variables live in the captured environment, not frame slots"
    );
    assert!(snapshot.continuations[0].env.is_some());
    let mut restored = Vm::restore(snapshot, build, &mut host).expect("restore at second await");
    restored
        .complete_operation("wait:b", Completion::Fulfilled(number(3.0)))
        .expect("complete b");
    restored.run_until_idle(&mut host).expect("finish");
    assert_eq!(
        restored.promise_state(&result).expect("result promise"),
        PromiseState::Fulfilled(Value::Number(15.0))
    );
    assert_eq!(host.count("inspect"), 1);
    assert_eq!(host.count("seed"), 1);
    assert_eq!(host.count("wait"), 2);
}

#[test]
fn rejection_resumes_at_the_reject_landing_point() {
    let build = build(RUN);
    let mut host = TestHost::default();
    let mut vm = Vm::new(build.clone(), &host).expect("bindings");
    let result = vm
        .start(&mut host, "run", vec![HostValue::String("input".into())])
        .expect("start");
    vm.run_until_idle(&mut host).expect("first await");
    let snapshot = vm
        .snapshot(std::slice::from_ref(&result))
        .expect("snapshot");
    let mut vm = Vm::restore(snapshot, build, &mut host).expect("restore");
    vm.complete_operation(
        "wait:a",
        Completion::Rejected(HostValue::String("boom".into())),
    )
    .expect("reject a");
    vm.run_until_idle(&mut host).expect("rejection resumes");
    assert_eq!(
        vm.promise_state(&result).expect("result"),
        PromiseState::Rejected(Value::String("boom".into()))
    );
    assert_eq!(vm.pending_operation_ids(), Vec::<String>::new());
}

#[test]
fn missing_bindings_and_mismatched_builds_are_rejected_before_execution() {
    let source = r#"
async function run() {
  const node = bind("root");
  setLabel(node, "before");
  await wait("a");
  setLabel(node, "after");
  return 1;
}
"#;
    let build = build(source);
    let mut host = TestHost::default();
    let mut vm = Vm::new(build.clone(), &host).expect("bindings");
    let result = vm.start(&mut host, "run", Vec::new()).expect("start");
    vm.run_until_idle(&mut host).expect("first await");
    let snapshot = vm.snapshot(&[result]).expect("snapshot");

    host.reject_native = true;
    let error = Vm::restore(snapshot.clone(), build.clone(), &mut host)
        .expect_err("missing native wrapper binding");
    assert!(matches!(error, VmError::MissingNative(_)));
    assert_eq!(host.count("setLabel"), 1, "restore must not run script");
    host.reject_native = false;

    host.reject_restore_operation = true;
    let error =
        Vm::restore(snapshot.clone(), build.clone(), &mut host).expect_err("missing operation");
    assert!(matches!(error, VmError::MissingOperation(id) if id == "wait:a"));
    host.reject_restore_operation = false;

    let mut mismatched = snapshot.clone();
    mismatched.build_id = "sha256:deadbeef".to_owned();
    let error = Vm::restore(mismatched, build.clone(), &mut host).expect_err("build mismatch");
    assert!(matches!(error, VmError::InvalidSnapshot(message) if message.contains("build id")));

    let mut mismatched = snapshot.clone();
    mismatched.bytecode_version += 1;
    let error =
        Vm::restore(mismatched, build.clone(), &mut host).expect_err("bytecode version mismatch");
    assert!(
        matches!(error, VmError::InvalidSnapshot(message) if message.contains("bytecode version"))
    );

    let mut mismatched = snapshot;
    mismatched.format_version += 1;
    let error = Vm::restore(mismatched, build, &mut host).expect_err("snapshot format mismatch");
    assert!(matches!(error, VmError::InvalidSnapshot(message) if message.contains("format")));
}

#[test]
fn snapshot_requires_a_drained_microtask_queue() {
    let source = r#"
async function run() {
  await 1;
  return 2;
}
"#;
    let build = build(source);
    let mut host = TestHost::default();
    let mut vm = Vm::new(build, &host).expect("bindings");
    let result = vm.start(&mut host, "run", Vec::new()).expect("start");
    let error = vm
        .snapshot(&[result])
        .expect_err("await on a plain value queues a resume microtask");
    assert!(matches!(error, VmError::InvalidState(message) if message.contains("microtask")));
}

#[test]
fn sync_throw_after_resume_rejects_the_async_result() {
    let source = r#"
function fail() {
  throw "sync boom";
}
async function run() {
  await wait("a");
  fail();
  return 1;
}
"#;
    let build = build(source);
    let mut host = TestHost::default();
    let mut vm = Vm::new(build, &host).expect("bindings");
    let result = vm.start(&mut host, "run", Vec::new()).expect("start");
    vm.run_until_idle(&mut host).expect("first await");
    vm.complete_operation("wait:a", Completion::Fulfilled(number(1.0)))
        .expect("complete");
    vm.run_until_idle(&mut host)
        .expect("resume drains and rejects the async result");
    assert_eq!(
        vm.promise_state(&result).expect("result"),
        PromiseState::Rejected(Value::String("sync boom".into()))
    );
}

#[test]
fn loader_rejects_corrupt_builds_and_resume_tables() {
    let mut host = TestHost::default();

    let mut corrupt = build("function run() { return 1; }");
    corrupt.bytecode_version += 1;
    let error = Vm::new(corrupt, &host).expect_err("bytecode version");
    assert!(
        matches!(error, VmError::InvalidBuild(message) if message.contains("bytecode version"))
    );

    let mut corrupt = build("function run() { return 1; }");
    corrupt.functions[0].code[0] = Instr::Const {
        dst: Slot::transient(u16::MAX),
        constant: 0,
    };
    corrupt.finalize();
    let error = Vm::new(corrupt, &host).expect_err("out-of-range slot");
    assert!(matches!(error, VmError::InvalidBuild(message) if message.contains("out-of-range")));

    let mut corrupt = build(RUN);
    corrupt.functions[0].resumes[0].saved_slots.push(u16::MAX);
    corrupt.finalize();
    let error = Vm::new(corrupt, &host).expect_err("bad saved slot");
    assert!(
        matches!(error, VmError::InvalidBuild(message) if message.contains("saves persistent slot") || message.contains("not strictly"))
    );

    let mut corrupt = build("function run() { return 1; }");
    corrupt.functions[0].code[0] = Instr::Jump { target: u32::MAX };
    corrupt.finalize();
    let error = Vm::new(corrupt, &host).expect_err("bad jump target");
    assert!(matches!(error, VmError::InvalidBuild(message) if message.contains("targets pc")));

    let _ = &mut host;
}

#[test]
fn corrupted_snapshot_associations_are_rejected() {
    let build = build(RUN);
    let mut host = TestHost::default();
    let mut vm = Vm::new(build.clone(), &host).expect("bindings");
    let result = vm.start(&mut host, "run", Vec::new()).expect("start");
    vm.run_until_idle(&mut host).expect("await");
    let snapshot = vm.snapshot(&[result]).expect("snapshot");

    let mut wrong_values = snapshot.clone();
    wrong_values.continuations[0]
        .values
        .push(Value::Number(999.0));
    let error = Vm::restore(wrong_values, build.clone(), &mut host).expect_err("slot value count");
    assert!(matches!(error, VmError::InvalidSnapshot(message) if message.contains("values")));

    let mut broken_reactions = snapshot.clone();
    let waiting_on = broken_reactions.continuations[0].waiting_on;
    for record in &mut broken_reactions.objects {
        if record.id == waiting_on
            && let Object::Promise(promise) = &mut record.object
        {
            promise.reactions.clear();
        }
    }
    let error = Vm::restore(broken_reactions, build, &mut host)
        .expect_err("continuation not associated with promise");
    assert!(matches!(error, VmError::InvalidSnapshot(message) if message.contains("exactly once")));
}

#[test]
fn unreachable_native_wrappers_do_not_require_rebinding() {
    let source = r#"
async function run() {
  bind("temporary");
  await wait("a");
  return 1;
}
"#;
    let build = build(source);
    let mut host = TestHost::default();
    let mut vm = Vm::new(build.clone(), &host).expect("bindings");
    let result = vm.start(&mut host, "run", Vec::new()).expect("start");
    vm.run_until_idle(&mut host).expect("await");
    let snapshot = vm
        .snapshot(&[result])
        .expect("snapshot only reachable objects");
    assert!(
        snapshot
            .objects
            .iter()
            .all(|record| !matches!(record.object, Object::Native(_)))
    );
    host.reject_native = true;
    Vm::restore(snapshot, build, &mut host).expect("transient native is not a root");
}

#[test]
fn settled_async_promises_resume_through_the_microtask_queue() {
    let source = r#"
async function inner() {
  return 5;
}
async function run() {
  const x = await inner();
  return x + 1;
}
"#;
    let build = build(source);
    let mut host = TestHost::default();
    let mut vm = Vm::new(build, &host).expect("bindings");
    let result = vm.start(&mut host, "run", Vec::new()).expect("start");
    vm.run_until_idle(&mut host).expect("drain");
    assert_eq!(
        vm.promise_state(&result).expect("result"),
        PromiseState::Fulfilled(Value::Number(6.0))
    );
}

#[test]
fn closures_mutate_captured_bindings() {
    let source = r#"
function run() {
  let x = 1;
  function inc() {
    x = x + 2;
    return x;
  }
  const first = inc();
  inc();
  return first * 10 + x;
}
"#;
    let build = build(source);
    let mut host = TestHost::default();
    let mut vm = Vm::new(build, &host).expect("bindings");
    let result = vm.start(&mut host, "run", Vec::new()).expect("start");
    assert_eq!(result, Value::Number(35.0));
}

#[test]
fn function_expressions_escape_with_independent_environments() {
    let source = r#"
function make() {
  let n = 0;
  return function() {
    n = n + 1;
    return n;
  };
}
function run() {
  const a = make();
  a();
  a();
  const b = make();
  return a() * 10 + b();
}
"#;
    let build = build(source);
    let mut host = TestHost::default();
    let mut vm = Vm::new(build, &host).expect("bindings");
    let result = vm.start(&mut host, "run", Vec::new()).expect("start");
    assert_eq!(result, Value::Number(31.0));
}

#[test]
fn closures_and_their_environment_survive_snapshot() {
    let source = r#"
async function run() {
  let n = 1;
  function step() {
    n = n + 1;
    return n;
  }
  const first = step();
  await wait("a");
  return first * 10 + step();
}
"#;
    let build = build(source);
    let mut host = TestHost::default();
    let mut vm = Vm::new(build.clone(), &host).expect("bindings");
    let result = vm.start(&mut host, "run", Vec::new()).expect("start");
    vm.run_until_idle(&mut host).expect("first await");
    let snapshot = vm
        .snapshot(std::slice::from_ref(&result))
        .expect("snapshot");
    assert!(
        snapshot
            .objects
            .iter()
            .any(|record| matches!(record.object, Object::Function(_))),
        "the escaped closure is reachable from the suspended environment"
    );
    assert!(
        snapshot
            .objects
            .iter()
            .any(|record| matches!(record.object, Object::Environment(_))),
        "the captured environment is reachable"
    );
    let json = snapshot.to_json().expect("snapshot json");
    let snapshot = yield_vm::Snapshot::from_json(&json).expect("snapshot parse");
    let mut restored = Vm::restore(snapshot, build, &mut host).expect("restore");
    restored
        .complete_operation("wait:a", Completion::Fulfilled(number(2.0)))
        .expect("complete");
    restored.run_until_idle(&mut host).expect("finish");
    assert_eq!(
        restored.promise_state(&result).expect("result"),
        PromiseState::Fulfilled(Value::Number(23.0))
    );
}

#[test]
fn object_and_array_literals_support_member_access() {
    let source = r#"
function run() {
  const obj = { a: 1, nested: { b: 2 }, list: [3, 4] };
  obj.a = obj.a + obj.nested.b;
  obj.list[1] += 5;
  const key = "computed";
  const extra = { [key]: 7 };
  return obj.a + obj.list.length + obj.list[0] + extra[key];
}
"#;
    let build = build(source);
    let mut host = TestHost::default();
    let mut vm = Vm::new(build, &host).expect("bindings");
    let result = vm.start(&mut host, "run", Vec::new()).expect("start");
    assert_eq!(result, Value::Number(15.0));
}

#[test]
fn object_identity_survives_snapshot() {
    let source = r#"
async function run() {
  const obj = { n: 1, list: [2] };
  const alias = obj;
  await wait("a");
  alias.n = 5;
  alias.list[0] = 6;
  return obj.n + obj.list[0];
}
"#;
    let build = build(source);
    let mut host = TestHost::default();
    let mut vm = Vm::new(build.clone(), &host).expect("bindings");
    let result = vm.start(&mut host, "run", Vec::new()).expect("start");
    vm.run_until_idle(&mut host).expect("await");
    let snapshot = vm
        .snapshot(std::slice::from_ref(&result))
        .expect("snapshot");
    assert!(
        snapshot
            .objects
            .iter()
            .any(|record| matches!(record.object, Object::Plain(_)))
    );
    assert!(
        snapshot
            .objects
            .iter()
            .any(|record| matches!(record.object, Object::Array(_)))
    );
    let mut restored = Vm::restore(snapshot, build, &mut host).expect("restore");
    restored
        .complete_operation("wait:a", Completion::Fulfilled(number(1.0)))
        .expect("complete");
    restored.run_until_idle(&mut host).expect("finish");
    assert_eq!(
        restored.promise_state(&result).expect("result"),
        PromiseState::Fulfilled(Value::Number(11.0))
    );
}

#[test]
fn try_catch_finally_runs_in_order() {
    let source = r#"
function run() {
  let out = 0;
  try {
    out = 1;
    throw 7;
  } catch (e) {
    out = out + e;
  } finally {
    out = out + 100;
  }
  return out;
}
"#;
    let build = build(source);
    let mut host = TestHost::default();
    let mut vm = Vm::new(build, &host).expect("bindings");
    let result = vm.start(&mut host, "run", Vec::new()).expect("start");
    assert_eq!(result, Value::Number(108.0));
}

#[test]
fn nested_finally_propagates_a_throw_to_the_outer_catch() {
    let source = r#"
function run() {
  let side = 0;
  try {
    try {
      throw 4;
    } finally {
      side = 9;
    }
  } catch (e) {
    return side * 10 + e;
  }
}
"#;
    let build = build(source);
    let mut host = TestHost::default();
    let mut vm = Vm::new(build, &host).expect("bindings");
    let result = vm.start(&mut host, "run", Vec::new()).expect("start");
    assert_eq!(result, Value::Number(94.0));
}

#[test]
fn finally_completion_overrides_return_and_throw() {
    let source = r#"
function run() {
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
    let mut vm = Vm::new(build_return, &host).expect("bindings");
    let result = vm.start(&mut host, "run", Vec::new()).expect("start");
    assert_eq!(result, Value::Number(5.0));

    let source = r#"
function run() {
  try {
    throw 1;
  } finally {
    return 2;
  }
}
"#;
    let build_override = build(source);
    let mut vm = Vm::new(build_override, &host).expect("bindings");
    let result = vm.start(&mut host, "run", Vec::new()).expect("start");
    assert_eq!(result, Value::Number(2.0));

    let source = r#"
function run() {
  try {
    return 1;
  } finally {
    throw 3;
  }
}
"#;
    let build_throw = build(source);
    let mut vm = Vm::new(build_throw, &host).expect("bindings");
    let error = vm
        .start(&mut host, "run", Vec::new())
        .expect_err("finally throw wins");
    assert!(matches!(error, VmError::Thrown(Value::Number(3.0))));
}

#[test]
fn async_try_catch_handles_operation_rejection_after_restore() {
    let source = r#"
async function run() {
  let base = 10;
  try {
    await wait("a");
    return 0;
  } catch (e) {
    return base + e;
  }
}
"#;
    let build = build(source);
    let mut host = TestHost::default();
    let mut vm = Vm::new(build.clone(), &host).expect("bindings");
    let result = vm.start(&mut host, "run", Vec::new()).expect("start");
    vm.run_until_idle(&mut host).expect("await");
    let snapshot = vm
        .snapshot(std::slice::from_ref(&result))
        .expect("snapshot");
    let mut restored = Vm::restore(snapshot, build, &mut host).expect("restore");
    restored
        .complete_operation("wait:a", Completion::Rejected(number(5.0)))
        .expect("reject");
    restored.run_until_idle(&mut host).expect("catch");
    assert_eq!(
        restored.promise_state(&result).expect("result"),
        PromiseState::Fulfilled(Value::Number(15.0))
    );
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
    let build = build(source);
    let mut host = TestHost::default();
    let mut vm = Vm::new(build.clone(), &host).expect("bindings");
    let result = vm.start(&mut host, "run", Vec::new()).expect("start");
    vm.run_until_idle(&mut host).expect("suspend in finally");
    assert_eq!(vm.pending_operation_ids(), vec!["wait:fin".to_owned()]);
    assert_eq!(host.count("setLabel"), 0);
    let snapshot = vm
        .snapshot(std::slice::from_ref(&result))
        .expect("snapshot");
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
    let mut restored = Vm::restore(snapshot, build, &mut host).expect("restore");
    restored
        .complete_operation("wait:fin", Completion::Fulfilled(number(0.0)))
        .expect("complete");
    restored.run_until_idle(&mut host).expect("finish");
    assert_eq!(host.count("setLabel"), 1);
    assert_eq!(
        restored.promise_state(&result).expect("result"),
        PromiseState::Fulfilled(Value::Number(7.0))
    );
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
    let build = build(source);
    let mut host = TestHost::default();
    let mut vm = Vm::new(build.clone(), &host).expect("bindings");
    let result = vm.start(&mut host, "run", Vec::new()).expect("start");
    vm.run_until_idle(&mut host).expect("suspend in finally");
    let snapshot = vm
        .snapshot(std::slice::from_ref(&result))
        .expect("snapshot");
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
    let mut restored = Vm::restore(snapshot, build, &mut host).expect("restore");
    restored
        .complete_operation("wait:fin", Completion::Fulfilled(number(0.0)))
        .expect("complete");
    restored.run_until_idle(&mut host).expect("rethrow");
    assert_eq!(
        restored.promise_state(&result).expect("result"),
        PromiseState::Rejected(Value::Number(5.0))
    );
}

#[test]
fn temporary_await_results_are_still_saved_across_resume_points() {
    let source = r#"
async function run() {
  return (await wait("a")) + (await wait("b"));
}
"#;
    let build = build(source);
    let mut host = TestHost::default();
    let mut vm = Vm::new(build.clone(), &host).expect("bindings");
    let result = vm.start(&mut host, "run", Vec::new()).expect("start");
    vm.run_until_idle(&mut host).expect("first await");
    vm.complete_operation("wait:a", Completion::Fulfilled(number(4.0)))
        .expect("complete a");
    vm.run_until_idle(&mut host).expect("second await");
    let snapshot = vm
        .snapshot(std::slice::from_ref(&result))
        .expect("snapshot");
    assert_eq!(
        snapshot.continuations[0].values,
        vec![Value::Number(4.0)],
        "the first await result is live across the second await"
    );
    let mut restored = Vm::restore(snapshot, build, &mut host).expect("restore");
    restored
        .complete_operation("wait:b", Completion::Fulfilled(number(5.0)))
        .expect("complete b");
    restored.run_until_idle(&mut host).expect("finish");
    assert_eq!(
        restored.promise_state(&result).expect("result"),
        PromiseState::Fulfilled(Value::Number(9.0))
    );
}

#[test]
fn loader_rejects_handler_target_inside_its_protected_range() {
    let mut corrupt = build("function run() { try { throw 1; } catch (e) { return 2; } }");
    let function = &mut corrupt.functions[0];
    let handler = &mut function.handlers[0];
    handler.target_pc = handler.start_pc;
    corrupt.finalize();
    let host = TestHost::default();
    let error = Vm::new(corrupt, &host).expect_err("bad exception target");
    assert!(
        matches!(error, VmError::InvalidBuild(message) if message.contains("inside its own protected range"))
    );
}

#[test]
fn arrows_capture_mutable_outer_bindings() {
    let source = r#"
function run() {
  let n = 3;
  const add = (x) => x + n;
  n = 4;
  return add(1);
}
"#;
    let build = build(source);
    let mut host = TestHost::default();
    let mut vm = Vm::new(build, &host).expect("bindings");
    let result = vm.start(&mut host, "run", Vec::new()).expect("start");
    assert_eq!(result, Value::Number(5.0));
}

#[test]
fn hoisted_functions_can_capture_later_bindings() {
    let source = r#"
function run() {
  return read();
  function read() {
    return x;
  }
  let x = 4;
}
"#;
    let build = build(source);
    let mut host = TestHost::default();
    let mut vm = Vm::new(build, &host).expect("bindings");
    let error = vm.start(&mut host, "run", Vec::new()).expect_err("read");
    assert!(matches!(error, VmError::InvalidState(message) if message.contains("binding")));
}

#[test]
fn hoisted_functions_can_be_called_before_their_declaration() {
    let source = r#"
function run() {
  return read();
  function read() {
    return 4;
  }
}
"#;
    let build = build(source);
    let mut host = TestHost::default();
    let mut vm = Vm::new(build, &host).expect("bindings");
    let result = vm.start(&mut host, "run", Vec::new()).expect("start");
    assert_eq!(result, Value::Number(4.0));
}

#[test]
fn hoisted_functions_run_after_later_bindings_initialize() {
    let source = r#"
function run() {
  function read() {
    return x;
  }
  let x = 4;
  return read();
}
"#;
    let build = build(source);
    let mut host = TestHost::default();
    let mut vm = Vm::new(build, &host).expect("bindings");
    let result = vm.start(&mut host, "run", Vec::new()).expect("start");
    assert_eq!(result, Value::Number(4.0));
}

#[test]
fn function_values_stored_in_objects_are_callable() {
    let source = r#"
function run() {
  const obj = {
    f: function(x) {
      return x + 1;
    }
  };
  return obj.f(4);
}
"#;
    let build = build(source);
    let mut host = TestHost::default();
    let mut vm = Vm::new(build, &host).expect("bindings");
    let result = vm.start(&mut host, "run", Vec::new()).expect("start");
    assert_eq!(result, Value::Number(5.0));
}

#[test]
fn catch_without_binding_and_call_exceptions_work() {
    let source = r#"
function boom() {
  throw 6;
}
function run() {
  try {
    boom();
  } catch {
    return 2;
  }
}
"#;
    let build = build(source);
    let mut host = TestHost::default();
    let mut vm = Vm::new(build, &host).expect("bindings");
    let result = vm.start(&mut host, "run", Vec::new()).expect("start");
    assert_eq!(result, Value::Number(2.0));
}

#[test]
fn return_propagates_through_nested_finalizers() {
    let source = r#"
function run() {
  let log = 0;
  try {
    try {
      return 1;
    } finally {
      log = log + 10;
    }
  } finally {
    log = log + 100;
    return log;
  }
}
"#;
    let build = build(source);
    let mut host = TestHost::default();
    let mut vm = Vm::new(build, &host).expect("bindings");
    let result = vm.start(&mut host, "run", Vec::new()).expect("start");
    assert_eq!(result, Value::Number(110.0));
}

#[test]
fn recursive_arrow_functions_use_the_captured_binding() {
    let source = r#"
function run() {
  const fact = (n) => n <= 1 ? 1 : n * fact(n - 1);
  return fact(5);
}
"#;
    let build = build(source);
    let mut host = TestHost::default();
    let mut vm = Vm::new(build, &host).expect("bindings");
    let result = vm.start(&mut host, "run", Vec::new()).expect("start");
    assert_eq!(result, Value::Number(120.0));
}

#[test]
fn typeof_reports_function_values() {
    let source = r#"
function run() {
  function f() {}
  const g = () => 1;
  return typeof f + ":" + typeof g;
}
"#;
    let build = build(source);
    let mut host = TestHost::default();
    let mut vm = Vm::new(build, &host).expect("bindings");
    let result = vm.start(&mut host, "run", Vec::new()).expect("start");
    assert_eq!(result, Value::String("function:function".into()));
}

#[test]
fn async_closures_survive_snapshot_and_resume() {
    let source = r#"
async function run() {
  const f = async () => {
    const x = await wait("a");
    return x + 1;
  };
  return await f();
}
"#;
    let build = build(source);
    let mut host = TestHost::default();
    let mut vm = Vm::new(build.clone(), &host).expect("bindings");
    let result = vm.start(&mut host, "run", Vec::new()).expect("start");
    vm.run_until_idle(&mut host).expect("suspend");
    assert_eq!(vm.pending_operation_ids(), vec!["wait:a".to_owned()]);
    let snapshot = vm
        .snapshot(std::slice::from_ref(&result))
        .expect("snapshot");
    let mut restored = Vm::restore(snapshot, build, &mut host).expect("restore");
    restored
        .complete_operation("wait:a", Completion::Fulfilled(number(4.0)))
        .expect("complete");
    restored.run_until_idle(&mut host).expect("finish");
    assert_eq!(
        restored.promise_state(&result).expect("result"),
        PromiseState::Fulfilled(Value::Number(5.0))
    );
}

#[test]
fn sync_control_flow_executes_before_any_await() {
    let source = r#"
function run(n) {
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
    let mut vm = Vm::new(build, &host).expect("bindings");
    let result = vm
        .start(&mut host, "run", vec![HostValue::Number(5.0)])
        .expect("start");
    assert!(matches!(
        vm.promise_state(&result),
        Err(VmError::InvalidState(message)) if message == "value is not an object"
    ));
    assert_eq!(result, Value::Number(6.0));
}
