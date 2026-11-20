use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use yield_compiler::{CompileOptions, compile};
use yield_vm::binding::{
    Bindings, CallCx, CodecError, DecodeCx, HostClass, HostFault, InspectValue, JsResult, JsValue,
    MethodDecl, ModuleDecl, Operation, OperationStart, OperationToken, Recovery, SlotDecl,
    SlotKind,
};
use yield_vm::bytecode::Build;
use yield_vm::host::Completion;
use yield_vm::{Host, Vm};

mod case;

pub use case::run_case;

const MATCHER_ACTUAL: usize = 0;

#[derive(Default)]
pub struct Counters(Mutex<BTreeMap<String, usize>>);

impl Counters {
    pub fn hit(&self, name: &str) {
        *self
            .0
            .lock()
            .expect("counter lock")
            .entry(name.to_owned())
            .or_default() += 1;
    }

    pub fn count(&self, name: &str) -> usize {
        self.0
            .lock()
            .expect("counter lock")
            .get(name)
            .copied()
            .unwrap_or(0)
    }
}

pub struct NodeFactory;

pub struct Node {
    pub label: String,
}

impl HostClass for Node {
    const TAG: &'static str = "Node";
    const PAYLOAD_SCHEMA: Option<u32> = Some(1);
    const METHODS: &'static [MethodDecl] = &[MethodDecl {
        name: "label",
        arity: 0,
        call: node_label,
    }];

    fn encode_payload(&self, out: &mut Vec<u8>) -> Result<(), CodecError> {
        out.extend_from_slice(self.label.as_bytes());
        Ok(())
    }

    fn decode_payload(input: &[u8], cx: &mut DecodeCx<'_>) -> Result<Self, CodecError> {
        if cx.service::<NodeFactory>("node-factory").is_none() {
            return Err(CodecError::new("node factory service is unavailable"));
        }
        let label = std::str::from_utf8(input)
            .map_err(|_| CodecError::new("node label is not UTF-8"))?
            .to_owned();
        Ok(Self { label })
    }
}

fn node_label(cx: &mut CallCx<'_>) -> JsResult<JsValue> {
    let this = cx.this::<Node>()?;
    let label = this.instance().payload(cx)?.label.clone();
    Ok(JsValue::string(label))
}

pub struct Matcher;

impl HostClass for Matcher {
    const TAG: &'static str = "Matcher";
    const SLOTS: &'static [SlotDecl] = &[SlotDecl {
        name: "actual",
        kind: SlotKind::Js,
    }];
    const METHODS: &'static [MethodDecl] = &[
        MethodDecl {
            name: "toBe",
            arity: 1,
            call: matcher_to_be,
        },
        MethodDecl {
            name: "toEqual",
            arity: 1,
            call: matcher_to_equal,
        },
        MethodDecl {
            name: "toBeTruthy",
            arity: 0,
            call: matcher_to_be_truthy,
        },
    ];
    const ACCESSORS: &'static [yield_vm::binding::AccessorDecl] =
        &[yield_vm::binding::AccessorDecl {
            name: "value",
            get: Some(matcher_get_value),
            set: Some(matcher_set_value),
        }];
}

fn matcher_get_value(cx: &mut CallCx<'_>) -> JsResult<JsValue> {
    let this = cx.this::<Matcher>()?;
    this.instance().slot(cx, MATCHER_ACTUAL)
}

fn matcher_set_value(cx: &mut CallCx<'_>) -> JsResult<JsValue> {
    let this = cx.this::<Matcher>()?;
    let value = cx.arg(0)?;
    this.instance().set_slot(cx, MATCHER_ACTUAL, value)?;
    Ok(JsValue::undefined())
}

fn matcher_to_be(cx: &mut CallCx<'_>) -> JsResult<JsValue> {
    let this = cx.this::<Matcher>()?;
    let actual = this.instance().slot(cx, MATCHER_ACTUAL)?;
    let expected = cx.arg(0)?;
    if cx.same_value(&actual, &expected) {
        return Ok(JsValue::undefined());
    }
    Err(cx.throw_string(format!("toBe: expected {expected}, actual {actual}")))
}

fn matcher_to_equal(cx: &mut CallCx<'_>) -> JsResult<JsValue> {
    let this = cx.this::<Matcher>()?;
    let actual = this.instance().slot(cx, MATCHER_ACTUAL)?;
    let expected = cx.arg(0)?;
    if structural_equal(cx, &actual, &expected) {
        return Ok(JsValue::undefined());
    }
    Err(cx.throw_string(format!("toEqual: expected {expected}, actual {actual}")))
}

