use std::cell::{Ref, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use yield_compiler::{CompileError, Compiler, MemoryResolver, Project};
use yield_vm::binding::{
    CallCx, CodecError, HostFault, InspectKey, InspectValue, Instance, JsResult, JsValue,
    Operation, OperationDriver, OperationToken, Registration, This,
};
use yield_vm::bytecode::Build;
use yield_vm::host::Completion;
use yield_vm::intrinsics::{Error, TypeError};
use yield_vm::{
    Absent, ConsoleEvent, ConsoleSink, CryptoService, Diagnostic, Diagnostics, GcRoots,
    GuestOutcome, Host, HostStep, JsString, Platform, PlatformServices, Services,
    SubtleCryptoService, TimeService, UserAgent, Vm, VmError, fetch::FetchService,
};
use yield_vm::{
    Decode, Encode, JsError, host, host_class, host_module, methods, module_exports, operation,
};

mod case;
pub mod fetch;
mod subtle;
pub mod timer;

pub use case::run_case;
pub use subtle::TestSubtleCrypto;
pub use timer::{FakeClock, Timer};

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

impl NodeFactory {
    fn create(&self, label: String) -> Node {
        Node { label }
    }
}

#[host_class(tag = "Node", methods, services = [factory: NodeFactory])]
#[derive(Encode)]
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

    #[method(symbol = Dispose)]
    fn dispose(cx: &mut CallCx<'_>, this: This<Node>) -> JsResult<JsValue> {
        Self::label(cx, this)
    }

    #[method(symbol = AsyncDispose)]
    fn async_dispose(cx: &mut CallCx<'_>, this: This<Node>) -> JsResult<JsValue> {
        let label = this.instance().payload(cx)?.label.clone();
        cx.promise(Wait { label })
    }
}

impl Decode<Node> for Node {
    fn decode(input: &mut &[u8], services: &Services<'_, Self>) -> Result<Self, CodecError> {
        let factory = Self::factory(services);
        let label = String::decode(input, services)
            .map_err(|error| CodecError::new(format!("field label: {error}")))?;
        Ok(factory.create(label))
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
        Err(matcher_error(
            cx,
            format!("toBe: expected {expected}, actual {actual}"),
        ))
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
        Err(matcher_error(
            cx,
            format!("toEqual: expected {expected}, actual {actual}"),
        ))
    }

    #[method(name = "toBeTruthy")]
    fn to_be_truthy(cx: &mut CallCx<'_>, this: This<Matcher>) -> JsResult<()> {
        let actual = this.actual(cx)?;
        if actual.truthy() {
            return Ok(());
        }
        Err(matcher_error(
            cx,
            format!("toBeTruthy: expected truthy, actual {actual}"),
        ))
    }

    #[method(name = "toThrow")]
    async fn to_throw(
        cx: &mut CallCx<'_>,
        this: This<Matcher>,
        expected: Option<JsValue>,
    ) -> JsResult<HostStep> {
        let actual = this.actual(cx)?;
        if !cx.is_callable(&actual)? {
            return Err(throw_type_error(cx, "toThrow: expected a callable"));
        }
        let expected = expected.unwrap_or_else(JsValue::undefined);
        if !expected.is_undefined()
            && expected.as_string().is_none()
            && !cx.inspect(expected.clone())?.is_regexp()
        {
            return Err(throw_type_error(cx, "toThrow: expected a string or RegExp"));
        }
        let outcome = cx.call(actual, JsValue::undefined(), vec![]).await;
        match_to_throw(cx, outcome, expected)
    }
}

/// Matcher and assert failures throw ordinary branded Errors carrying the
/// failure message.
fn matcher_error(cx: &mut CallCx<'_>, message: impl Into<JsString>) -> JsError {
    match cx.construct::<Error>(&[JsValue::string(message)]) {
        Ok(value) => cx.throw(value),
        Err(error) => error,
    }
}

fn throw_type_error(cx: &mut CallCx<'_>, message: &str) -> JsError {
    match cx.construct::<TypeError>(&[JsValue::string(message)]) {
        Ok(value) => cx.throw(value),
        Err(error) => error,
    }
}

