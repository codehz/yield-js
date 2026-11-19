use yield_poc::{TestHost, build, number};
use yield_vm::bytecode::{Instr, Slot};
use yield_vm::heap::EnvironmentObject;
use yield_vm::snapshot::ObjectRecord;
use yield_vm::{Completion, HostValue, Object, PromiseState, Value, Vm, VmError};

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
    assert_eq!(
        snapshot.continuations[0].values,
        vec![Value::Number(10.0)],
        "base is the only value live across the first await"
    );
    assert!(
        snapshot.continuations[0].env.is_none(),
        "uncaptured locals do not keep a call environment"
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
    assert_eq!(
        snapshot.continuations[0].values,
        vec![Value::Number(12.0)],
        "sum is the only value live across the second await"
    );
    assert!(snapshot.continuations[0].env.is_none());
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
    // Scope entry initializes `x`; the later initializer never runs.
    let result = vm
        .start(&mut host, "run", Vec::new())
        .expect("captured read");
    assert_eq!(result, Value::Undefined);
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

fn plain_tags(snapshot: &yield_vm::Snapshot) -> Vec<String> {
    snapshot
        .objects
        .iter()
        .filter_map(|record| {
            let Object::Plain(plain) = &record.object else {
                return None;
            };
            match plain.props.get("tag") {
                Some(Value::String(tag)) => Some(tag.clone()),
                _ => None,
            }
        })
        .collect()
}

fn has_environment(snapshot: &yield_vm::Snapshot) -> bool {
    snapshot
        .objects
        .iter()
        .any(|record| matches!(record.object, Object::Environment(_)))
}

#[test]
fn promoted_dead_object_is_absent_from_the_snapshot_graph() {
    let source = r#"
async function run() {
  const dead = {tag: "discard-only"};
  const live = {n: 7};
  await wait("a");
  return live.n;
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
        !plain_tags(&snapshot)
            .iter()
            .any(|tag| tag == "discard-only"),
        "dead must not remain reachable through a call environment or saved slot"
    );
    assert!(!has_environment(&snapshot));
    assert!(snapshot.continuations[0].env.is_none());
    let saved_objects: Vec<_> = snapshot.continuations[0]
        .values
        .iter()
        .filter_map(|value| match value {
            Value::Object(id) => Some(*id),
            _ => None,
        })
        .collect();
    assert_eq!(
        saved_objects.len(),
        1,
        "only live is saved across the await"
    );
    assert!(snapshot.objects.iter().any(|record| {
        record.id == saved_objects[0]
            && matches!(&record.object, Object::Plain(plain) if plain.props.get("n") == Some(&Value::Number(7.0)))
    }));

    let mut restored = Vm::restore(snapshot, build, &mut host).expect("restore");
    restored
        .complete_operation("wait:a", Completion::Fulfilled(number(0.0)))
        .expect("complete");
    restored.run_until_idle(&mut host).expect("finish");
    assert_eq!(
        restored.promise_state(&result).expect("result"),
        PromiseState::Fulfilled(Value::Number(7.0))
    );
}

#[test]
fn captured_binding_keeps_object_identity_across_restore() {
    let source = r#"
async function run() {
  const dead = {tag: "discard-only"};
  const live = {n: 7};
  const alias = () => live;
  await wait("a");
  return alias() === live;
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
        !plain_tags(&snapshot)
            .iter()
            .any(|tag| tag == "discard-only")
    );
    assert!(has_environment(&snapshot));
    assert!(snapshot.continuations[0].env.is_some());

    let mut restored = Vm::restore(snapshot, build, &mut host).expect("restore");
    restored
        .complete_operation("wait:a", Completion::Fulfilled(number(0.0)))
        .expect("complete");
    restored.run_until_idle(&mut host).expect("finish");
    assert_eq!(
        restored.promise_state(&result).expect("result"),
        PromiseState::Fulfilled(Value::Bool(true))
    );
}

#[test]
fn catch_reads_object_saved_across_the_reject_edge() {
    let source = r#"
async function run() {
  const kept = {tag: "kept"};
  try {
    await wait("a");
  } catch (e) {
    return kept.tag;
  }
  return "missing";
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
    assert!(plain_tags(&snapshot).iter().any(|tag| tag == "kept"));
    let mut restored = Vm::restore(snapshot, build, &mut host).expect("restore");
    restored
        .complete_operation(
            "wait:a",
            Completion::Rejected(HostValue::String("boom".into())),
        )
        .expect("reject");
    restored.run_until_idle(&mut host).expect("catch");
    assert_eq!(
        restored.promise_state(&result).expect("result"),
        PromiseState::Fulfilled(Value::String("kept".into()))
    );
}

#[test]
fn restore_rejects_an_environment_the_function_does_not_use() {
    let source = r#"
async function run() {
  const live = {n: 7};
  await wait("a");
  return live.n;
}
"#;
    let build = build(source);
    let mut host = TestHost::default();
    let mut vm = Vm::new(build.clone(), &host).expect("bindings");
    let result = vm.start(&mut host, "run", Vec::new()).expect("start");
    vm.run_until_idle(&mut host).expect("await");
    let mut snapshot = vm.snapshot(&[result]).expect("snapshot");
    assert!(snapshot.continuations[0].env.is_none());
    let env_id = snapshot
        .objects
        .iter()
        .map(|record| record.id)
        .max()
        .unwrap_or(0)
        + 1;
    let mut bindings = std::collections::BTreeMap::new();
    bindings.insert(1, Value::Number(1.0));
    snapshot.objects.push(ObjectRecord {
        id: env_id,
        object: Object::Environment(EnvironmentObject {
            parent: None,
            bindings,
        }),
    });
    snapshot.continuations[0].env = Some(env_id);
    let calls = host.count("wait");
    let error = Vm::restore(snapshot, build, &mut host).expect_err("extra environment");
    assert!(
        matches!(error, VmError::InvalidSnapshot(message) if message.contains("does not use an invocation environment"))
    );
    assert_eq!(host.count("wait"), calls, "restore must not run script");
}

#[test]
fn restore_rejects_a_missing_environment_required_by_layout() {
    let source = r#"
async function run() {
  const live = {n: 1};
  const alias = () => live;
  await wait("a");
  return alias().n;
}
"#;
    let build = build(source);
    let mut host = TestHost::default();
    let mut vm = Vm::new(build.clone(), &host).expect("bindings");
    let result = vm.start(&mut host, "run", Vec::new()).expect("start");
    vm.run_until_idle(&mut host).expect("await");
    let mut snapshot = vm.snapshot(&[result]).expect("snapshot");
    assert!(snapshot.continuations[0].env.is_some());
    snapshot.continuations[0].env = None;
    let calls = host.count("wait");
    let error = Vm::restore(snapshot, build, &mut host).expect_err("missing environment");
    assert!(
        matches!(error, VmError::InvalidSnapshot(message) if message.contains("no invocation environment"))
    );
    assert_eq!(host.count("wait"), calls, "restore must not run script");
}

#[test]
fn script_reaction_survives_restore_and_runs_on_fulfill() {
    let source = r#"
async function run() {
  return await wait("a").then(function (value) {
    return value + 1;
  });
}
"#;
    let build = build(source);
    let mut host = TestHost::default();
    let mut vm = Vm::new(build.clone(), &host).expect("bindings");
    let result = vm.start(&mut host, "run", Vec::new()).expect("start");
    vm.run_until_idle(&mut host).expect("await");
    assert_eq!(vm.pending_operation_ids(), vec!["wait:a".to_owned()]);
    let snapshot = vm
        .snapshot(std::slice::from_ref(&result))
        .expect("snapshot");
    assert!(
        snapshot.objects.iter().any(|record| matches!(
            &record.object,
            Object::Promise(promise) if promise.reactions.iter().any(|job| matches!(job, yield_vm::heap::ReactionJob::Script { .. }))
        )),
        "then() handler is a persisted reaction"
    );
    let mut restored = Vm::restore(snapshot, build, &mut host).expect("restore");
    restored
        .complete_operation("wait:a", Completion::Fulfilled(HostValue::Number(4.0)))
        .expect("fulfill");
    restored.run_until_idle(&mut host).expect("handler");
    assert_eq!(
        restored.promise_state(&result).expect("result"),
        PromiseState::Fulfilled(Value::Number(5.0))
    );
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
    let build = build(RUN);
    let mut host = TestHost::default();
    let mut vm = Vm::new(build.clone(), &host).expect("bindings");
    let result = vm.start(&mut host, "run", Vec::new()).expect("start");
    vm.run_until_idle(&mut host).expect("await");
    let mut snapshot = vm.snapshot(&[result]).expect("snapshot");
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
    let error = Vm::restore(snapshot, build, &mut host).expect_err("locked without adopt");
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
    let build = build(source);
    let mut host = TestHost::default();
    let mut vm = Vm::new(build.clone(), &host).expect("bindings");
    let result = vm.start(&mut host, "run", Vec::new()).expect("start");
    vm.run_until_idle(&mut host).expect("await");
    let mut snapshot = vm.snapshot(&[result]).expect("snapshot");
    let handler = snapshot
        .objects
        .iter()
        .find_map(|record| match &record.object {
            Object::Function(_) => Some(record.id),
            _ => None,
        })
        .expect("then handler");
    snapshot.objects.retain(|record| record.id != handler);
    let error = Vm::restore(snapshot, build, &mut host).expect_err("missing handler");
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
    let build = build(source);
    let mut host = TestHost::default();
    let mut vm = Vm::new(build.clone(), &host).expect("bindings");
    let result = vm.start(&mut host, "run", Vec::new()).expect("start");
    vm.run_until_idle(&mut host).expect("await");
    vm.complete_operation("wait:a", Completion::Fulfilled(HostValue::Number(2.0)))
        .expect("fulfill a");
    vm.run_until_idle(&mut host).expect("partial combine");
    let snapshot = vm
        .snapshot(std::slice::from_ref(&result))
        .expect("snapshot");
    assert_eq!(snapshot.combines.len(), 1);
    let mut restored = Vm::restore(snapshot, build, &mut host).expect("restore");
    restored
        .complete_operation("wait:b", Completion::Fulfilled(HostValue::Number(3.0)))
        .expect("fulfill b");
    restored.run_until_idle(&mut host).expect("finish all");
    assert_eq!(
        restored.promise_state(&result).expect("result"),
        PromiseState::Fulfilled(Value::Number(5.0))
    );
}

#[test]
fn builtin_roots_and_function_identity_survive_restore() {
    let source = r#"
async function run() {
  const then = Promise.prototype.then;
  const fn = demo;
  await wait("a");
  return then === Promise.prototype.then && fn === demo && new demo().n;
}
function demo() {
  this.n = 4;
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
    assert_eq!(snapshot.roots.len(), 3);
    assert!(!snapshot.functions.is_empty());

    let mut missing = snapshot.clone();
    missing.roots.pop_first();
    let error = Vm::restore(missing, build.clone(), &mut host).expect_err("missing root");
    assert!(matches!(error, VmError::InvalidSnapshot(_)));

    let mut restored = Vm::restore(snapshot, build, &mut host).expect("restore");
    restored
        .complete_operation("wait:a", Completion::Fulfilled(HostValue::Number(0.0)))
        .expect("complete");
    restored.run_until_idle(&mut host).expect("finish");
    assert_eq!(
        restored.promise_state(&result).expect("result"),
        PromiseState::Fulfilled(Value::Number(4.0))
    );
}
