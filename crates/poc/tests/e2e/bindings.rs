//! Host binding contract tests: static declarations, instance state, handles,
//! operation recovery strategies, and deterministic snapshots.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use yield_compiler::CompileOptions;
use yield_vm::binding::{
    Bindings, CodecError, HostClass, HostFault, JsError, JsValue, ModuleDecl, Operation,
    OperationStart, OperationToken, Recovery, SlotDecl, SlotKind,
};
use yield_vm::{Completion, Host, PromiseState, Value, Vm, VmError};

fn options(bindings: &Bindings) -> CompileOptions {
    let mut imports = BTreeMap::new();
    for module in bindings.module_names() {
        imports.insert(
            module.to_owned(),
            bindings
                .export_names(module)
                .map(str::to_owned)
                .collect::<BTreeSet<_>>(),
        );
    }
    CompileOptions { imports }
}

struct StrategyHost {
    bindings: Arc<Bindings>,
    tokens: BTreeMap<String, OperationToken>,
    resumed: Vec<String>,
    errors: usize,
}

impl StrategyHost {
    fn new(bindings: Bindings) -> Self {
        Self {
            bindings: Arc::new(bindings),
            tokens: BTreeMap::new(),
            resumed: Vec::new(),
            errors: 0,
        }
    }
}

impl Host for StrategyHost {
    fn bindings(&self) -> Arc<Bindings> {
        self.bindings.clone()
    }

    fn start_operation(&mut self, start: OperationStart<'_>) -> Result<(), HostFault> {
        self.tokens.insert(start.name.to_owned(), start.token);
        Ok(())
    }

    fn resume_operation(&mut self, start: OperationStart<'_>) -> Result<(), HostFault> {
        self.resumed.push(start.name.to_owned());
        self.start_operation(start)
    }

    fn report_error(&mut self, _error: JsValue) {
        self.errors += 1;
    }
}

macro_rules! strategy_operation {
    ($name:ident, $wire:literal, $recovery:expr, $message:literal) => {
        struct $name;

        impl Operation for $name {
            const NAME: &'static str = $wire;
            const SCHEMA: u32 = 1;
            const RECOVERY: Recovery = $recovery;
            const REJECTION: &'static str = $message;

            fn encode(&self, _out: &mut Vec<u8>) -> Result<(), CodecError> {
                Ok(())
            }

            fn decode(_input: &[u8], _bindings: &Bindings) -> Result<Self, CodecError> {
                Ok(Self)
            }
        }
    };
}

strategy_operation!(PauseOperation, "pause", Recovery::Pause, "pause rejected");
strategy_operation!(
    ResumeOperation,
    "resume",
    Recovery::Resume,
    "resume rejected"
);
strategy_operation!(
    ReissueOperation,
    "reissue",
    Recovery::Reissue,
    "reissue rejected"
);
strategy_operation!(
    RejectOperation,
    "reject",
    Recovery::Reject,
    "strategy rejected"
);

fn strategy_bindings() -> Bindings {
    Bindings::builder()
        .operation::<PauseOperation>()
        .operation::<ResumeOperation>()
        .operation::<ReissueOperation>()
        .operation::<RejectOperation>()
        .module(
            ModuleDecl::new("ops")
                .function("pause", 0, |cx| cx.start_operation(PauseOperation))
                .function("resume", 0, |cx| cx.start_operation(ResumeOperation))
                .function("reissue", 0, |cx| cx.start_operation(ReissueOperation))
                .function("reject", 0, |cx| cx.start_operation(RejectOperation)),
        )
        .build()
        .expect("strategy bindings")
}

