//! The loader and snapshot APIs reject invalid inputs and state.

use super::support::{RUN, Suspended, compile_main, count_delta, json_round_trip};
use yield_harness::{TestHost, build};
use yield_vm::bytecode::{Instr, ModuleTarget, Slot};
use yield_vm::heap::EnvironmentObject;
use yield_vm::snapshot::ObjectRecord;
use yield_vm::{
    Completion, JsString, JsValue, Object, PromiseState, Snapshot, Value, VerifiedBuild, Vm,
    VmError,
};

#[test]
fn sparse_function_ids_work_after_restore() {
    let mut build = build(
        r#"
import { wait } from "test";
function add_one(value) { return value + 1; }
export async function run() {
    const before = add_one(20);
    return before + add_one(await wait("sparse"));
}
"#,
    );
    let sparse = |id: u32| u32::MAX - id;
    for function in &mut build.functions {
        function.func_id = sparse(function.func_id);
        for instruction in &mut function.code {
            if let Instr::Closure { function, .. } = instruction {
                *function = sparse(*function);
            }
        }
    }
    for module in &mut build.modules {
        module.declaration = sparse(module.declaration);
        module.evaluation = sparse(module.evaluation);
        for export in module.exports.values_mut() {
            export.getter = sparse(export.getter);
        }
    }
    build
        .functions
        .sort_by_key(|function| std::cmp::Reverse(function.func_id));
    build.functions.rotate_left(1);
    build.finalize();

    let mut host = TestHost::default();
    let mut vm = Vm::new(build.clone(), &mut host).expect("load sparse function IDs");
    let result = vm.start(&mut host, "run", vec![]).expect("start");
    vm.run_until_idle(&mut host).expect("suspend");
    let snapshot = json_round_trip(
        &vm.snapshot(std::slice::from_ref(&result))
            .expect("snapshot"),
    );
    let mut vm = Vm::restore(snapshot, build, &mut host).expect("restore sparse function IDs");
    vm.start_recovered_operations().expect("restart operation");
    host.complete(
        &mut vm,
        "wait:sparse",
        Completion::Fulfilled(JsValue::number(20.0)),
    )
    .expect("complete operation");
    vm.run_until_idle(&mut host).expect("resume");
    assert_eq!(
        vm.promise_state(&result).unwrap(),
        PromiseState::Fulfilled(Value::number(42.0))
    );
}

#[test]
fn verified_build_shares_code_but_not_module_state() {
    let source = "let n = 0; export function run() { return ++n; }";
    let program = VerifiedBuild::new(build(source)).expect("verify build once");
    let mut host_a = TestHost::default();
    let mut host_b = TestHost::default();
    let mut a = Vm::new_verified(program.clone(), &mut host_a).unwrap();
    let mut b = Vm::new_verified(program.clone(), &mut host_b).unwrap();
    assert_eq!(
        a.start(&mut host_a, "run", vec![]).unwrap(),
        Value::number(1.0)
    );
    assert_eq!(
        a.start(&mut host_a, "run", vec![]).unwrap(),
        Value::number(2.0)
    );
    assert_eq!(
        b.start(&mut host_b, "run", vec![]).unwrap(),
        Value::number(1.0)
    );

    let snapshot = json_round_trip(&a.snapshot(&[]).unwrap());
    let different = VerifiedBuild::new(build("export function run() { return 0; }")).unwrap();
    assert!(matches!(
        Vm::restore_verified(snapshot.clone(), different, &mut host_b),
        Err(VmError::InvalidSnapshot(detail)) if detail.contains("build id mismatch")
    ));
    let mut restored = Vm::restore_verified(snapshot, program, &mut host_b).unwrap();
    assert_eq!(
        restored.start(&mut host_b, "run", vec![]).unwrap(),
        Value::number(3.0)
    );
}

#[test]
fn verified_build_checks_identity_and_structure() {
    let mut corrupt = build("export function run() { return 0; }");
    corrupt.entry = u32::MAX;
    assert!(matches!(
        VerifiedBuild::new(corrupt.clone()),
        Err(VmError::InvalidBuild(_))
    ));
    corrupt.finalize();
    assert!(matches!(
        VerifiedBuild::new(corrupt),
        Err(VmError::InvalidBuild(_))
    ));
}

#[test]
fn binary_snapshot_restores_suspended_values_and_object_identity() {
    let mut run = Suspended::start(
        r#"
import { wait } from "test";
const pattern = /ab/g;
export async function run() {
    const key = Symbol.for("\uD800");
    const object = { [key]: -0 };
    const weak = new WeakMap();
    weak.set(object, 4294967297n);
    const bytes = new Uint8Array([7, 255]);
    const resumed = await wait("binary");
    return resumed === 42 && pattern.test("ab") && pattern.lastIndex === 2 &&
        weak.get(object) === 4294967297n && 1 / object[key] === -Infinity &&
        key === Symbol.for("\uD800") && bytes[1] === 255;
}
"#,
        vec![],
    );
    let snapshot = run.snapshot();
    let encoded = snapshot.to_bytes().unwrap();
    let decoded = Snapshot::from_bytes(&encoded).unwrap();
    assert_eq!(decoded.to_json().unwrap(), snapshot.to_json().unwrap());
    let mut mismatched = decoded.clone();
    mismatched.build_id.push('x');
    assert!(matches!(
        run.restore(Snapshot::from_bytes(&mismatched.to_bytes().unwrap()).unwrap()),
        Err(VmError::InvalidSnapshot(detail)) if detail.contains("build id mismatch")
    ));
    run.restore(decoded).unwrap();
    run.complete("wait:binary", Completion::Fulfilled(JsValue::number(42.0)));
    assert_eq!(run.state(), PromiseState::Fulfilled(Value::boolean(true)));
}

