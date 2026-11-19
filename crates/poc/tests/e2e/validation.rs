//! The loader and snapshot APIs reject invalid inputs and state.

use super::support::{RUN, Suspended, count_delta};
use yield_poc::{TestHost, build};
use yield_vm::bytecode::{Instr, Slot};
use yield_vm::heap::EnvironmentObject;
use yield_vm::snapshot::ObjectRecord;
use yield_vm::{Object, Value, Vm, VmError};

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
    let mut s = Suspended::start(source, vec![]);
    let snapshot = s.snapshot();

    let set_label = s.host.count("setLabel");
    s.host.reject_native = true;
    let error = s
        .restore(snapshot.clone())
        .expect_err("missing native wrapper binding");
    assert!(matches!(error, VmError::MissingNative(_)));
    assert_eq!(
        count_delta(s.host.count("setLabel"), set_label),
        0,
        "restore must not run script"
    );
    s.host.reject_native = false;

    s.host.reject_restore_operation = true;
    let error = s.restore(snapshot.clone()).expect_err("missing operation");
    assert!(matches!(error, VmError::MissingOperation(id) if id == "wait:a"));
    s.host.reject_restore_operation = false;

    let mut mismatched = snapshot.clone();
    mismatched.build_id = "sha256:deadbeef".to_owned();
    let error = s.restore(mismatched).expect_err("build mismatch");
    assert!(matches!(error, VmError::InvalidSnapshot(message) if message.contains("build id")));

    let mut mismatched = snapshot.clone();
    mismatched.bytecode_version = 5;
    let error = s
        .restore(mismatched)
        .expect_err("bytecode version mismatch");
    assert!(
        matches!(error, VmError::InvalidSnapshot(message) if message.contains("bytecode version"))
    );

    let mut mismatched = snapshot;
    mismatched.format_version = 5;
    let error = s.restore(mismatched).expect_err("snapshot format mismatch");
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
    corrupt.bytecode_version = 5;
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
    let mut s = Suspended::start(RUN, vec![]);
    let snapshot = s.snapshot();

    let mut wrong_values = snapshot.clone();
    wrong_values.continuations[0]
        .values
        .push(Value::Number(999.0));
    let error = s.restore(wrong_values).expect_err("slot value count");
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
    let error = s
        .restore(broken_reactions)
        .expect_err("continuation not associated with promise");
    assert!(matches!(error, VmError::InvalidSnapshot(message) if message.contains("exactly once")));
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
fn restore_rejects_an_environment_the_function_does_not_use() {
    let source = r#"
async function run() {
  const live = {n: 7};
  await wait("a");
  return live.n;
}
"#;
    let mut s = Suspended::start(source, vec![]);
    let mut snapshot = s.snapshot();
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
    let calls = s.host.count("wait");
    let error = s.restore(snapshot).expect_err("extra environment");
    assert!(
        matches!(error, VmError::InvalidSnapshot(message) if message.contains("does not use an invocation environment"))
    );
    assert_eq!(s.host.count("wait"), calls, "restore must not run script");
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
    let mut s = Suspended::start(source, vec![]);
    let mut snapshot = s.snapshot();
    assert!(snapshot.continuations[0].env.is_some());
    snapshot.continuations[0].env = None;
    let calls = s.host.count("wait");
    let error = s.restore(snapshot).expect_err("missing environment");
    assert!(
        matches!(error, VmError::InvalidSnapshot(message) if message.contains("no invocation environment"))
    );
    assert_eq!(s.host.count("wait"), calls, "restore must not run script");
}

fn suspended_symbol_snapshot() -> (super::support::Suspended, yield_vm::Snapshot) {
    let source = r#"
async function run() {
  const key = Symbol("key");
  const obj = {};
  obj[key] = 5;
  obj[Symbol.for("k")] = 6;
  await wait("a");
  return obj[key];
}
"#;
    let s = super::support::Suspended::start(source, vec![]);
    let snapshot = s.snapshot();
    (s, snapshot)
}

fn live_symbol_id(snapshot: &yield_vm::Snapshot) -> yield_vm::value::SymbolId {
    *snapshot
        .symbols
        .iter()
        .find_map(|(id, description)| (description.as_deref() == Some("key")).then_some(id))
        .expect("live symbol record")
}

#[test]
fn symbol_snapshot_tampering_is_rejected() {
    let (mut s, snapshot) = suspended_symbol_snapshot();
    let symbol_id = live_symbol_id(&snapshot);

    let mut missing_key = snapshot.clone();
    assert!(missing_key.symbols.remove(&symbol_id).is_some());
    let error = s.restore(missing_key).expect_err("deleted live symbol");
    assert!(
        matches!(error, VmError::InvalidSnapshot(message) if message.contains("missing symbol"))
    );

    let mut iterator_registry = snapshot.clone();
    let iterator = *snapshot
        .symbols
        .iter()
        .find_map(|(id, description)| {
            (description.as_deref() == Some("Symbol.iterator")).then_some(id)
        })
        .expect("iterator symbol");
    iterator_registry
        .symbol_registry
        .insert("fake".into(), iterator);
    let error = s
        .restore(iterator_registry)
        .expect_err("registry points at iterator");
    assert!(matches!(error, VmError::InvalidSnapshot(message) if message.contains("iterator")));

    let mut lowered = snapshot.clone();
    lowered.next_symbol_id = symbol_id;
    let error = s.restore(lowered).expect_err("lowered high-water mark");
    assert!(
        matches!(error, VmError::InvalidSnapshot(message) if message.contains("next symbol id"))
    );

    let mut old_format = snapshot;
    old_format.format_version = 7;
    let error = s.restore(old_format).expect_err("format version 7");
    assert!(matches!(error, VmError::InvalidSnapshot(message) if message.contains("format")));
}

#[test]
fn symbols_never_reach_the_host_call() {
    let source = r#"
async function run() {
  return inspect(Symbol("hidden"));
}
"#;
    let mut host = TestHost::default();
    let mut vm = Vm::new(build(source), &host).expect("create VM");
    let error = vm
        .start(&mut host, "run", Vec::new())
        .expect_err("symbol argument is rejected");
    assert!(
        matches!(&error, VmError::InvalidState(message) if message.contains("Symbol")),
        "{error}"
    );
    assert_eq!(host.count("inspect"), 0, "host must not be called");
}
