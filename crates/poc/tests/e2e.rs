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
