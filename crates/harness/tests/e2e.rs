#[path = "e2e/mod.rs"]
mod suite;

use yield_vm::ValueRef;

#[test]
fn restore_rejects_the_previous_snapshot_format() {
    use yield_harness::{TestHost, build};
    use yield_vm::{SNAPSHOT_FORMAT_VERSION, Vm, VmError};

    let build = build("export function run() { return [1].entries(); }");
    let mut host = TestHost::default();
    let mut vm = Vm::new(build.clone(), &mut host).unwrap();
    let cursor = vm.start(&mut host, "run", vec![]).unwrap();
    let mut snapshot = vm.snapshot(&[cursor]).unwrap();
    assert_eq!(snapshot.format_version, SNAPSHOT_FORMAT_VERSION);
    snapshot.format_version -= 1;
    assert!(matches!(
        Vm::restore(snapshot, build, &mut host),
        Err(VmError::InvalidSnapshot(message)) if message.contains(&format!("unsupported snapshot format {}", SNAPSHOT_FORMAT_VERSION - 1))
    ));
}

#[test]
fn restore_rejects_a_missing_boxed_symbol_payload() {
    use yield_harness::{TestHost, build};
    use yield_vm::heap::BoxedPrimitive;
    use yield_vm::{Object, Vm, VmError};

    let build = build(
        r#"
import { wait } from "test";
export async function run() {
  const boxed = Object(Symbol("boxed-only"));
  await wait("a");
  return boxed;
}
"#,
    );
    let mut host = TestHost::default();
    let mut vm = Vm::new(build.clone(), &mut host).expect("create VM");
    let result = vm.start(&mut host, "run", vec![]).expect("start run");
    vm.run_until_idle(&mut host).expect("suspend run");
    let mut snapshot = vm.snapshot(&[result]).expect("capture boxed symbol");
    let symbol = snapshot
        .objects
        .iter()
        .find_map(|record| match &record.object {
            Object::Boxed {
                primitive: BoxedPrimitive::Symbol(id),
                ..
            } => Some(*id),
            _ => None,
        })
        .expect("boxed symbol object");
    assert_eq!(
        snapshot.symbols.remove(&symbol),
        Some(Some("boxed-only".into()))
    );
    assert!(matches!(
        Vm::restore(snapshot, build, &mut host),
        Err(VmError::InvalidSnapshot(message)) if message.contains("missing symbol")
    ));
}

#[test]
fn restore_rejects_a_string_for_in_cursor_missing_length_shadow() {
    use yield_harness::{TestHost, build};
    use yield_vm::{Object, Vm, VmError};

    let build = build(
        r#"
import { wait } from "test";
export async function run() {
  for (const key in "ab") await wait(key);
}
"#,
    );
    let mut host = TestHost::default();
    let mut vm = Vm::new(build.clone(), &mut host).expect("create VM");
    let result = vm.start(&mut host, "run", vec![]).expect("start run");
    vm.run_until_idle(&mut host).expect("suspend run");
    let mut snapshot = vm.snapshot(&[result]).expect("capture string cursor");
    let visited = snapshot
        .objects
        .iter_mut()
        .find_map(|record| match &mut record.object {
            Object::ForInIterator { visited, .. } => Some(visited),
            _ => None,
        })
        .expect("string cursor");
    visited.clear();
    assert!(matches!(
        Vm::restore(snapshot, build, &mut host),
        Err(VmError::InvalidSnapshot(message)) if message.contains("invalid cursor state")
    ));
}

#[test]
fn build_round_trips_bigint_constants() {
    use yield_vm::{Build, Constant};

    let build = yield_harness::build("export function run() { return 12345678901234567890n; }");
    let constant = build
        .constants
        .iter()
        .find_map(|constant| match constant {
            Constant::BigInt(value) => Some(value),
            _ => None,
        })
        .expect("BigInt constant");
    assert_eq!(constant.to_decimal(), "12345678901234567890");

    let json = serde_json::to_string(&build).unwrap();
    let decoded: Build = serde_json::from_str(&json).unwrap();
    decoded.validate().unwrap();
    assert_eq!(decoded.build_id, build.build_id);
    let decoded_constant = decoded
        .constants
        .iter()
        .find_map(|constant| match constant {
            Constant::BigInt(value) => Some(value),
            _ => None,
        })
        .expect("round-tripped BigInt constant");
    assert_eq!(decoded_constant, constant);
}

