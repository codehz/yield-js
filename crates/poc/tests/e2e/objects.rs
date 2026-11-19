//! Snapshot restore preserves reachable objects, descriptors, and identity.

use super::support::{Suspended, json_round_trip};
use yield_poc::number;
use yield_vm::heap::Property;
use yield_vm::{Completion, HostValue, Object, PromiseState, Value, VmError};

#[test]
fn accessor_only_closures_survive_restore_and_bad_descriptors_are_rejected() {
    let source = r#"
function make() {
  let hidden = 2;
  return {
    get x() { seed(); return hidden; },
    set x(v) { hidden += v; }
  };
}
async function run() {
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
            } = plain.props.get("x")?
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
        (None, None, "empty accessor"),
    ] {
        let mut corrupt = snapshot.clone();
        let Object::Plain(plain) = &mut corrupt.objects[index].object else {
            unreachable!()
        };
        *plain.props.get_mut("x").expect("accessor") = Property::Accessor { get, set };
        let error = s.restore(corrupt).expect_err("bad descriptor");
        assert!(matches!(error, VmError::InvalidSnapshot(detail) if detail.contains(message)));
    }

    s.restore(snapshot).expect("valid descriptor");
    assert_eq!(s.host.count("seed"), 0, "restore must not run getters");
    s.complete("wait:a", Completion::Fulfilled(number(0.0)));
    assert_eq!(s.state(), PromiseState::Fulfilled(Value::Number(7.0)));
    assert_eq!(s.host.count("seed"), 1);
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
    let mut s = Suspended::start(source, vec![]);
    let snapshot = s.snapshot();
    assert!(
        snapshot
            .objects
            .iter()
            .all(|record| !matches!(record.object, Object::Native(_)))
    );
    s.host.reject_native = true;
    s.restore(snapshot).expect("transient native is not a root");
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
    let mut s = Suspended::start(source, vec![]);
    let snapshot = s.snapshot();
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
    s.restore(json_round_trip(&snapshot)).expect("restore");
    s.complete("wait:a", Completion::Fulfilled(number(2.0)));
    assert_eq!(s.state(), PromiseState::Fulfilled(Value::Number(23.0)));
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
function Box() {}

async function run() {
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
            Object::Plain(plain) => match plain.props.get("prototype") {
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
            Object::Function(function) => function.env,
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
    let mut s = Suspended::start(source, vec![]);
    let snapshot = s.snapshot();
    assert_eq!(snapshot.roots.len(), 9);
    assert!(!snapshot.functions.is_empty());

    let mut missing = snapshot.clone();
    missing.roots.pop_first();
    let error = s.restore(missing).expect_err("missing root");
    assert!(matches!(error, VmError::InvalidSnapshot(_)));

    s.restore(snapshot).expect("restore");
    s.complete("wait:a", Completion::Fulfilled(HostValue::Number(0.0)));
    assert_eq!(s.state(), PromiseState::Fulfilled(Value::Number(4.0)));
}

#[test]
fn bound_function_survives_restore() {
    let source = r#"
async function run() {
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
    s.complete("wait:a", Completion::Fulfilled(HostValue::Number(2.0)));
    assert_eq!(s.state(), PromiseState::Fulfilled(Value::Number(14.0)));
}
