use yield_vm::binding::CallCx;
use yield_vm::{
    HostClass, Instance, JsResult, JsValue, ModuleCx, This, host_bindings, host_class, host_module,
    methods, module_exports,
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
struct ExampleModule;

#[module_exports]
impl ExampleModule {
    #[constructor(name = "Example", class = Example)]
    fn make(cx: &mut ModuleCx<'_, Self>, item: Option<JsValue>) -> JsResult<Instance<Example>> {
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
        dts: "dts/example.d.ts",
        modules: [ExampleModule], classes: [Example], operations: [],
        services: {},
    }?;
    assert_eq!(
        bindings.export_names("example").collect::<Vec<_>>(),
        ["Example", "version"]
    );
    assert_eq!(Example::SLOTS.len(), 2);
    Ok(())
}
