//! Host `JsWeak` slots: GC, snapshot capture without sweep, Full restore.

use super::weak_refs::{restore, restore_full, round_trip};
use yield_harness::{TestHost, build};
use yield_vm::heap::WeakKey;
use yield_vm::snapshot::HostSlotRecord;
use yield_vm::{Snapshot, Value, Vm, VmError};

const HOST_WEAK_SOURCE: &str = r#"
import { bind, getHostWeak, setHostWeak } from "test";
export let owner = bind("owner");
void Symbol.for("registered");
let extraObject;
let extraSymbol;
export function holdObject() {
  extraObject = { tag: 1 };
  extraObject.owner = owner;
  owner.setWeak(extraObject);
  setHostWeak(extraObject);
  return extraObject;
}
export function holdSymbol() {
  extraSymbol = Symbol("host-weak");
  owner.setWeak(extraSymbol);
  setHostWeak(extraSymbol);
  return extraSymbol;
}
export function dropObject() { extraObject = undefined; }
export function dropSymbol() { extraSymbol = undefined; }
export function readInstance() { return owner.getWeak(); }
export function readModule() { return getHostWeak(); }
"#;

fn instance_weak_slot(snapshot: &Snapshot) -> &HostSlotRecord {
    snapshot
        .host_instances
        .iter()
        .find(|record| record.class == "Node")
        .expect("Node instance")
        .slots
        .last()
        .expect("instance weak slot")
}

fn module_weak_slot(snapshot: &Snapshot) -> &HostSlotRecord {
    snapshot
        .module_states
        .iter()
        .find(|record| record.module == "test")
        .expect("test module")
        .slots
        .last()
        .expect("module weak slot")
}

fn set_instance_weak(snapshot: &mut Snapshot, slot: HostSlotRecord) {
    *snapshot
        .host_instances
        .iter_mut()
        .find(|record| record.class == "Node")
        .expect("Node instance")
        .slots
        .last_mut()
        .expect("instance weak slot") = slot;
}

fn set_module_weak(snapshot: &mut Snapshot, slot: HostSlotRecord) {
    *snapshot
        .module_states
        .iter_mut()
        .find(|record| record.module == "test")
        .expect("test module")
        .slots
        .last_mut()
        .expect("module weak slot") = slot;
}

#[test]
fn host_weak_slot_gc_keeps_rooted_targets() {
    let artifact = build(HOST_WEAK_SOURCE);
    let host = TestHost::default();
    let mut vm = Vm::new(artifact, host.host(), Default::default()).unwrap();
    let object = vm.start("holdObject", vec![]).unwrap();
    let owner = vm.get_export("owner").unwrap();
    vm.collect_garbage(&[owner.clone(), object.clone()])
        .unwrap();
    assert_eq!(vm.start("readInstance", vec![]).unwrap(), object);
    assert_eq!(vm.start("readModule", vec![]).unwrap(), object);

    let symbol = vm.start("holdSymbol", vec![]).unwrap();
    let owner = vm.get_export("owner").unwrap();
    vm.collect_garbage(&[owner, symbol.clone()]).unwrap();
    assert_eq!(vm.start("readInstance", vec![]).unwrap(), symbol);
    assert_eq!(vm.start("readModule", vec![]).unwrap(), symbol);
}

#[test]
fn host_weak_slot_gc_clears_unrooted_targets() {
    let artifact = build(HOST_WEAK_SOURCE);
    let host = TestHost::default();
    let mut vm = Vm::new(artifact, host.host(), Default::default()).unwrap();
    vm.start("holdObject", vec![]).unwrap();
    vm.start("dropObject", vec![]).unwrap();
    let owner = vm.get_export("owner").unwrap();
    vm.collect_garbage(std::slice::from_ref(&owner)).unwrap();
    assert_eq!(vm.start("readInstance", vec![]).unwrap(), Value::UNDEFINED);
    assert_eq!(vm.start("readModule", vec![]).unwrap(), Value::UNDEFINED);

    vm.start("holdSymbol", vec![]).unwrap();
    vm.start("dropSymbol", vec![]).unwrap();
    vm.collect_garbage(&[owner]).unwrap();
    assert_eq!(vm.start("readInstance", vec![]).unwrap(), Value::UNDEFINED);
    assert_eq!(vm.start("readModule", vec![]).unwrap(), Value::UNDEFINED);
}

