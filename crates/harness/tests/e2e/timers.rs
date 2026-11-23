//! Timer behavior across independent platform clocks and VM generations.

use yield_harness::{TestHost, build};
use yield_vm::snapshot::DeliveryTarget;
use yield_vm::{
    Completion, JsValue, PromiseState, RestoreOptions, Snapshot, SnapshotValidation, Value, Vm,
    VmError, VmOptions,
};

const SCHEDULE: &str = r#"
    import { spawnDelayedTask, cancelDelayedTask, sleep } from 'test';
    export let observations = '';
    export let sleeping;
    let second;
    export function run() {
        spawnDelayedTask(() => { observations += 'a'; }, 100);
        second = spawnDelayedTask(() => { observations += 'b'; }, 100);
        sleeping = sleep(100);
        sleeping.then(value => { observations += value === undefined ? 'p' : '?'; });
    }
    export function cancel() { cancelDelayedTask(second); }
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
fn timeout_rearms_early_tokens_and_restores_remaining_time() {
    let build = build(
        r#"
        const signal = AbortSignal.timeout(10);
        let events = 0;
        signal.onabort = () => { ++events; };
        export function run() { return signal.aborted ? signal.reason.code * 10 + events : events; }
    "#,
    );
    let mut source_host = TestHost::new();
    let clock = source_host.clock().clone();
    let mut vm = Vm::new(
        build.clone(),
        &mut source_host,
        VmOptions {
            test_clock: Some(clock),
            ..Default::default()
        },
    )
    .unwrap();
    source_host.advance(&mut vm, 4).unwrap();
    let early = source_host.clock().registrations()[0].0;
    assert!(matches!(
        vm.complete_operation(early, Completion::Fulfilled(JsValue::undefined())),
        Err(VmError::InvalidState(_))
    ));
    vm.wake_timer(early).unwrap();
    vm.wake_timer(early).unwrap();
    let (old, deadline) = source_host.clock().registrations()[0];
    assert_eq!(deadline, 10);
    assert_ne!(old, early);
    let image = vm.snapshot(&[]).unwrap();
    assert_eq!(source_host.clock().registrations(), [(old, 10)]);

    let mut host = TestHost::new();
    host.clock().set_time(1_000).unwrap();
    let clock = host.clock().clone();
    let mut vm = Vm::restore(
        Snapshot::from_bytes(&image.to_bytes().unwrap()).unwrap(),
        build,
        &mut host,
        RestoreOptions {
            test_clock: Some(clock),
            validation: SnapshotValidation::Full,
            fetch_environment: None,
        },
    )
    .unwrap();
    assert!(host.clock().registrations().is_empty());
    vm.start_recovered_operations().unwrap();
    let (current, deadline) = host.clock().registrations()[0];
    assert_eq!(deadline, 1_006);
    vm.wake_timer(old).unwrap();
    host.advance(&mut vm, 5).unwrap();
    assert_eq!(
        vm.start(&mut host, "run", vec![]).unwrap(),
        Value::number(0.0)
    );
    host.advance(&mut vm, 1).unwrap();
    assert_eq!(
        vm.start(&mut host, "run", vec![]).unwrap(),
        Value::number(231.0)
    );
    vm.wake_timer(current).unwrap();
}

#[test]
fn due_timeout_recovery_delivers_once_without_rearming() {
    let build = build(
        r#"
        const signal = AbortSignal.timeout(0);
        let observed = 0;
        signal.onabort = () => { ++observed; };
        export function run() { return observed; }
    "#,
    );
    let mut host = TestHost::new();
    let source = Vm::new(build.clone(), &mut host, Default::default()).unwrap();
    let image = source.snapshot(&[]).unwrap();
    let mut restored_host = TestHost::new();
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
    restored.start_recovered_operations().unwrap();
    restored.start_recovered_operations().unwrap();
    assert_eq!(restored_host.clock().arm_count(), 0);
    assert_eq!(
        restored.start(&mut restored_host, "run", vec![]).unwrap(),
        Value::number(0.0)
    );
    restored.run_until_idle(&mut restored_host).unwrap();
    assert_eq!(
        restored.start(&mut restored_host, "run", vec![]).unwrap(),
        Value::number(1.0)
    );
}

