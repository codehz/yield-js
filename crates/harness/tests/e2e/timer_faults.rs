use std::sync::Arc;

use super::support::{compile_main, declared, json_round_trip};
use yield_harness::{FakeClock, TestHost, Timer, build};
use yield_vm::binding::{CallCx, NativeTarget};
use yield_vm::snapshot::DeliveryTarget;
use yield_vm::{
    Bindings, Host, JsResult, JsValue, PromiseState, RestoreOptions, Snapshot, SnapshotValidation,
    Value, Vm, VmError, host_bindings, host_module,
};

const TIMERS: &str = r#"
    import { spawnDelayedTask, sleep } from 'test';
    export let calls = 0;
    export function schedule() { spawnDelayedTask(() => { calls += 1; }, 10); }
    export function wait() { return sleep(10); }
"#;

#[test]
fn malformed_timer_snapshots_never_arm_a_timer() {
    let build = build(TIMERS);
    let mut source = TestHost::new();
    let mut vm = Vm::new(build.clone(), &mut source, Default::default()).unwrap();
    vm.start(&mut source, "schedule", vec![]).unwrap();
    vm.start(&mut source, "schedule", vec![]).unwrap();
    let promise = vm.start(&mut source, "wait", vec![]).unwrap();
    let snapshot = json_round_trip(&vm.snapshot(&[promise]).unwrap());
    let [first, second] = snapshot.host_tasks.as_slice() else {
        panic!("expected two waiting timer callbacks");
    };
    let first_id = first.id;
    let first_op = snapshot
        .operations
        .iter()
        .position(|op| op.id == first.waiting_on)
        .unwrap();
    let second_op = snapshot
        .operations
        .iter()
        .position(|op| op.id == second.waiting_on)
        .unwrap();
    let promise_op = snapshot
        .operations
        .iter()
        .position(|op| matches!(op.target, DeliveryTarget::Promise(_)))
        .unwrap();
    let bindings = source.bindings();
    let sleep_owner = NativeTarget::Export(bindings.export("test", "sleep").unwrap().0);
    drop(vm);

    let mut target = TestHost::new();
    // The same image, unmodified, must get through validation without registering.
    drop(
        Vm::restore(
            snapshot.clone(),
            build.clone(),
            &mut target,
            Default::default(),
        )
        .unwrap(),
    );
    let assert_rejected = |label: &str, message: &str, error: VmError, target: &TestHost| {
        assert!(
            matches!(&error, VmError::InvalidSnapshot(detail) if detail.contains(message)),
            "{label}: {error}"
        );
        assert_eq!(target.clock().arm_count(), 0, "{label}");
        assert!(target.clock().registrations().is_empty(), "{label}");
        assert!(target.errors().is_empty(), "{label}");
    };

    for length in [3, 5] {
        let mut image = snapshot.clone();
        image.operations[first_op].state.resize(length, 0);
        let error = Vm::restore(image, build.clone(), &mut target, Default::default())
            .expect_err("timer codec length");
        assert_rejected(
            "timer codec length",
            "timer state must be four bytes",
            error,
            &target,
        );
    }
    let mut reject = |label: &str, message: &str, image: Snapshot| {
        let error = Vm::restore(
            image,
            build.clone(),
            &mut target,
            RestoreOptions {
                validation: SnapshotValidation::Full,
                ..Default::default()
            },
        )
        .expect_err(label);
        assert_rejected(label, message, error, &target);
    };
    let mut image = snapshot.clone();
    image.host_tasks[0].owner = sleep_owner;
    reject("owner has no CPS layout", "stage is not an external", image);

    let mut image = snapshot.clone();
    // spawnDelayedTask awaits the external timer first, then calls the guest callback.
    image.host_tasks[0].stage = 1;
    reject("guest call stage", "stage is not an external", image);

    let mut image = snapshot.clone();
    image.host_tasks[0].saved.clear();
    reject("missing saved callback", "saved value count", image);

    for (value, message) in [
        (Value::object(u32::MAX), "missing object"),
        (Value::symbol(u32::MAX), "missing symbol"),
    ] {
        let mut image = snapshot.clone();
        image.host_tasks[0].saved[0] = value.clone();
        reject("dangling saved value", message, image);
        let mut image = snapshot.clone();
        image.host_tasks[0].receiver = value;
        reject("dangling receiver", message, image);
    }

    let mut image = snapshot.clone();
    image.host_tasks[0].waiting_on = image.operations[promise_op].id;
    reject(
        "task points at Promise operation",
        "does not target this task",
        image,
    );

    let mut image = snapshot.clone();
    let mut duplicate = image.operations[second_op].clone();
    duplicate.id = image.operations.iter().map(|op| op.id).max().unwrap() + 1;
    duplicate.target = DeliveryTarget::Task(first_id);
    image.operations.push(duplicate);
    reject(
        "two operations target one task",
        "does not wait on it",
        image,
    );

    for (high_water, message) in [
        (0, "invalid next host task id"),
        (second.id, "high-water mark"),
        ((1_u64 << 53) + 1, "invalid next host task id"),
    ] {
        let mut image = snapshot.clone();
        image.next_host_task_id = high_water;
        reject("invalid handle high-water mark", message, image);
    }
}