#[test]
fn host_bigint_construction_requires_canonical_decimal() {
    use yield_vm::JsValue;

    let value = JsValue::bigint("12345678901234567890").expect("canonical BigInt");
    assert!(value.is_bigint());
    assert_eq!(
        value.as_bigint().expect("BigInt view").to_decimal(),
        "12345678901234567890"
    );
    assert!(JsValue::bigint("01").is_err());
    assert!(JsValue::bigint("-0").is_err());
    assert!(JsValue::bigint("+1").is_err());
}

#[test]
fn build_round_trips_special_numbers() {
    use yield_vm::{Build, bytecode::Constant};

    let mut build = yield_harness::build("export function run() { return 0; }");
    let start = build.constants.len();
    let numbers = [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.0];
    build.constants.extend(numbers.map(Constant::Number));
    build.finalize();
    let json = serde_json::to_string(&build).unwrap();
    let decoded: Build = serde_json::from_str(&json).unwrap();
    decoded.validate().unwrap();
    assert_eq!(decoded.build_id, build.build_id);
    for (constant, expected) in decoded.constants[start..].iter().zip(numbers) {
        let Constant::Number(value) = constant else {
            panic!("expected a number constant");
        };
        if expected.is_nan() {
            assert!(value.is_nan());
        } else {
            assert_eq!(value.to_bits(), expected.to_bits());
        }
    }
    build.constants[start + 1] = Constant::Number(f64::NEG_INFINITY);
    build.finalize();
    assert_ne!(decoded.build_id, build.build_id);
    for invalid in ["null", "\"unknown\""] {
        assert!(serde_json::from_str::<Build>(&json.replace("\"NaN\"", invalid)).is_err());
    }
}

#[test]
fn callable_initial_names_participate_in_build_identity() {
    let mut build = yield_harness::build("export function run() { return 0; }");
    let id = build.build_id.clone();
    let function = build
        .functions
        .iter_mut()
        .find(|f| f.name == "run")
        .unwrap();
    function.initial_name = "different".into();
    assert_ne!(build.compute_id(), id);
    build.bytecode_version = 38;
    build.finalize();
    assert!(build.validate().unwrap_err().contains("bytecode version"));
}

#[test]
fn non_finite_operation_completions_survive_restore() {
    use yield_harness::{TestHost, build};
    use yield_vm::{Completion, JsValue, PromiseState, Snapshot, Value, Vm};

    let build = build(
        r#"
import { wait } from 'test';
export async function run() {
  const value = await wait('fulfill');
  try {
    await wait('reject');
    return false;
  } catch (reason) {
    return value === 1e999 && Number.isNaN(reason);
  }
}
"#,
    );
    for restore in [false, true] {
        let mut host = TestHost::default();
        let mut vm = Vm::new(build.clone(), &mut host).unwrap();
        let result = vm.start(&mut host, "run", vec![]).unwrap();
        vm.run_until_idle(&mut host).unwrap();
        for (label, completion) in [
            (
                "wait:fulfill",
                Completion::Fulfilled(JsValue::number(f64::INFINITY)),
            ),
            (
                "wait:reject",
                Completion::Rejected(JsValue::number(f64::NAN)),
            ),
        ] {
            if restore {
                let snapshot = vm.snapshot(std::slice::from_ref(&result)).unwrap();
                let snapshot = Snapshot::from_json(&snapshot.to_json().unwrap()).unwrap();
                vm = Vm::restore(snapshot, build.clone(), &mut host).unwrap();
                vm.start_recovered_operations().unwrap();
            }
            vm.complete_operation(host.token(label).unwrap(), completion)
                .unwrap();
            vm.run_until_idle(&mut host).unwrap();
        }
        assert_eq!(
            vm.promise_state(&result).unwrap(),
            PromiseState::Fulfilled(Value::boolean(true))
        );
    }
}

