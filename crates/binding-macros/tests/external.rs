use yield_vm::binding::CallCx;
use yield_vm::{
    HostClass, Instance, JsResult, JsValue, This, host_bindings, host_class, host_module, methods,
};

#[path = "external/registry.rs"]
mod registry;

#[host_class(tag = "Example", methods)]
struct Example {
    #[js_slot]
    item: JsValue,
    #[js_list]
    history: Vec<JsValue>,
}

#[methods(Example)]
impl Example {
    #[get(name = "item")]
    fn read(cx: &mut CallCx<'_>, this: This<Example>) -> JsResult<JsValue> {
        this.item(cx)
    }

    #[set(name = "item")]
    fn write(cx: &mut CallCx<'_>, this: This<Example>, value: JsValue) -> JsResult<()> {
        this.history_push(cx, value.clone())?;
        this.set_item(cx, value)
    }
}

#[host_module(name = "example")]
mod example {
    use super::*;

    #[constructor(name = "Example", class = Example)]
    fn make(cx: &mut CallCx<'_>, item: Option<JsValue>) -> JsResult<Instance<Example>> {
        let example = cx.alloc(Example)?;
        if let Some(item) = item {
            example.set_item(cx, item)?;
        }
        Ok(example)
    }

    #[constant(name = "version")]
    const VERSION: f64 = 1.0;
}

#[test]
fn links_an_external_module_and_class() -> Result<(), String> {
    let bindings = host_bindings! {
        dts: "tests/dts/example.d.ts",
        modules: [example], classes: [Example], states: [], operations: [],
        services: ::yield_vm::Services::default(),
    }?;
    assert_eq!(
        bindings.export_names("example").collect::<Vec<_>>(),
        ["Example", "version"]
    );
    assert_eq!(Example::SLOTS.len(), 2);
    Ok(())
}
