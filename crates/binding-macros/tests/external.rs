use std::collections::{BTreeMap, BTreeSet};
use yield_vm::binding::CallCx;
use yield_vm::bytecode::{Build, Function, Instr, Slot, SourceModule};
use yield_vm::{
    Host, HostClass, Instance, JsResult, JsValue, ModuleCx, Services, This, Vm, host, host_class,
    host_module, methods, module_exports,
};

#[path = "external/registry.rs"]
mod registry;

// An empty guest with optional host imports is enough to exercise startup,
// without introducing a compiler dependency into the macro tests.
fn start_vm(host: Host, imports: BTreeMap<String, BTreeSet<String>>) -> Vm {
    let initializer = |func_id, name: &str| {
        let slot = Slot::transient(0);
        Function {
            func_id,
            name: name.into(),
            initial_name: name.into(),
            is_async: false,
            is_arrow: false,
            is_method: false,
            is_generator: false,
            length: 0,
            params: vec![],
            rest_param: None,
            this_param: None,
            new_target_param: None,
            home_param: None,
            arguments_param: None,
            transient_slots: 1,
            persistent_slots: 0,
            code: vec![Instr::Undefined { dst: slot }, Instr::Return { src: slot }],
            resumes: vec![],
            generator_start: None,
            generator_resumes: vec![],
            handlers: vec![],
            module: Some(0),
            stack_sites: vec![],
        }
    };
    let mut build = Build::new(
        vec![],
        vec![],
        imports,
        vec![initializer(0, "declare"), initializer(1, "evaluate")],
        vec![SourceModule {
            id: "entry".into(),
            declaration: 0,
            evaluation: 1,
            dependencies: vec![],
            exports: BTreeMap::new(),
        }],
        0,
        None,
    );
    build.build_id = build.compute_id();
    Vm::new(build, host, Default::default()).expect("start guest")
}

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

#[host_module(name = "example", classes = [Example])]
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
    let host = host! {
        dts: "dts/example.d.ts",
        modules: [ExampleModule],
        services: {},
    }?;
    start_vm(
        host,
        [(
            "example".into(),
            ["Example".into(), "version".into()].into(),
        )]
        .into(),
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

#[host_module(name = "implicit", classes = [Mixed], operations = [MixedWork])]
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
        _: &Services<'_>,
    ) -> Result<(), yield_vm::HostFault> {
        Ok(())
    }
}

#[test]
fn derived_payload_excludes_slots_and_checks_nested_and_top_level_boundaries() -> Result<(), String>
{
    use yield_vm::{Decode, Encode};

    let host = host! {
        dts: "dts/state.d.ts",
        modules: [Implicit, Explicit],
        services: {},
    }?;
    let services = Services::default();
    let value = MixedWork(Mixed {
        strings: Pair(String::new(), "é".into()),
    });
    let mut bytes = Vec::new();
    value.encode(&mut bytes, &services).unwrap();
    assert_eq!(&bytes[..4], &0u32.to_le_bytes());
    assert_eq!(&bytes[4..8], &2u32.to_le_bytes());
    assert_eq!(&bytes[8..], "é".as_bytes());
    let mut cursor = bytes.as_slice();
    assert_eq!(
        MixedWork::decode(&mut cursor, &services).unwrap().0,
        value.0
    );
    assert!(cursor.is_empty());
    let truncated =
        yield_vm::__macro_support::decode_operation(&host, "mixed", &bytes[..bytes.len() - 1])
            .unwrap_err();
    assert!(
        truncated
            .0
            .contains("field strings: field 1: truncated payload"),
        "{truncated}"
    );
    bytes.push(0);
    let trailing = yield_vm::__macro_support::decode_operation(&host, "mixed", &bytes).unwrap_err();
    assert_eq!(trailing.0, "trailing payload bytes");
    Ok(())
}