#[test]
fn restore_rejects_changed_has_instance_description() {
    use yield_harness::{TestHost, build};
    use yield_vm::{Vm, VmError};

    let build = build("export function run() { return 0; }");
    let mut host = TestHost::default();
    let vm = Vm::new(build.clone(), &mut host).unwrap();
    let mut snapshot = vm.snapshot(&[]).unwrap();
    let description = snapshot.symbols.get_mut(&8).expect("reserved symbol root");
    assert_eq!(*description, Some("Symbol.hasInstance".into()));
    *description = Some("tampered".into());
    assert!(matches!(
        Vm::restore(snapshot, build, &mut host),
        Err(VmError::InvalidSnapshot(message)) if message.contains("reserved symbol 8 has an invalid description")
    ));
}

#[test]
fn gc_reclaims_unrooted_cycles_without_reusing_ids() {
    use yield_harness::{TestHost, build};
    use yield_vm::{Value, Vm, VmError};

    let build = build(
        "export function cycle() { const object = {}; object.self = object; return object; }",
    );
    let mut host = TestHost::default();
    let mut vm = Vm::new(build, &mut host).unwrap();
    vm.collect_garbage(&[]).unwrap();

    let cycle = vm.start(&mut host, "cycle", vec![]).unwrap();
    let ValueRef::Object(id) = cycle.view() else {
        panic!("expected cycle object");
    };
    let roots = [Value::object(id), Value::object(id)];
    vm.collect_garbage(&roots).unwrap();
    let snapshot = vm.snapshot(&roots).unwrap();
    assert!(snapshot.objects.iter().any(|record| record.id == id));

    assert!(vm.collect_garbage(&[]).unwrap() >= 1);
    assert_eq!(vm.collect_garbage(&[]).unwrap(), 0);
    assert!(matches!(
        vm.collect_garbage(&roots),
        Err(VmError::InvalidState(_))
    ));

    let next = vm.start(&mut host, "cycle", vec![]).unwrap();
    let ValueRef::Object(next_id) = next.view() else {
        panic!("expected new cycle object");
    };
    assert_ne!(id, next_id);
    let missing = u32::MAX;
    assert!(matches!(
        vm.collect_garbage(&[Value::symbol(missing)]),
        Err(VmError::InvalidState(message))
            if message == format!("garbage collection references missing symbol {missing}")
    ));
    let snapshot = vm.snapshot(&[next]).expect("failed GC must not sweep");
    assert!(snapshot.objects.iter().any(|record| record.id == next_id));
}

#[test]
fn gc_traces_weak_map_values_only_for_live_keys() {
    use yield_harness::{TestHost, build};
    use yield_vm::{Value, Vm, VmError};

    let build = build(
        r#"
const second = new WeakMap();
const first = new WeakMap();
let key1 = {};
let key2 = {};
let backref = {};
first.set(key1, key2);
second.set(key2, { answer: 42 });
first.set(backref, { key: backref });
export function takeBackref() { const key = backref; backref = undefined; return key; }
export function takeKey2() { const key = key2; key2 = undefined; return key; }
export function takeKey1() { const key = key1; key1 = undefined; return key; }
export function read() { return second.get(first.get(key1)).answer; }
"#,
    );
    let mut host = TestHost::default();
    let mut vm = Vm::new(build, &mut host).unwrap();
    let key2 = vm.start(&mut host, "takeKey2", vec![]).unwrap();
    let backref = vm.start(&mut host, "takeBackref", vec![]).unwrap();

    // Only key1 remains in the script's roots. Neither key2 nor the
    // backreference key is a root just because the Rust test holds its ID.
    vm.collect_garbage(&[]).unwrap();
    assert_eq!(
        vm.start(&mut host, "read", vec![]).unwrap(),
        Value::number(42.0)
    );
    assert!(matches!(
        vm.collect_garbage(&[backref]),
        Err(VmError::InvalidState(_))
    ));

    let key1 = vm.start(&mut host, "takeKey1", vec![]).unwrap();
    vm.collect_garbage(&[key1]).unwrap();
    vm.snapshot(std::slice::from_ref(&key2))
        .expect("an explicit key1 root must still preserve key2");

    vm.collect_garbage(&[]).unwrap();
    assert!(matches!(
        vm.collect_garbage(&[key2]),
        Err(VmError::InvalidState(_))
    ));
}

