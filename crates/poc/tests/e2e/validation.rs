//! The loader and snapshot APIs reject invalid inputs and state.

use super::support::{RUN, Suspended, count_delta};
use yield_poc::{TestHost, build};
use yield_vm::bytecode::{Instr, Slot};
use yield_vm::heap::EnvironmentObject;
use yield_vm::snapshot::ObjectRecord;
use yield_vm::{Host, HostReply, HostValue, JsString, Object, Value, Vm, VmError};

#[test]
fn host_identifiers_require_well_formed_utf16() {
    let mut host = yield_poc::TestHost::default();
    let lone = HostValue::String(JsString::from_units(vec![0xD800]));
    assert!(host.call("inspect", std::slice::from_ref(&lone)).is_ok());
    for import in ["wait", "bind"] {
        assert!(host.call(import, std::slice::from_ref(&lone)).is_err());
    }
    let pair = HostValue::String("😀".into());
    assert!(
        matches!(host.call("wait", &[pair]), Ok(HostReply::Operation { operation_id }) if operation_id == "wait:😀")
    );
}

#[test]
fn missing_bindings_and_mismatched_builds_are_rejected_before_execution() {
    let source = r#"
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
    corrupt.functions[0].code[0] = Instr::Const {
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
    corrupt.functions[0].code[0] = Instr::Jump { target: u32::MAX };
    corrupt.finalize();
    let error = Vm::new(corrupt, &mut host).expect_err("bad jump target");
    assert!(matches!(error, VmError::InvalidBuild(message) if message.contains("targets pc")));
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
    let error = s.restore(snapshot).expect_err("missing environment");
    assert!(
        matches!(error, VmError::InvalidSnapshot(message) if message.contains("no invocation environment"))
    );
    assert_eq!(s.host.count("wait"), calls, "restore must not run script");
}

fn suspended_symbol_snapshot() -> (super::support::Suspended, yield_vm::Snapshot) {
    let source = r#"
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
    let error = s.restore(missing_key).expect_err("deleted live symbol");
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
        .restore(iterator_registry)
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
        .restore(missing_to_primitive)
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
        .restore(missing_species)
        .expect_err("deleted species symbol");
    assert!(
        matches!(error, VmError::InvalidSnapshot(message) if message.contains("Symbol.species"))
    );

    let mut lowered = snapshot.clone();
    lowered.next_symbol_id = symbol_id;
    let error = s.restore(lowered).expect_err("lowered high-water mark");
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
        .restore(missing)
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
            .restore(mismatched)
            .expect_err("mismatched globalThis root");
        assert!(
            matches!(error, VmError::InvalidSnapshot(message) if message.contains("globalThis"))
        );
    }
}

#[test]
fn array_iterator_snapshot_tampering_is_rejected() {
    let source = include_str!("../cases/objects/array_iterator_restore.js");
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
        .restore(invalid_source)
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
        .restore(invalid_values)
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
        .restore(missing_root)
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
    let source = include_str!("../cases/strings/string_iteration_restore.js");
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
        let error = s.restore(invalid).expect_err("invalid string cursor");
        assert!(
            matches!(error, VmError::InvalidSnapshot(message) if message.contains("invalid UTF-16 position"))
        );
    }
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
        let value = HostValue::String(JsString::from_units(units.clone()));
        let json = serde_json::to_value(&value).expect("serialize host value");
        assert_eq!(json, serde_json::json!({"string": payload}));
        let HostValue::String(decoded) =
            serde_json::from_value(json).expect("deserialize host value")
        else {
            panic!("expected host string");
        };
        assert_eq!(decoded.as_units(), units.as_slice());
    }
    for invalid in [-1, 65536] {
        let json = serde_json::json!({"string": {"utf16": [invalid]}});
        assert!(serde_json::from_value::<HostValue>(json).is_err());
    }
}

#[test]
fn restore_rejects_previous_snapshot_format_version() {
    let mut s = Suspended::start(RUN, vec![]);
    let snapshot = s.snapshot();
    for version in [7, 32] {
        let mut previous = snapshot.clone();
        previous.format_version = version;
        let error = s.restore(previous).expect_err("previous snapshot format");
        assert!(
            matches!(error, VmError::InvalidSnapshot(message) if message.contains("unsupported snapshot format"))
        );
    }
}