#[test]
fn host_weak_slot_capture_without_sweep_does_not_mutate_source() {
    let artifact = build(HOST_WEAK_SOURCE);
    let host = TestHost::default();
    let mut vm = Vm::new(artifact.clone(), host.host(), Default::default()).unwrap();
    let object = vm.start("holdObject", vec![]).unwrap();
    vm.start("dropObject", vec![]).unwrap();
    let owner = vm.get_export("owner").unwrap();

    let with_root = vm.snapshot(&[owner.clone(), object.clone()]).unwrap();
    assert!(matches!(
        instance_weak_slot(&with_root),
        HostSlotRecord::JsWeak(Some(_))
    ));
    assert!(matches!(
        module_weak_slot(&with_root),
        HostSlotRecord::JsWeak(Some(_))
    ));
    for image in round_trip(&with_root) {
        let mut restored = restore(&host, artifact.clone(), image);
        assert_eq!(restored.start("readInstance", vec![]).unwrap(), object);
        assert_eq!(restored.start("readModule", vec![]).unwrap(), object);
    }
    let without_root = vm.snapshot(std::slice::from_ref(&owner)).unwrap();
    assert!(matches!(
        instance_weak_slot(&without_root),
        HostSlotRecord::JsWeak(None)
    ));
    assert!(matches!(
        module_weak_slot(&without_root),
        HostSlotRecord::JsWeak(None)
    ));
    for image in round_trip(&without_root) {
        let mut restored = restore(&host, artifact.clone(), image);
        assert_eq!(
            restored.start("readInstance", vec![]).unwrap(),
            Value::UNDEFINED
        );
        assert_eq!(
            restored.start("readModule", vec![]).unwrap(),
            Value::UNDEFINED
        );
    }

    let again = vm.snapshot(&[owner, object.clone()]).unwrap();
    assert!(matches!(
        instance_weak_slot(&again),
        HostSlotRecord::JsWeak(Some(_))
    ));
    let mut restored = restore(&host, artifact, again);
    assert_eq!(restored.start("readInstance", vec![]).unwrap(), object);
}

#[test]
fn host_weak_slot_full_restore_rejects_corrupt_records() {
    let artifact = build(HOST_WEAK_SOURCE);
    let host = TestHost::default();
    let mut vm = Vm::new(artifact.clone(), host.host(), Default::default()).unwrap();
    let object = vm.start("holdObject", vec![]).unwrap();
    let owner = vm.get_export("owner").unwrap();
    let snapshot = vm.snapshot(&[owner, object]).unwrap();

    let mut missing = snapshot.clone();
    set_instance_weak(
        &mut missing,
        HostSlotRecord::JsWeak(Some(WeakKey::Object(u32::MAX))),
    );
    set_module_weak(
        &mut missing,
        HostSlotRecord::JsWeak(Some(WeakKey::Object(u32::MAX))),
    );
    assert!(matches!(
        restore_full(&host, artifact.clone(), missing),
        Err(VmError::InvalidSnapshot(_))
    ));

    let mut registered = snapshot.clone();
    let registered_id = *registered
        .symbol_registry
        .values()
        .next()
        .expect("registered symbol");
    set_instance_weak(
        &mut registered,
        HostSlotRecord::JsWeak(Some(WeakKey::Symbol(registered_id))),
    );
    assert!(matches!(
        restore_full(&host, artifact.clone(), registered),
        Err(VmError::InvalidSnapshot(_))
    ));

    let mut wrong_kind = snapshot;
    set_instance_weak(&mut wrong_kind, HostSlotRecord::Js(Value::UNDEFINED));
    assert!(matches!(
        restore_full(&host, artifact, wrong_kind),
        Err(VmError::InvalidSnapshot(_))
    ));
}
