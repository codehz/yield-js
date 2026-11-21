//! Timer behavior across independent platform clocks and VM generations.

use yield_harness::{TestHost, build};
use yield_vm::snapshot::DeliveryTarget;
use yield_vm::{Completion, JsValue, PromiseState, Snapshot, Value, Vm, VmError};

const SCHEDULE: &str = r#"
    import { setTimeout, clearTimeout, sleep } from 'test';
    export let observations = '';
    export let sleeping;
    let second;
    export function run() {
        setTimeout(() => { observations += 'a'; }, 100);
        second = setTimeout(() => { observations += 'b'; }, 100);
        sleeping = sleep(100);
        sleeping.then(value => { observations += value === undefined ? 'p' : '?'; });
    }
    export function cancel() { clearTimeout(second); }
    export async function observe() {
        await sleeping;
        return observations;
    }
"#;

fn expect_unknown(vm: &mut Vm, token: yield_vm::OperationToken) {
    assert!(matches!(
        vm.complete_operation(token, Completion::Fulfilled(JsValue::undefined())),
        Err(VmError::UnknownOperation(_))
    ));
}

#[test]
fn restore_arms_only_remaining_time_on_an_independent_clock() {
    let build = build(SCHEDULE);
    for restore in [false, true] {
        let mut host = TestHost::new();
        let mut vm = Vm::new(build.clone(), &mut host).unwrap();
        vm.start(&mut host, "run", vec![]).unwrap();
        let old = host.clock().registrations();
        assert_eq!(old.len(), 3);
        assert!(old.iter().all(|&(_, deadline)| deadline == 100));
        let promise = vm.get_export("sleeping").unwrap();
        let yield_vm::ValueRef::Object(promise_id) = promise.view() else {
            panic!("sleep didn't allocate a Promise");
        };
        host.advance(&mut vm, 30).unwrap();
        let image = vm.snapshot(&[]).unwrap();
        assert_eq!(image.host_tasks.len(), 2);
        assert_eq!(image.host_tasks[0].saved.len(), 1);
        assert_eq!(
            image
                .operations
                .iter()
                .filter(|op| op.target == DeliveryTarget::Promise(promise_id))
                .count(),
            1
        );
        assert_eq!(
            image
                .operations
                .iter()
                .filter(|op| matches!(op.target, DeliveryTarget::Task(_)))
                .count(),
            2
        );
        assert!(
            image
                .operations
                .iter()
                .all(|op| op.state == 70u32.to_le_bytes())
        );
        let json = image.to_json().unwrap();
        assert_eq!(vm.snapshot(&[]).unwrap().to_json().unwrap(), json);
        assert_eq!(host.clock().registrations(), old);

        if restore {
            drop(vm);
            assert!(host.clock().registrations().is_empty());
            host = TestHost::new();
            host.clock().set_time(10_000).unwrap();
            vm = Vm::restore(
                Snapshot::from_json(&json).unwrap(),
                build.clone(),
                &mut host,
            )
            .unwrap();
            assert!(host.clock().registrations().is_empty());
            assert_eq!(host.clock().arm_count(), 0);
            assert!(
                vm.snapshot(&[])
                    .unwrap()
                    .operations
                    .iter()
                    .all(|op| op.state == 70u32.to_le_bytes())
            );
            vm.start_recovered_operations().unwrap();
            vm.start_recovered_operations().unwrap();
            assert_eq!(host.clock().arm_count(), 3);
            assert!(
                host.clock()
                    .registrations()
                    .iter()
                    .all(|&(_, deadline)| deadline == 10_070)
            );
            for (token, _) in &old {
                expect_unknown(&mut vm, *token);
            }
        }
        let current = host.clock().registrations();
        assert_eq!(vm.get_export("sleeping").unwrap(), promise);
        vm.start(&mut host, "cancel", vec![]).unwrap();
        expect_unknown(&mut vm, current[1].0);
        let observed = vm.start(&mut host, "observe", vec![]).unwrap();
        host.advance(&mut vm, 69).unwrap();
        assert_eq!(
            vm.get_export("observations").unwrap(),
            Value::string("".into())
        );
        assert_eq!(vm.promise_state(&promise).unwrap(), PromiseState::Pending);
        host.advance(&mut vm, 1).unwrap();
        assert_eq!(
            vm.promise_state(&promise).unwrap(),
            PromiseState::Fulfilled(Value::UNDEFINED)
        );
        assert_eq!(
            vm.promise_state(&observed).unwrap(),
            PromiseState::Fulfilled(Value::string("ap".into()))
        );
        assert!(host.clock().registrations().is_empty());
        assert!(vm.pending_operations().is_empty());
        for (token, _) in current {
            expect_unknown(&mut vm, token);
        }
    }
}

