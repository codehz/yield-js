use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use yield_compiler::{CompileOptions, compile};
use yield_vm::binding::{
    Bindings, CallCx, CodecError, DecodeCx, HostFault, InspectKey, InspectValue, Instance,
    JsResult, JsValue, OperationStart, OperationToken, This,
};
use yield_vm::bytecode::Build;
use yield_vm::host::Completion;
use yield_vm::{GuestOutcome, Host, HostStep, JsString, Vm};
use yield_vm::{host_bindings, host_class, host_module, methods, operation};

mod case;

pub use case::run_case;

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

#[host_class(tag = "Node", methods, schema = 1, encode = Node::encode_payload, decode = Node::decode_payload)]
pub struct Node {
    pub label: String,
}

#[methods(Node)]
impl Node {
    #[method(name = "label")]
    fn label(cx: &mut CallCx<'_>, this: This<Node>) -> JsResult<JsValue> {
        let label = this.instance().payload(cx)?.label.clone();
        Ok(JsValue::string(label))
    }

    #[method(symbol = ToPrimitive)]
    fn to_primitive(
        cx: &mut CallCx<'_>,
        this: This<Node>,
        _hint: Option<JsValue>,
    ) -> JsResult<JsValue> {
        Self::label(cx, this)
    }

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

#[host_class(tag = "Matcher", methods)]
pub struct Matcher {
    #[js_slot]
    actual: JsValue,
}

#[methods(Matcher)]
impl Matcher {
    #[get(name = "value")]
    fn get_value(cx: &mut CallCx<'_>, this: This<Matcher>) -> JsResult<JsValue> {
        this.actual(cx)
    }

    #[set(name = "value")]
    fn set_value(cx: &mut CallCx<'_>, this: This<Matcher>, value: JsValue) -> JsResult<()> {
        this.set_actual(cx, value)
    }

    #[method(name = "toBe")]
    fn to_be(cx: &mut CallCx<'_>, this: This<Matcher>, expected: Option<JsValue>) -> JsResult<()> {
        let actual = this.actual(cx)?;
        let expected = match expected {
            Some(value) => value,
            None => cx.arg(0)?,
        };
        if cx.same_value(&actual, &expected) {
            return Ok(());
        }
        Err(cx.throw_string(format!("toBe: expected {expected}, actual {actual}")))
    }

    #[method(name = "toEqual")]
    fn to_equal(
        cx: &mut CallCx<'_>,
        this: This<Matcher>,
        expected: Option<JsValue>,
    ) -> JsResult<()> {
        let actual = this.actual(cx)?;
        let expected = match expected {
            Some(value) => value,
            None => cx.arg(0)?,
        };
        if structural_equal(cx, &actual, &expected) {
            return Ok(());
        }
        Err(cx.throw_string(format!("toEqual: expected {expected}, actual {actual}")))
    }

    #[method(name = "toBeTruthy")]
    fn to_be_truthy(cx: &mut CallCx<'_>, this: This<Matcher>) -> JsResult<()> {
        let actual = this.actual(cx)?;
        if actual.truthy() {
            return Ok(());
        }
        Err(cx.throw_string(format!("toBeTruthy: expected truthy, actual {actual}")))
    }