#[test]
fn timer_start_fault_leaves_no_pending_work() {
    let mut host = TestHost::new();
    let mut vm = Vm::new(build(TIMERS), &mut host, Default::default()).unwrap();
    host.clock().set_fail_start(true);
    for entry in ["schedule", "wait"] {
        let error = vm.start(&mut host, entry, vec![]).unwrap_err();
        assert!(matches!(&error, VmError::HostFault(_)), "{entry}: {error}");
        assert!(host.clock().registrations().is_empty());
        assert_eq!(host.clock().arm_count(), 0);
        let snapshot = vm.snapshot(&[]).unwrap();
        assert!(snapshot.host_tasks.is_empty());
        assert!(snapshot.operations.is_empty());
    }
    host.advance(&mut vm, 10).unwrap();
    assert_eq!(vm.get_export("calls").unwrap(), Value::number(0.0));
    assert!(host.errors().is_empty());
}

#[test]
fn timer_checkpoint_fault_does_not_disarm_the_source() {
    let mut host = TestHost::new();
    let mut vm = Vm::new(build(TIMERS), &mut host, Default::default()).unwrap();
    vm.start(&mut host, "schedule", vec![]).unwrap();
    let registrations = host.clock().registrations();
    assert_eq!(registrations.len(), 1);
    host.clock().set_fail_checkpoint(true);
    assert!(matches!(vm.snapshot(&[]), Err(VmError::HostFault(_))));
    assert_eq!(host.clock().registrations(), registrations);
    assert_eq!(host.clock().arm_count(), 1);

    host.advance(&mut vm, 9).unwrap();
    assert_eq!(vm.get_export("calls").unwrap(), Value::number(0.0));
    host.advance(&mut vm, 1).unwrap();
    assert_eq!(vm.get_export("calls").unwrap(), Value::number(1.0));
    assert!(host.errors().is_empty());
    assert!(host.clock().registrations().is_empty());
    let snapshot = vm.snapshot(&[]).unwrap();
    assert!(snapshot.host_tasks.is_empty());
    assert!(snapshot.operations.is_empty());
}

#[test]
fn timer_resume_fault_rejects_the_promise_and_reports_the_callback_failure() {
    let build = build(TIMERS);
    let mut source = TestHost::new();
    let mut vm = Vm::new(build.clone(), &mut source, Default::default()).unwrap();
    vm.start(&mut source, "schedule", vec![]).unwrap();
    let promise = vm.start(&mut source, "wait", vec![]).unwrap();
    let snapshot = json_round_trip(&vm.snapshot(std::slice::from_ref(&promise)).unwrap());
    drop(vm);

    let mut host = TestHost::new();
    host.clock().set_fail_resume(true);
    let mut vm = Vm::restore(snapshot, build, &mut host, Default::default()).unwrap();
    assert_eq!(vm.promise_state(&promise).unwrap(), PromiseState::Pending);
    vm.start_recovered_operations().unwrap();
    assert!(
        host.errors().is_empty(),
        "delivery must not run the callback inline"
    );
    vm.run_until_idle(&mut host).unwrap();
    assert_eq!(
        vm.promise_state(&promise).unwrap(),
        PromiseState::Rejected(Value::string("timer unavailable".into())),
    );
    vm.start_recovered_operations().unwrap();
    host.advance(&mut vm, 10).unwrap();
    assert_eq!(vm.get_export("calls").unwrap(), Value::number(0.0));
    let [error] = host.errors() else {
        panic!("expected exactly one background error");
    };
    assert_eq!(
        error.as_string().unwrap().to_utf8().unwrap(),
        "timer unavailable"
    );
    assert_eq!(host.clock().arm_count(), 0);
    assert!(host.clock().registrations().is_empty());
    let snapshot = vm.snapshot(&[]).unwrap();
    assert!(snapshot.host_tasks.is_empty());
    assert!(snapshot.operations.is_empty());
}

#[test]
fn a_throwing_timer_callback_does_not_block_the_next_timer() {
    let build = build(
        r#"
        import { spawnDelayedTask } from 'test';
        export let calls = 0;
        export function run() {
            spawnDelayedTask(() => { throw 'callback failed'; }, 1);
            spawnDelayedTask(() => { calls += 1; }, 2);
        }
    "#,
    );
    let mut host = TestHost::new();
    let mut vm = Vm::new(build, &mut host, Default::default()).unwrap();
    vm.start(&mut host, "run", vec![]).unwrap();
    host.advance(&mut vm, 2).unwrap();
    let [error] = host.errors() else {
        panic!("expected exactly one callback error");
    };
    assert_eq!(
        error.as_string().unwrap().to_utf8().unwrap(),
        "callback failed"
    );
    assert_eq!(vm.get_export("calls").unwrap(), Value::number(1.0));
    assert!(host.clock().registrations().is_empty());
    let snapshot = vm.snapshot(&[]).unwrap();
    assert!(snapshot.host_tasks.is_empty());
    assert!(snapshot.operations.is_empty());
}