#[test]
fn operation_strategies_settle_or_reregister_after_restore() {
    let source = r#"
import { pause, resume, reissue, reject } from "ops";
export async function run() {
  const p1 = pause();
  const p2 = resume();
  const p3 = reissue();
  const p4 = reject();
  let seen = "missing";
  try {
    await p4;
  } catch (error) {
    seen = error;
  }
  await p1;
  await p2;
  await p3;
  return seen;
}
"#;
    let bindings = strategy_bindings();
    let build = yield_compiler::compile(source, &options(&bindings)).expect("compile");
    let mut host = StrategyHost::new(bindings);
    let mut vm = Vm::new(build.clone(), &mut host).expect("create VM");
    let result = vm.start(&mut host, "run", vec![]).expect("start");
    vm.run_until_idle(&mut host).expect("suspend");
    assert_eq!(host.tokens.len(), 4);
    let snapshot = vm
        .snapshot(std::slice::from_ref(&result))
        .expect("snapshot");
    drop(vm);

    let mut vm = Vm::restore(snapshot, build, &mut host).expect("restore");
    vm.run_until_idle(&mut host)
        .expect("settle restored operations");
    assert_eq!(
        host.resumed,
        vec!["resume".to_owned(), "reissue".to_owned()]
    );
    assert_eq!(vm.promise_state(&result).unwrap(), PromiseState::Pending);

    for (name, value) in [("resume", 1.0), ("reissue", 2.0)] {
        let token = host.tokens[name];
        vm.complete_operation(token, Completion::Fulfilled(JsValue::number(value)))
            .expect("complete restored operation");
        vm.run_until_idle(&mut host).expect("drain completion");
    }
    assert_eq!(
        vm.promise_state(&result).unwrap(),
        PromiseState::Fulfilled(Value::String("strategy rejected".into()))
    );
    assert_eq!(host.errors, 0);
}

#[test]
fn stale_operation_tokens_are_rejected_after_restore() {
    let source = r#"
import { wait } from "test";
export async function run() {
  await wait("a");
  return 1;
}
"#;
    let build = yield_poc::build(source);
    let mut host = yield_poc::TestHost::default();
    let mut vm = Vm::new(build.clone(), &mut host).expect("create VM");
    let result = vm.start(&mut host, "run", vec![]).expect("start");
    vm.run_until_idle(&mut host).expect("suspend");
    let stale = host.token("wait:a").expect("old token");
    assert_eq!(host.pending_labels(), vec!["wait:a".to_owned()]);
    let snapshot = vm
        .snapshot(std::slice::from_ref(&result))
        .expect("snapshot");
    vm = Vm::restore(snapshot, build, &mut host).expect("restore");
    assert!(matches!(
        vm.complete_operation(stale, Completion::Fulfilled(JsValue::undefined())),
        Err(VmError::UnknownOperation(_))
    ));
    let fresh = host.token("wait:a").expect("new token");
    vm.complete_operation(fresh, Completion::Fulfilled(JsValue::undefined()))
        .expect("fresh token settles");
    vm.run_until_idle(&mut host).expect("resume");
    assert_eq!(
        vm.promise_state(&result).unwrap(),
        PromiseState::Fulfilled(Value::Number(1.0))
    );
}

#[test]
fn repeated_snapshots_of_the_same_graph_are_identical() {
    let source = r#"
import { expect, wait } from "test";
const matcher = expect({ nested: [1, 2, 3] });
const cycle = {};
cycle.self = cycle;
export async function run() {
  await wait("a");
  return cycle;
}
"#;
    let build = yield_poc::build(source);
    let mut host = yield_poc::TestHost::default();
    let mut vm = Vm::new(build, &mut host).expect("create VM");
    let result = vm.start(&mut host, "run", vec![]).expect("start");
    vm.run_until_idle(&mut host).expect("suspend");
    let first = vm
        .snapshot(std::slice::from_ref(&result))
        .expect("first snapshot")
        .to_json()
        .expect("encode");
    let second = vm
        .snapshot(std::slice::from_ref(&result))
        .expect("second snapshot")
        .to_json()
        .expect("encode");
    assert_eq!(first, second);
}

struct Sink;

impl HostClass for Sink {
    const TAG: &'static str = "Sink";
    const SLOTS: &'static [SlotDecl] = &[SlotDecl {
        name: "value",
        kind: SlotKind::Js,
    }];
}

type Stash = Arc<Mutex<Option<JsValue>>>;