fn match_to_throw(
    cx: &mut CallCx<'_>,
    outcome: GuestOutcome,
    expected: JsValue,
) -> JsResult<HostStep> {
    let GuestOutcome::Thrown(thrown) = outcome else {
        return Err(matcher_error(
            cx,
            "toThrow: function returned without throwing",
        ));
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
        message.ok_or_else(|| matcher_error(cx, "toThrow: thrown value has no string message"))?
    };

    if let Some(pattern) = expected.as_string() {
        if message.contains(pattern) {
            return Ok(success);
        }
        return Err(matcher_error(
            cx,
            format!("toThrow: expected message containing \"{pattern}\", actual \"{message}\""),
        ));
    }
    let regexp = cx.inspect(expected)?;
    if regexp.regexp_matches(&message) == Some(true) {
        return Ok(success);
    }
    let (source, flags) = regexp
        .regexp_source_flags()
        .expect("toThrow only saves a string or RegExp");
    Err(matcher_error(
        cx,
        format!("toThrow: expected message matching /{source}/{flags}, actual \"{message}\""),
    ))
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

#[operation(
    name = "wait",
    recovery = resume,
    rejection = "wait rejected",
    services = [labels: Rc<LabelService>],
)]
#[derive(Encode, Decode)]
pub struct Wait {
    pub label: String,
}

#[operation(
    name = "pause",
    recovery = pause,
    rejection = "pause rejected",
    services = [labels: Rc<LabelService>],
)]
#[derive(Encode, Decode)]
pub struct Pause {
    pub label: String,
}

#[operation(
    name = "reject",
    recovery = reject,
    rejection = "reject rejected during restore",
    services = [labels: Rc<LabelService>],
)]
#[derive(Encode, Decode)]
pub struct Reject {
    pub label: String,
}

#[operation(
    name = "reissue",
    recovery = reissue,
    rejection = "reissue rejected",
    services = [labels: Rc<LabelService>],
)]
#[derive(Encode, Decode)]
pub struct Reissue {
    pub label: String,
}

#[derive(Default)]
pub struct LabelService(Mutex<LabelState>);

#[derive(Default)]
struct LabelState {
    pending: BTreeMap<String, OperationToken>,
    resume_log: Vec<String>,
    reject_restore_operation: bool,
}

impl LabelService {
    pub fn token(&self, label: &str) -> Option<OperationToken> {
        self.0
            .lock()
            .expect("label lock")
            .pending
            .get(label)
            .copied()
    }

    pub fn pending_labels(&self) -> Vec<String> {
        self.0
            .lock()
            .expect("label lock")
            .pending
            .keys()
            .cloned()
            .collect()
    }

    pub fn resume_log(&self) -> Vec<String> {
        self.0.lock().expect("label lock").resume_log.clone()
    }

    pub fn set_reject_restore_operation(&self, reject: bool) {
        self.0.lock().expect("label lock").reject_restore_operation = reject;
    }

    pub fn register(
        self: &Rc<Self>,
        label: String,
        token: OperationToken,
        restoring: bool,
    ) -> Result<LabelRegistration, HostFault> {
        let mut state = self.0.lock().expect("label lock");
        if restoring {
            if state.reject_restore_operation {
                return Err(HostFault::new("restore rejected by host"));
            }
            state.resume_log.push(label.clone());
        }
        state.pending.insert(label.clone(), token);
        Ok(LabelRegistration {
            service: Rc::clone(self),
            label,
            token,
        })
    }
}

pub struct LabelRegistration {
    service: Rc<LabelService>,
    label: String,
    token: OperationToken,
}

impl<O: Operation> Registration<O> for LabelRegistration {}

impl Drop for LabelRegistration {
    fn drop(&mut self) {
        let mut state = self.service.0.lock().expect("label lock");
        if state.pending.get(&self.label) == Some(&self.token) {
            state.pending.remove(&self.label);
        }
    }
}

