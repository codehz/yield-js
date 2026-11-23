//! Timer delivery roots across GC, snapshots, and queued callbacks.

use yield_harness::{TestHost, build};
use yield_vm::snapshot::DeliveryTarget;
use yield_vm::{Completion, JsValue, Object, Snapshot, Value, Vm};

#[test]
fn waiting_timer_and_unobserved_sleep_chain_root_their_own_objects() {
    let program = build(
        r#"
        import { cancelDelayedTask, spawnDelayedTask, sleep } from 'test';
        export let callbackResult = 0;
        export let reactionResult = 0;
        let handle;
        function scheduleCallback() {
            const box = { n: 40 };
            handle = spawnDelayedTask(() => { callbackResult = box.n; }, 100);
        }
        function scheduleReaction() {
            const box = { n: 2 };
            sleep(100).then(() => { reactionResult = box.n; });
        }
        export function run() {
            scheduleCallback();
            scheduleReaction();
        }
        export function cancel() { cancelDelayedTask(handle); }
    "#,
    );
    let image = {
        let mut host = TestHost::new();
        let mut vm = Vm::new(program.clone(), &mut host, Default::default()).unwrap();
        assert_eq!(
            vm.start(&mut host, "run", vec![]).unwrap(),
            Value::UNDEFINED
        );
        vm.collect_garbage(&[]).unwrap();
        vm.snapshot(&[]).unwrap().to_json().unwrap()
    };

    for cancel in [false, true] {
        let snapshot = Snapshot::from_json(&image).unwrap();
        let [task] = snapshot.host_tasks.as_slice() else {
            panic!("expected one waiting callback task");
        };
        let [callback] = task.saved.as_slice() else {
            panic!("expected the callback as the task's only saved slot");
        };
        let [callback_op, sleep_op] = snapshot.operations.as_slice() else {
            panic!("expected one callback timer and one sleep timer");
        };
        assert_eq!(callback_op.id, task.waiting_on);
        assert_eq!(callback_op.target, DeliveryTarget::Task(task.id));
        let DeliveryTarget::Promise(sleep_id) = sleep_op.target else {
            panic!("sleep must target a Promise directly");
        };
        let yield_vm::ValueRef::Object(callback_id) = callback.view() else {
            panic!("expected a callback object");
        };
        assert!(
            snapshot
                .objects
                .iter()
                .any(|record| record.id == callback_id)
        );
        assert!(snapshot.objects.iter().any(|record| {
            record.id == sleep_id && matches!(&record.object, Object::Promise(_))
        }));

        let mut host = TestHost::new();
        let mut vm = Vm::restore(snapshot, program.clone(), &mut host, Default::default()).unwrap();
        assert!(host.clock().registrations().is_empty());
        vm.collect_garbage(&[]).unwrap();
        let recaptured = vm.snapshot(&[]).unwrap().to_json().unwrap();
        let mut vm = Vm::restore(
            Snapshot::from_json(&recaptured).unwrap(),
            program.clone(),
            &mut host,
            Default::default(),
        )
        .unwrap();
        assert_eq!(host.clock().arm_count(), 0);
        vm.start_recovered_operations().unwrap();
        if cancel {
            vm.start(&mut host, "cancel", vec![]).unwrap();
            assert_eq!(host.clock().registrations().len(), 1);
        }
        host.advance(&mut vm, 100).unwrap();
        assert_eq!(
            vm.get_export("callbackResult").unwrap(),
            Value::number(if cancel { 0.0 } else { 40.0 }),
        );
        assert_eq!(vm.get_export("reactionResult").unwrap(), Value::number(2.0));
        assert!(host.errors().is_empty());
        assert!(host.clock().registrations().is_empty());

        vm.collect_garbage(&[]).unwrap();
        let settled = vm.snapshot(&[]).unwrap();
        assert!(settled.host_tasks.is_empty());
        assert!(settled.operations.is_empty());
        assert!(
            settled
                .objects
                .iter()
                .all(|record| record.id != callback_id)
        );
        assert!(settled.objects.iter().all(|record| record.id != sleep_id));
    }
}

