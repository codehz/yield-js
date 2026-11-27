//! WeakRef / FinalizationRegistry snapshot identity, keep-alives, and Full restore.

use super::support::json_round_trip;
use yield_harness::{TestHost, TestVm, build};
use yield_vm::heap::{Object, WeakKey};
use yield_vm::snapshot::OrdinaryTask;
use yield_vm::{RestoreOptions, Snapshot, SnapshotValidation, Value, ValueRef, Vm, VmError};

fn full() -> RestoreOptions {
    RestoreOptions {
        validation: SnapshotValidation::Full,
        ..Default::default()
    }
}

pub(super) fn restore(host: &TestHost, artifact: yield_vm::Build, snapshot: Snapshot) -> TestVm {
    let mut vm = Vm::restore(snapshot, artifact, host.host(), Default::default()).unwrap();
    vm.start_recovered_operations().unwrap();
    vm
}

pub(super) fn restore_full(
    host: &TestHost,
    artifact: yield_vm::Build,
    snapshot: Snapshot,
) -> Result<TestVm, VmError> {
    Vm::restore(snapshot, artifact, host.host(), full())
}

const SOURCE: &str = r#"
let target = { tag: 1 };
export const held = { tag: "held" };
export const log = [];
export const registry = new FinalizationRegistry((value) => { log.push(value); });
registry.register(target, held);
export const ref = new WeakRef(target);
export function dropTarget() { target = undefined; }
export function deref() { return ref.deref(); }
export function logLength() { return log.length; }
"#;

#[test]
fn weak_refs_capture_before_gc_does_not_mutate_source() {
    let artifact = build(SOURCE);
    let host = TestHost::default();
    let mut vm = Vm::new(artifact.clone(), host.host(), Default::default()).unwrap();
    vm.start("dropTarget", vec![]).unwrap();
    vm.run_until_idle().unwrap();
    let roots = [
        vm.get_export("registry").unwrap(),
        vm.get_export("ref").unwrap(),
        vm.get_export("held").unwrap(),
        vm.get_export("log").unwrap(),
    ];
    let snapshot = vm.snapshot(&roots).unwrap();
    assert_ne!(vm.start("deref", vec![]).unwrap(), Value::UNDEFINED);
    assert_eq!(vm.start("logLength", vec![]).unwrap(), Value::number(0.0));

    for image in round_trip(&snapshot) {
        let mut restored = restore(&host, artifact.clone(), image);
        assert_eq!(
            restored.start("logLength", vec![]).unwrap(),
            Value::number(0.0)
        );
        assert_eq!(restored.start("deref", vec![]).unwrap(), Value::UNDEFINED);
        restored.run_until_idle().unwrap();
        assert_eq!(
            restored.start("logLength", vec![]).unwrap(),
            Value::number(1.0)
        );
        restored.run_until_idle().unwrap();
        assert_eq!(
            restored.start("logLength", vec![]).unwrap(),
            Value::number(1.0)
        );
    }
}

#[test]
fn weak_refs_capture_after_gc_before_delivery() {
    let artifact = build(SOURCE);
    let host = TestHost::default();
    let mut vm = Vm::new(artifact.clone(), host.host(), Default::default()).unwrap();
    vm.start("dropTarget", vec![]).unwrap();
    vm.run_until_idle().unwrap();
    let roots = [
        vm.get_export("registry").unwrap(),
        vm.get_export("ref").unwrap(),
        vm.get_export("held").unwrap(),
        vm.get_export("log").unwrap(),
    ];
    vm.collect_garbage(&roots).unwrap();
    assert_eq!(vm.start("deref", vec![]).unwrap(), Value::UNDEFINED);
    assert_eq!(vm.start("logLength", vec![]).unwrap(), Value::number(0.0));
    let snapshot = vm.snapshot(&roots).unwrap();
    assert_eq!(vm.start("logLength", vec![]).unwrap(), Value::number(0.0));

    for image in round_trip(&snapshot) {
        let mut restored = restore(&host, artifact.clone(), image);
        assert_eq!(
            restored.start("logLength", vec![]).unwrap(),
            Value::number(0.0)
        );
        restored.run_until_idle().unwrap();
        assert_eq!(
            restored.start("logLength", vec![]).unwrap(),
            Value::number(1.0)
        );
    }
}

#[test]
fn weak_refs_explicit_snapshot_root_keeps_target_identity() {
    let artifact = build(SOURCE);
    let host = TestHost::default();
    let mut vm = Vm::new(artifact.clone(), host.host(), Default::default()).unwrap();
    let target = vm.start("deref", vec![]).unwrap();
    vm.start("dropTarget", vec![]).unwrap();
    vm.run_until_idle().unwrap();
    let roots = [
        vm.get_export("registry").unwrap(),
        vm.get_export("ref").unwrap(),
        target.clone(),
    ];
    vm.collect_garbage(&roots).unwrap();
    let snapshot = vm.snapshot(&roots).unwrap();
    let mut restored = restore(&host, artifact, snapshot);
    assert_eq!(restored.start("deref", vec![]).unwrap(), target);
    restored.run_until_idle().unwrap();
    assert_eq!(
        restored.start("logLength", vec![]).unwrap(),
        Value::number(0.0)
    );
}

