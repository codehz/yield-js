//! Host binding contract tests: static declarations, instance state, handles,
//! operation recovery strategies, and deterministic snapshots.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use yield_compiler::CompileOptions;
use yield_vm::binding::{CallCx, CodecError, DecodeCx, HostFault, OperationStart, OperationToken};
use yield_vm::{
    Bindings, Completion, GuestOutcome, Host, HostStep, Instance, JsError, JsResult, JsValue,
    PromiseState, This, Value, Vm, VmError, host_bindings, host_class, host_module, host_state,
    methods, operation,
};

#[path = "signature_fingerprint.rs"]
mod signature_fingerprint;

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

mod strategy_fixture {
    use super::*;

    macro_rules! strategy_operation {
        ($name:ident, $wire:tt, $recovery:ident, $message:tt) => {
            #[operation(name = $wire, schema = 1, recovery = $recovery, rejection = $message, encode = $name::encode, decode = $name::decode)]
            struct $name;

            impl $name {
                fn encode(&self, _out: &mut Vec<u8>) -> Result<(), CodecError> {
                    Ok(())
                }

                fn decode(_input: &[u8], _bindings: &Bindings) -> Result<Self, CodecError> {
                    Ok(Self)
                }
            }
        };
    }

    strategy_operation!(PauseOperation, "pause", pause, "pause rejected");
    strategy_operation!(ResumeOperation, "resume", resume, "resume rejected");
    strategy_operation!(ReissueOperation, "reissue", reissue, "reissue rejected");
    strategy_operation!(RejectOperation, "reject", reject, "strategy rejected");

    #[host_module(name = "ops")]
    mod ops {
        use super::*;

        #[function(name = "pause")]
        fn pause(cx: &mut CallCx<'_>) -> JsResult<JsValue> {
            cx.start_operation(PauseOperation)
        }

        #[function(name = "resume")]
        fn resume(cx: &mut CallCx<'_>) -> JsResult<JsValue> {
            cx.start_operation(ResumeOperation)
        }

        #[function(name = "reissue")]
        fn reissue(cx: &mut CallCx<'_>) -> JsResult<JsValue> {
            cx.start_operation(ReissueOperation)
        }

        #[function(name = "reject")]
        fn reject(cx: &mut CallCx<'_>) -> JsResult<JsValue> {
            cx.start_operation(RejectOperation)
        }
    }

    pub(super) fn bindings() -> Bindings {
        host_bindings! {
            modules: [ops],
            classes: [],
            states: [],
            operations: [PauseOperation, ResumeOperation, ReissueOperation, RejectOperation],
            services: [],
        }
        .expect("strategy bindings")
    }
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
    let bindings = strategy_fixture::bindings();
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
    assert!(
        host.resumed.is_empty(),
        "restore must not start external work before returning"
    );
    assert_eq!(vm.pending_operations().len(), 2);
    vm.start_recovered_operations(&mut host)
        .expect("start recovered operations");
    vm.start_recovered_operations(&mut host)
        .expect("starting recovered operations is idempotent");
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
    vm.start_recovered_operations(&mut host)
        .expect("start recovered operations");
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

mod stale_fixture {
    use super::*;

    #[host_class(tag = "Sink")]
    struct Sink {
        #[js_slot]
        value: JsValue,
    }

    type Stash = Arc<Mutex<Option<JsValue>>>;

    #[host_module(name = "stale")]
    mod stale {
        use super::*;

        #[function(name = "stash")]
        fn stash(cx: &mut CallCx<'_>, value: JsValue) -> JsResult<()> {
            let stash = cx
                .service::<Stash>("stash")
                .ok_or_else(|| JsError::Fault(cx.fault("missing stash service")))?;
            *stash.lock().expect("stash lock") = Some(value);
            Ok(())
        }

        #[function(name = "unstash")]
        fn unstash(cx: &mut CallCx<'_>) -> JsResult<Instance<Sink>> {
            let value = cx
                .service::<Stash>("stash")
                .and_then(|stash| stash.lock().expect("stash lock").clone())
                .ok_or_else(|| JsError::Fault(cx.fault("empty stash")))?;
            let sink = cx.alloc(Sink)?;
            sink.set_value(cx, value)?;
            Ok(sink)
        }
    }