macro_rules! label_driver {
    ($($operation:ty),+ $(,)?) => {
        $(impl OperationDriver for $operation {
            type Live = LabelRegistration;

            fn start(&self, token: OperationToken, services: &Services<'_, Self>) -> Result<Self::Live, HostFault> {
                register_label::<Self>(
                    <$operation>::labels(services), &self.label, token, false,
                )
            }

            fn resume(&self, token: OperationToken, services: &Services<'_, Self>) -> Result<Self::Live, HostFault> {
                register_label::<Self>(
                    <$operation>::labels(services), &self.label, token, true,
                )
            }
        })+
    };
}

label_driver!(Wait, Pause, Reject, Reissue);

fn register_label<O: Operation>(
    service: &Rc<LabelService>,
    label: &str,
    token: OperationToken,
    restoring: bool,
) -> Result<LabelRegistration, HostFault> {
    service.register(format!("{}:{label}", O::NAME), token, restoring)
}

#[derive(Debug, PartialEq)]
pub enum RejectionEvent {
    Unhandled { promise: JsValue, reason: JsValue },
}

#[derive(Default)]
pub struct CapturedDiagnostics {
    errors: RefCell<Vec<JsValue>>,
    rejections: RefCell<Vec<RejectionEvent>>,
}

impl Diagnostics for CapturedDiagnostics {
    fn report(&self, event: Diagnostic<'_>) {
        match event {
            Diagnostic::Uncaught { error, .. } => self.errors.borrow_mut().push(error),
            Diagnostic::UnhandledRejection { promise, reason } => self
                .rejections
                .borrow_mut()
                .push(RejectionEvent::Unhandled { promise, reason }),
            _ => {}
        }
    }
}

#[derive(Default)]
pub struct CaseRoot(RefCell<Option<yield_vm::Value>>);

impl GcRoots for CaseRoot {
    fn visit(&self, visit: &mut dyn FnMut(&yield_vm::Value)) {
        self.0.borrow().iter().for_each(visit);
    }
}

#[derive(Default)]
pub struct CapturedConsole(Mutex<Vec<ConsoleEvent>>);

impl ConsoleSink for CapturedConsole {
    fn write(&self, event: &ConsoleEvent) {
        self.0.lock().expect("console lock").push(event.clone());
    }
}

// Test-only repeating sequence, deliberately not a cryptographic entropy source.
#[derive(Clone, Copy)]
pub struct DeterministicEntropy;

impl CryptoService for DeterministicEntropy {
    fn fill_random(&self, bytes: &mut [u8]) -> Result<(), HostFault> {
        for (slot, byte) in bytes.iter_mut().zip((0..=u8::MAX).cycle()) {
            *slot = byte;
        }
        Ok(())
    }
}

pub type TestPlatform<
    F = Absent,
    E = DeterministicEntropy,
    T = TestTimeService,
    S = TestSubtleCrypto,
> = Platform<
    Arc<CapturedConsole>,
    E,
    Rc<CapturedDiagnostics>,
    F,
    Rc<CaseRoot>,
    S,
    Arc<T>,
    Arc<FakeClock>,
>;

pub type TestVm<F = Absent, E = DeterministicEntropy, T = TestTimeService, S = TestSubtleCrypto> =
    Vm<TestPlatform<F, E, T, S>>;

pub struct TestHost<F = Absent, E = DeterministicEntropy, T = TestTimeService, S = TestSubtleCrypto>
{
    counters: Rc<Counters>,
    labels: Rc<LabelService>,
    clock: Arc<FakeClock>,
    console: Option<Arc<CapturedConsole>>,
    time: Arc<T>,
    timers: bool,
    fetch: Option<F>,
    crypto: Option<E>,
    subtle: Option<S>,
    user_agent: Option<String>,
    diagnostics: Rc<CapturedDiagnostics>,
    case_result: Rc<CaseRoot>,
    pub reject_native: bool,
    fetch_environment: yield_vm::FetchEnvironment,
}

