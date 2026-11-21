#[path = "e2e/mod.rs"]
mod suite;

#[test]
fn restore_rejects_a_missing_boxed_symbol_payload() {
    use yield_poc::{TestHost, build};
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
fn build_round_trips_special_numbers() {
    use yield_vm::{Build, bytecode::Constant};

    let mut build = yield_poc::build("export function run() { return 0; }");
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
fn non_finite_operation_completions_survive_restore() {
    use yield_poc::{TestHost, build};
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
                vm.start_recovered_operations(&mut host).unwrap();
            }
            vm.complete_operation(host.token(label).unwrap(), completion)
                .unwrap();
            vm.run_until_idle(&mut host).unwrap();
        }
        assert_eq!(
            vm.promise_state(&result).unwrap(),
            PromiseState::Fulfilled(Value::Bool(true))
        );
    }
}

#[test]
fn restore_rejects_changed_has_instance_description() {
    use yield_poc::{TestHost, build};
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
    use yield_poc::{TestHost, build};
    use yield_vm::{Value, Vm, VmError};

    let build = build(
        "export function cycle() { const object = {}; object.self = object; return object; }",
    );
    let mut host = TestHost::default();
    let mut vm = Vm::new(build, &mut host).unwrap();
    vm.collect_garbage(&[]).unwrap();

    let cycle = vm.start(&mut host, "cycle", vec![]).unwrap();
    let Value::Object(id) = cycle else {
        panic!("expected cycle object");
    };
    let roots = [Value::Object(id), Value::Object(id)];
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
    let Value::Object(next_id) = next else {
        panic!("expected new cycle object");
    };
    assert_ne!(id, next_id);
    let missing = u32::MAX;
    assert!(matches!(
        vm.collect_garbage(&[Value::Symbol(missing)]),
        Err(VmError::InvalidState(message))
            if message == format!("garbage collection references missing symbol {missing}")
    ));
    let snapshot = vm.snapshot(&[next]).expect("failed GC must not sweep");
    assert!(snapshot.objects.iter().any(|record| record.id == next_id));
}

#[test]
fn gc_traces_weak_map_values_only_for_live_keys() {
    use yield_poc::{TestHost, build};
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
        Value::Number(42.0)
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
    use yield_poc::{TestHost, build, number};
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
    let [
        Value::Object(map),
        Value::Object(set),
        Value::Symbol(b),
        Value::Symbol(backref),
    ] = [0.0, 1.0, 2.0, 3.0].map(|i| vm.start(&mut host, "take", vec![number(i)]).unwrap())
    else {
        panic!("expected two collections and two symbols");
    };
    let roots = [Value::Object(map), Value::Object(set), Value::Symbol(b)];
    vm.collect_garbage(&roots).unwrap();
    let snapshot = vm.snapshot(&roots).unwrap();
    let map_record = snapshot.objects.iter().find(|r| r.id == map).unwrap();
    let Object::WeakMap { entries, .. } = &map_record.object else {
        panic!("expected WeakMap");
    };
    assert_eq!(entries.len(), 2);
    let Value::Symbol(a) = entries[&WeakKey::Symbol(b)] else {
        panic!("expected symbol value");
    };
    assert!(a < b, "a must be visited before b unlocks it");
    let Value::Object(value) = entries[&WeakKey::Symbol(a)] else {
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
        vm.collect_garbage(&[Value::Symbol(backref)]),
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
    assert!(vm.collect_garbage(&[Value::Object(value)]).is_err());
    for id in [a, b] {
        assert!(!snapshot.symbols.contains_key(&id));
        assert!(matches!(
            vm.collect_garbage(&[Value::Symbol(id)]),
            Err(VmError::InvalidState(message))
                if message == format!("garbage collection references missing symbol {id}")
        ));
    }
}

#[test]
fn restore_rejects_missing_and_registered_symbol_keys_in_weak_collections() {
    use yield_poc::{TestHost, build};
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
    let Value::Symbol(registered) = vm.get_export("registered").unwrap() else {
        panic!("expected registered symbol");
    };
    let snapshot = vm.snapshot(&[]).unwrap();
    for (export, owner) in [("map", "WeakMap"), ("set", "WeakSet")] {
        let Value::Object(id) = vm.get_export(export).unwrap() else {
            panic!("expected weak collection");
        };
        for key in [WeakKey::Symbol(u32::MAX), WeakKey::Symbol(registered)] {
            let mut corrupt = snapshot.clone();
            let record = corrupt.objects.iter_mut().find(|r| r.id == id).unwrap();
            match &mut record.object {
                Object::WeakMap { entries, .. } => {
                    entries.insert(key, Value::Undefined);
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
    use yield_poc::{TestHost, build};
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
    let Value::Symbol(id) = vm.start(&mut host, "fresh", vec![]).unwrap() else {
        panic!("expected ordinary symbol");
    };
    let roots = [Value::Symbol(id)];
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
    use yield_poc::{TestHost, build};
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

    let expected = PromiseState::Fulfilled(Value::String("closure:payload!".into()));
    vm.complete_operation(token, Completion::Fulfilled(JsValue::string("!")))
        .unwrap();
    vm.run_until_idle(&mut host).unwrap();
    assert_eq!(vm.promise_state(&result).unwrap(), expected);

    for snapshot in [before, after] {
        let mut host = TestHost::default();
        let mut restored = Vm::restore(snapshot, build.clone(), &mut host).unwrap();
        restored.start_recovered_operations(&mut host).unwrap();
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
fn gc_rejects_a_nonempty_microtask_queue_without_draining_it() {
    use yield_poc::{TestHost, build};
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
        PromiseState::Fulfilled(Value::Number(42.0))
    );
}

#[test]
fn gc_discards_unreachable_uncalled_resolvers() {
    use yield_poc::{TestHost, build};
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
        PromiseState::Fulfilled(Value::Number(42.0))
    );
    vm.snapshot(roots)
        .expect("GC must remove stale resolver records");
}