#[test]
fn timer_arm_failure_is_not_an_abort() {
    let build = build(
        r#"
        export let signal;
        export function run() { signal = AbortSignal.timeout(10); }
        export function aborted() { return signal.aborted; }
    "#,
    );
    let mut host = TestHost::new();
    let clock = host.clock().clone();
    let mut vm = Vm::new(
        build.clone(),
        &mut host,
        VmOptions {
            test_clock: Some(clock),
            ..Default::default()
        },
    )
    .unwrap();
    host.clock().set_fail_start(true);
    assert!(matches!(
        vm.start(&mut host, "run", vec![]),
        Err(VmError::HostFault(_))
    ));
    assert!(vm.pending_operations().is_empty());
    host.clock().set_fail_start(false);
    vm.start(&mut host, "run", vec![]).unwrap();
    let image = vm.snapshot(&[]).unwrap();
    let mut other_host = TestHost::new();
    let clock = other_host.clock().clone();
    let mut restored = Vm::restore(
        image,
        build,
        &mut other_host,
        RestoreOptions {
            test_clock: Some(clock),
            validation: SnapshotValidation::Full,
            fetch_environment: None,
        },
    )
    .unwrap();
    other_host.clock().set_fail_start(true);
    assert!(matches!(
        restored.start_recovered_operations(),
        Err(VmError::HostFault(_))
    ));
    assert!(restored.pending_operations().is_empty());
    let token = host.clock().registrations()[0].0;
    host.clock().set_fail_start(true);
    assert!(matches!(vm.wake_timer(token), Err(VmError::HostFault(_))));
    assert!(host.clock().registrations().is_empty());
    vm.run_until_idle(&mut host).unwrap();
    assert_eq!(
        vm.start(&mut host, "aborted", vec![]).unwrap(),
        Value::boolean(false)
    );
}

#[test]
fn date_clock_rejects_out_of_range_changes_without_delivering_timers() {
    const MAX_DATE_MS: u64 = 8_640_000_000_000_000;
    let mut host = TestHost::new();
    assert_eq!(host.time_service().now_ms(), 0);
    assert_eq!(host.time_service().time_zone(), "Etc/UTC");
    host.set_time_zone("America/New_York");
    assert_eq!(host.time_service().time_zone(), "America/New_York");

    assert!(host.clock().set_time(MAX_DATE_MS + 1).is_err());
    assert_eq!(host.time_service().now_ms(), 0);
    host.clock().set_time(MAX_DATE_MS - 100).unwrap();

    let mut vm = Vm::new(build(SCHEDULE), &mut host, Default::default()).unwrap();
    vm.start(&mut host, "run", vec![]).unwrap();
    let registrations = host.clock().registrations();
    assert_eq!(registrations.len(), 3);
    for delta in [101, u64::MAX] {
        assert!(host.advance(&mut vm, delta).is_err());
        assert_eq!(host.time_service().now_ms(), (MAX_DATE_MS - 100) as i64);
        assert_eq!(host.clock().registrations(), registrations);
    }

    host.advance(&mut vm, 100).unwrap();
    assert_eq!(host.time_service().now_ms(), MAX_DATE_MS as i64);
    assert!(host.clock().registrations().is_empty());
}