/// Clock-backed wall-clock time with a mutable local zone.
pub struct TestTimeService {
    clock: Arc<FakeClock>,
    zone: std::sync::RwLock<String>,
}

impl TestTimeService {
    fn new(clock: Arc<FakeClock>) -> Arc<Self> {
        Arc::new(Self {
            clock,
            zone: std::sync::RwLock::new("Etc/UTC".into()),
        })
    }

    pub fn set_time_zone(&self, time_zone: impl Into<String>) {
        *self.zone.write().expect("time zone lock") = time_zone.into();
    }
}

impl yield_vm::host::TimeService for TestTimeService {
    fn now_ms(&self) -> i64 {
        self.clock.now_ms()
    }

    fn time_zone(&self) -> std::borrow::Cow<'_, str> {
        std::borrow::Cow::Owned(self.zone.read().expect("time zone lock").clone())
    }
}

impl Default for TestHost {
    fn default() -> Self {
        Self::new()
    }
}

impl TestHost {
    pub fn new() -> Self {
        Self::with_services(
            None,
            DeterministicEntropy,
            TestTimeService::new,
            TestSubtleCrypto,
        )
    }

    /// A host whose time service is the supplied adapter.
    pub fn with_time_service<T: TimeService + 'static>(
        time: Arc<T>,
    ) -> TestHost<Absent, DeterministicEntropy, T> {
        TestHost::with_services(None, DeterministicEntropy, |_| time, TestSubtleCrypto)
    }

    pub fn without_timer_service() -> Self {
        Self {
            timers: false,
            ..Self::new()
        }
    }

    pub fn without_console_service() -> Self {
        Self {
            console: None,
            ..Self::new()
        }
    }

    pub fn without_crypto_service() -> Self {
        Self {
            crypto: None,
            ..Self::new()
        }
    }

    pub fn with_crypto_service<E: CryptoService + 'static>(
        service: Arc<E>,
    ) -> TestHost<Absent, Arc<E>> {
        TestHost::with_services(None, service, TestTimeService::new, TestSubtleCrypto)
    }

    /// Leaves the entropy service enabled but omits the SubtleCrypto adapter.
    pub fn without_subtle_service() -> Self {
        Self {
            subtle: None,
            ..Self::new()
        }
    }

    /// Replaces the default SubtleCrypto adapter without changing entropy.
    pub fn with_subtle_service<S: SubtleCryptoService + Clone + 'static>(
        service: S,
    ) -> TestHost<Absent, DeterministicEntropy, TestTimeService, S> {
        TestHost::with_services(None, DeterministicEntropy, TestTimeService::new, service)
    }

    /// A host that also registers an explicit Fetch adapter.
    pub fn with_fetch_service<F: FetchService + 'static>(service: Arc<F>) -> TestHost<Arc<F>> {
        TestHost::with_services(
            Some(service),
            DeterministicEntropy,
            TestTimeService::new,
            TestSubtleCrypto,
        )
    }
}