#[test]
fn gc_closes_symbol_ephemerons_without_retaining_weak_keys() {
    use yield_harness::{TestHost, build, number};
    use yield_vm::heap::WeakKey;
    use yield_vm::{Object, Value, Vm, VmError};

    let build = build(
        r#"
function setup() {
  const a = Symbol('a');
  const b = Symbol('b');
  const backref = Symbol('backref');
  const map = new WeakMap([[a, {}], [b, a], [backref, { key: backref }]]);
  const set = new WeakSet([a]);
  return [map, set, b, backref];
}
const stash = setup();
export function take(i) { const value = stash[i]; delete stash[i]; return value; }
"#,
    );
    let mut host = TestHost::default();
    let mut vm = Vm::new(build, &mut host).unwrap();
    let values =
        [0.0, 1.0, 2.0, 3.0].map(|i| vm.start(&mut host, "take", vec![number(i)]).unwrap());
    let [
        ValueRef::Object(map),
        ValueRef::Object(set),
        ValueRef::Symbol(b),
        ValueRef::Symbol(backref),
    ] = values.each_ref().map(Value::view)
    else {
        panic!("expected two collections and two symbols");
    };
    let roots = [Value::object(map), Value::object(set), Value::symbol(b)];
    vm.collect_garbage(&roots).unwrap();
    let snapshot = vm.snapshot(&roots).unwrap();
    let map_record = snapshot.objects.iter().find(|r| r.id == map).unwrap();
    let Object::WeakMap { entries, .. } = &map_record.object else {
        panic!("expected WeakMap");
    };
    assert_eq!(entries.len(), 2);
    let ValueRef::Symbol(a) = entries[&WeakKey::Symbol(b)].view() else {
        panic!("expected symbol value");
    };
    assert!(a < b, "a must be visited before b unlocks it");
    let ValueRef::Object(value) = entries[&WeakKey::Symbol(a)].view() else {
        panic!("expected object value");
    };
    assert!(snapshot.objects.iter().any(|r| r.id == value));
    assert!(snapshot.symbols.contains_key(&a));
    let set_record = snapshot.objects.iter().find(|r| r.id == set).unwrap();
    let Object::WeakSet { entries, .. } = &set_record.object else {
        panic!("expected WeakSet");
    };
    assert_eq!(entries.len(), 1);
    assert!(entries.contains(&WeakKey::Symbol(a)));
    assert!(matches!(
        vm.collect_garbage(&[Value::symbol(backref)]),
        Err(VmError::InvalidState(message))
            if message == format!("garbage collection references missing symbol {backref}")
    ));

    let roots = &roots[..2];
    vm.collect_garbage(roots).unwrap();
    let snapshot = vm.snapshot(roots).unwrap();
    assert!(matches!(
        &snapshot.objects.iter().find(|r| r.id == map).unwrap().object,
        Object::WeakMap { entries, .. } if entries.is_empty()
    ));
    assert!(matches!(
        &snapshot.objects.iter().find(|r| r.id == set).unwrap().object,
        Object::WeakSet { entries, .. } if entries.is_empty()
    ));
    assert!(snapshot.objects.iter().all(|r| r.id != value));
    assert!(vm.collect_garbage(&[Value::object(value)]).is_err());
    for id in [a, b] {
        assert!(!snapshot.symbols.contains_key(&id));
        assert!(matches!(
            vm.collect_garbage(&[Value::symbol(id)]),
            Err(VmError::InvalidState(message))
                if message == format!("garbage collection references missing symbol {id}")
        ));
    }
}