fn matcher_to_be_truthy(cx: &mut CallCx<'_>) -> JsResult<JsValue> {
    let this = cx.this::<Matcher>()?;
    let actual = this.instance().slot(cx, MATCHER_ACTUAL)?;
    if actual.truthy() {
        return Ok(JsValue::undefined());
    }
    Err(cx.throw_string(format!("toBeTruthy: expected truthy, actual {actual}")))
}

fn structural_equal(cx: &CallCx<'_>, left: &JsValue, right: &JsValue) -> bool {
    fn equal(
        cx: &CallCx<'_>,
        left: JsValue,
        right: JsValue,
        seen: &mut BTreeSet<(u64, u64)>,
    ) -> bool {
        if cx.same_value(&left, &right) {
            return true;
        }
        let (Ok(left), Ok(right)) = (cx.inspect(left), cx.inspect(right)) else {
            return false;
        };
        if left.kind() != right.kind() {
            return false;
        }
        if left.kind() != yield_vm::binding::InspectKind::Object {
            return false;
        }
        let (Some(left_id), Some(right_id)) = (left.identity(), right.identity()) else {
            return false;
        };
        if !seen.insert((left_id, right_id)) {
            return true;
        }
        if left.array_length() != right.array_length() {
            return false;
        }
        let left_keys = left.own_keys();
        let right_keys = right.own_keys();
        if left_keys != right_keys {
            return false;
        }
        for key in left_keys {
            match (left.get(&key), right.get(&key)) {
                (Some(InspectValue::Accessor), Some(InspectValue::Accessor)) => {}
                (Some(InspectValue::Data(left_value)), Some(InspectValue::Data(right_value))) => {
                    if !equal(cx, left_value, right_value, seen) {
                        return false;
                    }
                }
                (None, None) => {}
                _ => return false,
            }
        }
        true
    }

    equal(cx, left.clone(), right.clone(), &mut BTreeSet::new())
}

pub struct Wait {
    pub label: String,
}

impl Operation for Wait {
    const NAME: &'static str = "wait";
    const SCHEMA: u32 = 1;
    const RECOVERY: Recovery = Recovery::Resume;
    const REJECTION: &'static str = "wait rejected";

    fn encode(&self, out: &mut Vec<u8>) -> Result<(), CodecError> {
        out.extend_from_slice(self.label.as_bytes());
        Ok(())
    }

    fn decode(input: &[u8], _bindings: &Bindings) -> Result<Self, CodecError> {
        let label = std::str::from_utf8(input)
            .map_err(|_| CodecError::new("wait label is not UTF-8"))?
            .to_owned();
        Ok(Self { label })
    }
}

pub struct TestHost {
    bindings: Arc<Bindings>,
    counters: Arc<Counters>,
    operations: BTreeMap<String, OperationToken>,
    errors: Vec<JsValue>,
    pub reject_restore_operation: bool,
    pub reject_native: bool,
}

impl Default for TestHost {
    fn default() -> Self {
        Self::new()
    }
}

impl TestHost {
    pub fn new() -> Self {
        let counters = Arc::new(Counters::default());
        let bindings = test_bindings(counters.clone()).expect("test bindings");
        Self {
            bindings: Arc::new(bindings),
            counters,
            operations: BTreeMap::new(),
            errors: Vec::new(),
            reject_restore_operation: false,
            reject_native: false,
        }
    }

    pub fn count(&self, name: &str) -> usize {
        self.counters.count(name)
    }

    pub fn errors(&self) -> &[JsValue] {
        &self.errors
    }

    pub fn pending_labels(&self) -> Vec<String> {
        self.operations
            .keys()
            .map(|label| format!("wait:{label}"))
            .collect()
    }

    pub fn token(&self, label: &str) -> Option<OperationToken> {
        self.operations
            .get(label)
            .or_else(|| self.operations.get(label.strip_prefix("wait:")?))
            .copied()
    }

    pub fn complete(
        &mut self,
        vm: &mut Vm,
        label: &str,
        completion: Completion,
    ) -> Result<(), String> {
        let token = self
            .token(label)
            .ok_or_else(|| format!("unknown operation {label:?}"))?;
        vm.complete_operation(token, completion)
            .map_err(|error| error.to_string())?;
        if let Some(label) = label.strip_prefix("wait:") {
            self.operations.remove(label);
        } else {
            self.operations.remove(label);
        }
        Ok(())
    }
}

