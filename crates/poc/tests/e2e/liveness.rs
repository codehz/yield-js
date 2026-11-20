//! Snapshots retain live values across suspension edges and omit dead objects.

use super::support::{Suspended, baseline_at_first_await, json_round_trip, values_beyond};
use yield_poc::number;
use yield_vm::heap::Property;
use yield_vm::{Completion, JsString, JsValue, Object, PromiseState, Value};

fn plain_tags(snapshot: &yield_vm::Snapshot) -> Vec<JsString> {
    snapshot
        .objects
        .iter()
        .filter_map(|record| {
            let Object::Plain(plain) = &record.object else {
                return None;
            };
            match plain.props.get(&JsString::from("tag")) {
                Some(Property::Data(Value::String(tag))) => Some(tag.clone()),
                _ => None,
            }
        })
        .collect()
}

fn has_call_environment(snapshot: &yield_vm::Snapshot) -> bool {
    snapshot.objects.iter().any(|record| {
        Some(record.id) != snapshot.script_env && matches!(record.object, Object::Environment(_))
    })
}

#[test]
fn promoted_dead_object_is_absent_from_the_snapshot_graph() {
    let source = r#"
import { wait } from "test";
export async function run() {
  const dead = {tag: "discard-only"};
  const live = {n: 7};
  await wait("a");
  return live.n;
}
"#;
    let mut s = Suspended::start(source, vec![]);
    let snapshot = s.snapshot();
    assert!(
        !plain_tags(&snapshot)
            .iter()
            .any(|tag| tag == &JsString::from("discard-only")),
        "dead must not remain reachable through a call environment or saved slot"
    );
    assert!(!has_call_environment(&snapshot));
    assert!(snapshot.continuations[0].env.is_none());
    let saved_objects: Vec<_> = values_beyond(
        &snapshot.continuations[0].values,
        &baseline_at_first_await().continuations[0].values,
    )
    .into_iter()
    .filter_map(|value| match value {
        Value::Object(id) => Some(id),
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
            && matches!(&record.object, Object::Plain(plain) if plain.props.get(&JsString::from("n")) == Some(&Property::Data(Value::Number(7.0))))
    }));

    s.restore(snapshot).expect("restore");
    s.complete("wait:a", Completion::Fulfilled(number(0.0)));
    assert_eq!(s.state(), PromiseState::Fulfilled(Value::Number(7.0)));
}

#[test]
fn captured_binding_keeps_object_identity_across_restore() {
    let source = r#"
import { wait } from "test";
export async function run() {
  const dead = {tag: "discard-only"};
  const live = {n: 7};
  const alias = () => live;
  await wait("a");
  return alias() === live;
}
"#;
    let mut s = Suspended::start(source, vec![]);
    let snapshot = s.snapshot();
    assert!(
        !plain_tags(&snapshot)
            .iter()
            .any(|tag| tag == &JsString::from("discard-only"))
    );
    assert!(has_call_environment(&snapshot));
    assert!(snapshot.continuations[0].env.is_some());

    s.restore(snapshot).expect("restore");
    s.complete("wait:a", Completion::Fulfilled(number(0.0)));
    assert_eq!(s.state(), PromiseState::Fulfilled(Value::Bool(true)));
}

#[test]
fn catch_reads_object_saved_across_the_reject_edge() {
    let source = r#"
import { wait } from "test";
export async function run() {
  const kept = {tag: "kept"};
  try {
    await wait("a");
  } catch (e) {
    return kept.tag;
  }
  return "missing";
}
"#;
    let mut s = Suspended::start(source, vec![]);
    let snapshot = s.snapshot();
    assert!(plain_tags(&snapshot).contains(&JsString::from("kept")));
    s.restore(snapshot).expect("restore");
    s.complete("wait:a", Completion::Rejected(JsValue::string("boom")));
    assert_eq!(
        s.state(),
        PromiseState::Fulfilled(Value::String("kept".into()))
    );
}

fn symbol_descriptions(snapshot: &yield_vm::Snapshot) -> Vec<Option<JsString>> {
    snapshot.symbols.values().cloned().collect()
}

#[test]
fn symbol_property_keys_survive_restore() {
    let source = r#"
import { wait } from "test";
export async function run() {
  const key = Symbol("key");
  const obj = {};
  obj[key] = 5;
  await wait("a");
  return obj[key];
}
"#;
    let mut s = Suspended::start(source, vec![]);
    let snapshot = json_round_trip(&s.snapshot());
    assert!(
        snapshot
            .symbols
            .values()
            .any(|description| description.as_ref() == Some(&JsString::from("Symbol.iterator")))
    );
    assert!(
        snapshot
            .symbols
            .values()
            .any(|description| description.as_ref() == Some(&JsString::from("key")))
    );
    s.restore(snapshot).expect("restore");
    s.complete("wait:a", Completion::Fulfilled(number(0.0)));
    assert_eq!(s.state(), PromiseState::Fulfilled(Value::Number(5.0)));
}

#[test]
fn unreferenced_symbols_are_absent_and_registry_keys_are_kept() {
    let source = r#"
import { wait } from "test";
export async function run() {
  Symbol("orphan");
  const dropped = Symbol.for("registry-only");
  await wait("a");
  return dropped === Symbol.for("registry-only");
}
"#;
    let mut s = Suspended::start(source, vec![]);
    let snapshot = s.snapshot();
    let descriptions = symbol_descriptions(&snapshot);
    assert!(
        !descriptions
            .iter()
            .any(|description| description.as_ref() == Some(&JsString::from("orphan"))),
        "a symbol with no reference must not reach the snapshot"
    );
    assert!(
        snapshot
            .symbols
            .values()
            .any(|description| description.as_ref() == Some(&JsString::from("registry-only"))),
        "registered keys stay alive without script references"
    );
    assert!(
        snapshot
            .symbol_registry
            .contains_key(&JsString::from("registry-only"))
    );
    s.restore(snapshot).expect("restore");
    s.complete("wait:a", Completion::Fulfilled(number(0.0)));
    assert_eq!(s.state(), PromiseState::Fulfilled(Value::Bool(true)));
}