#[test]
fn restore_rejects_missing_and_registered_symbol_keys_in_weak_collections() {
    use yield_harness::{TestHost, build};
    use yield_vm::heap::WeakKey;
    use yield_vm::{Object, Value, Vm, VmError};

    let build = build(
        r#"
export const map = new WeakMap();
export const set = new WeakSet();
export const registered = Symbol.for('registered');
"#,
    );
    let mut host = TestHost::default();
    let vm = Vm::new(build.clone(), &mut host).unwrap();
    let ValueRef::Symbol(registered) = vm.get_export("registered").unwrap().view() else {
        panic!("expected registered symbol");
    };
    let snapshot = vm.snapshot(&[]).unwrap();
    for (export, owner) in [("map", "WeakMap"), ("set", "WeakSet")] {
        let ValueRef::Object(id) = vm.get_export(export).unwrap().view() else {
            panic!("expected weak collection");
        };
        for key in [WeakKey::Symbol(u32::MAX), WeakKey::Symbol(registered)] {
            let mut corrupt = snapshot.clone();
            let record = corrupt.objects.iter_mut().find(|r| r.id == id).unwrap();
            match &mut record.object {
                Object::WeakMap { entries, .. } => {
                    entries.insert(key, Value::UNDEFINED);
                }
                Object::WeakSet { entries, .. } => {
                    entries.insert(key);
                }
                _ => panic!("expected weak collection"),
            }
            assert!(matches!(
                Vm::restore(corrupt, build.clone(), &mut host),
                Err(VmError::InvalidSnapshot(message))
                    if message == format!("{owner} {id} has an invalid key {key}")
            ));
        }
    }
}

#[test]
fn weak_key_json_preserves_tags_and_rejects_noncanonical_ids() {
    use yield_vm::heap::WeakKey;

    let object = WeakKey::Object(7);
    let symbol = WeakKey::Symbol(7);
    assert_ne!(object, symbol);
    for (key, text) in [(object, "\"o:7\""), (symbol, "\"s:7\"")] {
        assert_eq!(serde_json::to_string(&key).unwrap(), text);
        assert_eq!(serde_json::from_str::<WeakKey>(text).unwrap(), key);
    }
    for invalid in [
        "\"x:1\"",
        "\"o:\"",
        "\"s:01\"",
        "\"o:+1\"",
        "\"s:-1\"",
        "\"o: 1\"",
        "\"s:4294967296\"",
        "1",
    ] {
        assert!(
            serde_json::from_str::<WeakKey>(invalid).is_err(),
            "{invalid}"
        );
    }
}

#[test]
fn gc_roots_registered_symbols_and_reclaims_unrooted_ordinary_symbols() {
    use yield_harness::{TestHost, build};
    use yield_vm::{Value, Vm, VmError};

    let build = build(
        r#"
export function fresh() { return Symbol("ordinary"); }
export function registered() { return Symbol.for("registry"); }
"#,
    );
    let mut host = TestHost::default();
    let mut vm = Vm::new(build, &mut host).unwrap();
    vm.collect_garbage(&[]).unwrap();
    let registered = vm.start(&mut host, "registered", vec![]).unwrap();
    let ValueRef::Symbol(id) = vm.start(&mut host, "fresh", vec![]).unwrap().view() else {
        panic!("expected ordinary symbol");
    };
    let roots = [Value::symbol(id)];
    vm.collect_garbage(&roots).unwrap();
    let snapshot = vm.snapshot(&roots).unwrap();
    assert_eq!(snapshot.symbols[&id], Some("ordinary".into()));

    assert_eq!(
        vm.collect_garbage(&[]).unwrap(),
        0,
        "symbols are not counted"
    );
    assert!(matches!(
        vm.collect_garbage(&roots),
        Err(VmError::InvalidState(message))
            if message == format!("garbage collection references missing symbol {id}")
    ));
    assert_eq!(
        vm.start(&mut host, "registered", vec![]).unwrap(),
        registered
    );
}

#[test]
fn gc_preserves_suspended_closures_and_host_instances() {
    use yield_harness::{TestHost, build};
    use yield_vm::{Completion, JsValue, PromiseState, Value, Vm};

    let build = build(
        r#"
import { bind, expect, wait } from "test";
export async function run() {
  const matcher = expect(bind("payload"));
  const prefix = "closure:";
  const read = () => prefix + matcher.value.label();
  const suffix = await wait("a");
  return read() + suffix;
}
"#,
    );
    let mut host = TestHost::default();
    let mut vm = Vm::new(build.clone(), &mut host).unwrap();
    let result = vm.start(&mut host, "run", vec![]).unwrap();
    vm.run_until_idle(&mut host).unwrap();
    let token = host.token("wait:a").unwrap();
    let roots = std::slice::from_ref(&result);
    let before = vm.snapshot(roots).unwrap();
    vm.collect_garbage(roots).unwrap();
    let after = vm.snapshot(roots).unwrap();

    let expected = PromiseState::Fulfilled(Value::string("closure:payload!".into()));
    vm.complete_operation(token, Completion::Fulfilled(JsValue::string("!")))
        .unwrap();
    vm.run_until_idle(&mut host).unwrap();
    assert_eq!(vm.promise_state(&result).unwrap(), expected);

    for snapshot in [before, after] {
        let mut host = TestHost::default();
        let mut restored = Vm::restore(snapshot, build.clone(), &mut host).unwrap();
        restored.start_recovered_operations().unwrap();
        host.complete(
            &mut restored,
            "wait:a",
            Completion::Fulfilled(JsValue::string("!")),
        )
        .unwrap();
        restored.run_until_idle(&mut host).unwrap();
        assert_eq!(restored.promise_state(&result).unwrap(), expected);
    }
}