#[test]
fn binary_snapshot_rejects_invalid_envelopes() {
    let encoded = Snapshot::new("test".into()).to_bytes().unwrap();
    assert!(
        Snapshot::from_bytes(&encoded[..8])
            .unwrap_err()
            .contains("too short")
    );
    let mut corrupted = encoded.clone();
    corrupted[0] ^= 1;
    assert!(
        Snapshot::from_bytes(&corrupted)
            .unwrap_err()
            .contains("magic")
    );
    corrupted = encoded.clone();
    corrupted[8] ^= 128;
    assert!(
        Snapshot::from_bytes(&corrupted)
            .unwrap_err()
            .contains("binary snapshot version")
    );
    corrupted = encoded.clone();
    corrupted[12] ^= 128;
    assert!(
        Snapshot::from_bytes(&corrupted)
            .unwrap_err()
            .contains("snapshot format")
    );
    corrupted = encoded.clone();
    corrupted[16] ^= 1;
    assert!(
        Snapshot::from_bytes(&corrupted)
            .unwrap_err()
            .contains("header does not match")
    );
    corrupted = encoded;
    corrupted.push(0);
    assert!(Snapshot::from_bytes(&corrupted).is_err());
}

#[test]
fn loader_rejects_private_index_outside_its_lexical_class() {
    let mut corrupt = build("class A { #x; read(){ return this.#x; } } class B { #x; #y; }");
    let index = corrupt
        .functions
        .iter_mut()
        .flat_map(|f| &mut f.code)
        .find_map(|instr| {
            if let Instr::GetPrivate { index, .. } = instr {
                Some(index)
            } else {
                None
            }
        })
        .expect("private access");
    *index = 1; // An index in B, but not in the class this instruction belongs to.
    corrupt.finalize();
    assert!(matches!(Vm::new(corrupt, &mut TestHost::default()),
        Err(VmError::InvalidBuild(detail)) if detail.contains("invalid class private index")));
}

#[test]
fn loader_rejects_invalid_class_constructor_reference() {
    let mut corrupt = build("export class C {}");
    let constructor = corrupt
        .functions
        .iter_mut()
        .flat_map(|f| &mut f.code)
        .find_map(|instr| {
            if let Instr::CreateClass { constructor, .. } = instr {
                Some(constructor)
            } else {
                None
            }
        })
        .expect("class instruction");
    *constructor = u32::MAX;
    corrupt.finalize();
    assert!(matches!(Vm::new(corrupt, &mut TestHost::default()),
        Err(VmError::InvalidBuild(detail)) if detail.contains("invalid class constructor")));
}

#[test]
fn host_identifiers_require_well_formed_utf16() {
    let source = r#"
import { wait } from "test";
export async function run(value) {
  try {
    await wait(value);
  } catch (error) {
    return error instanceof TypeError;
  }
  return false;
}
"#;
    let build = build(source);
    let mut host = TestHost::default();
    let mut vm = Vm::new(build, &mut host).expect("bindings");
    let lone = JsValue::string(JsString::from_units(vec![0xD800]));
    let result = vm.start(&mut host, "run", vec![lone]).expect("start");
    vm.run_until_idle(&mut host).expect("drain");
    assert_eq!(
        vm.promise_state(&result).unwrap(),
        PromiseState::Fulfilled(Value::boolean(true))
    );

    let _result = vm
        .start(&mut host, "run", vec![JsValue::string("😀")])
        .expect("start");
    vm.run_until_idle(&mut host).expect("suspend");
    assert_eq!(host.pending_labels(), vec!["wait:😀".to_owned()]);
}

