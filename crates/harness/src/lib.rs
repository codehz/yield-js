use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use yield_compiler::{CompileError, Compiler, MemoryResolver, Project};
use yield_vm::binding::{
    Bindings, CallCx, CodecError, HostFault, InspectKey, InspectValue, Instance, JsResult, JsValue,
    Operation, OperationDriver, OperationToken, Registration, This,
};
use yield_vm::bytecode::Build;
use yield_vm::host::Completion;
use yield_vm::{
    ConsoleEvent, ConsoleKey, ConsoleSink, CryptoKey, CryptoService, FetchKey, GuestOutcome, Host,
    HostStep, JsString, SubtleCryptoKey, SubtleCryptoService, TimerKey, Vm, VmError,
};
use yield_vm::{
    Decode, Encode, host_bindings, host_class, host_module, methods, module_exports, operation,
};

mod case;
pub mod fetch;
mod subtle;
pub mod timer;

pub use case::run_case;
pub use timer::{FakeClock, Timer};

#[derive(Default)]
pub struct Counters(Mutex<BTreeMap<String, usize>>);

pub struct CountersKey;

impl yield_vm::ServiceKey for CountersKey {
    type Value = Arc<Counters>;
    const ID: &'static str = "counters";
    const VERSION: u32 = 1;
}

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

pub struct NodeFactoryKey;

impl yield_vm::ServiceKey for NodeFactoryKey {
    type Value = NodeFactory;
    const ID: &'static str = "node-factory";
    const VERSION: u32 = 1;
}

#[host_class(tag = "Node", methods)]
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

impl Decode for Node {
    fn decode(input: &mut &[u8], bindings: &Bindings) -> Result<Self, CodecError> {
        if bindings.service::<NodeFactoryKey>().is_none() {
            return Err(CodecError::new("node factory service is unavailable"));
        }
        let label = String::decode(input, bindings)
            .map_err(|error| CodecError::new(format!("field label: {error}")))?;
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

#[operation(name = "wait", recovery = resume, rejection = "wait rejected")]
#[derive(Encode, Decode)]
pub struct Wait {
    pub label: String,
}

#[operation(name = "pause", recovery = pause, rejection = "pause rejected")]
#[derive(Encode, Decode)]
pub struct Pause {
    pub label: String,
}

#[operation(name = "reject", recovery = reject, rejection = "reject rejected during restore")]
#[derive(Encode, Decode)]
pub struct Reject {
    pub label: String,
}

#[operation(name = "reissue", recovery = reissue, rejection = "reissue rejected")]
#[derive(Encode, Decode)]
pub struct Reissue {
    pub label: String,
}

#[derive(Default)]
pub struct LabelService(Mutex<LabelState>);

pub struct LabelServiceKey;

impl yield_vm::ServiceKey for LabelServiceKey {
    type Value = Arc<LabelService>;
    const ID: &'static str = "labels";
    const VERSION: u32 = 1;
}

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
        self: &Arc<Self>,
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
            service: Arc::clone(self),
            label,
            token,
        })
    }
}

pub struct LabelRegistration {
    service: Arc<LabelService>,
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

            fn start(&self, token: OperationToken, bindings: &Bindings) -> Result<Self::Live, HostFault> {
                register_label::<Self>(bindings, &self.label, token, false)
            }

            fn resume(&self, token: OperationToken, bindings: &Bindings) -> Result<Self::Live, HostFault> {
                register_label::<Self>(bindings, &self.label, token, true)
            }
        })+
    };
}

label_driver!(Wait, Pause, Reject, Reissue);

fn register_label<O: Operation>(
    bindings: &Bindings,
    label: &str,
    token: OperationToken,
    restoring: bool,
) -> Result<LabelRegistration, HostFault> {
    let service = bindings
        .service::<LabelServiceKey>()
        .ok_or_else(|| HostFault::new("missing labels service"))?;
    service.register(format!("{}:{label}", O::NAME), token, restoring)
}

#[derive(Debug, PartialEq)]
pub enum RejectionEvent {
    Unhandled { promise: JsValue, reason: JsValue },
}

#[derive(Default)]
struct CapturedConsole(Mutex<Vec<ConsoleEvent>>);

impl ConsoleSink for CapturedConsole {
    fn write(&self, event: &ConsoleEvent) {
        self.0.lock().expect("console lock").push(event.clone());
    }
}