    #[method(name = "toThrow")]
    async fn to_throw(
        cx: &mut CallCx<'_>,
        this: This<Matcher>,
        expected: Option<JsValue>,
    ) -> JsResult<HostStep> {
        let actual = this.actual(cx)?;
        if !cx.is_callable(&actual)? {
            return Err(cx.throw_string("toThrow: expected a callable"));
        }
        let expected = expected.unwrap_or_else(JsValue::undefined);
        if !expected.is_undefined()
            && expected.as_string().is_none()
            && !cx.inspect(expected.clone())?.is_regexp()
        {
            return Err(cx.throw_string("toThrow: expected a string or RegExp"));
        }
        let outcome = cx.call(actual, JsValue::undefined(), vec![]).await;
        match_to_throw(cx, outcome, expected)
    }
}

fn match_to_throw(
    cx: &mut CallCx<'_>,
    outcome: GuestOutcome,
    expected: JsValue,
) -> JsResult<HostStep> {
    let GuestOutcome::Thrown(thrown) = outcome else {
        return Err(cx.throw_string("toThrow: function returned without throwing"));
    };
    let success = HostStep::Return(JsValue::undefined());
    if expected.is_undefined() {
        return Ok(success);
    }
    let message = if let Some(text) = thrown.as_string() {
        text.clone()
    } else {
        let inspect = cx.inspect(thrown)?;
        let message = match inspect.get(&InspectKey::String("message".into())) {
            Some(InspectValue::Data(value)) => value.as_string().cloned(),
            None if inspect.is_error() => Some(JsString::new()),
            _ => None,
        };
        message.ok_or_else(|| cx.throw_string("toThrow: thrown value has no string message"))?
    };

    if let Some(pattern) = expected.as_string() {
        if message.contains(pattern) {
            return Ok(success);
        }
        return Err(cx.throw_string(format!(
            "toThrow: expected message containing \"{pattern}\", actual \"{message}\""
        )));
    }
    let regexp = cx.inspect(expected)?;
    if regexp.regexp_matches(&message) == Some(true) {
        return Ok(success);
    }
    let (source, flags) = regexp
        .regexp_source_flags()
        .expect("toThrow only saves a string or RegExp");
    Err(cx.throw_string(format!(
        "toThrow: expected message matching /{source}/{flags}, actual \"{message}\""
    )))
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

#[operation(name = "wait", schema = 1, recovery = resume, rejection = "wait rejected", encode = Wait::encode, decode = Wait::decode)]
pub struct Wait {
    pub label: String,
}

impl Wait {
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

#[operation(name = "pause", schema = 1, recovery = pause, rejection = "pause rejected", encode = Pause::encode, decode = Pause::decode)]
pub struct Pause {
    pub label: String,
}

impl Pause {
    fn encode(&self, out: &mut Vec<u8>) -> Result<(), CodecError> {
        out.extend_from_slice(self.label.as_bytes());
        Ok(())
    }

    fn decode(input: &[u8], _bindings: &Bindings) -> Result<Self, CodecError> {
        decode_operation_label(input).map(|label| Self { label })
    }
}

#[operation(name = "reject", schema = 1, recovery = reject, rejection = "reject rejected during restore", encode = Reject::encode, decode = Reject::decode)]
pub struct Reject {
    pub label: String,
}

impl Reject {
    fn encode(&self, out: &mut Vec<u8>) -> Result<(), CodecError> {
        out.extend_from_slice(self.label.as_bytes());
        Ok(())
    }

    fn decode(input: &[u8], _bindings: &Bindings) -> Result<Self, CodecError> {
        decode_operation_label(input).map(|label| Self { label })
    }
}

#[operation(name = "reissue", schema = 1, recovery = reissue, rejection = "reissue rejected", encode = Reissue::encode, decode = Reissue::decode)]
pub struct Reissue {
    pub label: String,
}

impl Reissue {
    fn encode(&self, out: &mut Vec<u8>) -> Result<(), CodecError> {
        out.extend_from_slice(self.label.as_bytes());
        Ok(())
    }

    fn decode(input: &[u8], _bindings: &Bindings) -> Result<Self, CodecError> {
        decode_operation_label(input).map(|label| Self { label })
    }
}

fn decode_operation_label(input: &[u8]) -> Result<String, CodecError> {
    std::str::from_utf8(input)
        .map(str::to_owned)
        .map_err(|_| CodecError::new("operation label is not UTF-8"))
}

pub struct TestHost {
    bindings: Arc<Bindings>,
    counters: Arc<Counters>,
    operations: BTreeMap<String, OperationToken>,
    errors: Vec<JsValue>,
    gc_roots: Vec<yield_vm::Value>,
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
            gc_roots: Vec::new(),
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

    /// Replaces the values kept alive by guest-triggered GC. The values must
    /// belong to the VM this host is about to run.
    pub fn set_gc_roots(&mut self, roots: Vec<yield_vm::Value>) {
        self.gc_roots = roots;
    }

    pub fn pending_labels(&self) -> Vec<String> {
        self.operations.keys().cloned().collect()
    }

    pub fn token(&self, label: &str) -> Option<OperationToken> {
        self.operations.get(label).copied()
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
        self.operations.remove(label);
        Ok(())
    }
}

impl Host for TestHost {
    fn bindings(&self) -> Arc<Bindings> {
        self.bindings.clone()
    }

    fn gc_roots(&self) -> &[yield_vm::Value] {
        &self.gc_roots
    }

    fn start_operation(&mut self, start: OperationStart<'_>) -> Result<(), HostFault> {
        if !matches!(start.name, "wait" | "pause" | "reject" | "reissue") {
            return Err(HostFault::new(format!("unknown operation {}", start.name)));
        }
        let label = std::str::from_utf8(start.state)
            .map_err(|_| HostFault::new("operation state is not UTF-8"))?;
        self.operations
            .insert(format!("{}:{label}", start.name), start.token);
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
    host_bindings! {
        modules: [test],
        classes: [Node, Matcher],
        states: [],
        operations: [Wait, Pause, Reject, Reissue],
        services: [("counters", 1, counters), ("node-factory", 1, NodeFactory)],
    }
}

#[host_module(name = "test")]
mod test {
    use super::*;

    #[function(name = "gc")]
    async fn gc(cx: &mut CallCx<'_>) -> JsResult<HostStep> {
        let outcome = cx.gc().await;
        match outcome {
            GuestOutcome::Returned(value) => Ok(HostStep::Return(value)),
            GuestOutcome::Thrown(value) => Err(cx.throw(value)),
        }
    }

    #[function(name = "inspect")]
    fn inspect(cx: &mut CallCx<'_>, _value: Option<JsValue>) -> JsResult<()> {
        if let Some(counters) = cx.service::<Arc<Counters>>("counters") {
            counters.hit("inspect");
        }
        Ok(())
    }

    #[function(name = "seed")]
    fn seed(cx: &mut CallCx<'_>) -> JsResult<JsValue> {
        if let Some(counters) = cx.service::<Arc<Counters>>("counters") {
            counters.hit("seed");
        }
        Ok(JsValue::number(10.0))
    }

    #[function(name = "wait")]
    fn wait(cx: &mut CallCx<'_>, arg: Option<JsValue>) -> JsResult<JsValue> {
        if let Some(counters) = cx.service::<Arc<Counters>>("counters") {
            counters.hit("wait");
        }
        let arg = match arg {
            Some(value) => value,
            None => cx.arg(0)?,
        };
        let label = cx
            .string(&arg)?
            .to_utf8()
            .map_err(|_| cx.type_error("wait requires a valid UTF-16 label"))?;
        cx.start_operation(Wait { label })
    }

    #[function(name = "pause")]
    fn pause(cx: &mut CallCx<'_>, arg: JsValue) -> JsResult<JsValue> {
        let label = cx
            .string(&arg)?
            .to_utf8()
            .map_err(|_| cx.type_error("pause requires a valid UTF-16 label"))?;
        cx.start_operation(Pause { label })
    }

    #[function(name = "rejectAfterRestore")]
    fn reject_after_restore(cx: &mut CallCx<'_>, arg: JsValue) -> JsResult<JsValue> {
        let label = cx
            .string(&arg)?
            .to_utf8()
            .map_err(|_| cx.type_error("reject requires a valid UTF-16 label"))?;
        cx.start_operation(Reject { label })
    }

    #[function(name = "reissue")]
    fn reissue(cx: &mut CallCx<'_>, arg: JsValue) -> JsResult<JsValue> {
        let label = cx
            .string(&arg)?
            .to_utf8()
            .map_err(|_| cx.type_error("reissue requires a valid UTF-16 label"))?;
        cx.start_operation(Reissue { label })
    }

    #[function(name = "bind")]
    fn bind(cx: &mut CallCx<'_>, arg: JsValue) -> JsResult<Instance<Node>> {
        let label = cx
            .string(&arg)?
            .to_utf8()
            .map_err(|_| cx.type_error("bind requires a valid UTF-16 label"))?;
        cx.alloc(Node { label })
    }

    #[function(name = "setLabel")]
    fn set_label(
        cx: &mut CallCx<'_>,
        _node: Option<JsValue>,
        _label: Option<JsValue>,
    ) -> JsResult<()> {
        if let Some(counters) = cx.service::<Arc<Counters>>("counters") {
            counters.hit("setLabel");
        }
        Ok(())
    }

    #[function(name = "expect")]
    fn expect(cx: &mut CallCx<'_>, actual: Option<JsValue>) -> JsResult<Instance<Matcher>> {
        if let Some(counters) = cx.service::<Arc<Counters>>("counters") {
            counters.hit("expect");
        }
        let actual = actual.unwrap_or_else(JsValue::undefined);
        let matcher = cx.alloc(Matcher)?;
        if !actual.is_undefined()
            && let Some(counters) = cx.service::<Arc<Counters>>("counters")
        {
            counters.hit("expect:value");
        }
        matcher.set_actual(cx, actual)?;
        Ok(matcher)
    }

    #[function(name = "assert")]
    fn assert(
        cx: &mut CallCx<'_>,
        actual: Option<JsValue>,
        message: Option<JsValue>,
    ) -> JsResult<()> {
        let actual = actual.unwrap_or_else(JsValue::undefined);
        if actual.truthy() {
            return Ok(());
        }
        let message = match message {
            Some(value) => match cx.string(&value) {
                Ok(text) => format!(": {text}"),
                Err(_) => format!(": {value}"),
            },
            None => String::new(),
        };
        Err(cx.throw_string(format!("assert: expected truthy, actual {actual}{message}")))
    }
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