#[test]
fn missing_bindings_and_mismatched_builds_are_rejected_before_execution() {
    let source = r#"
import { bind, setLabel, wait } from "test";
export async function run() {
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
    s.host.labels().set_reject_restore_operation(true);
    s.restore(snapshot.clone())
        .expect("restore publishes the VM");
    s.vm.run_until_idle(&mut s.host)
        .expect("settle rejected start");
    assert_eq!(
        s.state(),
        PromiseState::Rejected(Value::string("wait rejected".into()))
    );
    assert_eq!(
        count_delta(s.host.count("setLabel"), set_label),
        0,
        "restore must not run script"
    );
    s.host.labels().set_reject_restore_operation(false);

    let mut mismatched = snapshot.clone();
    mismatched.binding_fingerprint = "deadbeef".to_owned();
    let error = s.restore(mismatched).expect_err("binding mismatch");
    assert!(
        matches!(error, VmError::InvalidSnapshot(message) if message.contains("binding fingerprint"))
    );

    let mut mismatched = snapshot.clone();
    mismatched.build_id = "xxh3-128:deadbeef".to_owned();
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
export async function run() {
  await 1;
  return 2;
}
"#;
    let build = build(source);
    let mut host = TestHost::default();
    let mut vm = Vm::new(build, &mut host).expect("bindings");
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
    let error = Vm::new(corrupt, &mut host).expect_err("bytecode version");
    assert!(
        matches!(error, VmError::InvalidBuild(message) if message.contains("bytecode version"))
    );

    let mut corrupt = build("function run() { return 1; }");
    let run = corrupt
        .functions
        .iter_mut()
        .find(|f| f.name == "run")
        .unwrap();
    run.code[0] = Instr::Const {
        dst: Slot::transient(u16::MAX),
        constant: 0,
    };
    corrupt.finalize();
    let error = Vm::new(corrupt, &mut host).expect_err("out-of-range slot");
    assert!(matches!(error, VmError::InvalidBuild(message) if message.contains("out-of-range")));

    let mut corrupt = build(RUN);
    corrupt
        .functions
        .iter_mut()
        .find(|f| f.name == "run")
        .unwrap()
        .resumes[0]
        .saved_slots
        .push(u16::MAX);
    corrupt.finalize();
    let error = Vm::new(corrupt, &mut host).expect_err("bad saved slot");
    assert!(
        matches!(error, VmError::InvalidBuild(message) if message.contains("saves persistent slot") || message.contains("not strictly"))
    );

    let mut corrupt = build("function run() { return 1; }");
    let run = corrupt
        .functions
        .iter_mut()
        .find(|f| f.name == "run")
        .unwrap();
    run.code[0] = Instr::Jump { target: u32::MAX };
    corrupt.finalize();
    let error = Vm::new(corrupt, &mut host).expect_err("bad jump target");
    assert!(matches!(error, VmError::InvalidBuild(message) if message.contains("targets pc")));
}

#[test]
fn loader_rejects_duplicate_generator_start_boundaries() {
    let mut artifact = build("function* run() { yield 1; }");
    artifact.validate().expect("valid generator bytecode");

    let run = artifact
        .functions
        .iter_mut()
        .find(|function| function.name == "run")
        .unwrap();
    let instruction = run
        .code
        .iter_mut()
        .find(|instruction| matches!(instruction, Instr::Yield { .. }))
        .expect("generator has a Yield");
    *instruction = Instr::GeneratorStart;
    artifact.finalize();

    let error = artifact.validate().expect_err("duplicate GeneratorStart");
    assert!(
        error.ends_with("generator needs exactly one start boundary"),
        "{error}"
    );
}

#[test]
fn loader_rejects_out_of_range_import_indices() {
    let mut host = TestHost::default();
    let mut corrupt = build("import { inspect } from 'test'; inspect(1);");
    let get_import = corrupt
        .functions
        .iter_mut()
        .flat_map(|function| &mut function.code)
        .find(|instruction| matches!(instruction, Instr::GetImport { .. }))
        .expect("GetImport instruction");
    let Instr::GetImport { import, .. } = get_import else {
        unreachable!()
    };
    import.module = u32::MAX;
    corrupt.finalize();
    assert!(matches!(
        Vm::new(corrupt, &mut host),
        Err(VmError::InvalidBuild(message)) if message.contains("GetImport") && message.contains("module")
    ));

    let mut corrupt = build("import { inspect } from 'test'; inspect(1);");
    let get_import = corrupt
        .functions
        .iter_mut()
        .flat_map(|function| &mut function.code)
        .find(|instruction| matches!(instruction, Instr::GetImport { .. }))
        .expect("GetImport instruction");
    let Instr::GetImport { import, .. } = get_import else {
        unreachable!()
    };
    import.export = Some(u32::MAX);
    corrupt.finalize();
    assert!(matches!(
        Vm::new(corrupt, &mut host),
        Err(VmError::InvalidBuild(message)) if message.contains("GetImport") && message.contains("export")
    ));
}

#[test]
fn corrupted_snapshot_associations_are_rejected() {
    let mut s = Suspended::start(RUN, vec![]);
    let snapshot = s.snapshot();

    let mut wrong_values = snapshot.clone();
    wrong_values.continuations[0]
        .values
        .push(Value::number(999.0));
    let error = s
        .restore_validated(wrong_values)
        .expect_err("slot value count");
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
        .restore_validated(broken_reactions)
        .expect_err("continuation not associated with promise");
    assert!(matches!(error, VmError::InvalidSnapshot(message) if message.contains("exactly once")));
}

#[test]
fn loader_rejects_handler_target_inside_its_protected_range() {
    let mut corrupt = build("function run() { try { throw 1; } catch (e) { return 2; } }");
    let function = corrupt
        .functions
        .iter_mut()
        .find(|f| f.name == "run")
        .unwrap();
    let handler = &mut function.handlers[0];
    handler.target_pc = handler.start_pc;
    corrupt.finalize();
    let mut host = TestHost::default();
    let error = Vm::new(corrupt, &mut host).expect_err("bad exception target");
    assert!(
        matches!(error, VmError::InvalidBuild(message) if message.contains("inside its own protected range"))
    );
}

#[test]
fn restore_rejects_an_environment_the_function_does_not_use() {
    let source = r#"
import { wait } from "test";
export async function run() {
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
    bindings.insert(1, Value::number(1.0));
    snapshot.objects.push(ObjectRecord {
        id: env_id,
        object: Object::Environment(EnvironmentObject::new(None, bindings)),
    });
    snapshot.continuations[0].env = Some(env_id);
    let calls = s.host.count("wait");
    let error = s
        .restore_validated(snapshot)
        .expect_err("extra environment");
    assert!(
        matches!(error, VmError::InvalidSnapshot(message) if message.contains("does not use an invocation environment"))
    );
    assert_eq!(s.host.count("wait"), calls, "restore must not run script");
}

#[test]
fn restore_rejects_a_missing_environment_required_by_layout() {
    let source = r#"
import { wait } from "test";
export async function run() {
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
    let error = s
        .restore_validated(snapshot)
        .expect_err("missing environment");
    assert!(
        matches!(error, VmError::InvalidSnapshot(message) if message.contains("no invocation environment"))
    );
    assert_eq!(s.host.count("wait"), calls, "restore must not run script");
}

fn suspended_symbol_snapshot() -> (super::support::Suspended, yield_vm::Snapshot) {
    let source = r#"
import { wait } from "test";
export async function run() {
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
        .find_map(|(id, description)| {
            (description.as_ref() == Some(&JsString::from("key"))).then_some(id)
        })
        .expect("live symbol record")
}

#[test]
fn symbol_snapshot_tampering_is_rejected() {
    let (mut s, snapshot) = suspended_symbol_snapshot();
    let symbol_id = live_symbol_id(&snapshot);

    let mut missing_key = snapshot.clone();
    assert!(missing_key.symbols.remove(&symbol_id).is_some());
    let error = s
        .restore_validated(missing_key)
        .expect_err("deleted live symbol");
    assert!(
        matches!(error, VmError::InvalidSnapshot(message) if message.contains("missing symbol"))
    );

    let mut iterator_registry = snapshot.clone();
    let iterator = *snapshot
        .symbols
        .iter()
        .find_map(|(id, description)| {
            (description.as_ref() == Some(&JsString::from("Symbol.iterator"))).then_some(id)
        })
        .expect("iterator symbol");
    iterator_registry
        .symbol_registry
        .insert("fake".into(), iterator);
    let error = s
        .restore_validated(iterator_registry)
        .expect_err("registry points at iterator");
    assert!(matches!(error, VmError::InvalidSnapshot(message) if message.contains("iterator")));

    let mut missing_to_primitive = snapshot.clone();
    let to_primitive = *snapshot
        .symbols
        .iter()
        .find_map(|(id, description)| {
            (description.as_ref() == Some(&JsString::from("Symbol.toPrimitive"))).then_some(id)
        })
        .expect("well-known conversion symbol");
    missing_to_primitive.symbols.remove(&to_primitive);
    let error = s
        .restore_validated(missing_to_primitive)
        .expect_err("deleted well-known symbol");
    assert!(
        matches!(error, VmError::InvalidSnapshot(message) if message.contains("Symbol.toPrimitive"))
    );

    let mut missing_species = snapshot.clone();
    let species = snapshot
        .symbols
        .iter()
        .find_map(|(id, description)| {
            (description.as_ref() == Some(&JsString::from("Symbol.species"))).then_some(*id)
        })
        .expect("well-known species symbol");
    missing_species.symbols.remove(&species);
    let error = s
        .restore_validated(missing_species)
        .expect_err("deleted species symbol");
    assert!(
        matches!(error, VmError::InvalidSnapshot(message) if message.contains("Symbol.species"))
    );

    let mut lowered = snapshot.clone();
    lowered.next_symbol_id = symbol_id;
    let error = s
        .restore_validated(lowered)
        .expect_err("lowered high-water mark");
    assert!(
        matches!(error, VmError::InvalidSnapshot(message) if message.contains("next symbol id"))
    );
}

#[test]
fn restore_rejects_missing_or_mismatched_global_this_root() {
    let source = include_str!("../cases/async/global_this_snapshot.js");
    let mut suspended = Suspended::start(source, vec![]);
    let snapshot = suspended.snapshot();

    let mut missing = snapshot.clone();
    assert!(missing.roots.remove("globalThis").is_some());
    let error = suspended
        .restore_validated(missing)
        .expect_err("missing globalThis root");
    assert!(
        matches!(error, VmError::InvalidSnapshot(message) if message.contains("builtin roots"))
    );

    for root in ["promise", "object.prototype"] {
        let mut mismatched = snapshot.clone();
        mismatched
            .roots
            .insert("globalThis".to_owned(), snapshot.roots[root]);
        let error = suspended
            .restore_validated(mismatched)
            .expect_err("mismatched globalThis root");
        assert!(
            matches!(error, VmError::InvalidSnapshot(message) if message.contains("globalThis"))
        );
    }
}

#[test]
fn array_iterator_snapshot_tampering_is_rejected() {
    let source = include_str!("../cases/arrays/iterator_restore.js");
    let mut s = Suspended::start(source, vec![]);
    let snapshot = s.snapshot();

    let iterator = snapshot
        .objects
        .iter()
        .position(|record| matches!(record.object, Object::ArrayIterator { array: Some(_), .. }))
        .expect("live array iterator");
    let mut invalid_source = snapshot.clone();
    let Object::ArrayIterator { array, .. } = &mut invalid_source.objects[iterator].object else {
        unreachable!();
    };
    *array = Some(yield_vm::value::ObjectId::MAX);
    let error = s
        .restore_validated(invalid_source)
        .expect_err("missing iterator source");
    assert!(
        matches!(error, VmError::InvalidSnapshot(message) if message.contains("array iterator"))
    );

    let mut invalid_values = snapshot.clone();
    invalid_values.roots.insert(
        "array.values".to_owned(),
        snapshot.roots["object.prototype"],
    );
    let error = s
        .restore_validated(invalid_values)
        .expect_err("invalid array.values root");
    assert!(matches!(error, VmError::InvalidSnapshot(message) if message.contains("array.values")));

    let mut missing_root = snapshot.clone();
    assert!(
        missing_root
            .roots
            .remove("arrayIterator.prototype")
            .is_some()
    );
    let error = s
        .restore_validated(missing_root)
        .expect_err("missing iterator prototype root");
    assert!(matches!(error, VmError::InvalidSnapshot(message) if message.contains("builtin root")));

    let mut old_format = snapshot;
    old_format.format_version = 8;
    let error = s.restore(old_format).expect_err("format version 8");
    assert!(matches!(error, VmError::InvalidSnapshot(message) if message.contains("format")));
    assert_eq!(s.host.count("wait"), 1, "restore must not run the script");
}

#[test]
fn string_iterator_snapshot_rejects_invalid_offsets() {
    let source = include_str!("../cases/strings/iteration_restore.js");
    let mut s = Suspended::start(source, vec![]);
    let snapshot = s.snapshot();
    let iterator = snapshot
        .objects
        .iter()
        .position(|record| matches!(record.object, Object::StringIterator { .. }))
        .expect("live string iterator");
    for offset in [2, usize::MAX] {
        let mut invalid = snapshot.clone();
        let Object::StringIterator { position, .. } = &mut invalid.objects[iterator].object else {
            unreachable!();
        };
        *position = offset;
        let error = s
            .restore_validated(invalid)
            .expect_err("invalid string cursor");
        assert!(
            matches!(error, VmError::InvalidSnapshot(message) if message.contains("invalid UTF-16 position"))
        );
    }
}

#[test]
fn text_decoder_snapshot_rejects_invalid_stream_state() {
    let source = r#"
import { wait } from "test";
export async function run() {
  const decoder = new TextDecoder();
  decoder.decode(Uint8Array.from([0xe2]), { stream: true });
  await wait("a");
  return decoder.decode();
}
"#;
    let mut s = Suspended::start(source, vec![]);
    let snapshot = s.snapshot();
    let decoder = snapshot
        .objects
        .iter()
        .position(|record| matches!(record.object, Object::TextDecoder(_)))
        .expect("live decoder");

    let mut too_long = snapshot.clone();
    let Object::TextDecoder(state) = &mut too_long.objects[decoder].object else {
        unreachable!();
    };
    state.prefix_len = 4;
    let error = s.restore_validated(too_long).expect_err("prefix length 4");
    assert!(
        matches!(error, VmError::InvalidSnapshot(message) if message.contains("invalid stream state"))
    );

    let mut invalid_prefix = snapshot.clone();
    let Object::TextDecoder(state) = &mut invalid_prefix.objects[decoder].object else {
        unreachable!();
    };
    state.prefix_len = 1;
    state.prefix[0] = 0xff;
    let error = s
        .restore_validated(invalid_prefix)
        .expect_err("invalid prefix bytes");
    assert!(
        matches!(error, VmError::InvalidSnapshot(message) if message.contains("invalid stream state"))
    );

    let mut closed_with_pending = snapshot;
    let Object::TextDecoder(state) = &mut closed_with_pending.objects[decoder].object else {
        unreachable!();
    };
    state.streaming = false;
    state.prefix_len = 0;
    state.pending = vec![0x41];
    let error = s
        .restore_validated(closed_with_pending)
        .expect_err("pending bytes on a closed stream");
    assert!(
        matches!(error, VmError::InvalidSnapshot(message) if message.contains("invalid stream state"))
    );
}

#[test]
fn text_decoder_snapshot_restores_a_streaming_prefix() {
    let source = r#"
import { wait } from "test";
export async function run() {
  const decoder = new TextDecoder();
  decoder.decode(Uint8Array.from([0xe2]), { stream: true });
  const x = await wait("a");
  return decoder.decode(Uint8Array.from([0x82, 0xac])) + x;
}
"#;
    let mut s = Suspended::start(source, vec![]);
    let snapshot = s.snapshot();
    s.restore(json_round_trip(&snapshot)).expect("restore");
    s.complete("wait:a", Completion::Fulfilled(JsValue::string("!")));
    assert_eq!(
        s.state(),
        PromiseState::Fulfilled(Value::string("€!".into()))
    );
}

#[test]
fn host_value_serde_preserves_utf16_units() {
    for (units, payload) in [
        (vec![0xD83D, 0xDE00], serde_json::json!("😀")),
        (
            vec![0xD800, 0xFFFD, 0xDC00],
            serde_json::json!({"utf16": [55296, 65533, 56320]}),
        ),
    ] {
        let value = JsString::from_units(units.clone());
        let json = serde_json::to_value(&value).expect("serialize host value");
        assert_eq!(json, payload);
        let decoded = serde_json::from_value::<JsString>(json).expect("deserialize host value");
        assert_eq!(decoded.to_utf16().as_ref(), units.as_slice());
    }
    for invalid in [-1, 65536] {
        let json = serde_json::json!({"utf16": [invalid]});
        assert!(serde_json::from_value::<JsString>(json).is_err());
    }
}

#[test]
fn restore_rejects_previous_snapshot_format_version() {
    let mut s = Suspended::start(RUN, vec![]);
    let snapshot = s.snapshot();
    for version in [7, 32, 37, 49, 67] {
        let mut previous = snapshot.clone();
        previous.format_version = version;
        let error = s.restore(previous).expect_err("previous snapshot format");
        assert!(
            matches!(error, VmError::InvalidSnapshot(message) if message.contains("unsupported snapshot format"))
        );
    }
    let mut previous = snapshot;
    previous.bytecode_version = 38;
    let error = s.restore(previous).expect_err("previous bytecode version");
    assert!(
        matches!(error, VmError::InvalidSnapshot(message) if message.contains("bytecode version"))
    );
}

#[test]
fn snapshot_deserialization_rejects_an_unknown_intrinsic_key() {
    let suspended = Suspended::start(RUN, vec![]);
    let snapshot = suspended.snapshot();
    let array_values = snapshot.roots["array.values"];
    let mut wire = serde_json::to_value(snapshot).expect("serialize snapshot");
    let record = wire["objects"]
        .as_array_mut()
        .expect("object records")
        .iter_mut()
        .find(|record| record["id"] == serde_json::json!(array_values))
        .expect("array.values record");
    let intrinsic = &mut record["object"]["native"]["Intrinsic"]["intrinsic"];
    assert_eq!(intrinsic["owner"], "Array");
    assert_eq!(intrinsic["member"], "values");
    intrinsic["member"] = serde_json::json!("not_a_declared_member");
    let error = serde_json::from_value::<yield_vm::Snapshot>(wire).unwrap_err();
    assert!(error.to_string().contains("unknown intrinsic key"));
}

#[test]
fn restore_rejects_duplicate_intrinsic_identity() {
    let mut suspended = Suspended::start(RUN, vec![]);
    let mut snapshot = suspended.snapshot();
    let array_values = snapshot.roots["array.values"];
    let mut duplicate = snapshot
        .objects
        .iter()
        .find(|record| record.id == array_values)
        .expect("array.values record")
        .clone();
    duplicate.id = snapshot
        .objects
        .iter()
        .map(|record| record.id)
        .max()
        .unwrap()
        + 1;
    snapshot.objects.push(duplicate);
    let error = suspended
        .restore_validated(json_round_trip(&snapshot))
        .expect_err("duplicate intrinsic object identity");
    assert!(
        matches!(error, VmError::InvalidSnapshot(message) if message.contains("duplicate native object identity"))
    );
}

#[test]
fn for_in_snapshot_rejects_invalid_cursor_state() {
    let source = include_str!("../cases/loops/for_in_restore.js");
    let mut s = Suspended::start(source, vec![]);
    let snapshot = s.snapshot();
    let iterator = snapshot
        .objects
        .iter()
        .position(|record| {
            matches!(
                record.object,
                Object::ForInIterator {
                    pending: Some(_),
                    ..
                }
            )
        })
        .expect("live property enumeration cursor");

    for target in [
        None,
        Some(snapshot.objects[iterator].id),
        Some(yield_vm::value::ObjectId::MAX),
    ] {
        let mut invalid = snapshot.clone();
        let Object::ForInIterator { current, .. } = &mut invalid.objects[iterator].object else {
            unreachable!();
        };
        *current = target;
        let error = s
            .restore_validated(invalid)
            .expect_err("invalid enumeration cursor");
        assert!(
            matches!(error, VmError::InvalidSnapshot(message) if message.contains("for-in iterator"))
        );
    }
}

#[test]
fn symbols_cross_the_host_boundary_as_handles() {
    let source = r#"
import { inspect } from "test";
export function run() {
  return inspect(Symbol("hidden"));
}
"#;
    let mut host = TestHost::default();
    let mut vm = Vm::new(build(source), &mut host).expect("create VM");
    let value = vm.start(&mut host, "run", Vec::new()).expect("start");
    assert_eq!(value, Value::UNDEFINED);
    assert_eq!(host.count("inspect"), 1);
}

#[test]
fn script_startup_validates_before_calling_the_host() {
    let mut host = TestHost::default();
    let mut invalid = build("import { inspect } from 'test'; inspect(1); export function run() {}");
    invalid.modules[invalid.entry as usize]
        .exports
        .get_mut("run")
        .unwrap()
        .target = ModuleTarget::Binding {
        module: invalid.entry,
        binding: u32::MAX,
    };
    invalid.finalize();
    assert!(matches!(
        Vm::new(invalid, &mut host),
        Err(VmError::InvalidBuild(_))
    ));

    let mut options = super::support::declared(super::support::TEST_DECLARATIONS);
    options
        .imports
        .get_mut("test")
        .expect("test declarations")
        .insert("missing".to_owned());
    let missing = compile_main("import { missing } from 'test';", &options).unwrap();
    let error = Vm::new(missing, &mut host).expect_err("missing module export is rejected eagerly");
    assert!(
        matches!(error, VmError::MissingImport(detail) if detail.contains("test") && detail.contains("missing"))
    );
    assert_eq!(host.count("inspect"), 0);

    let throws = build("import { inspect } from 'test'; throw 7; inspect(1);");
    assert!(matches!(
        Vm::new(throws, &mut host),
        Err(VmError::Thrown(value)) if value == Value::number(7.0)
    ));
    assert_eq!(host.count("inspect"), 0);

    let standalone = build("import { inspect } from 'test'; inspect(2);");
    let vm = Vm::new(standalone.clone(), &mut host).unwrap();
    Vm::restore(vm.snapshot(&[]).unwrap(), standalone, &mut host).unwrap();
    assert_eq!(host.count("inspect"), 1);
}

#[test]
fn namespace_and_function_import_identity_survive_snapshot_restore() {
    let source = r#"
import * as test from "test";
export async function run() {
  const wait = test.wait;
  await wait("identity");
  return test.wait === wait && test.wait === test.wait && wait === wait;
}
"#;
    let build = build(source);
    let mut host = TestHost::default();
    let mut vm = Vm::new(build.clone(), &mut host).expect("resolve host imports");
    let result = vm.start(&mut host, "run", vec![]).expect("start");
    vm.run_until_idle(&mut host).expect("suspend");
    let snapshot = json_round_trip(
        &vm.snapshot(std::slice::from_ref(&result))
            .expect("snapshot"),
    );
    vm = Vm::restore(snapshot, build, &mut host).expect("restore imports");
    vm.start_recovered_operations()
        .expect("start recovered operations");
    host.complete(
        &mut vm,
        "wait:identity",
        Completion::Fulfilled(JsValue::undefined()),
    )
    .expect("complete operation");
    vm.run_until_idle(&mut host).expect("resume");
    assert_eq!(
        vm.promise_state(&result).unwrap(),
        PromiseState::Fulfilled(Value::boolean(true))
    );
}

#[test]
fn import_bindings_and_namespace_properties_are_readonly() {
    let mut host = TestHost::default();
    let local_write = build("import { wait } from 'test'; wait = 1;");
    assert!(matches!(
        Vm::new(local_write, &mut host),
        Err(VmError::Thrown(_))
    ));
    let namespace_write = build("import * as test from 'test'; test.wait = 1;");
    assert!(matches!(
        Vm::new(namespace_write, &mut host),
        Err(VmError::Thrown(_))
    ));
}

#[test]
fn restore_rejects_a_pending_operation_with_a_different_import_binding() {
    let mut s = Suspended::start(RUN, vec![]);
    let mut snapshot = s.snapshot();
    assert_eq!(snapshot.operations[0].name, "wait");
    assert_eq!(snapshot.operations[0].schema, 1);
    snapshot.operations[0].name = "inspect".to_owned();
    assert!(matches!(
        s.restore_validated(snapshot),
        Err(VmError::InvalidSnapshot(message)) if message.contains("is not declared")
    ));
}

#[test]
fn build_deserialization_rejects_duplicate_export_names() {
    let build = build("export function run() {}");
    let json = serde_json::to_string(&build).unwrap();
    let export = &build.modules[build.entry as usize].exports["run"];
    let member = format!(r#""run":{}"#, serde_json::to_string(export).unwrap());
    let export_map = format!(r#""exports":{{{member}}}"#);
    assert!(json.contains(&export_map));
    // Duplicate the identical value so last-write-wins would pass the digest check.
    let duplicate = json.replacen(
        &export_map,
        &format!(r#""exports":{{{member},{member}}}"#),
        1,
    );
    let error = serde_json::from_str::<yield_vm::Build>(&duplicate).unwrap_err();
    assert!(
        error.to_string().contains("duplicate export name"),
        "{error}"
    );
}

#[test]
fn restore_preserves_live_entries_without_replaying_startup() {
    let build = build(
        r#"
import { inspect, seed } from "test";
inspect(1);
let count = seed();
export { entry as run, count as total };
function entry() { throw 'stale entry'; }
entry = () => ++count;
export function dropEntry() { entry = 0; }
"#,
    );
    let mut host = TestHost::default();
    let mut vm = Vm::new(build.clone(), &mut host).unwrap();
    assert_eq!(vm.get_export("total").unwrap(), Value::number(10.0));
    for name in ["entry", "count"] {
        let expected = format!("unknown export {name:?}");
        assert!(
            matches!(vm.get_export(name), Err(VmError::InvalidState(message)) if message == expected)
        );
        assert!(
            matches!(vm.start(&mut host, name, vec![]), Err(VmError::InvalidState(message)) if message == expected)
        );
    }
    assert_eq!(
        vm.start(&mut host, "run", vec![]).unwrap(),
        Value::number(11.0)
    );
    assert_eq!(vm.get_export("total").unwrap(), Value::number(11.0));

    let snapshot = vm.snapshot(&[]).unwrap();
    let entry = build.entry as usize;
    let mut missing = snapshot.clone();
    missing.module_envs[entry] = None;
    assert!(matches!(
        Vm::restore_validated(missing, build.clone(), &mut host),
        Err(VmError::InvalidSnapshot(_))
    ));
    let mut incorrect = snapshot.clone();
    incorrect.module_envs[entry] = Some(snapshot.module_namespaces[entry]);
    assert!(matches!(
        Vm::restore_validated(incorrect, build.clone(), &mut host),
        Err(VmError::InvalidSnapshot(_))
    ));
    let mut absent_export = snapshot.clone();
    let entry_env = absent_export.module_envs[entry].unwrap();
    let record = absent_export
        .objects
        .iter_mut()
        .find(|record| record.id == entry_env)
        .unwrap();
    let Object::Environment(env) = &mut record.object else {
        panic!("not the entry module environment")
    };
    let ModuleTarget::Binding { module, binding } = build.modules[entry].exports["run"].target
    else {
        panic!("expected a local run binding");
    };
    assert_eq!(module, build.entry);
    assert!(env.bindings.remove(&binding).is_some());
    assert!(matches!(
        Vm::restore_validated(absent_export, build.clone(), &mut host),
        Err(VmError::InvalidSnapshot(_))
    ));

    let mut vm = Vm::restore(snapshot, build.clone(), &mut host).unwrap();
    assert_eq!(vm.get_export("total").unwrap(), Value::number(11.0));
    assert_eq!(
        vm.start(&mut host, "run", vec![]).unwrap(),
        Value::number(12.0)
    );
    assert_eq!(vm.get_export("total").unwrap(), Value::number(12.0));

    vm.start(&mut host, "dropEntry", vec![]).unwrap();
    let mut vm = Vm::restore(vm.snapshot(&[]).unwrap(), build, &mut host).unwrap();
    assert_eq!(vm.get_export("run").unwrap(), Value::number(0.0));
    assert!(matches!(
        vm.start(&mut host, "run", vec![]),
        Err(VmError::Thrown(_))
    ));
    assert_eq!(host.count("inspect"), 1);
    assert_eq!(host.count("seed"), 1);
}

#[test]
fn imported_function_identity_survives_snapshot_restore() {
    let mut host = TestHost::default();
    let build = build("import { wait } from 'test'; export function run() { return wait; }");
    let mut vm = Vm::new(build.clone(), &mut host).expect("resolve host imports");
    let before = vm.start(&mut host, "run", vec![]).expect("start");
    let snapshot = vm.snapshot(&[]).expect("snapshot");
    vm = Vm::restore(snapshot, build, &mut host).expect("restore");
    let yield_vm::ValueRef::Object(function) = before.view() else {
        panic!("host function import is an object");
    };
    let value = vm
        .start(&mut host, "run", vec![])
        .expect("start after restore");
    assert_eq!(
        value,
        Value::object(function),
        "same function object identity"
    );
}

#[test]
fn restore_rejects_invalid_template_objects() {
    let build = build(
        r#"
import { wait } from "test";
function tag(strings) {
  return strings;
}
export async function run() {
  const template = tag`a${1}b`;
  await wait("a");
  return template;
}
"#,
    );
    let mut host = TestHost::default();
    let mut vm = Vm::new(build.clone(), &mut host).expect("create VM");
    let result = vm.start(&mut host, "run", vec![]).expect("start run");
    vm.run_until_idle(&mut host).expect("suspend run");
    let snapshot = vm.snapshot(&[result]).expect("capture template");
    assert_eq!(snapshot.templates.len(), 1);

    let mut shortened = snapshot.clone();
    shortened.templates.clear();
    assert!(matches!(
        Vm::restore_validated(shortened, build.clone(), &mut host),
        Err(VmError::InvalidSnapshot(message)) if message.contains("template count mismatch")
    ));

    let mut wrong = snapshot;
    wrong.templates[0] = wrong.roots["array.prototype"];
    assert!(matches!(
        Vm::restore_validated(wrong, build, &mut host),
        Err(VmError::InvalidSnapshot(message)) if message.contains("template 0")
    ));
}
