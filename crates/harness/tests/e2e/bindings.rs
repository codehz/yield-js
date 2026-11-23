//! Host binding contract tests: static declarations, instance state, handles,
//! operation recovery strategies, and deterministic snapshots.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::support::{compile_main, declared};
use yield_harness::{LabelRegistration, LabelService, LabelServiceKey};
use yield_vm::binding::{CallCx, CodecError, DecodeCx, HostFault, OperationToken};
use yield_vm::{
    Bindings, Completion, GuestOutcome, Host, HostStep, Instance, JsError, JsResult, JsValue,
    OperationDriver, PromiseState, RestoreOptions, SnapshotValidation, This, Value, Vm, VmError,
    VmOptions, host_bindings, host_class, host_module, methods, module_exports, operation,
};

#[path = "signature_fingerprint.rs"]
mod signature_fingerprint;

struct StrategyHost {
    bindings: Arc<Bindings>,
    errors: usize,
}

impl StrategyHost {
    fn new(bindings: Bindings) -> Self {
        Self {
            bindings: Arc::new(bindings),
            errors: 0,
        }
    }
}

impl Host for StrategyHost {
    fn bindings(&self) -> Arc<Bindings> {
        self.bindings.clone()
    }

    fn report_error(&mut self, _error: JsValue) {
        self.errors += 1;
    }
}

fn register_label(
    bindings: &Bindings,
    label: &str,
    token: OperationToken,
    restoring: bool,
) -> Result<LabelRegistration, HostFault> {
    bindings
        .service::<LabelServiceKey>()
        .ok_or_else(|| HostFault::new("missing labels service"))?
        .register(label.to_owned(), token, restoring)
}

macro_rules! label_driver {
    ($operation:ty, $wire:tt) => {
        impl OperationDriver for $operation {
            type Live = LabelRegistration;

            fn start(
                &self,
                token: OperationToken,
                bindings: &Bindings,
            ) -> Result<Self::Live, HostFault> {
                register_label(bindings, $wire, token, false)
            }

            fn resume(
                &self,
                token: OperationToken,
                bindings: &Bindings,
            ) -> Result<Self::Live, HostFault> {
                register_label(bindings, $wire, token, true)
            }
        }
    };
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

            label_driver!($name, $wire);
        };
    }

    strategy_operation!(PauseOperation, "pause", pause, "pause rejected");
    strategy_operation!(ResumeOperation, "resume", resume, "resume rejected");
    strategy_operation!(ReissueOperation, "reissue", reissue, "reissue rejected");
    strategy_operation!(RejectOperation, "reject", reject, "strategy rejected");

    #[host_module(name = "ops")]
    struct Ops;

    #[module_exports]
    impl Ops {
        #[function(name = "pause")]
        fn pause(cx: &mut yield_vm::ModuleCx<'_, Self>) -> JsResult<JsValue> {
            cx.promise(PauseOperation)
        }

        #[function(name = "resume")]
        fn resume(cx: &mut yield_vm::ModuleCx<'_, Self>) -> JsResult<JsValue> {
            cx.promise(ResumeOperation)
        }

        #[function(name = "withSignal")]
        fn with_signal(
            cx: &mut yield_vm::ModuleCx<'_, Self>,
            signal: JsValue,
        ) -> JsResult<JsValue> {
            cx.promise_with_signal(ResumeOperation, &signal)
        }

        #[function(name = "abortReason")]
        fn abort_reason(
            cx: &mut yield_vm::ModuleCx<'_, Self>,
            signal: JsValue,
        ) -> JsResult<JsValue> {
            Ok(cx.abort_reason(&signal)?.unwrap_or_else(JsValue::undefined))
        }

        #[function(name = "reissue")]
        fn reissue(cx: &mut yield_vm::ModuleCx<'_, Self>) -> JsResult<JsValue> {
            cx.promise(ReissueOperation)
        }

        #[function(name = "reject")]
        fn reject(cx: &mut yield_vm::ModuleCx<'_, Self>) -> JsResult<JsValue> {
            cx.promise(RejectOperation)
        }
    }

    pub(super) fn bindings(labels: &Arc<LabelService>) -> Bindings {
        host_bindings! {
            dts: "../dts/ops.d.ts",
            modules: [Ops],
            classes: [],
            operations: [PauseOperation, ResumeOperation, ReissueOperation, RejectOperation],
            services: {
                LabelServiceKey => Arc::clone(labels),
            },
        }
        .expect("strategy bindings")
    }
}