// All variants have the same wire exports, services and operation. Only
// `extra_slot` reads the second named argument after the external boundary.
macro_rules! timer_contract {
    ($name:ident, $extra:ident, ($($before:tt)*), ($($after:tt)*), $($argv:tt)+) => {
        mod $name {
            use super::*;

            #[host_module(name = "contract")]
            mod api {
                use super::*;

                $($before)*

                #[function(name = "schedule")]
                fn schedule(cx: &mut CallCx<'_>, cb: JsValue, $extra: JsValue) -> JsResult<JsValue> {
                    let timer = Timer::from_delay(cx, &JsValue::number(10.0))?;
                    cx.spawn(async move {
                        cx.effect(timer).await?;
                        cx.call(cb, JsValue::undefined(), $($argv)+).await?;
                        Ok(())
                    })
                }

                $($after)*
            }

            pub(super) fn bindings(clock: &Arc<FakeClock>) -> Bindings {
                let mut services = yield_vm::Services::default();
                services.insert::<yield_harness::timer::FakeClockKey>(Arc::clone(clock)).unwrap();
                host_bindings! {
                    dts: "tests/dts/contract.d.ts",
                    modules: [api],
                    classes: [],
                    states: [],
                    operations: [Timer],
                    services: services,
                }.unwrap()
            }
        }
    };
}

timer_contract!(
    original, _extra, (),
    (#[function(name = "other")] fn other(_cx: &mut CallCx<'_>) -> JsResult<()> { Ok(()) }),
    Vec::new()
);
timer_contract!(
    reordered, _extra,
    (#[function(name = "other")] fn other(_cx: &mut CallCx<'_>) -> JsResult<()> { Ok(()) }),
    (), Vec::new()
);
timer_contract!(
    extra_slot, extra, (),
    (#[function(name = "other")] fn other(_cx: &mut CallCx<'_>) -> JsResult<()> { Ok(()) }),
    Vec::from([extra])
);

struct ContractHost(Arc<Bindings>);

impl Host for ContractHost {
    fn bindings(&self) -> Arc<Bindings> {
        Arc::clone(&self.0)
    }

    fn report_error(&mut self, error: JsValue) {
        panic!("unexpected callback error: {error}");
    }
}

#[test]
fn timer_slot_layout_changes_the_fingerprint_but_declaration_order_does_not() {
    let source_clock = Arc::new(FakeClock::default());
    let mut source = ContractHost(Arc::new(original::bindings(&source_clock)));
    let build = compile_main(
        r#"
        import { schedule } from 'contract';
        export let calls = 0;
        export function run() { schedule(() => { calls += 1; }, 42); }
        "#,
        &declared(include_str!("../dts/contract.d.ts")),
    )
    .unwrap();
    let mut vm = Vm::new(build.clone(), &mut source, Default::default()).unwrap();
    vm.start(&mut source, "run", vec![]).unwrap();
    let snapshot = json_round_trip(&vm.snapshot(&[]).unwrap());
    assert_eq!(snapshot.host_tasks[0].saved.len(), 1);
    drop(vm);

    let changed_clock = Arc::new(FakeClock::default());
    let mut changed = ContractHost(Arc::new(extra_slot::bindings(&changed_clock)));
    // Establish that the alternate binding really saves the extra argument.
    let mut alternate = Vm::new(build.clone(), &mut changed, Default::default()).unwrap();
    alternate.start(&mut changed, "run", vec![]).unwrap();
    let alternate_image = alternate.snapshot(&[]).unwrap();
    assert_eq!(alternate_image.host_tasks[0].saved.len(), 2);
    assert_eq!(alternate_image.host_tasks[0].saved[1], Value::number(42.0));
    drop(alternate);

    let rejected_clock = Arc::new(FakeClock::default());
    let mut rejected = ContractHost(Arc::new(extra_slot::bindings(&rejected_clock)));
    let error = Vm::restore(
        snapshot.clone(),
        build.clone(),
        &mut rejected,
        Default::default(),
    )
    .unwrap_err();
    assert!(
        matches!(&error, VmError::InvalidSnapshot(message) if message.contains("binding fingerprint mismatch")),
        "{error}"
    );
    assert_eq!(rejected_clock.arm_count(), 0);
    assert!(rejected_clock.registrations().is_empty());

    let restored_clock = Arc::new(FakeClock::default());
    let mut reordered = ContractHost(Arc::new(reordered::bindings(&restored_clock)));
    assert_eq!(reordered.0.fingerprint(), snapshot.binding_fingerprint);
    let mut restored = Vm::restore(snapshot, build, &mut reordered, Default::default()).unwrap();
    assert!(restored_clock.registrations().is_empty());
    restored.start_recovered_operations().unwrap();
    assert_eq!(restored_clock.arm_count(), 1);
    assert_eq!(restored_clock.registrations().len(), 1);
    restored_clock
        .advance(&mut restored, &mut reordered, 10)
        .unwrap();
    assert!(restored_clock.registrations().is_empty());
    assert_eq!(restored.get_export("calls").unwrap(), Value::number(1.0));
}