#[test]
fn gc_active_preserves_only_registered_external_roots() {
    use yield_harness::{TestHost, build};
    use yield_vm::{Value, Vm, VmError};

    let build = build(
        r#"
        import { gc } from "test";
        export function make() { return { value: 42 }; }
        export function collect() { return gc(); }
    "#,
    );
    let mut host = TestHost::default();
    let mut vm = Vm::new(build, &mut host).unwrap();
    let value = vm.start(&mut host, "make", vec![]).unwrap();
    let ValueRef::Object(id) = value.view() else {
        panic!("expected object")
    };
    let roots = [Value::object(id)];
    host.set_gc_roots(roots.to_vec());
    vm.start(&mut host, "collect", vec![]).unwrap();
    let snapshot = vm.snapshot(&roots).unwrap();
    assert!(snapshot.objects.iter().any(|object| object.id == id));

    host.set_gc_roots(vec![]);
    vm.start(&mut host, "collect", vec![]).unwrap();
    assert!(matches!(vm.snapshot(&roots), Err(VmError::InvalidState(_))));
}

#[test]
fn gc_active_rejects_a_missing_root_without_sweeping() {
    use yield_harness::{TestHost, build};
    use yield_vm::{Value, Vm, VmError};

    let build = build(
        r#"
        import { gc } from "test";
        export function make() { return {}; }
        export function collect() {
            try { return gc(); } catch (error) { return "caught"; }
        }
    "#,
    );
    let mut host = TestHost::default();
    let mut vm = Vm::new(build, &mut host).unwrap();
    let value = vm.start(&mut host, "make", vec![]).unwrap();
    host.set_gc_roots(vec![Value::symbol(u32::MAX)]);
    assert!(matches!(
        vm.start(&mut host, "collect", vec![]),
        Err(VmError::InvalidState(message))
            if message == format!("garbage collection references missing symbol {}", u32::MAX)
    ));
    host.set_gc_roots(vec![]);
    vm.snapshot(&[value])
        .expect("failed collection must not sweep the unrooted object");
}

#[test]
fn gc_rejects_a_nonempty_microtask_queue_without_draining_it() {
    use yield_harness::{TestHost, build};
    use yield_vm::{PromiseState, Value, Vm, VmError};

    let build = build("export async function run() { return await 42; }");
    let mut host = TestHost::default();
    let mut vm = Vm::new(build, &mut host).unwrap();
    let result = vm.start(&mut host, "run", vec![]).unwrap();
    let roots = std::slice::from_ref(&result);
    assert!(matches!(
        vm.collect_garbage(roots),
        Err(VmError::InvalidState(message))
            if message == "garbage collection requires a drained microtask queue"
    ));
    assert_eq!(vm.promise_state(&result).unwrap(), PromiseState::Pending);
    vm.run_until_idle(&mut host).unwrap();
    vm.collect_garbage(roots).unwrap();
    assert_eq!(
        vm.promise_state(&result).unwrap(),
        PromiseState::Fulfilled(Value::number(42.0))
    );
}