#[test]
fn restored_signal_cancels_original_operation_and_rejects_stale_delivery() {
    let source = r#"
        import { withSignal, abortReason } from "ops";
        const source = new AbortController();
        const dependent = AbortSignal.any([source.signal]);
        const reason = {};
        const target = new EventTarget();
        let valid = true;
        target.addEventListener("x", () => { valid = false; }, {signal: dependent});
        source.signal.addEventListener("abort", event => {
            valid = dependent.aborted && abortReason(dependent) === reason;
            event.stopImmediatePropagation();
        });
        export async function run() {
            try { await withSignal(dependent); }
            catch (error) { return error === reason && valid; }
            return false;
        }
        export function cancel() {
            source.abort(reason);
            target.dispatchEvent(new Event("x"));
        }
        export async function already() {
            try { await withSignal(AbortSignal.abort(reason)); }
            catch (error) { return error === reason; }
            return false;
        }
        export function badSignal() {
            try { abortReason({}); }
            catch (error) { return error instanceof TypeError; }
            return false;
        }
    "#;
    let build = compile_main(source, &declared(include_str!("../dts/ops.d.ts"))).unwrap();
    let labels = Arc::new(LabelService::default());
    let mut host = StrategyHost::new(strategy_fixture::bindings(&labels));
    let mut vm = Vm::new(build.clone(), &mut host, Default::default()).unwrap();
    let result = vm.start(&mut host, "run", vec![]).unwrap();
    let old = labels.token("resume").unwrap();
    let image = vm.snapshot(std::slice::from_ref(&result)).unwrap();
    let restored_labels = Arc::new(LabelService::default());
    let mut restored_host = StrategyHost::new(strategy_fixture::bindings(&restored_labels));
    let mut no_backlink = image.clone();
    no_backlink.operations[0].signal = None;
    assert!(matches!(
        Vm::restore(
            no_backlink,
            build.clone(),
            &mut restored_host,
            RestoreOptions {
                validation: SnapshotValidation::Full,
                ..Default::default()
            },
        ),
        Err(VmError::InvalidSnapshot(_))
    ));
    let mut wrong_signal = image.clone();
    let global = wrong_signal.roots["globalThis"];
    let controller = wrong_signal
        .objects
        .iter_mut()
        .find_map(|record| match &mut record.object {
            yield_vm::Object::AbortController { signal, .. } => Some(signal),
            _ => None,
        })
        .expect("controller record");
    *controller = global;
    assert!(matches!(
        Vm::restore(
            wrong_signal,
            build.clone(),
            &mut restored_host,
            RestoreOptions {
                validation: SnapshotValidation::Full,
                ..Default::default()
            },
        ),
        Err(VmError::InvalidSnapshot(_))
    ));
    let mut restored = Vm::restore(
        image,
        build,
        &mut restored_host,
        RestoreOptions {
            validation: SnapshotValidation::Full,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(restored_labels.pending_labels().is_empty());
    restored.start_recovered_operations().unwrap();
    assert_eq!(restored_labels.resume_log(), ["resume"]);
    let current = restored_labels.token("resume").unwrap();
    restored
        .start(&mut restored_host, "cancel", vec![])
        .unwrap();
    assert!(restored_labels.pending_labels().is_empty());
    for token in [old, current] {
        assert!(matches!(
            restored.complete_operation(token, Completion::Fulfilled(JsValue::null())),
            Err(VmError::UnknownOperation(_))
        ));
    }
    let already = restored
        .start(&mut restored_host, "already", vec![])
        .unwrap();
    assert!(restored_labels.pending_labels().is_empty());
    assert_eq!(
        restored
            .start(&mut restored_host, "badSignal", vec![])
            .unwrap(),
        Value::boolean(true)
    );
    restored.run_until_idle(&mut restored_host).unwrap();
    for promise in [result, already] {
        assert_eq!(
            restored.promise_state(&promise).unwrap(),
            PromiseState::Fulfilled(Value::boolean(true))
        );
    }
    assert_eq!(labels.token("resume"), Some(old));
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
    let labels = Arc::new(LabelService::default());
    let bindings = strategy_fixture::bindings(&labels);
    let build = compile_main(source, &declared(include_str!("../dts/ops.d.ts"))).expect("compile");
    let mut host = StrategyHost::new(bindings);
    let mut vm = Vm::new(build.clone(), &mut host, Default::default()).expect("create VM");
    let result = vm.start(&mut host, "run", vec![]).expect("start");
    vm.run_until_idle(&mut host).expect("suspend");
    assert_eq!(labels.pending_labels().len(), 4);
    let snapshot = vm
        .snapshot(std::slice::from_ref(&result))
        .expect("snapshot");
    drop(vm);

    let mut vm = Vm::restore(snapshot, build, &mut host, Default::default()).expect("restore");
    assert!(labels.pending_labels().is_empty());
    assert!(
        labels.resume_log().is_empty(),
        "restore must not start external work before returning"
    );
    assert_eq!(vm.pending_operations().len(), 2);
    vm.start_recovered_operations()
        .expect("start recovered operations");
    vm.start_recovered_operations()
        .expect("starting recovered operations is idempotent");
    vm.run_until_idle(&mut host)
        .expect("settle restored operations");
    assert_eq!(
        labels.resume_log(),
        vec!["resume".to_owned(), "reissue".to_owned()]
    );
    assert_eq!(vm.promise_state(&result).unwrap(), PromiseState::Pending);

    for (name, value) in [("resume", 1.0), ("reissue", 2.0)] {
        let token = labels.token(name).expect("restored token");
        vm.complete_operation(token, Completion::Fulfilled(JsValue::number(value)))
            .expect("complete restored operation");
        vm.run_until_idle(&mut host).expect("drain completion");
    }
    assert_eq!(
        vm.promise_state(&result).unwrap(),
        PromiseState::Fulfilled(Value::string("strategy rejected".into()))
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
    let build = yield_harness::build(source);
    let mut host = yield_harness::TestHost::default();
    let mut vm = Vm::new(build.clone(), &mut host, Default::default()).expect("create VM");
    let result = vm.start(&mut host, "run", vec![]).expect("start");
    vm.run_until_idle(&mut host).expect("suspend");
    let stale = host.token("wait:a").expect("old token");
    assert_eq!(host.pending_labels(), vec!["wait:a".to_owned()]);
    let snapshot = vm
        .snapshot(std::slice::from_ref(&result))
        .expect("snapshot");
    vm = Vm::restore(snapshot, build, &mut host, Default::default()).expect("restore");
    vm.start_recovered_operations()
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
        PromiseState::Fulfilled(Value::number(1.0))
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
    let build = yield_harness::build(source);
    let mut host = yield_harness::TestHost::default();
    let clock = host.clock().clone();
    let mut vm = Vm::new(
        build,
        &mut host,
        VmOptions {
            test_clock: Some(clock),
            ..Default::default()
        },
    )
    .expect("create VM");
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
    struct StashKey;
    impl yield_vm::ServiceKey for StashKey {
        type Value = Stash;
        const ID: &'static str = "stash";
        const VERSION: u32 = 1;
    }

    #[host_module(name = "stale")]
    struct Stale;

    #[module_exports]
    impl Stale {
        #[function(name = "stash")]
        fn stash(cx: &mut yield_vm::ModuleCx<'_, Self>, value: JsValue) -> JsResult<()> {
            let stash = cx
                .service::<StashKey>()
                .ok_or_else(|| JsError::Fault(cx.fault("missing stash service")))?;
            *stash.lock().expect("stash lock") = Some(value);
            Ok(())
        }

        #[function(name = "unstash")]
        fn unstash(cx: &mut yield_vm::ModuleCx<'_, Self>) -> JsResult<Instance<Sink>> {
            let value = cx
                .service::<StashKey>()
                .and_then(|stash| stash.lock().expect("stash lock").clone())
                .ok_or_else(|| JsError::Fault(cx.fault("empty stash")))?;
            let sink = cx.alloc(Sink)?;
            sink.set_value(cx, value)?;
            Ok(sink)
        }
    }

    pub(super) fn bindings() -> Bindings {
        host_bindings! {
            dts: "../dts/stale.d.ts",
            modules: [Stale],
            classes: [Sink],
            operations: [],
            services: {
                StashKey => Stash::default(),
            },
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
    let build =
        compile_main(source, &declared(include_str!("../dts/stale.d.ts"))).expect("compile");
    let mut host = StrategyHost::new(bindings);
    let mut vm = Vm::new(build, &mut host, Default::default()).expect("create VM");
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
    struct StashKey;
    impl yield_vm::ServiceKey for StashKey {
        type Value = Stash;
        const ID: &'static str = "stash";
        const VERSION: u32 = 1;
    }

    struct RunsKey;
    impl yield_vm::ServiceKey for RunsKey {
        type Value = Arc<AtomicUsize>;
        const ID: &'static str = "runs";
        const VERSION: u32 = 1;
    }

    #[host_module(name = "guest")]
    struct Guest;

    #[module_exports]
    impl Guest {
        #[function(name = "later")]
        fn later(cx: &mut yield_vm::ModuleCx<'_, Self>, callee: JsValue) -> JsResult<HostStep> {
            cx.request_call(
                callee,
                JsValue::undefined(),
                vec![],
                vec![],
                unexpected_resume,
            )
        }

        #[function(name = "stash")]
        fn stash(cx: &mut yield_vm::ModuleCx<'_, Self>, callee: JsValue) -> JsResult<()> {
            let request = Self::later(cx, callee)?;
            *cx.service::<StashKey>().unwrap().lock().unwrap() = Some(request);
            Ok(())
        }

        #[function(name = "stashGc")]
        fn stash_gc(cx: &mut yield_vm::ModuleCx<'_, Self>) -> JsResult<()> {
            let request = cx.request_gc(vec![], unexpected_resume)?;
            *cx.service::<StashKey>().unwrap().lock().unwrap() = Some(request);
            Ok(())
        }

        #[function(name = "replay")]
        fn replay(cx: &mut yield_vm::ModuleCx<'_, Self>) -> JsResult<HostStep> {
            Ok(cx
                .service::<StashKey>()
                .unwrap()
                .lock()
                .unwrap()
                .take()
                .unwrap())
        }

        #[function(name = "fault")]
        fn fault(cx: &mut yield_vm::ModuleCx<'_, Self>) -> JsResult<()> {
            Err(JsError::Fault(cx.fault("guest host fault")))
        }

        #[function(name = "mark")]
        fn mark(cx: &mut yield_vm::ModuleCx<'_, Self>) -> JsResult<()> {
            cx.service::<RunsKey>()
                .unwrap()
                .fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        #[function(name = "twice")]
        fn twice(
            cx: &mut yield_vm::ModuleCx<'_, Self>,
            callee: JsValue,
            seed: JsValue,
        ) -> JsResult<HostStep> {
            cx.request_call(
                callee.clone(),
                cx.this_value(),
                vec![seed],
                vec![callee],
                again,
            )
        }

        #[function(name = "catchThenCompare")]
        async fn catch_then_compare(
            cx: &mut yield_vm::ModuleCx<'_, Self>,
            original: JsValue,
            throwing: JsValue,
            returning: JsValue,
        ) -> JsResult<HostStep> {
            let first = cx.call(throwing, JsValue::undefined(), vec![]).await;
            match first {
                GuestOutcome::Thrown(_) => {}
                GuestOutcome::Returned(_) => return Err(cx.type_error("expected guest throw")),
            }

            let second = cx.call(returning, JsValue::undefined(), Vec::new()).await;
            match second {
                GuestOutcome::Returned(value) => Ok(HostStep::Return(JsValue::boolean(
                    cx.same_value(&value, &original),
                ))),
                GuestOutcome::Thrown(value) => Err(JsError::Throw(value)),
            }
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
            dts: "../dts/guest.d.ts",
            modules: [Guest],
            classes: [],
            operations: [],
            services: {
                StashKey => Stash::default(),
                RunsKey => runs.clone(),
            },
        }
        .expect("guest bindings");
        let build =
            compile_main(source, &declared(include_str!("../dts/guest.d.ts"))).expect("compile");
        let mut host = StrategyHost::new(bindings);
        let mut vm = Vm::new(build, &mut host, Default::default()).expect("create VM");
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
        matches!(result, Err(VmError::HostFault(message)) if message == "SuspendRequest used outside the call that produced it")
    );
    assert_eq!(runs, 0);
}

#[test]
fn gc_request_cannot_be_replayed() {
    let (result, _) = guest_call_fixture::run(
        r#"
        import { stashGc, replay } from 'guest';
        export function run() {
            stashGc();
            return replay();
        }
    "#,
    );
    assert!(
        matches!(result, Err(VmError::HostFault(message)) if message == "SuspendRequest used outside the call that produced it")
    );
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
    assert_eq!(result.expect("chained guest calls"), Value::number(8.0));
}

#[test]
fn async_guest_calls_catch_throw_and_preserve_object_identity() {
    let (result, runs) = guest_call_fixture::run(
        r#"
        import { catchThenCompare, mark } from 'guest';
        export function run() {
            const original = {};
            let throwingRuns = 0;
            let returningRuns = 0;
            const same = catchThenCompare(original, () => {
                throwingRuns += 1;
                mark();
                throw 'guest throw';
            }, () => {
                returningRuns += 1;
                mark();
                return original;
            });
            if (throwingRuns !== 1) throw 'throwing guest must run exactly once';
            if (returningRuns !== 1) throw 'returning guest must run exactly once';
            return same;
        }
    "#,
    );
    assert_eq!(
        result.expect("two awaited guest calls"),
        Value::boolean(true)
    );
    assert_eq!(runs, 2);
}

mod counter_fixture {
    use super::*;
    use yield_harness::Wait;

    #[host_module(name = "counter", schema = 1, init = Self::new, encode = Self::encode_payload, decode = Self::decode_payload)]
    struct Counter {
        count: u32,
    }

    impl Counter {
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

    #[module_exports]
    impl Counter {
        #[function(name = "bump")]
        fn bump(cx: &mut yield_vm::ModuleCx<'_, Self>) -> JsResult<()> {
            cx.state().count += 1;
            Ok(())
        }

        #[function(name = "value")]
        fn value(cx: &mut yield_vm::ModuleCx<'_, Self>) -> JsResult<JsValue> {
            Ok(JsValue::number(f64::from(cx.state().count)))
        }
    }

    #[host_module(name = "test")]
    struct TestModule;

    #[module_exports]
    impl TestModule {
        #[function(name = "wait")]
        fn wait(cx: &mut yield_vm::ModuleCx<'_, Self>, arg: JsValue) -> JsResult<JsValue> {
            let label = cx
                .string(&arg)?
                .to_utf8()
                .map_err(|_| cx.type_error("wait requires a valid label"))?;
            cx.promise(Wait { label })
        }
    }

    pub(super) fn bindings(labels: &Arc<LabelService>) -> Bindings {
        host_bindings! {
            dts: "../dts/counter-test.d.ts",
            modules: [Counter, TestModule],
            classes: [],
            operations: [Wait],
            services: {
                LabelServiceKey => Arc::clone(labels),
            },
        }
        .expect("counter bindings")
    }
}

#[test]
fn module_state_survives_restore() {
    let source = r#"
import { bump, value } from "counter";
import { wait } from "test";
export function read() { return value(); }
export async function run() {
  bump();
  bump();
  await wait("a");
  bump();
  return value();
}
"#;
    let labels = Arc::new(LabelService::default());
    let bindings = counter_fixture::bindings(&labels);
    let build =
        compile_main(source, &declared(include_str!("../dts/counter-test.d.ts"))).expect("compile");
    let mut host = StrategyHost::new(bindings);
    let mut vm = Vm::new(build.clone(), &mut host, Default::default()).expect("create VM");
    let result = vm.start(&mut host, "run", vec![]).expect("start");
    vm.run_until_idle(&mut host).expect("suspend");
    let snapshot = vm
        .snapshot(std::slice::from_ref(&result))
        .expect("snapshot");
    let [counter, test] = snapshot.module_states.as_slice() else {
        panic!("expected exactly one record per registered module");
    };
    assert_eq!(counter.module, "counter");
    assert_eq!(counter.payload.bytes, 2u32.to_le_bytes());
    assert_eq!(test.module, "test");
    assert_eq!(test.payload.schema, 1);
    assert!(test.payload.bytes.is_empty());

    let mut second = Vm::new(build.clone(), &mut host, Default::default()).expect("second VM");
    assert_eq!(
        second.start(&mut host, "read", vec![]).unwrap(),
        Value::number(0.0)
    );

    let mut missing = snapshot.clone();
    missing.module_states.pop();
    let mut wrong_schema = snapshot.clone();
    wrong_schema.module_states[0].payload.schema += 1;
    let mut nonempty = snapshot.clone();
    nonempty.module_states[1].payload.bytes.push(0);
    for (invalid, expected) in [
        (missing, "module state count mismatch"),
        (wrong_schema, "state payload schema mismatch"),
        (nonempty, "unexpected module payload"),
    ] {
        let options = RestoreOptions {
            validation: SnapshotValidation::Full,
            ..Default::default()
        };
        assert!(
            matches!(
                Vm::restore(invalid, build.clone(), &mut host, options),
                Err(VmError::InvalidSnapshot(message)) if message.contains(expected)
            ),
            "restore must reject {expected}"
        );
    }
    drop(vm);

    let mut vm = Vm::restore(snapshot, build, &mut host, Default::default()).expect("restore");
    vm.start_recovered_operations()
        .expect("start recovered operations");
    let token = labels.token("wait:a").expect("restored wait token");
    vm.complete_operation(token, Completion::Fulfilled(JsValue::undefined()))
        .expect("complete wait");
    vm.run_until_idle(&mut host).expect("resume");
    assert_eq!(
        vm.promise_state(&result).unwrap(),
        PromiseState::Fulfilled(Value::number(3.0))
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
    struct Geom;

    #[module_exports]
    impl Geom {
        #[constructor(name = "Point", class = Point)]
        fn point(
            cx: &mut yield_vm::ModuleCx<'_, Self>,
            value: JsValue,
        ) -> JsResult<Instance<Point>> {
            let x = cx.number(&value)?;
            cx.alloc(Point { x })
        }
    }

    pub(super) fn bindings() -> Bindings {
        host_bindings! {
            dts: "../dts/geom.d.ts",
            modules: [Geom],
            classes: [Point],
            operations: [],
            services: {},
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
    let build = compile_main(source, &declared(include_str!("../dts/geom.d.ts"))).expect("compile");
    let mut host = StrategyHost::new(bindings);
    let mut vm = Vm::new(build, &mut host, Default::default()).expect("create VM");
    assert_eq!(
        vm.start(&mut host, "run", vec![]).expect("start"),
        Value::number(3.0)
    );
}

mod loose_fixture {
    use super::*;

    #[host_class(tag = "Loose")]
    struct Loose {
        _n: u32,
    }

    #[host_module(name = "loose")]
    struct LooseModule;

    #[module_exports]
    impl LooseModule {
        #[function(name = "make")]
        fn make(cx: &mut yield_vm::ModuleCx<'_, Self>) -> JsResult<Instance<Loose>> {
            cx.alloc(Loose { _n: 1 })
        }
    }

    pub(super) fn bindings() -> Bindings {
        host_bindings! {
            dts: "../dts/loose.d.ts",
            modules: [LooseModule],
            classes: [Loose],
            operations: [],
            services: {},
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
    let build =
        compile_main(source, &declared(include_str!("../dts/loose.d.ts"))).expect("compile");
    let mut host = StrategyHost::new(bindings);
    let mut vm = Vm::new(build, &mut host, Default::default()).expect("create VM");
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
    let build = yield_harness::build(source);
    let mut host = yield_harness::TestHost::default();
    let mut vm = Vm::new(build.clone(), &mut host, Default::default()).expect("create VM");
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
    let error =
        Vm::restore(snapshot, build, &mut host, Default::default()).expect_err("invalid payload");
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

    label_driver!(InheritWait, "inherit-wait");

    #[host_class(tag = "Base", methods, schema = 1, encode = Base::encode_payload, decode = Base::decode_payload)]
    struct Base {
        payload_value: u8,
        #[js_slot]
        base: JsValue,
        #[js_slot]
        sidecar: JsValue,
    }

    impl Base {
        fn encode_payload(&self, out: &mut Vec<u8>) -> Result<(), CodecError> {
            out.push(self.payload_value);
            Ok(())
        }

        fn decode_payload(input: &[u8], _cx: &mut DecodeCx<'_>) -> Result<Self, CodecError> {
            let [payload_value] = input else {
                return Err(CodecError::new("invalid Base payload"));
            };
            Ok(Self {
                payload_value: *payload_value,
            })
        }
    }

    #[methods(Base)]
    impl Base {
        #[method(name = "read")]
        fn read(cx: &mut CallCx<'_>, this: This<Base>) -> JsResult<JsValue> {
            this.base(cx)
        }

        #[method(name = "readPayload")]
        fn read_payload(cx: &mut CallCx<'_>, this: This<Base>) -> JsResult<JsValue> {
            Ok(JsValue::number(f64::from(
                this.instance().payload(cx)?.payload_value,
            )))
        }

        #[method(name = "sidecar")]
        fn sidecar(cx: &mut CallCx<'_>, this: This<Base>) -> JsResult<JsValue> {
            this.sidecar(cx)
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
    struct Inherit;

    #[module_exports]
    impl Inherit {
        #[function(name = "wait")]
        fn wait(cx: &mut yield_vm::ModuleCx<'_, Self>) -> JsResult<JsValue> {
            cx.promise(InheritWait)
        }

        #[function(name = "assert")]
        fn assert(
            cx: &mut yield_vm::ModuleCx<'_, Self>,
            value: JsValue,
            _message: Option<JsValue>,
        ) -> JsResult<()> {
            if value.truthy() {
                return Ok(());
            }
            Err(cx.throw_string("assert failed".to_owned()))
        }

        #[constructor(name = "Base", class = Base)]
        fn base(cx: &mut yield_vm::ModuleCx<'_, Self>) -> JsResult<Instance<Base>> {
            let sidecar = cx.alloc(Derived)?;
            sidecar.set_value(cx, JsValue::number(2.0))?;
            let base = cx.alloc(Base { payload_value: 9 })?;
            base.set_base(cx, JsValue::number(1.0))?;
            let sidecar = sidecar.value(cx);
            base.set_sidecar(cx, sidecar)?;
            Ok(base)
        }

        #[constructor(name = "Derived", class = Derived)]
        fn derived(cx: &mut yield_vm::ModuleCx<'_, Self>) -> JsResult<Instance<Derived>> {
            let derived = cx.alloc(Derived)?;
            derived.set_value(cx, JsValue::number(2.0))?;
            Ok(derived)
        }
    }

    pub(super) fn bindings(labels: &Arc<LabelService>) -> Bindings {
        host_bindings! {
            dts: "../dts/inherit.d.ts",
            modules: [Inherit],
            classes: [Base, Derived],
            operations: [InheritWait],
            services: {
                LabelServiceKey => Arc::clone(labels),
            },
        }
        .expect("derived bindings")
    }
}

#[test]
fn derived_host_classes_inherit_prototypes_and_slot_layout() {
    let labels = Arc::new(LabelService::default());
    let bindings = inherit_fixture::bindings(&labels);
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
    let build =
        compile_main(source, &declared(include_str!("../dts/inherit.d.ts"))).expect("compile");
    let mut host = StrategyHost::new(bindings);
    let mut vm = Vm::new(build.clone(), &mut host, Default::default()).expect("create VM");
    let result = vm.start(&mut host, "run", vec![]).expect("start");
    vm.run_until_idle(&mut host).expect("suspend");
    let snapshot = vm
        .snapshot(std::slice::from_ref(&result))
        .expect("snapshot derived instance");
    drop(vm);

    let mut vm = Vm::restore(snapshot, build, &mut host, Default::default()).expect("restore");
    vm.start_recovered_operations()
        .expect("start recovered operation");
    let token = labels.token("inherit-wait").expect("restored wait token");
    vm.complete_operation(token, Completion::Fulfilled(JsValue::undefined()))
        .expect("complete wait");
    vm.run_until_idle(&mut host).expect("drain");
    assert_eq!(
        vm.promise_state(&result).unwrap(),
        PromiseState::Fulfilled(Value::boolean(true))
    );
}

#[test]
fn script_subclass_of_host_class_survives_restore() {
    let labels = Arc::new(LabelService::default());
    let bindings = inherit_fixture::bindings(&labels);
    let source = r#"
import { assert, Base, Derived, wait } from "inherit";

export async function run() {
  class Script extends Base {
    #count = 3;
    increment() { this.#count += 1; }
    count() { return this.#count; }
  }

  const instance = new Script();
  instance.increment();
  const sidecar = instance.sidecar();
  const direct = new Base();
  const forged = Object.create(Script.prototype);
  class Replacing extends Base {
    constructor() { super(); return direct; }
  }
  const replacement = new Replacing();

  function check() {
    assert(Object.getPrototypeOf(instance) === Script.prototype);
    assert(Object.getPrototypeOf(Script.prototype) === Base.prototype);
    assert(instance instanceof Script);
    assert(instance instanceof Base);
    assert(instance.read() === 1);
    assert(instance.readPayload() === 9);
    assert(instance.count() === 4);

    // The host constructor also allocates a different host class in the same call.
    assert(instance.sidecar() === sidecar);
    assert(Object.getPrototypeOf(sidecar) === Derived.prototype);
    assert(sidecar instanceof Derived);
    assert(!(sidecar instanceof Script));
    assert(sidecar.value() === 2);
    assert(Object.getPrototypeOf(direct) === Base.prototype);
    assert(!(direct instanceof Script));
    assert(direct.read() === 1);
    assert(replacement === direct);
    assert(!(replacement instanceof Replacing));

    assert(forged instanceof Base);
    assert(forged instanceof Script);
    let error;
    try { forged.read(); } catch (caught) { error = caught; }
    assert(error instanceof TypeError);
  }

  check();
  await wait();
  check();
  return true;
}
"#;
    let build =
        compile_main(source, &declared(include_str!("../dts/inherit.d.ts"))).expect("compile");
    let mut host = StrategyHost::new(bindings);
    let mut vm = Vm::new(build.clone(), &mut host, Default::default()).expect("create VM");
    let result = vm.start(&mut host, "run", vec![]).expect("start");
    vm.run_until_idle(&mut host).expect("suspend");
    assert_eq!(vm.promise_state(&result).unwrap(), PromiseState::Pending);
    let snapshot = vm
        .snapshot(std::slice::from_ref(&result))
        .expect("snapshot script subclass");
    drop(vm);

    let mut vm = Vm::restore(snapshot, build, &mut host, Default::default()).expect("restore");
    vm.start_recovered_operations()
        .expect("start recovered operation");
    let token = labels.token("inherit-wait").expect("restored wait token");
    vm.complete_operation(token, Completion::Fulfilled(JsValue::undefined()))
        .expect("complete wait");
    vm.run_until_idle(&mut host).expect("drain");
    assert_eq!(
        vm.promise_state(&result).unwrap(),
        PromiseState::Fulfilled(Value::boolean(true))
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
    let build = yield_harness::build(source);
    let mut host = yield_harness::TestHost::default();
    let mut vm = Vm::new(build, &mut host, Default::default()).expect("create VM");
    assert_eq!(
        vm.start(&mut host, "run", vec![]).expect("start"),
        Value::boolean(true)
    );
    assert_eq!(host.count("wait"), 1);
    assert!(host.pending_labels().is_empty());
}

mod subset_fixture {
    use super::*;

    #[host_module(name = "subset")]
    struct Subset;

    #[module_exports]
    impl Subset {
        #[function(name = "hidden")]
        fn hidden(_cx: &mut yield_vm::ModuleCx<'_, Self>) -> JsResult<JsValue> {
            Ok(JsValue::number(1.0))
        }

        #[function(name = "alpha")]
        fn alpha(_cx: &mut yield_vm::ModuleCx<'_, Self>) -> JsResult<JsValue> {
            Ok(JsValue::number(4.0))
        }
    }

    pub(super) fn bindings() -> Bindings {
        host_bindings! {
            dts: "../dts/subset.d.ts",
            modules: [Subset],
            classes: [],
            operations: [],
            services: {},
        }
        .expect("subset bindings")
    }
}

#[test]
fn declared_names_select_implementations_and_hide_extra_exports() {
    let bindings = subset_fixture::bindings();
    let source = r#"
import * as shared from "subset";
export function run() {
  if (shared.hidden !== undefined) return -1;
  if ("hidden" in shared) return -2;
  return shared.alpha();
}
"#;
    let build =
        compile_main(source, &declared(include_str!("../dts/subset.d.ts"))).expect("compile");
    let mut host = StrategyHost::new(bindings);
    let mut vm = Vm::new(build, &mut host, Default::default()).expect("create VM");
    assert_eq!(
        vm.start(&mut host, "run", vec![]).expect("start"),
        Value::number(4.0)
    );
}