impl<F, E, T, S> TestHost<F, E, T, S>
where
    F: FetchService + Clone + 'static,
    E: CryptoService + Clone + 'static,
    T: TimeService + 'static,
    S: SubtleCryptoService + Clone + 'static,
{
    fn with_services(
        fetch: Option<F>,
        crypto: E,
        time: impl FnOnce(Arc<FakeClock>) -> Arc<T>,
        subtle: S,
    ) -> Self {
        let clock = Arc::new(FakeClock::default());
        let time = time(clock.clone());
        Self {
            counters: Rc::default(),
            labels: Rc::default(),
            clock,
            console: Some(Arc::default()),
            time,
            timers: true,
            fetch,
            crypto: Some(crypto),
            subtle: Some(subtle),
            user_agent: None,
            diagnostics: Rc::default(),
            case_result: Rc::default(),
            reject_native: false,
            fetch_environment: yield_vm::FetchEnvironment::default(),
        }
    }

    /// Assembles a VM host that shares this controller's resource handles.
    pub fn host(&self) -> Host<TestPlatform<F, E, T, S>> {
        let host = host! {
            dts: "../test.d.ts",
            modules: [TestModule],
            services: {
                Rc<Counters> => self.counters.clone(),
                NodeFactory => NodeFactory,
                Rc<LabelService> => self.labels.clone(),
                Arc<FakeClock> => self.clock.clone(),
            },
        }
        .expect("test host");
        host.with_platform(Platform {
            console: self.console.clone(),
            crypto: self.crypto.clone(),
            diagnostics: Some(self.diagnostics.clone()),
            fetch: self.fetch.clone(),
            gc_roots: Some(self.case_result.clone()),
            subtle_crypto: self.subtle.clone(),
            time: Some(self.time.clone()),
            timer: self.timers.then(|| self.clock.clone()),
            user_agent: self.user_agent.clone().map(UserAgent),
        })
    }

    /// Origin, referrer, partition and authorization recorded in snapshots.
    pub fn fetch_environment(&self) -> yield_vm::FetchEnvironment {
        self.fetch_environment.clone()
    }

    pub fn set_fetch_environment(&mut self, environment: yield_vm::FetchEnvironment) {
        self.fetch_environment = environment;
    }

    pub fn count(&self, name: &str) -> usize {
        self.counters.count(name)
    }

    /// Borrows uncaught errors. Drop the guard before running the VM again.
    pub fn errors(&self) -> Ref<'_, [JsValue]> {
        Ref::map(self.diagnostics.errors.borrow(), Vec::as_slice)
    }

    /// Borrows rejection reports. Drop the guard before running the VM again.
    pub fn rejection_events(&self) -> Ref<'_, [RejectionEvent]> {
        Ref::map(self.diagnostics.rejections.borrow(), Vec::as_slice)
    }

    pub fn take_console_events(&self) -> Vec<ConsoleEvent> {
        self.console.as_ref().map_or_else(Vec::new, |sink| {
            std::mem::take(&mut *sink.0.lock().expect("console lock"))
        })
    }

    /// Stores the case result, if any, and keeps it alive during guest GC.
    /// The value must belong to the VM this host is about to run.
    pub fn set_case_result(&mut self, result: Option<yield_vm::Value>) {
        *self.case_result.0.borrow_mut() = result;
    }

    pub fn case_result(&self) -> Option<Ref<'_, yield_vm::Value>> {
        Ref::filter_map(self.case_result.0.borrow(), Option::as_ref).ok()
    }

    pub fn labels(&self) -> &Rc<LabelService> {
        &self.labels
    }

    pub fn clock(&self) -> &Arc<FakeClock> {
        &self.clock
    }

    /// The time service registered in each assembled host.
    pub fn time_service(&self) -> &Arc<T> {
        &self.time
    }

    /// Overrides the value exposed as `navigator.userAgent` for new VMs.
    pub fn set_user_agent(&mut self, user_agent: impl Into<String>) {
        self.user_agent = Some(user_agent.into());
    }

    pub fn advance(&mut self, vm: &mut Vm<impl PlatformServices>, ms: u64) -> Result<(), String> {
        self.clock.advance(vm, ms)
    }

    /// Runs a VM turn and waits for accepted SubtleCrypto work to be delivered.
    /// Other host operations remain pending until their normal delivery turns.
    /// Returns a host error if SubtleCrypto work is still pending after five seconds.
    pub fn drain_subtle(&mut self, vm: &mut Vm<impl PlatformServices>) -> Result<(), VmError> {
        vm.run_until_idle()?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while vm.has_pending_subtle_operations() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(VmError::Host(
                    "timed out draining SubtleCrypto operations".into(),
                ));
            }
            std::thread::sleep(remaining.min(Duration::from_millis(1)));
            vm.run_until_idle()?;
        }
        Ok(())
    }

    pub fn pending_labels(&self) -> Vec<String> {
        self.labels.pending_labels()
    }

    pub fn token(&self, label: &str) -> Option<OperationToken> {
        self.labels.token(label)
    }

    pub fn complete(
        &mut self,
        vm: &mut Vm<impl PlatformServices>,
        label: &str,
        completion: Completion,
    ) -> Result<(), String> {
        let token = self
            .token(label)
            .ok_or_else(|| format!("unknown operation {label:?}"))?;
        vm.complete_operation(token, completion)
            .map_err(|error| error.to_string())
    }
}