#[test]
fn gc_discards_unreachable_uncalled_resolvers() {
    use yield_harness::{TestHost, build};
    use yield_vm::{PromiseState, Value, Vm, VmError};

    let build = build(
        r#"
let saved;
export function pending() {
  new Promise(() => {});
  return new Promise(resolve => { saved = resolve; });
}
export function settle() { saved(42); }
"#,
    );
    let mut host = TestHost::default();
    let mut vm = Vm::new(build, &mut host).unwrap();
    let result = vm.start(&mut host, "pending", vec![]).unwrap();
    let roots = std::slice::from_ref(&result);
    assert!(matches!(
        vm.snapshot(roots),
        Err(VmError::InvalidState(message)) if message == "snapshot during pending thenable assimilation"
    ));
    assert!(vm.collect_garbage(roots).unwrap() >= 3);
    vm.start(&mut host, "settle", vec![]).unwrap();
    assert_eq!(
        vm.promise_state(&result).unwrap(),
        PromiseState::Fulfilled(Value::number(42.0))
    );
    vm.snapshot(roots)
        .expect("GC must remove stale resolver records");
}

#[test]
fn packed_value_distinguishes_nan_tags_and_empty_slots() {
    use std::mem::size_of;
    use yield_vm::{JsString, Value};

    assert_eq!(size_of::<Value>(), 8);
    assert_eq!(size_of::<Option<Value>>(), 8);
    assert_eq!(size_of::<JsString>(), 8);

    for prefix in 0xfff8_u64..=0xffff {
        let value = Value::number(f64::from_bits(prefix << 48 | 1));
        assert!(
            matches!(value.view(), ValueRef::Number(n) if n.to_bits() == 0x7ff8_0000_0000_0000)
        );
        assert_ne!(value, value.clone());
    }

    for number in [
        0.0,
        -0.0,
        f64::from_bits(1),
        f64::INFINITY,
        f64::NEG_INFINITY,
    ] {
        assert!(
            matches!(Value::number(number).view(), ValueRef::Number(n) if n.to_bits() == number.to_bits())
        );
    }
    assert_eq!(Value::number(-0.0), Value::number(0.0));
    assert!(matches!(
        Some(Value::UNDEFINED).as_ref().map(Value::view),
        Some(ValueRef::Undefined)
    ));
    assert_ne!(Some(Value::UNDEFINED), None);

    let empty = Value::string(JsString::new());
    assert!(matches!(empty.view(), ValueRef::String(s) if s.is_empty()));
    assert_ne!(Some(empty.clone()), None);
    assert!(empty.into_string().unwrap().is_empty());
    assert!(Value::UNDEFINED.into_string().is_none());

    let object = Value::object(u32::MAX);
    let symbol = Value::symbol(u32::MAX);
    assert!(matches!(object.view(), ValueRef::Object(u32::MAX)));
    assert!(matches!(symbol.view(), ValueRef::Symbol(u32::MAX)));
    assert_ne!(object, symbol);
}

#[test]
fn packed_value_transfers_string_ownership_and_keeps_copy_on_write() {
    use yield_vm::{JsString, Value};

    for units in [vec![0x41, 0xe9], vec![0x41, 0xd800]] {
        let original = Value::string(JsString::from_units(units.clone()));
        let (owned, left, right) = (original.clone(), original.clone(), original.clone());
        drop(original);

        let mut owned = owned.into_string().unwrap();
        owned.push('!');
        assert!(
            owned
                .units()
                .eq(units.iter().copied().chain([u16::from(b'!')]))
        );

        for saved in [left, right] {
            let ValueRef::String(saved) = saved.view() else {
                panic!("expected a string");
            };
            assert!(saved.units().eq(units.iter().copied()));
        }
    }
}

#[test]
fn packed_value_keeps_legacy_json() {
    use yield_vm::Value;

    let number_json = r#"{"number":"NaN"}"#;
    let value: Value = serde_json::from_str(number_json).unwrap();
    assert!(matches!(value.view(), ValueRef::Number(n) if n.is_nan()));
    assert_eq!(serde_json::to_string(&value).unwrap(), number_json);

    let string_json = r#"{"string":{"utf16":[55296]}}"#;
    let value: Value = serde_json::from_str(string_json).unwrap();
    assert!(matches!(value.view(), ValueRef::String(s) if s.units().eq([0xd800])));
    assert_eq!(serde_json::to_string(&value).unwrap(), string_json);
    assert!(serde_json::from_str::<Value>(r#"{"number":"unknown"}"#).is_err());
}
