use std::sync::Arc;

use super::support::{compile_main, declared};
use yield_vm::binding::CallCx;
use yield_vm::{
    Bindings, Host, Instance, JsResult, JsValue, RestoreOptions, SnapshotValidation, This, Value,
    Vm, host_bindings, host_class, host_module, methods,
};

#[test]
fn abort_gc_keeps_only_observable_dependents() {
    let source = r#"
        const controller = new AbortController();
        let observed = 0;
        export function setup() {
            AbortSignal.any([controller.signal]);
            AbortSignal.any([controller.signal]).onabort = () => { ++observed; };
            makeCycle();
        }
        function makeCycle() {
            const target = new EventTarget();
            target.addEventListener("cycle", () => target);
        }
        export function run() {
            controller.abort();
            return observed;
        }
    "#;
    let build = compile_main(source, &declared(include_str!("../dts/event-fixture.d.ts"))).unwrap();
    let mut host = EventHost::new();
    let mut vm = Vm::new(build.clone(), &mut host, Default::default()).unwrap();
    vm.start(&mut host, "setup", vec![]).unwrap();
    assert!(vm.collect_garbage(&[]).unwrap() > 0);
    let image = vm.snapshot(&[]).unwrap();
    let dependent_count = image.objects.iter().filter(|record| {
        matches!(&record.object, yield_vm::Object::AbortSignal { signal, .. } if signal.dependent)
    }).count();
    assert_eq!(dependent_count, 1);
    assert!(!image.objects.iter().any(|record| {
        matches!(&record.object, yield_vm::Object::EventTarget { target, .. } if !target.listeners.is_empty())
    }));
    let mut restored = Vm::restore(
        image,
        build,
        &mut host,
        RestoreOptions {
            validation: SnapshotValidation::Full,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        restored.start(&mut host, "run", vec![]).unwrap(),
        Value::number(1.0)
    );
}

#[host_class(tag = "EventNode", methods, event_parent = parent)]
struct EventNode {
    #[js_slot]
    parent: JsValue,
}

#[methods(EventNode)]
impl EventNode {
    #[method(name = "setParent")]
    fn set_parent(cx: &mut CallCx<'_>, node: This<EventNode>, parent: JsValue) -> JsResult<()> {
        node.instance().set_parent(cx, parent)
    }
}

#[host_module(name = "event-fixture")]
mod fixture {
    use super::*;

    #[function(name = "node")]
    fn node(cx: &mut CallCx<'_>, parent: JsValue) -> JsResult<Instance<EventNode>> {
        let node = cx.alloc(EventNode)?;
        node.set_parent(cx, parent)?;
        Ok(node)
    }

    #[function(name = "dispatch")]
    async fn dispatch(
        cx: &mut CallCx<'_>,
        node: JsValue,
        detail: JsValue,
        bubbles: JsValue,
    ) -> JsResult<yield_vm::HostStep> {
        let bubbles = bubbles.as_bool().unwrap_or(false);
        let event = cx.custom_event("probe", detail, bubbles, true)?;
        let result = cx.dispatch_event(node, event).await?;
        Ok(yield_vm::HostStep::Return(result))
    }
}

struct EventHost(Arc<Bindings>);

impl EventHost {
    fn new() -> Self {
        Self(Arc::new(
            host_bindings! {
                dts: "tests/dts/event-fixture.d.ts",
                modules: [fixture], classes: [EventNode],
                states: [], operations: [], services: [],
            }
            .unwrap(),
        ))
    }
}

impl Host for EventHost {
    fn bindings(&self) -> Arc<Bindings> {
        self.0.clone()
    }
    fn now_ms(&self) -> i64 {
        0
    }
    fn time_zone(&self) -> &str {
        "Etc/UTC"
    }
    fn report_error(&mut self, error: JsValue) {
        panic!("unexpected listener throw: {error:?}")
    }
}

#[test]
fn missing_timer_service_only_disables_timeout() {
    let source = r#"
        export function run() {
            const controller = new AbortController();
            controller.abort(null);
            try { AbortSignal.timeout(0); }
            catch (error) {
                return controller.signal.aborted && controller.signal.reason === null &&
                    error instanceof DOMException && error.name === "NotSupportedError";
            }
            return false;
        }
    "#;
    let build = compile_main(source, &declared(include_str!("../dts/event-fixture.d.ts"))).unwrap();
    let mut host = EventHost::new();
    let mut vm = Vm::new(build, &mut host, Default::default()).unwrap();
    assert_eq!(
        vm.start(&mut host, "run", vec![]).unwrap(),
        Value::boolean(true)
    );
}

#[test]
fn host_events_keep_identity_and_freeze_parent_path_across_callbacks_and_restore() {
    let source = r#"
        import { node, dispatch } from "event-fixture";
        const root = node(null), parent = node(root), leaf = node(parent);
        const detail = { original: leaf };
        const log = [];
        let stable = true, first = true;
        leaf.dispatchEvent = () => { throw "overridden"; };
        for (const [current, label, phase] of [[root, "r", 1], [parent, "p", 1], [leaf, "t", 2]]) {
            current.addEventListener("probe", function(e) {
                stable = stable && this === current && e.currentTarget === current &&
                    e.target === leaf && e.detail === detail && e.isTrusted && e.eventPhase === phase;
                log.push(label + "c");
                if (first && current === parent) leaf.setParent(root);
            }, true);
            current.addEventListener("probe", e => {
                stable = stable && e.eventPhase === (current === leaf ? 2 : 3);
                log.push(label + "b");
                if (current === leaf) e.preventDefault();
            });
        }
        export function run() {
            if (dispatch(leaf, detail, true) !== false) return "not canceled";
            first = false;
            if (dispatch(leaf, detail, false) !== false) return "not canceled";
            if (log.join(",") !== "rc,pc,tc,tb,pb,rb,rc,tc,tb" || !stable) return log.join(",");
            root.setParent(leaf);
            const count = log.length;
            try { dispatch(leaf, detail, true); return "cycle accepted"; }
            catch (error) { if (error.name !== "HierarchyRequestError" || log.length !== count) return "cycle"; }
            root.setParent({});
            try { dispatch(leaf, detail, true); return "bad parent accepted"; }
            catch (error) { return error instanceof TypeError && log.length === count; }
        }
    "#;
    let build = compile_main(source, &declared(include_str!("../dts/event-fixture.d.ts"))).unwrap();
    let mut host = EventHost::new();
    let vm = Vm::new(build.clone(), &mut host, Default::default()).unwrap();
    let image = vm.snapshot(&[]).unwrap();
    let mut new_host = EventHost::new();
    let mut restored = Vm::restore(
        image,
        build,
        &mut new_host,
        RestoreOptions {
            validation: SnapshotValidation::Full,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        restored.start(&mut new_host, "run", vec![]).unwrap(),
        Value::boolean(true)
    );
    restored.run_until_idle(&mut new_host).unwrap();
}

#[test]
fn global_events_and_handler_order_survive_binary_restore() {
    use yield_harness::{TestHost, build};
    use yield_vm::{Object, Snapshot, VmError};

    let build = build(
        r#"
        export let errorEvent, rejectionEvent;
        export let order = '';
        globalThis.addEventListener('error', e => { errorEvent = e; order += 'a'; });
        onerror = () => { order += 'b'; return true; };
        globalThis.addEventListener('error', () => { order += 'c'; });
        onunhandledrejection = e => { rejectionEvent = e; return false; };
        export function makeEvents() {
            reportError({ marker: 'error' });
            return Promise.reject({ marker: 'reason' });
        }
        export function errorValue() { return errorEvent.error; }
        export function promiseValue() { return rejectionEvent.promise; }
        export function reasonValue() { return rejectionEvent.reason; }
        export function dispatchAgain() {
            return globalThis.dispatchEvent(new ErrorEvent('error', { cancelable: true }));
        }
    "#,
    );
    let mut host = TestHost::default();
    let mut vm = Vm::new(build.clone(), &mut host, Default::default()).unwrap();
    let promise = vm.start(&mut host, "makeEvents", vec![]).unwrap();
    vm.run_until_idle(&mut host).unwrap();
    let error = vm.start(&mut host, "errorValue", vec![]).unwrap();
    let reason = vm.start(&mut host, "reasonValue", vec![]).unwrap();
    vm.collect_garbage(&[]).unwrap();
    let image = vm.snapshot(&[]).unwrap();
    let decoded = Snapshot::from_bytes(&image.to_bytes().unwrap()).unwrap();

    let mut invalid_handler = image;
    let global = invalid_handler.roots["globalThis"];
    let global = invalid_handler
        .objects
        .iter_mut()
        .find(|record| record.id == global)
        .unwrap();
    let Object::EventTarget { target, .. } = &mut global.object else {
        panic!("global EventTarget")
    };
    // A regular error listener has the right type and flags but is not the handler.
    target.global_handlers[0] = Some(target.listeners[0].id);
    let mut invalid_payload = decoded.clone();
    let event = invalid_payload
        .objects
        .iter_mut()
        .find_map(|record| match &mut record.object {
            Object::Event { event, .. } => match &mut event.payload {
                yield_vm::heap::EventPayload::PromiseRejectionEvent { promise, .. } => {
                    Some(promise)
                }
                _ => None,
            },
            _ => None,
        })
        .unwrap();
    *event = u32::MAX;
    assert!(matches!(
        Vm::restore(
            invalid_payload,
            build.clone(),
            &mut host,
            RestoreOptions {
                validation: SnapshotValidation::Full,
                ..Default::default()
            },
        ),
        Err(VmError::InvalidSnapshot(_))
    ));
    let mut host = TestHost::default();
    assert!(matches!(
        Vm::restore(
            invalid_handler,
            build.clone(),
            &mut host,
            RestoreOptions {
                validation: SnapshotValidation::Full,
                ..Default::default()
            },
        ),
        Err(VmError::InvalidSnapshot(_))
    ));
    let mut vm = Vm::restore(
        decoded,
        build,
        &mut host,
        RestoreOptions {
            validation: SnapshotValidation::Full,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(vm.get_export("order").unwrap(), Value::string("abc".into()));
    for (name, expected) in [
        ("errorValue", error),
        ("promiseValue", promise.clone()),
        ("reasonValue", reason.clone()),
    ] {
        assert_eq!(vm.start(&mut host, name, vec![]).unwrap(), expected);
    }
    assert_eq!(
        vm.promise_state(&promise).unwrap(),
        yield_vm::PromiseState::Rejected(reason)
    );
    for _ in 0..2 {
        vm.run_until_idle(&mut host).unwrap();
    }
    assert_eq!(
        vm.start(&mut host, "dispatchAgain", vec![]).unwrap(),
        Value::boolean(false)
    );
    assert_eq!(
        vm.get_export("order").unwrap(),
        Value::string("abcabc".into())
    );
    assert!(host.errors().is_empty());
    assert!(host.rejection_events().is_empty());
}