impl<F, E, S> TestHost<F, E, TestTimeService, S> {
    /// Sets the local time zone of the default clock-backed time service.
    pub fn set_time_zone(&mut self, time_zone: impl Into<String>) {
        self.time.set_time_zone(time_zone);
    }
}

#[host_class(tag = "test:ParseError", parent = yield_vm::intrinsics::Error, prototype_name = "ParseError")]
struct ParseError;

#[host_class(tag = "test:ErrorBase", parent = yield_vm::intrinsics::Error, methods)]
struct ErrorBase {
    #[js_slot]
    detail: JsValue,
}

#[methods(ErrorBase)]
impl ErrorBase {
    #[method(name = "detail")]
    fn detail(cx: &mut CallCx<'_>, this: This<Self>) -> JsResult<JsValue> {
        this.detail(cx)
    }
}

#[host_class(tag = "test:ErrorDerived", parent = ErrorBase)]
struct ErrorDerived;

#[host_class(tag = "test:HostDOMException", parent = yield_vm::intrinsics::DOMException)]
struct HostDOMException;

#[host_class(tag = "test:HostTypeError", parent = TypeError)]
struct HostTypeError;

#[host_module(
    name = "test",
    classes = [Node, Matcher, ParseError, ErrorBase, ErrorDerived, HostDOMException, HostTypeError],
    operations = [Wait, Pause, Reject, Reissue, Timer],
    services = [counters: Rc<Counters>],
)]
struct TestModule;

#[module_exports]
impl TestModule {
    #[constructor(name = "ParseError", class = ParseError)]
    fn parse_error(
        cx: &mut yield_vm::ModuleCx<'_, Self>,
        message: Option<JsValue>,
    ) -> JsResult<Instance<ParseError>> {
        let args = [
            message.unwrap_or_else(JsValue::undefined),
            cx.args().get(1).cloned().unwrap_or_else(JsValue::undefined),
        ];
        cx.alloc_with_super(ParseError, &args)
    }

    #[constructor(name = "ErrorBase", class = ErrorBase)]
    fn error_base(cx: &mut yield_vm::ModuleCx<'_, Self>) -> JsResult<Instance<ErrorBase>> {
        cx.alloc(ErrorBase {})
    }

    #[constructor(name = "ErrorDerived", class = ErrorDerived)]
    fn error_derived(
        cx: &mut yield_vm::ModuleCx<'_, Self>,
        detail: JsValue,
    ) -> JsResult<Instance<ErrorDerived>> {
        let instance = cx.alloc(ErrorDerived)?;
        let value = instance.value(cx);
        cx.instance::<ErrorBase>(&value)?.set_detail(cx, detail)?;
        Ok(instance)
    }

    #[constructor(name = "HostDOMException", class = HostDOMException)]
    fn host_dom_exception(
        cx: &mut yield_vm::ModuleCx<'_, Self>,
        message: Option<JsValue>,
        name: Option<JsValue>,
    ) -> JsResult<Instance<HostDOMException>> {
        cx.alloc_with_super(
            HostDOMException,
            &[
                message.unwrap_or_else(JsValue::undefined),
                name.unwrap_or_else(JsValue::undefined),
            ],
        )
    }

    #[constructor(name = "HostTypeError", class = HostTypeError)]
    fn host_type_error(
        cx: &mut yield_vm::ModuleCx<'_, Self>,
        message: JsValue,
        options: Option<JsValue>,
    ) -> JsResult<Instance<HostTypeError>> {
        cx.alloc_with_super(
            HostTypeError,
            &[message, options.unwrap_or_else(JsValue::undefined)],
        )
    }