impl Host for TestHost {
    fn bindings(&self) -> Arc<Bindings> {
        self.bindings.clone()
    }

    fn start_operation(&mut self, start: OperationStart<'_>) -> Result<(), HostFault> {
        let label = match start.name {
            "wait" => std::str::from_utf8(start.state)
                .map_err(|_| HostFault::new("wait state is not UTF-8"))?
                .to_owned(),
            other => return Err(HostFault::new(format!("unknown operation {other}"))),
        };
        self.operations.insert(label, start.token);
        Ok(())
    }

    fn resume_operation(&mut self, start: OperationStart<'_>) -> Result<(), HostFault> {
        if self.reject_restore_operation {
            return Err(HostFault::new("restore rejected by host"));
        }
        self.start_operation(start)
    }

    fn report_error(&mut self, error: JsValue) {
        self.errors.push(error);
    }
}

fn test_bindings(counters: Arc<Counters>) -> Result<Bindings, String> {
    Bindings::builder()
        .service("counters", 1, counters)
        .service("node-factory", 1, NodeFactory)
        .class::<Node>()
        .class::<Matcher>()
        .operation::<Wait>()
        .module(
            ModuleDecl::new("test")
                .function("inspect", 1, |cx| {
                    if let Some(counters) = cx.service::<Arc<Counters>>("counters") {
                        counters.hit("inspect");
                    }
                    Ok(JsValue::undefined())
                })
                .function("seed", 0, |cx| {
                    if let Some(counters) = cx.service::<Arc<Counters>>("counters") {
                        counters.hit("seed");
                    }
                    Ok(JsValue::number(10.0))
                })
                .function("wait", 1, |cx| {
                    if let Some(counters) = cx.service::<Arc<Counters>>("counters") {
                        counters.hit("wait");
                    }
                    let arg = cx.arg(0)?;
                    let label = cx
                        .string(&arg)?
                        .to_utf8()
                        .map_err(|_| cx.type_error("wait requires a valid UTF-16 label"))?;
                    cx.start_operation(Wait { label })
                })
                .function("bind", 1, |cx| {
                    let arg = cx.arg(0)?;
                    let label = cx
                        .string(&arg)?
                        .to_utf8()
                        .map_err(|_| cx.type_error("bind requires a valid UTF-16 label"))?;
                    let node = cx.alloc(Node { label })?;
                    Ok(node.value(cx))
                })
                .function("setLabel", 2, |cx| {
                    if let Some(counters) = cx.service::<Arc<Counters>>("counters") {
                        counters.hit("setLabel");
                    }
                    Ok(JsValue::undefined())
                })
                .function("expect", 1, |cx| {
                    if let Some(counters) = cx.service::<Arc<Counters>>("counters") {
                        counters.hit("expect");
                    }
                    let actual = cx.arg(0).unwrap_or_else(|_| JsValue::undefined());
                    let matcher = cx.alloc(Matcher)?;
                    if !actual.is_undefined()
                        && let Some(counters) = cx.service::<Arc<Counters>>("counters")
                    {
                        counters.hit("expect:value");
                    }
                    matcher.set_slot(cx, MATCHER_ACTUAL, actual)?;
                    Ok(matcher.value(cx))
                })
                .function("assert", 2, |cx| {
                    let actual = cx.arg(0).unwrap_or_else(|_| JsValue::undefined());
                    if actual.truthy() {
                        return Ok(JsValue::undefined());
                    }
                    let message = match cx.arg(1) {
                        Ok(value) => match cx.string(&value) {
                            Ok(text) => format!(": {text}"),
                            Err(_) => format!(": {value}"),
                        },
                        Err(_) => String::new(),
                    };
                    Err(cx
                        .throw_string(format!("assert: expected truthy, actual {actual}{message}")))
                }),
        )
        .build()
}

pub(crate) fn compile_options() -> CompileOptions {
    let host = TestHost::new();
    let bindings = host.bindings();
    let mut imports = BTreeMap::new();
    for module in bindings.module_names() {
        imports.insert(
            module.to_owned(),
            bindings
                .export_names(module)
                .map(str::to_owned)
                .collect::<BTreeSet<_>>(),
        );
    }
    CompileOptions { imports }
}

pub fn build(source: &str) -> Build {
    compile(source, &compile_options()).expect("source compiles")
}

pub fn number(value: f64) -> JsValue {
    JsValue::number(value)
}

pub fn string(value: &str) -> JsValue {
    JsValue::string(value)
}