#[test]
fn for_in_snapshot_rejects_invalid_cursor_state() {
    let source = include_str!("../cases/control/for_in_restore.js");
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
        let error = s.restore(invalid).expect_err("invalid enumeration cursor");
        assert!(
            matches!(error, VmError::InvalidSnapshot(message) if message.contains("for-in iterator"))
        );
    }
}

#[test]
fn symbols_never_reach_the_host_call() {
    let source = r#"
export async function run() {
  return inspect(Symbol("hidden"));
}
"#;
    let mut host = TestHost::default();
    let mut vm = Vm::new(build(source), &mut host).expect("create VM");
    let error = vm
        .start(&mut host, "run", Vec::new())
        .expect_err("symbol argument is rejected");
    assert!(
        matches!(&error, VmError::InvalidState(message) if message.contains("Symbol")),
        "{error}"
    );
    assert_eq!(host.count("inspect"), 0, "host must not be called");
}

#[test]
fn script_startup_validates_before_calling_the_host() {
    let mut host = TestHost::default();
    let mut invalid = build("inspect(1); export function run() {}");
    *invalid.exports.get_mut("run").unwrap() = u32::MAX;
    invalid.finalize();
    assert!(matches!(
        Vm::new(invalid, &mut host),
        Err(VmError::InvalidBuild(_))
    ));

    let options = yield_compiler::CompileOptions {
        imports: vec!["inspect".into(), "missing".into()],
    };
    let missing = yield_compiler::compile("inspect(1);", &options).unwrap();
    assert!(
        matches!(Vm::new(missing, &mut host), Err(VmError::MissingImport(name)) if name == "missing")
    );
    assert_eq!(host.count("inspect"), 0);

    let throws = build("throw 7; inspect(1);");
    assert!(matches!(
        Vm::new(throws, &mut host),
        Err(VmError::Thrown(Value::Number(7.0)))
    ));
    assert_eq!(host.count("inspect"), 0);

    let standalone = build("inspect(2);");
    let vm = Vm::new(standalone.clone(), &mut host).unwrap();
    Vm::restore(vm.snapshot(&[]).unwrap(), standalone, &mut host).unwrap();
    assert_eq!(host.count("inspect"), 1);
}

#[test]
fn build_deserialization_rejects_duplicate_export_names() {
    let build = build("export function run() {}");
    let json = serde_json::to_string(&build).unwrap();
    let member = format!(r#""run":{}"#, build.exports["run"]);
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
    assert_eq!(vm.get_export("total").unwrap(), Value::Number(10.0));
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
        Value::Number(11.0)
    );
    assert_eq!(vm.get_export("total").unwrap(), Value::Number(11.0));

    let snapshot = vm.snapshot(&[]).unwrap();
    let mut missing = snapshot.clone();
    missing.script_env = None;
    assert!(matches!(
        Vm::restore(missing, build.clone(), &mut host),
        Err(VmError::InvalidSnapshot(_))
    ));
    let mut incorrect = snapshot.clone();
    incorrect.script_env = snapshot.roots.values().next().copied();
    assert!(matches!(
        Vm::restore(incorrect, build.clone(), &mut host),
        Err(VmError::InvalidSnapshot(_))
    ));
    let mut absent_export = snapshot.clone();
    let script = absent_export.script_env.unwrap();
    let record = absent_export
        .objects
        .iter_mut()
        .find(|record| record.id == script)
        .unwrap();
    let Object::Environment(env) = &mut record.object else {
        panic!("not the script environment")
    };
    env.bindings.remove(&build.exports["run"]);
    assert!(matches!(
        Vm::restore(absent_export, build.clone(), &mut host),
        Err(VmError::InvalidSnapshot(_))
    ));

    let mut vm = Vm::restore(snapshot, build.clone(), &mut host).unwrap();
    assert_eq!(vm.get_export("total").unwrap(), Value::Number(11.0));
    assert_eq!(
        vm.start(&mut host, "run", vec![]).unwrap(),
        Value::Number(12.0)
    );
    assert_eq!(vm.get_export("total").unwrap(), Value::Number(12.0));

    vm.start(&mut host, "dropEntry", vec![]).unwrap();
    let mut vm = Vm::restore(vm.snapshot(&[]).unwrap(), build, &mut host).unwrap();
    assert_eq!(vm.get_export("run").unwrap(), Value::Number(0.0));
    assert!(matches!(
        vm.start(&mut host, "run", vec![]),
        Err(VmError::Thrown(_))
    ));
    assert_eq!(host.count("inspect"), 1);
    assert_eq!(host.count("seed"), 1);
}