#[test]
fn zero_deadline_delivery_is_ordered_and_handles_are_never_reused() {
    let build = build(
        r#"
        import { setTimeout, clearTimeout } from 'test';
        export let total = 0;
        let second;
        let completed;
        export function run() {
            completed = setTimeout(() => { total += 1; clearTimeout(second); }, 0);
            second = setTimeout(() => { total += 1000; }, 0);
            return completed;
        }
        export function again() {
            const fresh = setTimeout(() => { total += 2; }, 0);
            clearTimeout(completed);
            return fresh;
        }
    "#,
    );
    let mut host = TestHost::new();
    let mut vm = Vm::new(build.clone(), &mut host).unwrap();
    let completed = vm.start(&mut host, "run", vec![]).unwrap();
    assert_eq!(vm.get_export("total").unwrap(), Value::number(0.0));
    host.advance(&mut vm, 0).unwrap();
    assert_eq!(vm.get_export("total").unwrap(), Value::number(1.0));
    let image = super::support::json_round_trip(&vm.snapshot(&[]).unwrap());
    assert!(image.host_tasks.is_empty());
    assert!(image.operations.is_empty());
    let mut host = TestHost::new();
    let mut vm = Vm::restore(image, build, &mut host).unwrap();
    let fresh = vm.start(&mut host, "again", vec![]).unwrap();
    assert_ne!(fresh, completed);
    host.advance(&mut vm, 0).unwrap();
    assert_eq!(vm.get_export("total").unwrap(), Value::number(3.0));
}

#[yield_vm::host_module(name = "chain")]
mod chain {
    use yield_harness::Timer;
    use yield_vm::binding::CallCx;
    use yield_vm::{JsResult, JsValue};

    #[function(name = "start")]
    fn start(cx: &mut CallCx<'_>, cb: JsValue, mode: JsValue) -> JsResult<JsValue> {
        cx.spawn(async move {
            let delay = if cx.number(&mode)? == 1.0 { 10.0 } else { 90.0 };
            let timer = Timer::from_delay(cx, &JsValue::number(delay))?;
            match delay {
                10.0 => {
                    cx.effect(timer).await?;
                }
                _ => {
                    cx.effect(timer).await?;
                }
            }
            cx.call(cb.clone(), JsValue::undefined(), Vec::new())
                .await?;
            cx.effect(Timer::from_delay(cx, &JsValue::number(20.0))?)
                .await?;
            cx.call(cb, JsValue::undefined(), Vec::new()).await?;
            Ok(())
        })
    }

    #[function(name = "cancel")]
    fn cancel(cx: &mut CallCx<'_>, handle: JsValue) -> JsResult<()> {
        cx.cancel_task(&handle)
    }
}

struct ChainHost(std::sync::Arc<yield_vm::Bindings>);

impl yield_vm::Host for ChainHost {
    fn bindings(&self) -> std::sync::Arc<yield_vm::Bindings> {
        self.0.clone()
    }
    fn report_error(&mut self, error: JsValue) {
        panic!("unexpected task error: {error}");
    }
}

#[test]
fn consecutive_timer_awaits_restore_without_replacing_the_handle() {
    use super::support::compile_main;
    use std::sync::Arc;
    use yield_compiler::CompileOptions;
    use yield_harness::Timer;

    let clock = Arc::new(yield_harness::FakeClock::default());
    let bindings = yield_vm::host_bindings! {
        dts: "tests/dts/chain.d.ts",
        modules: [chain], classes: [], states: [], operations: [Timer],
        services: [("clock", 1, Arc::clone(&clock))],
    }
    .unwrap();
    let mut host = ChainHost(Arc::new(bindings));
    let build = compile_main(
        r#"
        import { start, cancel } from 'chain';
        export let count = 0;
        let handle;
        export function run() { handle = start(() => { count += 1; }, 1); return handle; }
        export function clear() { cancel(handle); }
    "#,
        &CompileOptions {
            imports: [("chain".into(), ["start".into(), "cancel".into()].into())].into(),
        },
    )
    .unwrap();
    let mut vm = Vm::new(build.clone(), &mut host).unwrap();
    let handle = vm.start(&mut host, "run", vec![]).unwrap();
    let first = vm.pending_operations()[0].token;
    assert_eq!(clock.registrations(), [(first, 10)]);
    let image = super::support::json_round_trip(&vm.snapshot(&[]).unwrap());
    vm = Vm::restore(image, build.clone(), &mut host).unwrap();
    vm.start_recovered_operations().unwrap();
    let first_recovered = vm.pending_operations()[0].token;
    clock.advance(&mut vm, &mut host, 10).unwrap();
    let second = vm.pending_operations()[0].token;
    assert_eq!(clock.registrations(), [(second, 30)]);
    assert_eq!(vm.get_export("count").unwrap(), Value::number(1.0));
    expect_unknown(&mut vm, first_recovered);
    let image = super::support::json_round_trip(&vm.snapshot(&[]).unwrap());
    assert_eq!(Value::number(image.host_tasks[0].id as f64), handle);
    vm = Vm::restore(image, build, &mut host).unwrap();
    vm.start_recovered_operations().unwrap();
    let second_recovered = vm.pending_operations()[0].token;
    vm.start(&mut host, "clear", vec![]).unwrap();
    expect_unknown(&mut vm, second_recovered);
    assert!(clock.registrations().is_empty());
    assert_eq!(vm.get_export("count").unwrap(), Value::number(1.0));
}