fn stale_bindings() -> Bindings {
    Bindings::builder()
        .service("stash", 1, Arc::new(Mutex::new(None::<JsValue>)))
        .class::<Sink>()
        .module(
            ModuleDecl::new("stale")
                .function("stash", 1, |cx| {
                    let value = cx.arg(0)?;
                    let stash = cx
                        .service::<Stash>("stash")
                        .ok_or_else(|| JsError::Fault(cx.fault("missing stash service")))?;
                    *stash.lock().expect("stash lock") = Some(value);
                    Ok(JsValue::undefined())
                })
                .function("unstash", 0, |cx| {
                    let value = cx
                        .service::<Stash>("stash")
                        .and_then(|stash| stash.lock().expect("stash lock").clone())
                        .ok_or_else(|| JsError::Fault(cx.fault("empty stash")))?;
                    let sink = cx.alloc(Sink)?;
                    sink.set_slot(cx, 0, value)?;
                    Ok(sink.value(cx))
                }),
        )
        .build()
        .expect("stale bindings")
}

#[test]
fn handles_cannot_cross_calls() {
    let bindings = stale_bindings();
    let source = r#"
import { stash, unstash } from "stale";
export function run() {
  stash({});
  return unstash();
}
"#;
    let build = yield_compiler::compile(source, &options(&bindings)).expect("compile");
    let mut host = StrategyHost::new(bindings);
    let mut vm = Vm::new(build, &mut host).expect("create VM");
    let error = vm
        .start(&mut host, "run", vec![])
        .expect_err("stale handle is a host fault");
    assert!(
        matches!(&error, VmError::HostFault(message) if message.contains("outside the call")),
        "{error:?}"
    );
}

struct CounterState {
    count: u32,
}

impl yield_vm::binding::HostModuleState for CounterState {
    const MODULE: &'static str = "counter";
    const PAYLOAD_SCHEMA: u32 = 1;

    fn new() -> Self {
        Self { count: 0 }
    }

    fn encode_payload(&self, out: &mut Vec<u8>) -> Result<(), CodecError> {
        out.extend_from_slice(&self.count.to_le_bytes());
        Ok(())
    }

    fn decode_payload(
        input: &[u8],
        _cx: &mut yield_vm::binding::DecodeCx<'_>,
    ) -> Result<Self, CodecError> {
        let bytes: [u8; 4] = input
            .try_into()
            .map_err(|_| CodecError::new("counter state must be four bytes"))?;
        Ok(Self {
            count: u32::from_le_bytes(bytes),
        })
    }
}

#[test]
fn module_state_survives_restore() {
    let source = r#"
import { bump, value } from "counter";
import { wait } from "test";
export async function run() {
  bump();
  bump();
  await wait("a");
  bump();
  return value();
}
"#;
    let bindings = Bindings::builder()
        .state::<CounterState>()
        .operation::<yield_poc::Wait>()
        .module(
            ModuleDecl::new("counter")
                .function("bump", 0, |cx| {
                    cx.state::<CounterState>()?.count += 1;
                    Ok(JsValue::undefined())
                })
                .function("value", 0, |cx| {
                    Ok(JsValue::number(f64::from(
                        cx.state::<CounterState>()?.count,
                    )))
                }),
        )
        .module(ModuleDecl::new("test").function("wait", 1, |cx| {
            let arg = cx.arg(0)?;
            let label = cx
                .string(&arg)?
                .to_utf8()
                .map_err(|_| cx.type_error("wait requires a valid label"))?;
            cx.start_operation(yield_poc::Wait { label })
        }))
        .build()
        .expect("counter bindings");
    let build = yield_compiler::compile(source, &options(&bindings)).expect("compile");
    let mut host = StrategyHost::new(bindings);
    let mut vm = Vm::new(build.clone(), &mut host).expect("create VM");
    let result = vm.start(&mut host, "run", vec![]).expect("start");
    vm.run_until_idle(&mut host).expect("suspend");
    let snapshot = vm
        .snapshot(std::slice::from_ref(&result))
        .expect("snapshot");
    assert_eq!(snapshot.module_states.len(), 1);
    assert_eq!(snapshot.module_states[0].module, "counter");
    drop(vm);

    let mut vm = Vm::restore(snapshot, build, &mut host).expect("restore");
    let token = host.tokens["wait"];
    vm.complete_operation(token, Completion::Fulfilled(JsValue::undefined()))
        .expect("complete wait");
    vm.run_until_idle(&mut host).expect("resume");
    assert_eq!(
        vm.promise_state(&result).unwrap(),
        PromiseState::Fulfilled(Value::Number(3.0))
    );
}

