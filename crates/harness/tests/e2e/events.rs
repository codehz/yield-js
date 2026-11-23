use std::sync::Arc;

use super::support::{compile_main, declared};
use yield_vm::binding::CallCx;
use yield_vm::{
    Bindings, Host, Instance, JsResult, JsValue, This, Value, Vm, host_bindings, host_class,
    host_module, methods,
};

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
    let mut vm = Vm::new(build, &mut host).unwrap();
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
    let vm = Vm::new(build.clone(), &mut host).unwrap();
    let image = vm.snapshot(&[]).unwrap();
    let mut new_host = EventHost::new();
    let mut restored = Vm::restore_validated(image, build, &mut new_host).unwrap();
    assert_eq!(
        restored.start(&mut new_host, "run", vec![]).unwrap(),
        Value::boolean(true)
    );
    restored.run_until_idle(&mut new_host).unwrap();
}