    #[function(name = "getTypeErrorConstructor")]
    fn get_type_error_constructor(cx: &mut yield_vm::ModuleCx<'_, Self>) -> JsResult<JsValue> {
        cx.constructor::<TypeError>()
    }

    #[function(name = "throwTypeError")]
    fn throw_intrinsic_type_error(
        cx: &mut yield_vm::ModuleCx<'_, Self>,
        message: JsValue,
        options: Option<JsValue>,
    ) -> JsResult<()> {
        let error =
            cx.construct::<TypeError>(&[message, options.unwrap_or_else(JsValue::undefined)])?;
        Err(cx.throw(error))
    }

    #[function(name = "allocError")]
    fn allocate_parse_error(
        cx: &mut yield_vm::ModuleCx<'_, Self>,
        message: Option<JsValue>,
    ) -> JsResult<JsValue> {
        let message = match message {
            Some(value) => cx.string(&value)?.clone(),
            None => JsString::from("host allocError"),
        };
        Ok(cx
            .alloc_with_super(ParseError, &[JsValue::string(message)])?
            .value(cx))
    }

    #[function(name = "lazyThrow")]
    fn lazy_throw(cx: &mut yield_vm::ModuleCx<'_, Self>) -> JsResult<()> {
        let error = cx.alloc_with_super(ParseError, &[JsValue::string("lazy parse failure")])?;
        Err(cx.throw(error.value(cx)))
    }
    #[function(name = "spawnDelayedTask")]
    fn spawn_delayed_task(
        cx: &mut yield_vm::ModuleCx<'_, Self>,
        cb: JsValue,
        ms: JsValue,
    ) -> JsResult<JsValue> {
        if !cx.is_callable(&cb)? {
            return Err(throw_type_error(cx, "expected a callable"));
        }
        let timer = Timer::from_delay(cx, &ms)?;
        cx.spawn(async move {
            cx.effect(timer).await?;
            cx.call(cb, JsValue::undefined(), Vec::new()).await?;
            Ok(())
        })
    }

    #[function(name = "cancelDelayedTask")]
    fn cancel_delayed_task(
        cx: &mut yield_vm::ModuleCx<'_, Self>,
        handle: Option<JsValue>,
    ) -> JsResult<()> {
        if let Some(h) = handle {
            cx.cancel_task(&h)?;
        }
        Ok(())
    }

    #[function(name = "sleep")]
    fn sleep(cx: &mut yield_vm::ModuleCx<'_, Self>, ms: JsValue) -> JsResult<JsValue> {
        let timer = Timer::from_delay(cx, &ms)?;
        cx.promise(timer)
    }

    #[function(name = "gc")]
    async fn gc(cx: &mut yield_vm::ModuleCx<'_, Self>) -> JsResult<HostStep> {
        let outcome = cx.gc().await;
        match outcome {
            GuestOutcome::Returned(value) => Ok(HostStep::Return(value)),
            GuestOutcome::Thrown(value) => Err(cx.throw(value)),
        }
    }

    #[function(name = "inspect")]
    fn inspect(cx: &mut yield_vm::ModuleCx<'_, Self>, _value: Option<JsValue>) -> JsResult<()> {
        Self::counters(cx.services()).hit("inspect");
        Ok(())
    }

    #[function(name = "seed")]
    fn seed(cx: &mut yield_vm::ModuleCx<'_, Self>) -> JsResult<JsValue> {
        Self::counters(cx.services()).hit("seed");
        Ok(JsValue::number(10.0))
    }

    #[function(name = "wait")]
    fn wait(cx: &mut yield_vm::ModuleCx<'_, Self>, arg: Option<JsValue>) -> JsResult<JsValue> {
        Self::counters(cx.services()).hit("wait");
        let arg = match arg {
            Some(value) => value,
            None => cx.arg(0)?,
        };
        let label = cx
            .string(&arg)?
            .to_utf8()
            .map_err(|_| throw_type_error(cx, "wait requires a valid UTF-16 label"))?;
        cx.promise(Wait { label })
    }