#[test]
fn weak_refs_dead_registry_is_not_kept_by_queued_cleanup() {
    let artifact = build(
        r#"
let registry = new FinalizationRegistry(() => {});
let target = {};
registry.register(target, { held: true });
export const ref = new WeakRef(target);
export function dropAll() { registry = undefined; target = undefined; }
export function deref() { return ref.deref(); }
"#,
    );
    let host = TestHost::default();
    let mut vm = Vm::new(artifact, host.host(), Default::default()).unwrap();
    vm.start("dropAll", vec![]).unwrap();
    vm.run_until_idle().unwrap();
    let reference = vm.get_export("ref").unwrap();
    vm.collect_garbage(std::slice::from_ref(&reference))
        .unwrap();
    let snapshot = vm.snapshot(&[reference]).unwrap();
    assert!(
        !snapshot
            .objects
            .iter()
            .any(|record| { matches!(record.object, Object::FinalizationRegistry { .. }) })
    );
    assert!(
        !snapshot
            .ordinary_tasks
            .iter()
            .any(|task| matches!(task, OrdinaryTask::FinalizationCleanup(_)))
    );
}

#[test]
fn weak_refs_kept_objects_round_trip_without_checkpoint() {
    let artifact = build(
        r#"
export function make() {
  const target = { live: true };
  return [new WeakRef(target), target];
}
"#,
    );
    let host = TestHost::default();
    let mut vm = Vm::new(artifact.clone(), host.host(), Default::default()).unwrap();
    let pair = vm.start("make", vec![]).unwrap();
    let snapshot = vm.snapshot(&[pair]).unwrap();
    assert!(!snapshot.kept_objects.is_empty());
    let decoded = json_round_trip(&snapshot);
    assert_eq!(decoded.kept_objects, snapshot.kept_objects);
    restore_full(&host, artifact, decoded).unwrap();
}

#[test]
fn weak_refs_dead_token_does_not_keep_the_token_object() {
    let artifact = build(
        r#"
let token = { token: true };
export const registry = new FinalizationRegistry(() => {});
export const target = {};
registry.register(target, "held", token);
export function dropToken() { token = undefined; }
"#,
    );
    let host = TestHost::default();
    let mut vm = Vm::new(artifact, host.host(), Default::default()).unwrap();
    vm.start("dropToken", vec![]).unwrap();
    vm.run_until_idle().unwrap();
    let roots = [
        vm.get_export("registry").unwrap(),
        vm.get_export("target").unwrap(),
    ];
    vm.collect_garbage(&roots).unwrap();
    let snapshot = vm.snapshot(&roots).unwrap();
    let ValueRef::Object(id) = vm.get_export("registry").unwrap().view() else {
        panic!("registry");
    };
    let record = snapshot
        .objects
        .iter()
        .find(|record| record.id == id)
        .unwrap();
    let Object::FinalizationRegistry { cells, .. } = &record.object else {
        panic!("registry object");
    };
    assert!(cells[0].token.is_none());
}

#[test]
fn weak_refs_full_restore_rejects_corrupt_weak_state() {
    let artifact = build(SOURCE);
    let host = TestHost::default();
    let vm = Vm::new(artifact.clone(), host.host(), Default::default()).unwrap();
    let snapshot = vm.snapshot(&[]).unwrap();
    let ValueRef::Object(ref_id) = vm.get_export("ref").unwrap().view() else {
        panic!("ref");
    };
    let ValueRef::Object(reg_id) = vm.get_export("registry").unwrap().view() else {
        panic!("registry");
    };

    let mut bad_target = snapshot.clone();
    let record = bad_target
        .objects
        .iter_mut()
        .find(|r| r.id == ref_id)
        .unwrap();
    if let Object::WeakRef { target, .. } = &mut record.object {
        *target = Some(WeakKey::Object(u32::MAX));
    }
    assert!(matches!(
        restore_full(&host, artifact.clone(), bad_target),
        Err(VmError::InvalidSnapshot(_))
    ));

    let mut bad_symbol = snapshot.clone();
    let record = bad_symbol
        .objects
        .iter_mut()
        .find(|r| r.id == ref_id)
        .unwrap();
    if let Object::WeakRef { target, .. } = &mut record.object {
        *target = Some(WeakKey::Symbol(u32::MAX));
    }
    assert!(matches!(
        restore_full(&host, artifact.clone(), bad_symbol),
        Err(VmError::InvalidSnapshot(_))
    ));

    let mut bad_callback = snapshot.clone();
    let record = bad_callback
        .objects
        .iter_mut()
        .find(|r| r.id == reg_id)
        .unwrap();
    if let Object::FinalizationRegistry { callback, .. } = &mut record.object {
        *callback = Value::number(1.0);
    }
    assert!(matches!(
        restore_full(&host, artifact.clone(), bad_callback),
        Err(VmError::InvalidSnapshot(_))
    ));

    let mut duplicate = snapshot.clone();
    duplicate
        .ordinary_tasks
        .push(OrdinaryTask::FinalizationCleanup(reg_id));
    duplicate
        .ordinary_tasks
        .push(OrdinaryTask::FinalizationCleanup(reg_id));
    assert!(matches!(
        restore_full(&host, artifact.clone(), duplicate),
        Err(VmError::InvalidSnapshot(_))
    ));

    let mut missing = snapshot;
    if let Object::FinalizationRegistry { cells, .. } = &mut missing
        .objects
        .iter_mut()
        .find(|r| r.id == reg_id)
        .unwrap()
        .object
        && let Some(cell) = cells.first_mut()
    {
        cell.target = None;
    }
    missing
        .ordinary_tasks
        .retain(|task| !matches!(task, OrdinaryTask::FinalizationCleanup(_)));
    assert!(matches!(
        restore_full(&host, artifact, missing),
        Err(VmError::InvalidSnapshot(_))
    ));
}

pub(super) fn round_trip(snapshot: &Snapshot) -> [Snapshot; 2] {
    [
        json_round_trip(snapshot),
        Snapshot::from_bytes(&snapshot.to_bytes().unwrap()).unwrap(),
    ]
}