struct Point {
    x: f64,
}

impl HostClass for Point {
    const TAG: &'static str = "Point";
    const PAYLOAD_SCHEMA: Option<u32> = Some(1);
    const METHODS: &'static [yield_vm::binding::MethodDecl] = &[yield_vm::binding::MethodDecl {
        name: "x",
        arity: 0,
        call: |cx| {
            let this = cx.this::<Point>()?;
            Ok(JsValue::number(this.instance().payload(cx)?.x))
        },
    }];

    fn encode_payload(&self, out: &mut Vec<u8>) -> Result<(), CodecError> {
        out.extend_from_slice(&self.x.to_le_bytes());
        Ok(())
    }

    fn decode_payload(
        input: &[u8],
        _cx: &mut yield_vm::binding::DecodeCx<'_>,
    ) -> Result<Self, CodecError> {
        let bytes: [u8; 8] = input
            .try_into()
            .map_err(|_| CodecError::new("point payload must be eight bytes"))?;
        Ok(Self {
            x: f64::from_le_bytes(bytes),
        })
    }
}

#[test]
fn declared_constructor_exports_create_class_instances() {
    let bindings = Bindings::builder()
        .class::<Point>()
        .module(
            ModuleDecl::new("geom").constructor_for::<Point>("Point", 1, |cx| {
                let value = cx.arg(0)?;
                let x = cx.number(&value)?;
                let point = cx.alloc(Point { x })?;
                Ok(point.value(cx))
            }),
        )
        .build()
        .expect("geometry bindings");
    let source = r#"
import { Point } from "geom";
export function run() {
  const point = new Point(3);
  if (!(point instanceof Point)) return -1;
  if (Object.getPrototypeOf(point) !== Point.prototype) return -2;
  return point.x();
}
"#;
    let build = yield_compiler::compile(source, &options(&bindings)).expect("compile");
    let mut host = StrategyHost::new(bindings);
    let mut vm = Vm::new(build, &mut host).expect("create VM");
    assert_eq!(
        vm.start(&mut host, "run", vec![]).expect("start"),
        Value::Number(3.0)
    );
}

#[test]
fn class_data_requires_a_declared_payload_schema() {
    struct Loose {
        _n: u32,
    }

    impl HostClass for Loose {
        const TAG: &'static str = "Loose";
    }

    let bindings = Bindings::builder()
        .class::<Loose>()
        .module(ModuleDecl::new("loose").function("make", 0, |cx| {
            let loose = cx.alloc(Loose { _n: 1 })?;
            Ok(loose.value(cx))
        }))
        .build()
        .expect("loose bindings");
    let source = r#"
import { make } from "loose";
export function run() {
  return make();
}
"#;
    let build = yield_compiler::compile(source, &options(&bindings)).expect("compile");
    let mut host = StrategyHost::new(bindings);
    let mut vm = Vm::new(build, &mut host).expect("create VM");
    let error = vm
        .start(&mut host, "run", vec![])
        .expect_err("payload without a schema is a host fault");
    assert!(
        matches!(&error, VmError::HostFault(message) if message.contains("no payload schema")),
        "{error:?}"
    );
}

#[test]
fn corrupt_instance_payload_rejects_restore() {
    let source = r#"
import { bind, wait } from "test";
export async function run() {
  const node = bind("payload");
  await wait("a");
  return node;
}
"#;
    let build = yield_poc::build(source);
    let mut host = yield_poc::TestHost::default();
    let mut vm = Vm::new(build.clone(), &mut host).expect("create VM");
    let result = vm.start(&mut host, "run", vec![]).expect("start");
    vm.run_until_idle(&mut host).expect("suspend");
    let mut snapshot = vm
        .snapshot(std::slice::from_ref(&result))
        .expect("snapshot");
    let payload = snapshot
        .host_instances
        .iter_mut()
        .find(|record| record.class == "Node")
        .and_then(|record| record.payload.as_mut())
        .expect("node payload");
    payload.bytes = vec![0xff, 0xfe];
    let error = Vm::restore(snapshot, build, &mut host).expect_err("invalid payload");
    assert!(
        matches!(&error, VmError::InvalidSnapshot(message) if message.contains("payload")),
        "{error:?}"
    );
}