#[test]
fn callback_gc_keeps_ready_tasks_and_nested_guest_frames_alive() {
    let mut host = TestHost::new();
    let mut vm = Vm::new(
        build(
            r#"
            import { gc, spawnDelayedTask } from 'test';
            export let result = 0;
            function collectThroughGetter() {
                return ({ get value() { return gc(); } }).value;
            }
            function nested() {
                const local = { n: 2 };
                collectThroughGetter();
                return local.n;
            }
            function scheduleFirst() {
                const box = { n: 40 };
                spawnDelayedTask(() => {
                    gc();
                    const extra = nested();
                    result = box.n + extra;
                }, 100);
            }
            function scheduleSecond() {
                const box = { n: 7 };
                spawnDelayedTask(() => { result = 10 * result + box.n; }, 100);
            }
            export function run() {
                scheduleFirst();
                scheduleSecond();
            }
        "#,
        ),
        &mut host,
        Default::default(),
    )
    .unwrap();
    vm.start(&mut host, "run", vec![]).unwrap();

    // Complete both operations without draining the queue. A clock advance
    // would run the first callback before the second task became ready.
    let registrations = host.clock().registrations();
    let [(first, _), (second, _)] = registrations.as_slice() else {
        panic!("expected two timer registrations");
    };
    for token in [*first, *second] {
        vm.complete_operation(token, Completion::Fulfilled(JsValue::undefined()))
            .unwrap();
    }
    assert!(host.clock().registrations().is_empty());
    assert_eq!(vm.get_export("result").unwrap(), Value::number(0.0));
    vm.run_until_idle(&mut host).unwrap();
    assert_eq!(vm.get_export("result").unwrap(), Value::number(427.0));
    assert!(host.errors().is_empty());
    let idle = vm.snapshot(&[]).unwrap();
    assert!(idle.host_tasks.is_empty());
    assert!(idle.operations.is_empty());
}

#[yield_vm::host_module(name = "repeat_timer")]
struct RepeatTimer;

#[yield_vm::module_exports]
impl RepeatTimer {
    #[function(name = "start")]
    fn start(
        cx: &mut yield_vm::ModuleCx<'_, Self>,
        callback: JsValue,
    ) -> yield_vm::JsResult<JsValue> {
        cx.spawn(async move {
            cx.effect(yield_harness::Timer::from_delay(
                cx,
                &JsValue::number(10.0),
            )?)
            .await?;
            cx.call(callback, JsValue::undefined(), Vec::new()).await?;
            cx.effect(yield_harness::Timer::from_delay(
                cx,
                &JsValue::number(20.0),
            )?)
            .await?;
            Ok(())
        })
    }

    #[function(name = "cancelDelayedTask")]
    fn cancel_delayed_task(
        cx: &mut yield_vm::ModuleCx<'_, Self>,
        handle: JsValue,
    ) -> yield_vm::JsResult<()> {
        cx.cancel_task(&handle)
    }
}

#[test]
fn running_callback_can_clear_itself_and_prevent_the_next_timer_arm() {
    use super::support::{compile_main, declared};
    use std::sync::Arc;
    use yield_harness::{FakeClock, Timer};
    use yield_vm::{Bindings, Host};

    struct RepeatHost(Arc<Bindings>);
    impl Host for RepeatHost {
        fn bindings(&self) -> Arc<Bindings> {
            Arc::clone(&self.0)
        }

        fn report_error(&mut self, error: JsValue) {
            panic!("unexpected callback error: {error}");
        }
    }

    let clock = Arc::new(FakeClock::default());
    let bindings = yield_vm::host_bindings! {
        dts: "../dts/repeat_timer.d.ts",
        modules: [RepeatTimer], classes: [], operations: [Timer],
        services: {
            yield_harness::timer::FakeClockKey => Arc::clone(&clock),
        },
    }
    .unwrap();
    let mut host = RepeatHost(Arc::new(bindings));
    let program = compile_main(
        r#"
        import { start, cancelDelayedTask } from 'repeat_timer';
        export let result = 0;
        export function run() {
            const handle = start(() => {
                result = 1;
                cancelDelayedTask(handle);
                result = 2;
            });
        }
    "#,
        &declared(include_str!("../dts/repeat_timer.d.ts")),
    )
    .unwrap();
    let mut vm = Vm::new(program, &mut host, Default::default()).unwrap();
    vm.start(&mut host, "run", vec![]).unwrap();
    let registrations = clock.registrations();
    let [(token, 10)] = registrations.as_slice() else {
        panic!("expected one initial timer registration");
    };
    vm.complete_operation(*token, Completion::Fulfilled(JsValue::undefined()))
        .unwrap();
    vm.run_until_idle(&mut host).unwrap();
    assert_eq!(vm.get_export("result").unwrap(), Value::number(2.0));
    assert_eq!(clock.arm_count(), 1);
    assert!(clock.registrations().is_empty());
    let idle = vm.snapshot(&[]).unwrap();
    assert!(idle.host_tasks.is_empty());
    assert!(idle.operations.is_empty());
}