    #[function(name = "pause")]
    fn pause(cx: &mut yield_vm::ModuleCx<'_, Self>, arg: JsValue) -> JsResult<JsValue> {
        let label = cx
            .string(&arg)?
            .to_utf8()
            .map_err(|_| throw_type_error(cx, "pause requires a valid UTF-16 label"))?;
        cx.promise(Pause { label })
    }

    #[function(name = "rejectAfterRestore")]
    fn reject_after_restore(
        cx: &mut yield_vm::ModuleCx<'_, Self>,
        arg: JsValue,
    ) -> JsResult<JsValue> {
        let label = cx
            .string(&arg)?
            .to_utf8()
            .map_err(|_| throw_type_error(cx, "reject requires a valid UTF-16 label"))?;
        cx.promise(Reject { label })
    }

    #[function(name = "reissue")]
    fn reissue(cx: &mut yield_vm::ModuleCx<'_, Self>, arg: JsValue) -> JsResult<JsValue> {
        let label = cx
            .string(&arg)?
            .to_utf8()
            .map_err(|_| throw_type_error(cx, "reissue requires a valid UTF-16 label"))?;
        cx.promise(Reissue { label })
    }

    #[function(name = "bind")]
    fn bind(cx: &mut yield_vm::ModuleCx<'_, Self>, arg: JsValue) -> JsResult<Instance<Node>> {
        let label = cx
            .string(&arg)?
            .to_utf8()
            .map_err(|_| throw_type_error(cx, "bind requires a valid UTF-16 label"))?;
        cx.alloc(Node { label })
    }

    #[function(name = "setLabel")]
    fn set_label(
        cx: &mut yield_vm::ModuleCx<'_, Self>,
        _node: Option<JsValue>,
        _label: Option<JsValue>,
    ) -> JsResult<()> {
        Self::counters(cx.services()).hit("setLabel");
        Ok(())
    }

    #[function(name = "expect")]
    fn expect(
        cx: &mut yield_vm::ModuleCx<'_, Self>,
        actual: Option<JsValue>,
    ) -> JsResult<Instance<Matcher>> {
        let counters = Self::counters(cx.services());
        counters.hit("expect");
        let actual = actual.unwrap_or_else(JsValue::undefined);
        let matcher = cx.alloc(Matcher)?;
        if !actual.is_undefined() {
            counters.hit("expect:value");
        }
        matcher.set_actual(cx, actual)?;
        Ok(matcher)
    }

    #[function(name = "lastArgument")]
    fn last_argument(_: &mut yield_vm::ModuleCx<'_, Self>, rest: &[JsValue]) -> JsResult<JsValue> {
        Ok(rest.last().cloned().unwrap_or_else(JsValue::null))
    }

    #[function(name = "restAt")]
    async fn rest_at(
        cx: &mut yield_vm::ModuleCx<'_, Self>,
        index: JsValue,
        optional: Option<JsValue>,
        rest: &[JsValue],
    ) -> JsResult<HostStep> {
        let index = cx.number(&index)?;
        let value = if index == -1.0 {
            optional.unwrap_or_else(JsValue::null)
        } else {
            rest.get(index as usize)
                .cloned()
                .unwrap_or_else(JsValue::null)
        };
        cx.gc().await?;
        Ok(HostStep::Return(value))
    }

    #[function(name = "assert")]
    fn assert(
        cx: &mut yield_vm::ModuleCx<'_, Self>,
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
        Err(matcher_error(
            cx,
            format!("assert: expected truthy, actual {actual}{message}"),
        ))
    }
}

fn compile_source(source: &str) -> Result<Build, CompileError> {
    let resolver = MemoryResolver::new([("main.js", source)]).expect("valid fixture path");
    let options =
        Compiler::from_dts(include_str!("../test.d.ts")).expect("test declarations parse");
    options.compile(Project::new("main.js", resolver))
}

pub fn build(source: &str) -> Build {
    compile_source(source).expect("source compiles")
}

pub fn number(value: f64) -> JsValue {
    JsValue::number(value)
}

pub fn string(value: &str) -> JsValue {
    JsValue::string(value)
}
