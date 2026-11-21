//! Snapshot restore preserves reachable objects, descriptors, and identity.

use super::support::{Suspended, baseline_at_first_await, count_delta, json_round_trip};
use yield_harness::number;
use yield_vm::heap::{Property, WeakKey};
use yield_vm::{Completion, JsString, JsValue, Object, PromiseState, Value, VmError};

#[test]
fn restore_rejects_private_stamp_with_non_class_brand() {
    let mut suspended = Suspended::start(
        r#"
        import { wait } from 'test';
        class C { #x = 4; read() { return this.#x; } }
        export async function run() {
            const value = new C();
            await wait('private');
            return value.read();
        }
    "#,
        vec![],
    );
    let snapshot = suspended.snapshot();
    let mut corrupt = serde_json::to_value(&snapshot).expect("serialize snapshot");
    let stamp = &mut corrupt["private_elements"][0];
    stamp["brand"] = stamp["receiver"].clone();
    let corrupt = serde_json::from_value(corrupt).expect("deserialize snapshot");
    assert!(
        matches!(suspended.restore(corrupt), Err(VmError::InvalidSnapshot(detail)) if detail.contains("private brand"))
    );
    suspended.restore(snapshot).expect("valid snapshot");
    suspended.complete("wait:private", Completion::Fulfilled(number(0.0)));
    assert_eq!(
        suspended.state(),
        PromiseState::Fulfilled(Value::Number(4.0))
    );
}

#[test]
fn restore_rejects_missing_method_home() {
    let source = r#"
import { wait } from 'test';
class A { m() { return 3; } }
class B extends A { m() { return super.m(); } }
export async function run() {
  const b = new B();
  await wait('home');
  return b.m();
}
"#;
    let mut suspended = Suspended::start(source, vec![]);
    let snapshot = suspended.snapshot();
    let mut corrupt = serde_json::to_value(&snapshot).expect("serialize snapshot");
    let records = corrupt["objects"].as_array_mut().expect("object records");
    let record = records
        .iter_mut()
        .find_map(|record| {
            let function = record.get_mut("object")?.get_mut("function")?;
            (function["has_class_metadata"] == true && function["home"].is_number())
                .then_some(function)
        })
        .expect("class method record");
    record["home"] = serde_json::Value::Null;
    let corrupt = serde_json::from_value(corrupt).expect("deserialize snapshot");
    assert!(
        matches!(suspended.restore(corrupt), Err(VmError::InvalidSnapshot(detail)) if detail.contains("home object"))
    );
    suspended.restore(snapshot).expect("valid snapshot");
    suspended.complete("wait:home", Completion::Fulfilled(number(0.0)));
    assert_eq!(
        suspended.state(),
        PromiseState::Fulfilled(Value::Number(3.0))
    );
}

#[test]
fn accessor_only_closures_survive_restore_and_bad_descriptors_are_rejected() {
    let source = r#"
import { seed, wait } from "test";
function make() {
  let hidden = 2;
  return {
    get x() { seed(); return hidden; },
    set x(v) { hidden += v; }
  };
}
export async function run() {
  const o = make();
  await wait("a");
  o.x = 5;
  return o.x;
}
"#;
    let mut s = Suspended::start(source, vec![]);
    let snapshot = s.snapshot();
    let (index, get, set) = snapshot
        .objects
        .iter()
        .enumerate()
        .find_map(|(index, record)| {
            let Object::Plain(plain) = &record.object else {
                return None;
            };
            let Property::Accessor {
                get: Some(get),
                set: Some(set),
            } = plain.props.get(&JsString::from("x"))?
            else {
                return None;
            };
            Some((index, *get, *set))
        })
        .expect("accessor pair");

    for (get, set, message) in [
        (Some(u32::MAX), Some(set), "missing object"),
        (
            Some(get),
            Some(snapshot.objects[index].id),
            "invalid target",
        ),
    ] {
        let mut corrupt = snapshot.clone();
        let Object::Plain(plain) = &mut corrupt.objects[index].object else {
            unreachable!()
        };
        *plain.props.get_mut(&JsString::from("x")).expect("accessor") =
            Property::Accessor { get, set };
        let error = s.restore(corrupt).expect_err("bad descriptor");
        assert!(matches!(error, VmError::InvalidSnapshot(detail) if detail.contains(message)));
    }

    let seed = s.host.count("seed");
    s.restore(snapshot).expect("valid descriptor");
    assert_eq!(
        count_delta(s.host.count("seed"), seed),
        0,
        "restore must not run getters"
    );
    let seed = s.host.count("seed");
    s.complete("wait:a", Completion::Fulfilled(number(0.0)));
    assert_eq!(s.state(), PromiseState::Fulfilled(Value::Number(7.0)));
    assert_eq!(count_delta(s.host.count("seed"), seed), 1);
}

#[test]
fn unreachable_native_wrappers_do_not_require_rebinding() {
    let source = r#"
import { bind, wait } from "test";
export async function run() {
  bind("temporary");
  await wait("a");
  return 1;
}
"#;
    let mut s = Suspended::start(source, vec![]);
    let snapshot = s.snapshot();
    assert!(
        snapshot
            .host_instances
            .iter()
            .all(|record| record.class != "Node")
    );
    s.restore(snapshot).expect("transient native is not a root");
}

#[test]
fn closures_and_their_environment_survive_snapshot() {
    let source = r#"
import { wait } from "test";
export async function run() {
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
    let mut s = Suspended::start(source, vec![]);
    let snapshot = s.snapshot();
    assert!(
        snapshot
            .objects
            .iter()
            .any(|record| matches!(&record.object, Object::Function(function) if Some(function.env) == snapshot.continuations[0].env)),
        "the escaped closure is reachable from the suspended environment"
    );
    assert!(
        snapshot
            .objects
            .iter()
            .any(|record| Some(record.id) == snapshot.continuations[0].env
                && matches!(record.object, Object::Environment(_))),
        "the captured environment is reachable"
    );
    s.restore(json_round_trip(&snapshot)).expect("restore");
    s.complete("wait:a", Completion::Fulfilled(number(2.0)));
    assert_eq!(s.state(), PromiseState::Fulfilled(Value::Number(23.0)));
}

#[test]
fn object_identity_survives_snapshot() {
    let source = r#"
import { wait } from "test";
export async function run() {
  const obj = { n: 1, list: [2] };
  const alias = obj;
  await wait("a");
  alias.n = 5;
  alias.list[0] = 6;
  return obj.n + obj.list[0];
}
"#;
    let mut s = Suspended::start(source, vec![]);
    let snapshot = s.snapshot();
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
    s.restore(snapshot).expect("restore");
    s.complete("wait:a", Completion::Fulfilled(number(1.0)));
    assert_eq!(s.state(), PromiseState::Fulfilled(Value::Number(11.0)));
}

#[test]
fn restore_validates_internal_prototypes_but_allows_property_cycles() {
    let source = r#"
import { wait } from "test";
function Box() {}

export async function run() {
  Box.prototype.x = 7;
  const first = new Box();
  const second = new Box();
  first.prototype = second;
  second.peer = first;
  const n = 1;
  function captured() { return n; }
  await wait("a");
  return first.x === 7 && second.x === 7 &&
    first.prototype === second && second.peer === first && captured() === 1;
}
"#;
    let mut s = Suspended::start(source, vec![]);
    let snapshot = s.snapshot();
    let (first, second) = snapshot
        .objects
        .iter()
        .find_map(|record| match &record.object {
            Object::Plain(plain) => match plain.props.get(&JsString::from("prototype")) {
                Some(Property::Data(Value::Object(peer))) => Some((record.id, *peer)),
                _ => None,
            },
            _ => None,
        })
        .expect("instances linked by a regular property named prototype");
    let env = snapshot
        .objects
        .iter()
        .find_map(|record| match &record.object {
            Object::Function(function) => Some(function.env),
            _ => None,
        })
        .expect("captured environment");
    let missing = u32::MAX;
    for (case, target, reverse, expected) in [
        (
            "self cycle",
            first,
            false,
            format!("prototype chain of object {first} is cyclic"),
        ),
        (
            "two-node cycle",
            second,
            true,
            format!("prototype chain of object {} is cyclic", first.min(second)),
        ),
        (
            "missing target",
            missing,
            false,
            format!("object {first} prototype {missing} does not exist"),
        ),
        (
            "environment target",
            env,
            false,
            format!("object {first} prototype {env} is an environment"),
        ),
    ] {
        let mut invalid = snapshot.clone();
        for record in &mut invalid.objects {
            if let Object::Plain(plain) = &mut record.object {
                if record.id == first {
                    plain.prototype = Some(target);
                } else if reverse && record.id == second {
                    plain.prototype = Some(first);
                }
            }
        }
        let error = s.restore(invalid).expect_err(case);
        assert!(
            matches!(&error, VmError::InvalidSnapshot(message) if *message == expected),
            "{case}: {error}"
        );
    }

    s.restore(json_round_trip(&snapshot)).expect("restore");
    s.complete("wait:a", Completion::Fulfilled(number(1.0)));
    assert_eq!(s.state(), PromiseState::Fulfilled(Value::Bool(true)));
}

#[test]
fn restore_allows_reparented_builtin_prototypes_but_rejects_non_null_object_prototype() {
    let source = r#"
import { wait } from "test";
export async function run() {
  const arrayPrototype = Object.getPrototypeOf([]);
  Object.setPrototypeOf(arrayPrototype, null);
  await wait("a");
  return Object.getPrototypeOf([]) === arrayPrototype &&
    Object.getPrototypeOf(arrayPrototype) === null &&
    Object.getPrototypeOf(Object.prototype) === null;
}
"#;
    let mut s = Suspended::start(source, vec![]);
    let snapshot = s.snapshot();
    let mut invalid = snapshot.clone();
    let object_prototype = invalid.roots["object.prototype"];
    let array_prototype = invalid.roots["array.prototype"];
    let record = invalid
        .objects
        .iter_mut()
        .find(|record| record.id == object_prototype)
        .expect("Object.prototype in snapshot");
    let Object::Plain(plain) = &mut record.object else {
        panic!("Object.prototype must be a plain object");
    };
    // The array prototype has a null parent, so this edge does not create a cycle.
    plain.prototype = Some(array_prototype);
    let error = s
        .restore(invalid)
        .expect_err("non-null Object.prototype parent");
    assert!(matches!(
        error,
        VmError::InvalidSnapshot(detail) if detail == "Object.prototype must have a null prototype"
    ));

    s.restore(json_round_trip(&snapshot)).expect("restore");
    s.complete("wait:a", Completion::Fulfilled(number(0.0)));
    assert_eq!(s.state(), PromiseState::Fulfilled(Value::Bool(true)));
}

#[test]
fn builtin_roots_and_function_identity_survive_restore() {
    let source = r#"
import { wait } from "test";
export async function run() {
  const then = Promise.prototype.then;
  const fn = demo;
  const arrayConstructor = Array;
  const copied = Array.from([8, 4]).slice(1);
  await wait("a");
  return then === Promise.prototype.then && fn === demo && new demo().n === copied[0]
    && arrayConstructor === Array && Array.prototype.constructor === Array
    && Object.getPrototypeOf(copied) === Array.prototype
    && Object.getPrototypeOf([]) === Array.prototype && copied[0];
}
function demo() {
  this.n = 4;
}
"#;
    let mut s = Suspended::start(source, vec![]);
    let snapshot = s.snapshot();
    let baseline = baseline_at_first_await();
    assert_eq!(count_delta(snapshot.roots.len(), baseline.roots.len()), 0);
    assert_eq!(
        snapshot.roots.keys().collect::<Vec<_>>(),
        baseline.roots.keys().collect::<Vec<_>>()
    );

    let mut missing = snapshot.clone();
    missing.roots.remove("array").expect("Array root");
    let error = s.restore(missing).expect_err("missing root");
    assert!(matches!(error, VmError::InvalidSnapshot(_)));

    s.restore(snapshot).expect("restore");
    s.complete("wait:a", Completion::Fulfilled(JsValue::number(0.0)));
    assert_eq!(s.state(), PromiseState::Fulfilled(Value::Number(4.0)));
}

#[test]
fn bound_function_survives_restore() {
    let source = r#"
import { wait } from "test";
export async function run() {
  const captured = 4;
  const fn = function (arg, v) { return captured + this.n + arg.n + v; };
  const bound = fn.bind({ n: 3 }, { n: 5 });
  const v = await wait("a");
  return ({ own: true }).hasOwnProperty("own") && bound(v);
}
"#;
    let mut s = Suspended::start(source, vec![]);
    let snapshot = s.snapshot();
    let bound_record = snapshot
        .objects
        .iter()
        .find(|record| matches!(record.object, Object::Bound(_)))
        .expect("bound function is reachable");

    let mut invalid = snapshot.clone();
    let record = invalid
        .objects
        .iter_mut()
        .find(|r| r.id == bound_record.id)
        .expect("bound record in cloned snapshot");
    let Object::Bound(bound) = &mut record.object else {
        unreachable!();
    };
    bound.target = invalid.roots["object.prototype"];
    let error = s.restore(invalid).expect_err("invalid bound target");
    assert!(matches!(error, VmError::InvalidSnapshot(message) if message.contains("bound target")));

    let bound_record = bound_record.clone();
    s.restore(snapshot).expect("restore");
    let recaptured = s.snapshot();
    assert_eq!(
        recaptured.objects.iter().find(|r| r.id == bound_record.id),
        Some(&bound_record)
    );
    s.complete("wait:a", Completion::Fulfilled(JsValue::number(2.0)));
    assert_eq!(s.state(), PromiseState::Fulfilled(Value::Number(14.0)));
}

#[test]
fn restore_rejects_invalid_property_ordinals() {
    let source = r#"
import { wait } from "test";
export async function run() {
  const obj = { z: 1, a: 2 };
  await wait("a");
  return Reflect.ownKeys(obj)[0];
}
"#;
    let mut s = Suspended::start(source, vec![]);
    let snapshot = s.snapshot();
    let index = snapshot.objects.iter().position(|record| {
        matches!(&record.object, Object::Plain(plain) if plain.props.get(&JsString::from("z")).is_some())
    }).expect("live object");
    for invalid_ordinal in [0, 2] {
        let mut corrupt = serde_json::to_value(&snapshot).expect("serialize snapshot");
        let props = corrupt["objects"][index]["object"]["plain"]["props"]
            .as_array_mut()
            .expect("property records");
        let entry = props
            .iter_mut()
            .find(|entry| entry[0].as_str() == Some("a"))
            .expect("property a");
        entry[1]["ordinal"] = serde_json::json!(invalid_ordinal);
        let error = s
            .restore(serde_json::from_value(corrupt).expect("deserialize snapshot"))
            .expect_err("invalid property ordinal");
        assert!(
            matches!(error, VmError::InvalidSnapshot(detail) if detail.contains("invalid property tables"))
        );
    }
}

#[test]
fn snapshot_deserialization_rejects_duplicate_property_records_and_invalid_units() {
    let source = r#"
import { wait } from "test";
export async function run() {
  const obj = { "\uD800": 42 };
  await wait("a");
  return obj["\uD800"];
}
"#;
    let s = Suspended::start(source, vec![]);
    let snapshot = serde_json::to_value(s.snapshot()).expect("serialize snapshot");
    let key = serde_json::json!({"utf16": [55296]});
    let index = snapshot["objects"]
        .as_array()
        .unwrap()
        .iter()
        .position(|record| {
            record["object"]["plain"]["props"]
                .as_array()
                .is_some_and(|props| props.iter().any(|entry| entry[0] == key))
        })
        .expect("live lone-surrogate property");

    let mut duplicate = snapshot.clone();
    let props = duplicate["objects"][index]["object"]["plain"]["props"]
        .as_array_mut()
        .unwrap();
    let record = props.iter().find(|entry| entry[0] == key).unwrap().clone();
    props.push(record);
    let error = serde_json::from_value::<yield_vm::Snapshot>(duplicate).unwrap_err();
    assert!(error.to_string().contains("duplicate"), "{error}");

    let mut invalid = snapshot;
    let props = invalid["objects"][index]["object"]["plain"]["props"]
        .as_array_mut()
        .unwrap();
    let record = props.iter_mut().find(|entry| entry[0] == key).unwrap();
    record[0]["utf16"][0] = serde_json::json!(65536);
    assert!(serde_json::from_value::<yield_vm::Snapshot>(invalid).is_err());
}

#[test]
fn restore_rejects_writable_object_prototype() {
    let mut s = Suspended::start(
        "import { wait } from 'test'; export async function run() { await wait('a'); }",
        vec![],
    );
    let snapshot = s.snapshot();
    let index = snapshot
        .objects
        .iter()
        .position(|record| record.id == snapshot.roots["object"])
        .expect("Object constructor in snapshot");
    let mut corrupt = serde_json::to_value(snapshot).expect("serialize snapshot");
    let props = corrupt["objects"][index]["object"]["native"]["Intrinsic"]["object"]["props"]
        .as_array_mut()
        .expect("property records");
    let entry = props
        .iter_mut()
        .find(|entry| entry[0].as_str() == Some("prototype"))
        .expect("prototype property");
    entry[1]["attributes"]["writable"] = serde_json::json!(true);
    let error = s
        .restore(serde_json::from_value(corrupt).expect("deserialize snapshot"))
        .expect_err("writable Object.prototype");
    assert!(
        matches!(error, VmError::InvalidSnapshot(detail) if detail.contains("Object.prototype"))
    );
}

#[test]
fn restore_rejects_array_index_past_length() {
    let mut s = Suspended::start(
        "import { wait } from 'test'; export async function run() { const a = []; Object.defineProperty(a, '9', { value: 4 }); await wait('a'); return a[9]; }",
        vec![],
    );
    let mut snapshot = serde_json::to_value(s.snapshot()).expect("serialize snapshot");
    let array = snapshot["objects"]
        .as_array_mut()
        .expect("object records")
        .iter_mut()
        .filter_map(|record| record["object"].get_mut("array"))
        .find(|array| array["length"] == 10)
        .expect("sparse array in snapshot");
    array["length"] = serde_json::json!(9);
    let error = s
        .restore(serde_json::from_value(snapshot).expect("deserialize snapshot"))
        .expect_err("array element outside length");
    assert!(
        matches!(error, VmError::InvalidSnapshot(detail) if detail.contains("invalid property tables"))
    );
}

#[test]
fn snapshot_decode_rejects_invalid_regexp_source_and_flags() {
    let s = Suspended::start(
        "import { wait } from 'test'; export async function run() { const re = /foo/g; await wait('a'); return re.test('foo'); }",
        vec![],
    );
    let snapshot = serde_json::to_value(s.snapshot()).expect("serialize snapshot");
    let index = snapshot["objects"]
        .as_array()
        .expect("object records")
        .iter()
        .position(|record| record["object"]["reg_exp"]["source"] == "foo")
        .expect("live RegExp in snapshot");
    for (field, invalid) in [("source", "["), ("flags", "gg")] {
        let mut corrupt = snapshot.clone();
        corrupt["objects"][index]["object"]["reg_exp"][field] = serde_json::json!(invalid);
        let json = serde_json::to_string(&corrupt).expect("serialize corrupted snapshot");
        assert!(
            yield_vm::Snapshot::from_json(&json).is_err(),
            "accepted invalid RegExp {field}: {invalid:?}"
        );
    }
}

#[test]
fn restore_rejects_corrupt_collection_records() {
    let mut s = Suspended::start(
        "import { wait } from 'test'; export async function run() { const m = new Map([[{}, Symbol()]]); const i = m.keys(); const s = new Set([{}]); await wait('a'); return i.next().done || s.size; }",
        vec![],
    );
    let snapshot = s.snapshot();
    for (fault, message) in [
        (0, "missing object"),
        (1, "missing symbol"),
        (2, "missing object"),
        (3, "invalid target"),
        (4, "invalid target"),
        (5, "invalid target"),
    ] {
        let mut corrupt = snapshot.clone();
        let set_id = corrupt
            .objects
            .iter()
            .find(|record| matches!(record.object, Object::Set { .. }))
            .unwrap()
            .id;
        let changed = corrupt.objects.iter_mut().any(|record| {
            match (fault, &mut record.object) {
                (0, Object::Map { entries, .. }) => {
                    entries[0].as_mut().unwrap().0 = Value::Object(u32::MAX)
                }
                (1, Object::Map { entries, .. }) => {
                    entries[0].as_mut().unwrap().1 = Value::Symbol(u32::MAX)
                }
                (2, Object::Set { elements, .. }) => elements[0] = Some(Value::Object(u32::MAX)),
                (3, Object::CollectionIterator { target, .. }) => *target = Some(set_id),
                (4, Object::CollectionIterator { target, .. }) => *target = Some(u32::MAX),
                (5, Object::CollectionIterator { next_index, .. }) => *next_index = usize::MAX,
                _ => return false,
            }
            true
        });
        assert!(changed);
        let error = s.restore(corrupt).expect_err("corrupt collection record");
        assert!(matches!(error, VmError::InvalidSnapshot(detail) if detail.contains(message)));
    }
}

#[test]
fn restore_rejects_malformed_proxy_records() {
    let mut s = Suspended::start(
        "import { wait } from 'test'; export async function run() { const p = new Proxy({}, {}); await wait('a'); return p.x; }",
        vec![],
    );
    let snapshot = s.snapshot();
    assert_eq!(snapshot.format_version, yield_vm::SNAPSHOT_FORMAT_VERSION);
    let index = snapshot
        .objects
        .iter()
        .position(|record| matches!(&record.object, Object::Proxy { .. }))
        .expect("live proxy record");
    let proxy_id = snapshot.objects[index].id;
    let constructor = snapshot.roots["proxy"];
    let environment = snapshot
        .objects
        .iter()
        .find(|record| matches!(&record.object, Object::Environment(_)))
        .expect("live environment")
        .id;
    for (target_override, handler_override, message) in [
        (Some(u32::MAX), None, "missing object"),
        (None, Some(u32::MAX), "missing object"),
        (Some(constructor), None, "invalid target"),
        (None, Some(environment), "invalid handler"),
        (Some(proxy_id), None, "cyclic"),
    ] {
        let mut corrupt = snapshot.clone();
        let Object::Proxy {
            target, handler, ..
        } = &mut corrupt.objects[index].object
        else {
            unreachable!("proxy record");
        };
        if let Some(id) = target_override {
            *target = id;
        }
        if let Some(id) = handler_override {
            *handler = id;
        }
        let error = s.restore(corrupt).expect_err("malformed proxy record");
        assert!(matches!(error, VmError::InvalidSnapshot(detail) if detail.contains(message)));
    }
}

#[test]
fn weakmap_snapshot_drops_unreachable_entries_and_values() {
    let mut s = Suspended::start(
        r#"
import { bind, wait } from "test";
const live = {};
function makeMap() {
  const a = {};
  const b = {};
  return new WeakMap([[a, b], [b, { back: a, native: bind('weak-only') }], [live, 7]]);
}
export async function run() {
  const map = makeMap();
  await wait('a');
  return map.get(live);
}
"#,
        vec![],
    );
    let snapshot = json_round_trip(&s.snapshot());
    let entries = snapshot
        .objects
        .iter()
        .find_map(|record| match &record.object {
            Object::WeakMap { entries, .. } => Some(entries),
            _ => None,
        })
        .expect("reachable WeakMap");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries.values().next(), Some(&Value::Number(7.0)));
    assert!(
        snapshot
            .host_instances
            .iter()
            .all(|record| record.class != "Node")
    );
    s.restore(snapshot)
        .expect("weak-only native is not rebound");
    s.complete("wait:a", Completion::Fulfilled(JsValue::undefined()));
    assert_eq!(s.state(), PromiseState::Fulfilled(Value::Number(7.0)));
}

#[test]
fn restore_rejects_dangling_weakmap_entries() {
    let mut s = Suspended::start(
        "import { wait } from 'test'; export async function run() { const key = {}; const map = new WeakMap([[key, 1]]); await wait('a'); return map.get(key); }",
        vec![],
    );
    let snapshot = s.snapshot();
    for (key, value) in [
        (Some(WeakKey::Object(u32::MAX)), Value::Number(1.0)),
        (None, Value::Object(u32::MAX)),
        (None, Value::Symbol(u32::MAX)),
    ] {
        let mut corrupt = snapshot.clone();
        let entries = corrupt
            .objects
            .iter_mut()
            .find_map(|record| match &mut record.object {
                Object::WeakMap { entries, .. } => Some(entries),
                _ => None,
            })
            .expect("reachable WeakMap");
        let (original_key, _) = entries.pop_first().expect("live entry");
        entries.insert(key.unwrap_or(original_key), value);
        let error = s.restore(corrupt).expect_err("dangling WeakMap entry");
        assert!(matches!(error, VmError::InvalidSnapshot(_)), "{error:?}");
    }
}