// Test-only repeating sequence, deliberately not a cryptographic entropy source.
struct DeterministicEntropy;

impl CryptoService for DeterministicEntropy {
    fn fill_random(&self, bytes: &mut [u8]) -> Result<(), HostFault> {
        for (slot, byte) in bytes.iter_mut().zip((0..=u8::MAX).cycle()) {
            *slot = byte;
        }
        Ok(())
    }
}

pub struct TestHost {
    bindings: Arc<Bindings>,
    counters: Arc<Counters>,
    labels: Arc<LabelService>,
    clock: Arc<FakeClock>,
    console: Option<Arc<CapturedConsole>>,
    time: Arc<dyn yield_vm::TimeService>,
    test_time: Option<Arc<TestTimeService>>,
    user_agent: Option<String>,
    errors: Vec<JsValue>,
    rejection_events: Vec<RejectionEvent>,
    case_result: Option<yield_vm::Value>,
    pub reject_native: bool,
    fetch_environment: yield_vm::FetchEnvironment,
}

/// Clock-backed wall-clock time with a mutable local zone.
pub struct TestTimeService {
    clock: Arc<FakeClock>,
    zone: std::sync::RwLock<String>,
}

impl TestTimeService {
    fn new(clock: Arc<FakeClock>) -> Self {
        Self {
            clock,
            zone: std::sync::RwLock::new("Etc/UTC".into()),
        }
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
            true,
            None,
            true,
            Some(Arc::new(DeterministicEntropy)),
            Self::default_subtle_service(),
        )
    }

    /// A host whose `wintertc.time` service is the supplied adapter.
    pub fn with_time_service(time: Arc<dyn yield_vm::TimeService>) -> Self {
        Self::with_services(
            Some(time),
            true,
            None,
            true,
            Some(Arc::new(DeterministicEntropy)),
            Self::default_subtle_service(),
        )
    }

    pub fn without_timer_service() -> Self {
        Self::with_services(
            None,
            false,
            None,
            true,
            Some(Arc::new(DeterministicEntropy)),
            Self::default_subtle_service(),
        )
    }

    pub fn without_console_service() -> Self {
        Self::with_services(
            None,
            true,
            None,
            false,
            Some(Arc::new(DeterministicEntropy)),
            Self::default_subtle_service(),
        )
    }

    pub fn without_crypto_service() -> Self {
        Self::with_services(None, true, None, true, None, Self::default_subtle_service())
    }

    pub fn with_crypto_service(service: Arc<dyn CryptoService>) -> Self {
        Self::with_services(
            None,
            true,
            None,
            true,
            Some(service),
            Self::default_subtle_service(),
        )
    }

    /// Leaves the entropy service enabled but omits the SubtleCrypto adapter.
    pub fn without_subtle_service() -> Self {
        Self::with_services(
            None,
            true,
            None,
            true,
            Some(Arc::new(DeterministicEntropy)),
            None,
        )
    }

    /// Replaces the default SubtleCrypto adapter without changing entropy.
    pub fn with_subtle_service(service: Arc<dyn SubtleCryptoService>) -> Self {
        Self::with_services(
            None,
            true,
            None,
            true,
            Some(Arc::new(DeterministicEntropy)),
            Some(service),
        )
    }

    fn default_subtle_service() -> Option<Arc<dyn SubtleCryptoService>> {
        Some(Arc::new(subtle::TestSubtleCrypto))
    }

    /// A host that also registers an explicit Fetch adapter.
    pub fn with_fetch_service(service: Arc<dyn yield_vm::fetch::FetchService>) -> Self {
        Self::with_services(
            None,
            true,
            Some(service),
            true,
            Some(Arc::new(DeterministicEntropy)),
            Self::default_subtle_service(),
        )
    }

    fn with_services(
        time: Option<Arc<dyn yield_vm::TimeService>>,
        timers: bool,
        fetch: Option<Arc<dyn yield_vm::fetch::FetchService>>,
        capture_console: bool,
        crypto: Option<Arc<dyn CryptoService>>,
        subtle: Option<Arc<dyn SubtleCryptoService>>,
    ) -> Self {
        let counters = Arc::new(Counters::default());
        let labels = Arc::new(LabelService::default());
        let clock = Arc::new(FakeClock::default());
        let test_time = time
            .is_none()
            .then(|| Arc::new(TestTimeService::new(Arc::clone(&clock))));
        let time = test_time
            .clone()
            .map(|time| time as Arc<dyn yield_vm::TimeService>)
            .or(time)
            .expect("a time service");
        let console = capture_console.then(|| Arc::new(CapturedConsole::default()));
        let sink = console.clone().map(|sink| -> Arc<dyn ConsoleSink> { sink });
        let bindings = test_bindings(
            (Arc::clone(&counters), Arc::clone(&labels)),
            &clock,
            timers,
            fetch,
            sink,
            crypto,
            subtle,
            Arc::clone(&time),
        )
        .expect("test bindings");
        Self {
            bindings: Arc::new(bindings),
            counters,
            labels,
            clock,
            console,
            time,
            test_time,
            user_agent: None,
            errors: Vec::new(),
            rejection_events: Vec::new(),
            case_result: None,
            reject_native: false,
            fetch_environment: yield_vm::FetchEnvironment::default(),
        }
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

    pub fn errors(&self) -> &[JsValue] {
        &self.errors
    }

    pub fn rejection_events(&self) -> &[RejectionEvent] {
        &self.rejection_events
    }

    pub fn take_console_events(&self) -> Vec<ConsoleEvent> {
        self.console.as_ref().map_or_else(Vec::new, |sink| {
            std::mem::take(&mut *sink.0.lock().expect("console lock"))
        })
    }

    /// Stores the case result, if any, and keeps it alive during guest GC.
    /// The value must belong to the VM this host is about to run.
    pub fn set_case_result(&mut self, result: Option<yield_vm::Value>) {
        self.case_result = result;
    }

    pub fn case_result(&self) -> Option<&yield_vm::Value> {
        self.case_result.as_ref()
    }

    pub fn labels(&self) -> &Arc<LabelService> {
        &self.labels
    }

    pub fn clock(&self) -> &Arc<FakeClock> {
        &self.clock
    }

    /// The `wintertc.time` service linked into this host's bindings.
    pub fn time_service(&self) -> &Arc<dyn yield_vm::TimeService> {
        &self.time
    }

    /// Sets the local time zone to the supplied IANA identifier. Only the
    /// default clock-backed time service accepts zone changes.
    pub fn set_time_zone(&mut self, time_zone: impl Into<String>) {
        if let Some(time) = &self.test_time {
            time.set_time_zone(time_zone);
        }
    }

    /// Overrides the value exposed as `navigator.userAgent` for new VMs.
    pub fn set_user_agent(&mut self, user_agent: impl Into<String>) {
        self.user_agent = Some(user_agent.into());
    }

    pub fn advance(&mut self, vm: &mut Vm, ms: u64) -> Result<(), String> {
        Arc::clone(&self.clock).advance(vm, self, ms)
    }

    /// Runs a VM turn and waits for accepted SubtleCrypto work to be delivered.
    /// Other host operations remain pending until their normal delivery turns.
    /// Returns a host error if SubtleCrypto work is still pending after five seconds.
    pub fn drain_subtle(&mut self, vm: &mut Vm) -> Result<(), VmError> {
        vm.run_until_idle(self)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while vm.has_pending_subtle_operations() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(VmError::Host(
                    "timed out draining SubtleCrypto operations".into(),
                ));
            }
            std::thread::sleep(remaining.min(Duration::from_millis(1)));
            vm.run_until_idle(self)?;
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
        vm: &mut Vm,
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

impl Host for TestHost {
    fn bindings(&self) -> Arc<Bindings> {
        self.bindings.clone()
    }

    fn user_agent(&self) -> Option<&str> {
        self.user_agent.as_deref()
    }

    fn visit_gc_roots(&self, visit: &mut dyn FnMut(&yield_vm::Value)) {
        self.case_result.iter().for_each(visit);
    }

    fn report_error(&mut self, error: JsValue) {
        self.errors.push(error);
    }

    fn unhandled_rejection(&mut self, promise: JsValue, reason: JsValue) {
        self.rejection_events
            .push(RejectionEvent::Unhandled { promise, reason });
    }
}

#[allow(clippy::too_many_arguments)]
fn test_bindings(
    (counters, labels): (Arc<Counters>, Arc<LabelService>),
    clock: &Arc<FakeClock>,
    timers: bool,
    fetch: Option<Arc<dyn yield_vm::fetch::FetchService>>,
    console: Option<Arc<dyn ConsoleSink>>,
    crypto: Option<Arc<dyn CryptoService>>,
    subtle: Option<Arc<dyn SubtleCryptoService>>,
    time: Arc<dyn yield_vm::TimeService>,
) -> Result<Bindings, String> {
    host_bindings! {
        dts: "../test.d.ts",
        modules: [TestModule],
        services: {
            CountersKey => counters,
            NodeFactoryKey => NodeFactory,
            LabelServiceKey => labels,
            timer::FakeClockKey => Arc::clone(clock),
            yield_vm::host::TimeKey => time,
            TimerKey? => timers.then(|| clock.clone()),
            FetchKey? => fetch,
            ConsoleKey? => console,
            CryptoKey? => crypto,
            SubtleCryptoKey? => subtle,
        },
    }
}

#[host_module(
    name = "test",
    classes = [Node, Matcher],
    operations = [Wait, Pause, Reject, Reissue, Timer],
)]
struct TestModule;

