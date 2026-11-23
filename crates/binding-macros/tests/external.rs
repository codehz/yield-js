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

#[derive(Debug, PartialEq, yield_vm::Encode, yield_vm::Decode)]
struct Pair<T>(T, T)
where
    T: PartialEq;

#[host_class(tag = "Mixed")]
#[derive(Debug, PartialEq, yield_vm::Encode, yield_vm::Decode)]
struct Mixed {
    #[js_slot]
    item: JsValue,
    strings: Pair<String>,
    #[js_list]
    history: Vec<JsValue>,
}

#[host_module(name = "implicit")]
struct Implicit {
    #[js_slot]
    item: JsValue,
}

#[module_exports]
impl Implicit {}

#[host_module(name = "explicit")]
#[derive(::core::default::Default, ::yield_vm::Encode)]
struct Explicit {
    #[js_list]
    history: Vec<JsValue>,
}

#[module_exports]
impl Explicit {}

#[yield_vm::operation(name = "mixed", recovery = pause)]
#[derive(yield_vm::Decode, yield_vm::Encode)]
struct MixedWork(Mixed);

impl yield_vm::Registration<MixedWork> for () {}

impl yield_vm::OperationDriver for MixedWork {
    type Live = ();

    fn start(
        &self,
        _: yield_vm::OperationToken,
        _: &yield_vm::Bindings,
    ) -> Result<(), yield_vm::HostFault> {
        Ok(())
    }
}

#[test]
fn derived_payload_excludes_slots_and_checks_nested_and_top_level_boundaries() -> Result<(), String>
{
    use yield_vm::{Decode, Encode};

    let bindings = host_bindings! {
        dts: "dts/state.d.ts",
        modules: [Implicit, Explicit], classes: [Mixed], operations: [MixedWork],
        services: {},
    }?;
    let value = MixedWork(Mixed {
        strings: Pair(String::new(), "é".into()),
    });
    let mut bytes = Vec::new();
    value.encode(&mut bytes, &bindings).unwrap();
    assert_eq!(&bytes[..4], &0u32.to_le_bytes());
    assert_eq!(&bytes[4..8], &2u32.to_le_bytes());
    assert_eq!(&bytes[8..], "é".as_bytes());
    let mut cursor = bytes.as_slice();
    assert_eq!(
        MixedWork::decode(&mut cursor, &bindings).unwrap().0,
        value.0
    );
    assert!(cursor.is_empty());
    let truncated = bindings
        .decode_operation("mixed", &bytes[..bytes.len() - 1])
        .unwrap_err();
    assert!(
        truncated
            .0
            .contains("field strings: field 1: truncated payload"),
        "{truncated}"
    );
    bytes.push(0);
    let trailing = bindings.decode_operation("mixed", &bytes).unwrap_err();
    assert_eq!(trailing.0, "trailing payload bytes");
    Ok(())
}