#[test]
fn restore_arms_only_remaining_time_on_an_independent_clock() {
    let build = build(SCHEDULE);
    for restore in [false, true] {
        let mut host = TestHost::new();
        let clock = host.clock().clone();
        let mut vm = Vm::new(
            build.clone(),
            &mut host,
            VmOptions {
                test_clock: Some(clock),
                ..Default::default()
            },
        )
        .unwrap();
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
            let clock = host.clock().clone();
            vm = Vm::restore(
                Snapshot::from_json(&json).unwrap(),
                build.clone(),
                &mut host,
                RestoreOptions {
                    test_clock: Some(clock),
                    ..Default::default()
                },
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
        import { spawnDelayedTask, cancelDelayedTask } from 'test';
        export let total = 0;
        let second;
        let completed;
        export function run() {
            completed = spawnDelayedTask(() => { total += 1; cancelDelayedTask(second); }, 0);
            second = spawnDelayedTask(() => { total += 1000; }, 0);
            return completed;
        }
        export function again() {
            const fresh = spawnDelayedTask(() => { total += 2; }, 0);
            cancelDelayedTask(completed);
            return fresh;
        }
    "#,
    );
    let mut host = TestHost::new();
    let mut vm = Vm::new(build.clone(), &mut host, Default::default()).unwrap();
    let completed = vm.start(&mut host, "run", vec![]).unwrap();
    assert_eq!(vm.get_export("total").unwrap(), Value::number(0.0));
    host.advance(&mut vm, 0).unwrap();
    assert_eq!(vm.get_export("total").unwrap(), Value::number(1.0));
    let image = super::support::json_round_trip(&vm.snapshot(&[]).unwrap());
    assert!(image.host_tasks.is_empty());
    assert!(image.operations.is_empty());
    let mut host = TestHost::new();
    let mut vm = Vm::restore(image, build, &mut host, Default::default()).unwrap();
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
    use super::support::declared;
    use std::sync::Arc;
    use yield_harness::Timer;

    let clock = Arc::new(yield_harness::FakeClock::default());
    let mut services = yield_vm::Services::default();
    services
        .insert::<yield_harness::timer::FakeClockKey>(Arc::clone(&clock))
        .unwrap();
    let bindings = yield_vm::host_bindings! {
        dts: "tests/dts/chain.d.ts",
        modules: [chain], classes: [], states: [], operations: [Timer],
        services: services,
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
        &declared(include_str!("../dts/chain.d.ts")),
    )
    .unwrap();
    let mut vm = Vm::new(build.clone(), &mut host, Default::default()).unwrap();
    let handle = vm.start(&mut host, "run", vec![]).unwrap();
    let first = vm.pending_operations()[0].token;
    assert_eq!(clock.registrations(), [(first, 10)]);
    let image = super::support::json_round_trip(&vm.snapshot(&[]).unwrap());
    vm = Vm::restore(image, build.clone(), &mut host, Default::default()).unwrap();
    vm.start_recovered_operations().unwrap();
    let first_recovered = vm.pending_operations()[0].token;
    clock.advance(&mut vm, &mut host, 10).unwrap();
    let second = vm.pending_operations()[0].token;
    assert_eq!(clock.registrations(), [(second, 30)]);
    assert_eq!(vm.get_export("count").unwrap(), Value::number(1.0));
    expect_unknown(&mut vm, first_recovered);
    let image = super::support::json_round_trip(&vm.snapshot(&[]).unwrap());
    assert_eq!(Value::number(image.host_tasks[0].id as f64), handle);
    vm = Vm::restore(image, build, &mut host, Default::default()).unwrap();
    vm.start_recovered_operations().unwrap();
    let second_recovered = vm.pending_operations()[0].token;
    vm.start(&mut host, "clear", vec![]).unwrap();
    expect_unknown(&mut vm, second_recovered);
    assert!(clock.registrations().is_empty());
    assert_eq!(vm.get_export("count").unwrap(), Value::number(1.0));
}

#[test]
fn fractional_performance_clock_survives_restore_without_timer_service() {
    use std::time::Duration;

    let build = build(
        r#"
        export function now() { return performance.now(); }
        export function origin() { return performance.timeOrigin; }
        export function unsupported() {
            for (const schedule of [setTimeout, setInterval]) {
                try { schedule(() => {}, 1); return false; }
                catch (error) {
                    if (!(error instanceof DOMException) || error.name !== 'NotSupportedError') {
                        return false;
                    }
                }
            }
            return clearTimeout(123) === undefined && clearInterval(123) === undefined;
        }
    "#,
    );
    let mut host = TestHost::without_timer_service();
    let clock = std::sync::Arc::new(yield_harness::FakeClock::default());
    let mut vm = Vm::new(
        build.clone(),
        &mut host,
        VmOptions {
            test_clock: Some(clock.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        vm.start(&mut host, "now", vec![]).unwrap(),
        Value::number(0.0)
    );
    assert_eq!(
        vm.start(&mut host, "origin", vec![]).unwrap(),
        Value::number(0.0)
    );
    clock.advance_monotonic(Duration::from_micros(250));
    assert_eq!(
        vm.start(&mut host, "now", vec![]).unwrap(),
        Value::number(0.25)
    );
    let image = super::support::json_round_trip(&vm.snapshot(&[]).unwrap());

    let restored_clock = std::sync::Arc::new(yield_harness::FakeClock::default());
    restored_clock.set_time(100_000).unwrap();
    let mut vm = Vm::restore(
        image,
        build,
        &mut host,
        RestoreOptions {
            test_clock: Some(restored_clock.clone()),
            validation: SnapshotValidation::Full,
            fetch_environment: None,
        },
    )
    .unwrap();
    assert_eq!(
        vm.start(&mut host, "now", vec![]).unwrap(),
        Value::number(0.25)
    );
    restored_clock.advance_monotonic(Duration::from_micros(500));
    assert_eq!(
        vm.start(&mut host, "now", vec![]).unwrap(),
        Value::number(0.75)
    );
    assert_eq!(
        vm.start(&mut host, "origin", vec![]).unwrap(),
        Value::number(0.0)
    );
    assert_eq!(
        vm.start(&mut host, "unsupported", vec![]).unwrap(),
        Value::boolean(true)
    );
    assert!(vm.pending_operations().is_empty());
    assert_eq!(restored_clock.arm_count(), 0);
}

#[test]
fn script_timeout_snapshot_cuts_preserve_roots_and_cancellation() {
    use std::time::Duration;
    use yield_vm::snapshot::TimerState;

    let build = build(
        r#"
        export let log = '';
        let id;
        export function schedule() {
            const payload = { text: 'fired' };
            id = setTimeout(function (arg) {
                log += arg.payload === payload && this === globalThis ? arg.payload.text : 'wrong';
            }, 10, { payload });
        }
        export function cancel() { clearInterval(id); }
    "#,
    );
    for cut in ["armed", "rearmed", "queued"] {
        for cancel in [false, true] {
            let mut host = TestHost::new();
            let clock = host.clock().clone();
            let mut vm = Vm::new(
                build.clone(),
                &mut host,
                VmOptions {
                    test_clock: Some(clock.clone()),
                    ..Default::default()
                },
            )
            .unwrap();
            vm.start(&mut host, "schedule", vec![]).unwrap();
            let original = clock.registrations()[0].0;
            clock.advance_monotonic(Duration::from_micros(3_250));
            let mut stale = vec![original];
            if cut == "rearmed" {
                vm.wake_timer(original).unwrap();
                let replacement = clock.registrations()[0].0;
                assert_ne!(replacement, original);
                assert_eq!(clock.arm_count(), 2);
                assert_eq!(clock.registrations()[0].1, 7);
                vm.wake_timer(original).unwrap();
                assert_eq!(clock.registrations()[0].0, replacement);
                stale.push(replacement);
            } else if cut == "queued" {
                clock.advance_monotonic(Duration::from_micros(6_750));
                vm.wake_timer(original).unwrap();
                vm.wake_timer(original).unwrap();
            }
            vm.collect_garbage(&[]).unwrap();
            let source_registrations = clock.registrations();
            let image = super::support::json_round_trip(&vm.snapshot(&[]).unwrap());
            assert_eq!(clock.registrations(), source_registrations);
            assert_eq!(vm.get_export("log").unwrap(), Value::string("".into()));
            assert_eq!(image.script_timers.len(), 1);
            let queued = cut == "queued";
            let expected_arms = if queued { 0 } else { 1 };
            assert_eq!(image.ordinary_tasks.len(), 1 - expected_arms);
            assert_eq!(image.operations.len(), expected_arms);
            let state = image.script_timers.values().next().unwrap().state;
            assert_eq!(state == TimerState::Queued, queued);
            drop(vm);
            assert!(clock.registrations().is_empty());

            let mut host = TestHost::new();
            let clock = host.clock().clone();
            clock.set_time(20_000).unwrap();
            let mut vm = Vm::restore(
                image,
                build.clone(),
                &mut host,
                RestoreOptions {
                    test_clock: Some(clock.clone()),
                    validation: SnapshotValidation::Full,
                    fetch_environment: None,
                },
            )
            .unwrap();
            assert_eq!(clock.arm_count(), 0);
            vm.start_recovered_operations().unwrap();
            vm.start_recovered_operations().unwrap();
            assert_eq!(clock.arm_count(), expected_arms);
            for token in stale {
                vm.wake_timer(token).unwrap();
                vm.wake_timer(token).unwrap();
            }
            vm.collect_garbage(&[]).unwrap();
            let current = clock.registrations().first().copied();
            if let Some((_, deadline)) = current {
                assert_eq!(deadline, 20_007);
            }
            if cancel {
                vm.start(&mut host, "cancel", vec![]).unwrap();
                vm.collect_garbage(&[]).unwrap();
            }
            let armed_count = clock.arm_count();
            if !queued {
                host.advance(&mut vm, 6).unwrap();
                assert_eq!(vm.get_export("log").unwrap(), Value::string("".into()));
                host.advance(&mut vm, 1).unwrap();
            }
            if let Some((token, _)) = current {
                vm.wake_timer(token).unwrap();
                vm.wake_timer(token).unwrap();
            }
            vm.run_until_idle(&mut host).unwrap();
            assert_eq!(clock.arm_count(), armed_count);
            let expected = if cancel { "" } else { "fired" };
            assert_eq!(
                vm.get_export("log").unwrap(),
                Value::string(expected.into())
            );
            let done = vm.snapshot(&[]).unwrap();
            assert!(done.script_timers.is_empty());
            assert!(done.ordinary_tasks.is_empty());
            assert!(done.operations.is_empty());
            assert!(clock.registrations().is_empty());
        }
    }
}

#[test]
fn reversed_script_timer_wakes_keep_fifo_and_microtask_order() {
    use std::time::Duration;

    let build = build(
        r#"
        export let log = '';
        export function schedule(cancel) {
            let second;
            setTimeout(() => {
                log += 'A';
                queueMicrotask(() => { log += 'M'; });
                if (cancel) clearInterval(second);
            }, 10);
            second = setTimeout(() => { log += 'B'; }, 10);
        }
    "#,
    );
    for cancel in [false, true] {
        let mut host = TestHost::new();
        let clock = host.clock().clone();
        let mut vm = Vm::new(
            build.clone(),
            &mut host,
            VmOptions {
                test_clock: Some(clock.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        vm.start(&mut host, "schedule", vec![JsValue::boolean(cancel)])
            .unwrap();
        let registrations = clock.registrations();
        assert_eq!(registrations.len(), 2);
        clock.advance_monotonic(Duration::from_millis(10));
        for &(token, _) in registrations.iter().rev() {
            vm.wake_timer(token).unwrap();
            vm.wake_timer(token).unwrap();
        }
        assert!(clock.registrations().is_empty());
        assert_eq!(vm.get_export("log").unwrap(), Value::string("".into()));
        assert_eq!(vm.snapshot(&[]).unwrap().ordinary_tasks.len(), 2);
        vm.run_until_idle(&mut host).unwrap();
        let expected = if cancel { "AM" } else { "AMB" };
        assert_eq!(
            vm.get_export("log").unwrap(),
            Value::string(expected.into())
        );
        assert!(vm.snapshot(&[]).unwrap().script_timers.is_empty());
    }
}

#[test]
fn corrupt_script_timer_sequence_and_target_fail_before_arming() {
    use yield_vm::snapshot::TimerTarget;

    let build = build(
        r#"
        export let calls = 0;
        export function schedule() { setTimeout(() => { ++calls; }, 10); }
        export function repeat() { setInterval(() => { ++calls; }, 10); }
    "#,
    );
    let mut source = TestHost::new();
    let clock = source.clock().clone();
    let mut vm = Vm::new(
        build.clone(),
        &mut source,
        VmOptions {
            test_clock: Some(clock),
            ..Default::default()
        },
    )
    .unwrap();
    vm.start(&mut source, "schedule", vec![]).unwrap();
    let image = vm.snapshot(&[]).unwrap();
    for corrupt_sequence in [false, true] {
        let mut bad = image.clone();
        let DeliveryTarget::Timeout {
            target: TimerTarget::Script(key),
            sequence,
            ..
        } = &mut bad.operations[0].target
        else {
            panic!("expected a script timeout operation");
        };
        if corrupt_sequence {
            key.sequence = *sequence + 1;
        } else {
            key.id += 1;
        }
        let mut host = TestHost::new();
        let clock = host.clock().clone();
        let restored = Vm::restore(
            bad,
            build.clone(),
            &mut host,
            RestoreOptions {
                test_clock: Some(clock.clone()),
                validation: SnapshotValidation::Full,
                fetch_environment: None,
            },
        );
        assert!(matches!(restored, Err(VmError::InvalidSnapshot(_))));
        assert_eq!(clock.arm_count(), 0);
        assert!(clock.registrations().is_empty());
    }

    let mut host = TestHost::without_timer_service();
    assert!(matches!(
        Vm::restore(image, build.clone(), &mut host, Default::default()),
        Err(VmError::InvalidSnapshot(message)) if message.contains("wintertc.timer")
    ));

    let token = source.clock().registrations()[0].0;
    source
        .clock()
        .advance_monotonic(std::time::Duration::from_millis(10));
    vm.wake_timer(token).unwrap();
    let queued = vm.snapshot(&[]).unwrap();
    let mut restored = Vm::restore(
        queued,
        build.clone(),
        &mut host,
        RestoreOptions {
            validation: SnapshotValidation::Full,
            ..Default::default()
        },
    )
    .unwrap();
    restored.start_recovered_operations().unwrap();
    restored.run_until_idle(&mut host).unwrap();
    assert_eq!(restored.get_export("calls").unwrap(), Value::number(1.0));
    assert_eq!(host.clock().arm_count(), 0);

    vm.run_until_idle(&mut source).unwrap();
    vm.start(&mut source, "repeat", vec![]).unwrap();
    let token = source.clock().registrations()[0].0;
    source
        .clock()
        .advance_monotonic(std::time::Duration::from_millis(10));
    vm.wake_timer(token).unwrap();
    let queued = vm.snapshot(&[]).unwrap();
    assert!(matches!(
        Vm::restore(queued, build, &mut host, Default::default()),
        Err(VmError::InvalidSnapshot(message)) if message.contains("wintertc.timer")
    ));
}

#[test]
fn restored_interval_does_not_catch_up_and_can_cancel_itself_after_throwing() {
    use std::time::Duration;

    let build = build(
        r#"
        export let log = '';
        export function schedule() {
            let count = 0;
            globalThis.addEventListener('error', event => {
                log += event.error === 'boom' ? 'E' : '?';
                event.preventDefault();
            });
            const id = setInterval(() => {
                log += 'I';
                if (++count === 1) throw 'boom';
                clearTimeout(id);
            }, 10);
            setTimeout(() => {
                log += 'T';
                queueMicrotask(() => { log += 'M'; });
            }, 10);
        }
    "#,
    );
    let mut host = TestHost::new();
    let clock = host.clock().clone();
    let mut vm = Vm::new(
        build.clone(),
        &mut host,
        VmOptions {
            test_clock: Some(clock.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    vm.start(&mut host, "schedule", vec![]).unwrap();
    let stale = clock.registrations()[0].0;
    clock.advance_monotonic(Duration::from_secs(1));
    let image = super::support::json_round_trip(&vm.snapshot(&[]).unwrap());

    let mut host = TestHost::new();
    let clock = host.clock().clone();
    clock.set_time(90_000).unwrap();
    let mut vm = Vm::restore(
        image,
        build,
        &mut host,
        RestoreOptions {
            test_clock: Some(clock.clone()),
            validation: SnapshotValidation::Full,
            fetch_environment: None,
        },
    )
    .unwrap();
    vm.start_recovered_operations().unwrap();
    vm.start_recovered_operations().unwrap();
    vm.wake_timer(stale).unwrap();
    assert_eq!(clock.arm_count(), 0);
    vm.run_until_idle(&mut host).unwrap();
    assert_eq!(vm.get_export("log").unwrap(), Value::string("IETM".into()));
    assert_eq!(clock.arm_count(), 1);
    assert_eq!(clock.registrations()[0].1, 90_010);
    host.advance(&mut vm, 9).unwrap();
    assert_eq!(vm.get_export("log").unwrap(), Value::string("IETM".into()));
    host.advance(&mut vm, 1).unwrap();
    host.advance(&mut vm, 100).unwrap();
    assert_eq!(vm.get_export("log").unwrap(), Value::string("IETMI".into()));
    assert_eq!(clock.arm_count(), 1);
    assert!(clock.registrations().is_empty());
    assert!(vm.snapshot(&[]).unwrap().script_timers.is_empty());
}

#[test]
fn interval_rearm_fault_discards_registration_without_a_second_callback() {
    use std::time::Duration;

    let build = build(
        r#"
        export let count = 0;
        export function schedule() { setInterval(() => { ++count; }, 10); }
    "#,
    );
    let mut host = TestHost::new();
    let clock = host.clock().clone();
    let mut vm = Vm::new(
        build,
        &mut host,
        VmOptions {
            test_clock: Some(clock.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    vm.start(&mut host, "schedule", vec![]).unwrap();
    let token = clock.registrations()[0].0;
    clock.advance_monotonic(Duration::from_millis(10));
    vm.wake_timer(token).unwrap();
    clock.set_fail_start(true);
    assert!(matches!(
        vm.run_until_idle(&mut host),
        Err(VmError::HostFault(_))
    ));
    assert!(clock.registrations().is_empty());
    let image = vm.snapshot(&[]).unwrap();
    assert!(image.script_timers.is_empty());
    assert!(image.operations.is_empty());
    clock.set_fail_start(false);
    vm.wake_timer(token).unwrap();
    vm.run_until_idle(&mut host).unwrap();
    assert_eq!(vm.get_export("count").unwrap(), Value::number(1.0));
    assert_eq!(clock.arm_count(), 1);
}