    pub(super) fn bindings() -> Bindings {
        host_bindings! {
            modules: [stale],
            classes: [Sink],
            states: [],
            operations: [],
            services: [("stash", 1, Arc::new(Mutex::new(None::<JsValue>)))],
        }
        .expect("stale bindings")
    }
}

#[test]
fn handles_cannot_cross_calls() {
    let bindings = stale_fixture::bindings();
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

mod guest_call_fixture {
    use super::*;

    type Stash = Mutex<Option<HostStep>>;

    #[host_module(name = "guest")]
    mod guest {
        use super::*;

        #[function(name = "later")]
        fn later(cx: &mut CallCx<'_>, callee: JsValue) -> JsResult<HostStep> {
            cx.request_call(
                callee,
                JsValue::undefined(),
                vec![],
                vec![],
                unexpected_resume,
            )
        }

        #[function(name = "stash")]
        fn stash(cx: &mut CallCx<'_>, callee: JsValue) -> JsResult<()> {
            let request = later(cx, callee)?;
            *cx.service::<Stash>("stash").unwrap().lock().unwrap() = Some(request);
            Ok(())
        }

        #[function(name = "replay")]
        fn replay(cx: &mut CallCx<'_>) -> JsResult<HostStep> {
            Ok(cx
                .service::<Stash>("stash")
                .unwrap()
                .lock()
                .unwrap()
                .take()
                .unwrap())
        }

        #[function(name = "fault")]
        fn fault(cx: &mut CallCx<'_>) -> JsResult<()> {
            Err(JsError::Fault(cx.fault("guest host fault")))
        }

        #[function(name = "mark")]
        fn mark(cx: &mut CallCx<'_>) -> JsResult<()> {
            cx.service::<Arc<AtomicUsize>>("runs")
                .unwrap()
                .fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        #[function(name = "twice")]
        fn twice(cx: &mut CallCx<'_>, callee: JsValue, seed: JsValue) -> JsResult<HostStep> {
            cx.request_call(
                callee.clone(),
                cx.this_value(),
                vec![seed],
                vec![callee],
                again,
            )
        }
    }

    fn unexpected_resume(_: &mut CallCx<'_>, _: GuestOutcome) -> JsResult<HostStep> {
        panic!("faulting or stale requests must not resume")
    }

    fn again(cx: &mut CallCx<'_>, outcome: GuestOutcome) -> JsResult<HostStep> {
        let GuestOutcome::Returned(value) = outcome else {
            panic!("callback should return")
        };
        assert_eq!(cx.args().len(), 1);
        assert!(!cx.is_construct());
        let callee = cx.arg(0)?;
        cx.request_call(callee, cx.this_value(), vec![value], vec![], finish)
    }

    fn finish(_: &mut CallCx<'_>, outcome: GuestOutcome) -> JsResult<HostStep> {
        let GuestOutcome::Returned(value) = outcome else {
            panic!("callback should return")
        };
        Ok(HostStep::Return(value))
    }

    pub(super) fn run(source: &str) -> (Result<Value, VmError>, usize) {
        let runs = Arc::new(AtomicUsize::new(0));
        let bindings = host_bindings! {
            modules: [guest],
            classes: [],
            states: [],
            operations: [],
            services: [("stash", 1, Stash::default()), ("runs", 1, runs.clone())],
        }
        .expect("guest bindings");
        let build = yield_compiler::compile(source, &options(&bindings)).expect("compile");
        let mut host = StrategyHost::new(bindings);
        let mut vm = Vm::new(build, &mut host).expect("create VM");
        let result = vm.start(&mut host, "run", vec![]);
        (result, runs.load(Ordering::Relaxed))
    }
}

#[test]
fn guest_call_cannot_be_replayed() {
    let (result, runs) = guest_call_fixture::run(
        r#"
        import { stash, replay, mark } from 'guest';
        export function run() {
            stash(() => mark());
            return replay();
        }
    "#,
    );
    assert!(
        matches!(result, Err(VmError::HostFault(message)) if message.contains("GuestCall used outside the call"))
    );
    assert_eq!(runs, 0);
}

#[test]
fn guest_call_host_fault_bypasses_resume_and_script_catch() {
    let (result, _) = guest_call_fixture::run(
        r#"
        import { later, fault } from 'guest';
        export function run() {
            try { return later(() => fault()); }
            catch (e) { return 'caught'; }
        }
    "#,
    );
    assert!(matches!(result, Err(VmError::HostFault(message)) if message == "guest host fault"));
}

#[test]
fn guest_call_resume_can_request_another_call() {
    let (result, _) = guest_call_fixture::run(
        r#"
        import { twice } from 'guest';
        export function run() {
            const receiver = { delta: 3, twice };
            return receiver.twice(function(value) { return value + this.delta; }, 2);
        }
    "#,
    );
    assert_eq!(result.expect("chained guest calls"), Value::Number(8.0));
}

mod counter_fixture {
    use super::*;
    use yield_poc::Wait;

    #[host_state(module = "counter", schema = 1, init = CounterState::new, encode = CounterState::encode_payload, decode = CounterState::decode_payload)]
    struct CounterState {
        count: u32,
    }

    impl CounterState {
        fn new() -> Self {
            Self { count: 0 }
        }

        fn encode_payload(&self, out: &mut Vec<u8>) -> Result<(), CodecError> {
            out.extend_from_slice(&self.count.to_le_bytes());
            Ok(())
        }

        fn decode_payload(input: &[u8], _cx: &mut DecodeCx<'_>) -> Result<Self, CodecError> {
            let bytes: [u8; 4] = input
                .try_into()
                .map_err(|_| CodecError::new("counter state must be four bytes"))?;
            Ok(Self {
                count: u32::from_le_bytes(bytes),
            })
        }
    }

    #[host_module(name = "counter")]
    mod counter {
        use super::*;

        #[function(name = "bump")]
        fn bump(cx: &mut CallCx<'_>) -> JsResult<()> {
            cx.state::<CounterState>()?.count += 1;
            Ok(())
        }

        #[function(name = "value")]
        fn value(cx: &mut CallCx<'_>) -> JsResult<JsValue> {
            Ok(JsValue::number(f64::from(
                cx.state::<CounterState>()?.count,
            )))
        }
    }

    #[host_module(name = "test")]
    mod test {
        use super::*;

        #[function(name = "wait")]
        fn wait(cx: &mut CallCx<'_>, arg: JsValue) -> JsResult<JsValue> {
            let label = cx
                .string(&arg)?
                .to_utf8()
                .map_err(|_| cx.type_error("wait requires a valid label"))?;
            cx.start_operation(Wait { label })
        }
    }

    pub(super) fn bindings() -> Bindings {
        host_bindings! {
            modules: [counter, test],
            classes: [],
            states: [CounterState],
            operations: [Wait],
            services: [],
        }
        .expect("counter bindings")
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
    let bindings = counter_fixture::bindings();
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
    vm.start_recovered_operations(&mut host)
        .expect("start recovered operations");
    let token = host.tokens["wait"];
    vm.complete_operation(token, Completion::Fulfilled(JsValue::undefined()))
        .expect("complete wait");
    vm.run_until_idle(&mut host).expect("resume");
    assert_eq!(
        vm.promise_state(&result).unwrap(),
        PromiseState::Fulfilled(Value::Number(3.0))
    );
}

mod geom_fixture {
    use super::*;

    #[host_class(tag = "Point", methods, schema = 1, encode = Point::encode_payload, decode = Point::decode_payload)]
    struct Point {
        x: f64,
    }

    #[methods(Point)]
    impl Point {
        #[method(name = "x")]
        fn x(cx: &mut CallCx<'_>, this: This<Point>) -> JsResult<JsValue> {
            Ok(JsValue::number(this.instance().payload(cx)?.x))
        }

        fn encode_payload(&self, out: &mut Vec<u8>) -> Result<(), CodecError> {
            out.extend_from_slice(&self.x.to_le_bytes());
            Ok(())
        }

        fn decode_payload(input: &[u8], _cx: &mut DecodeCx<'_>) -> Result<Self, CodecError> {
            let bytes: [u8; 8] = input
                .try_into()
                .map_err(|_| CodecError::new("point payload must be eight bytes"))?;
            Ok(Self {
                x: f64::from_le_bytes(bytes),
            })
        }
    }

    #[host_module(name = "geom")]
    mod geom {
        use super::*;

        #[constructor(name = "Point", class = Point)]
        fn point(cx: &mut CallCx<'_>, value: JsValue) -> JsResult<Instance<Point>> {
            let x = cx.number(&value)?;
            cx.alloc(Point { x })
        }
    }

    pub(super) fn bindings() -> Bindings {
        host_bindings! {
            modules: [geom],
            classes: [Point],
            states: [],
            operations: [],
            services: [],
        }
        .expect("geometry bindings")
    }
}

#[test]
fn declared_constructor_exports_create_class_instances() {
    let bindings = geom_fixture::bindings();
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

mod loose_fixture {
    use super::*;

    #[host_class(tag = "Loose")]
    struct Loose {
        _n: u32,
    }

    #[host_module(name = "loose")]
    mod loose {
        use super::*;

        #[function(name = "make")]
        fn make(cx: &mut CallCx<'_>) -> JsResult<Instance<Loose>> {
            cx.alloc(Loose { _n: 1 })
        }
    }

    pub(super) fn bindings() -> Bindings {
        host_bindings! {
            modules: [loose],
            classes: [Loose],
            states: [],
            operations: [],
            services: [],
        }
        .expect("loose bindings")
    }
}

#[test]
fn class_data_requires_a_declared_payload_schema() {
    let bindings = loose_fixture::bindings();
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

mod inherit_fixture {
    use super::*;

    #[operation(name = "inherit-wait", schema = 1, recovery = resume, encode = InheritWait::encode, decode = InheritWait::decode)]
    struct InheritWait;

    impl InheritWait {
        fn encode(&self, _out: &mut Vec<u8>) -> Result<(), CodecError> {
            Ok(())
        }

        fn decode(_input: &[u8], _bindings: &Bindings) -> Result<Self, CodecError> {
            Ok(Self)
        }
    }

    #[host_class(tag = "Base", methods)]
    struct Base {
        #[js_slot]
        base: JsValue,
    }

    #[methods(Base)]
    impl Base {
        #[method(name = "read")]
        fn read(cx: &mut CallCx<'_>, this: This<Base>) -> JsResult<JsValue> {
            this.base(cx)
        }
    }

    #[host_class(tag = "Derived", parent = Base, methods)]
    struct Derived {
        #[js_slot]
        value: JsValue,
    }

    #[methods(Derived)]
    impl Derived {
        #[method(name = "value")]
        fn value(cx: &mut CallCx<'_>, this: This<Derived>) -> JsResult<JsValue> {
            DerivedSlots::value(&this, cx)
        }
    }

    #[host_module(name = "inherit")]
    mod inherit {
        use super::*;

        #[function(name = "wait")]
        fn wait(cx: &mut CallCx<'_>) -> JsResult<JsValue> {
            cx.start_operation(InheritWait)
        }

        #[function(name = "assert")]
        fn assert(cx: &mut CallCx<'_>, value: JsValue, _message: Option<JsValue>) -> JsResult<()> {
            if value.truthy() {
                return Ok(());
            }
            Err(cx.throw_string("assert failed".to_owned()))
        }

        #[constructor(name = "Base", class = Base)]
        fn base(cx: &mut CallCx<'_>) -> JsResult<Instance<Base>> {
            let base = cx.alloc(Base)?;
            base.set_base(cx, JsValue::number(1.0))?;
            Ok(base)
        }

        #[constructor(name = "Derived", class = Derived)]
        fn derived(cx: &mut CallCx<'_>) -> JsResult<Instance<Derived>> {
            let derived = cx.alloc(Derived)?;
            derived.set_value(cx, JsValue::number(2.0))?;
            Ok(derived)
        }
    }

    pub(super) fn bindings() -> Bindings {
        host_bindings! {
            modules: [inherit],
            classes: [Base, Derived],
            states: [],
            operations: [InheritWait],
            services: [],
        }
        .expect("derived bindings")
    }
}

#[test]
fn derived_host_classes_inherit_prototypes_and_slot_layout() {
    let bindings = inherit_fixture::bindings();
    let source = r#"
import { assert, Base, Derived, wait } from "inherit";
export async function run() {
  const derived = new Derived();
  const base = new Base();
  await wait();
  assert(derived.read() === undefined);
  assert(derived.value() === 2);
  assert(base.read() === 1);
  assert(base instanceof Base);
  assert(derived instanceof Base);
  assert(derived instanceof Derived);
  assert(!(base instanceof Derived));
  assert(derived.read === base.read);
  let naked = "missing";
  try {
    const read = derived.read;
    read();
  } catch (error) {
    naked = error instanceof TypeError ? "type" : "other";
  }
  assert(naked === "type");
  return true;
}
"#;
    let build = yield_compiler::compile(source, &options(&bindings)).expect("compile");
    let mut host = StrategyHost::new(bindings);
    let mut vm = Vm::new(build.clone(), &mut host).expect("create VM");
    let result = vm.start(&mut host, "run", vec![]).expect("start");
    vm.run_until_idle(&mut host).expect("suspend");
    let snapshot = vm
        .snapshot(std::slice::from_ref(&result))
        .expect("snapshot derived instance");
    drop(vm);

    let mut vm = Vm::restore(snapshot, build, &mut host).expect("restore");
    vm.start_recovered_operations(&mut host)
        .expect("start recovered operation");
    let token = host.tokens["inherit-wait"];
    vm.complete_operation(token, Completion::Fulfilled(JsValue::undefined()))
        .expect("complete wait");
    vm.run_until_idle(&mut host).expect("drain");
    assert_eq!(
        vm.promise_state(&result).unwrap(),
        PromiseState::Fulfilled(Value::Bool(true))
    );
}

#[test]
fn missing_wait_label_counts_call_without_starting_operation() {
    let source = r#"
import { wait } from "test";
export function run() {
  try {
    wait();
  } catch (error) {
    return error instanceof TypeError;
  }
  return false;
}
"#;
    let build = yield_poc::build(source);
    let mut host = yield_poc::TestHost::default();
    let mut vm = Vm::new(build, &mut host).expect("create VM");
    assert_eq!(
        vm.start(&mut host, "run", vec![]).expect("start"),
        Value::Bool(true)
    );
    assert_eq!(host.count("wait"), 1);
    assert!(host.pending_labels().is_empty());
}