#[module_exports]
impl TestModule {
    #[function(name = "spawnDelayedTask")]
    fn spawn_delayed_task(
        cx: &mut yield_vm::ModuleCx<'_, Self>,
        cb: JsValue,
        ms: JsValue,
    ) -> JsResult<JsValue> {
        if !cx.is_callable(&cb)? {
            return Err(cx.type_error("expected a callable"));
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
        if let Some(counters) = cx.service::<CountersKey>() {
            counters.hit("inspect");
        }
        Ok(())
    }

    #[function(name = "seed")]
    fn seed(cx: &mut yield_vm::ModuleCx<'_, Self>) -> JsResult<JsValue> {
        if let Some(counters) = cx.service::<CountersKey>() {
            counters.hit("seed");
        }
        Ok(JsValue::number(10.0))
    }

    #[function(name = "wait")]
    fn wait(cx: &mut yield_vm::ModuleCx<'_, Self>, arg: Option<JsValue>) -> JsResult<JsValue> {
        if let Some(counters) = cx.service::<CountersKey>() {
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
        cx.promise(Wait { label })
    }

    #[function(name = "pause")]
    fn pause(cx: &mut yield_vm::ModuleCx<'_, Self>, arg: JsValue) -> JsResult<JsValue> {
        let label = cx
            .string(&arg)?
            .to_utf8()
            .map_err(|_| cx.type_error("pause requires a valid UTF-16 label"))?;
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
            .map_err(|_| cx.type_error("reject requires a valid UTF-16 label"))?;
        cx.promise(Reject { label })
    }

    #[function(name = "reissue")]
    fn reissue(cx: &mut yield_vm::ModuleCx<'_, Self>, arg: JsValue) -> JsResult<JsValue> {
        let label = cx
            .string(&arg)?
            .to_utf8()
            .map_err(|_| cx.type_error("reissue requires a valid UTF-16 label"))?;
        cx.promise(Reissue { label })
    }

    #[function(name = "bind")]
    fn bind(cx: &mut yield_vm::ModuleCx<'_, Self>, arg: JsValue) -> JsResult<Instance<Node>> {
        let label = cx
            .string(&arg)?
            .to_utf8()
            .map_err(|_| cx.type_error("bind requires a valid UTF-16 label"))?;
        cx.alloc(Node { label })
    }

    #[function(name = "setLabel")]
    fn set_label(
        cx: &mut yield_vm::ModuleCx<'_, Self>,
        _node: Option<JsValue>,
        _label: Option<JsValue>,
    ) -> JsResult<()> {
        if let Some(counters) = cx.service::<CountersKey>() {
            counters.hit("setLabel");
        }
        Ok(())
    }

    #[function(name = "expect")]
    fn expect(
        cx: &mut yield_vm::ModuleCx<'_, Self>,
        actual: Option<JsValue>,
    ) -> JsResult<Instance<Matcher>> {
        if let Some(counters) = cx.service::<CountersKey>() {
            counters.hit("expect");
        }
        let actual = actual.unwrap_or_else(JsValue::undefined);
        let matcher = cx.alloc(Matcher)?;
        if !actual.is_undefined()
            && let Some(counters) = cx.service::<CountersKey>()
        {
            counters.hit("expect:value");
        }
        matcher.set_actual(cx, actual)?;
        Ok(matcher)
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
        Err(cx.throw_string(format!("assert: expected truthy, actual {actual}{message}")))
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
