use std::cell::Cell;
use std::sync::Arc;
use yield_vm::binding::{CodecError, DecodeCx};
use yield_vm::{
    Bindings, ConsoleEvent, ConsoleKey, ConsoleSink, HostFault, HostModuleState, Operation,
    OperationDriver, OperationToken, Recovery, Registration, ServiceKey, Services, host_bindings,
    host_module, host_state, operation,
};

#[host_module(name = "state")]
mod state {}

#[host_state(module = "state", schema = 1, init = State::init, encode = State::encode, decode = State::decode)]
struct State {
    #[js_slot]
    current: JsValue,
    #[js_list]
    history: Vec<JsValue>,
    count: u8,
}

impl State {
    fn init() -> Self {
        Self { count: 3 }
    }

    fn encode(&self, out: &mut Vec<u8>) -> Result<(), CodecError> {
        out.push(self.count);
        Ok(())
    }

    fn decode(input: &[u8], _: &mut DecodeCx<'_>) -> Result<Self, CodecError> {
        match input {
            &[count] => Ok(Self { count }),
            _ => Err(CodecError::new("invalid state payload")),
        }
    }
}

#[operation(name = "work", schema = 1, recovery = pause, encode = Work::write, decode = Work::read)]
struct Work;

impl Work {
    fn write(&self, _: &mut Vec<u8>) -> Result<(), CodecError> {
        Ok(())
    }

    fn read(input: &[u8], _: &Bindings) -> Result<Self, CodecError> {
        if input.is_empty() {
            Ok(Work)
        } else {
            Err(CodecError::new("invalid work payload"))
        }
    }
}

struct Live;

impl Registration<Work> for Live {}

impl OperationDriver for Work {
    type Live = Live;

    fn start(&self, _: OperationToken, _: &Bindings) -> Result<Live, HostFault> {
        Ok(Live)
    }
}

struct ClockV2;
impl ServiceKey for ClockV2 {
    type Value = u64;
    const ID: &'static str = "clock";
    const VERSION: u32 = 2;
}

struct OtherClock;
impl ServiceKey for OtherClock {
    type Value = u64;
    const ID: &'static str = ClockV2::ID;
    const VERSION: u32 = ClockV2::VERSION;
}

struct ClockV3;
impl ServiceKey for ClockV3 {
    type Value = u64;
    const ID: &'static str = ClockV2::ID;
    const VERSION: u32 = 3;
}

struct CounterKey;
impl ServiceKey for CounterKey {
    type Value = u64;
    const ID: &'static str = "counter";
    const VERSION: u32 = 1;
}

#[test]
fn registers_state_operation_and_services() -> Result<(), String> {
    let mut services = Services::default();
    services.insert::<ClockV2>(17)?;
    assert_eq!(services.get::<OtherClock>(), None);
    assert_eq!(services.get::<CounterKey>(), None);
    assert_eq!(
        services.insert::<ClockV3>(23).unwrap_err(),
        "duplicate host service \"clock\""
    );
    assert_eq!(services.get::<ClockV2>(), Some(&17));

    let evaluations = Cell::new(0);
    let bindings = host_bindings! {
        dts: "tests/dts/state.d.ts",
        modules: [state], classes: [], states: [State], operations: [Work],
        services: { evaluations.set(evaluations.get() + 1); services },
    }?;
    assert_eq!(evaluations.get(), 1);
    assert_eq!(bindings.service::<ClockV2>(), Some(&17));
    assert_eq!(bindings.service::<OtherClock>(), None);
    assert_eq!(State::SLOTS[State::SLOT_CURRENT].name, "current");
    assert_eq!(State::SLOTS[State::SLOT_HISTORY].name, "history");
    assert_eq!(Work::RECOVERY, Recovery::Pause);
    Ok(())
}

#[test]
fn fingerprints_only_stable_service_contracts() -> Result<(), String> {
    struct Sink;
    impl ConsoleSink for Sink {
        fn write(&self, _: &ConsoleEvent) {}
    }

    let bindings = |services| {
        host_bindings! {
            dts: "tests/dts/state.d.ts",
            modules: [], classes: [], states: [], operations: [],
            services: services,
        }
    };
    let table = |clock| {
        let mut services = Services::default();
        services.insert::<ClockV2>(clock)?;
        services.insert::<CounterKey>(0)?;
        Ok::<_, String>(services)
    };
    let baseline = bindings(table(17)?)?;
    assert_eq!(baseline.fingerprint(), bindings(table(42)?)?.fingerprint());

    let mut reordered = Services::default();
    reordered.insert::<CounterKey>(0)?;
    reordered.insert::<ClockV2>(17)?;
    assert_eq!(baseline.fingerprint(), bindings(reordered)?.fingerprint());

    let mut with_console = table(17)?;
    with_console.insert::<ConsoleKey>(Arc::new(Sink))?;
    assert_eq!(
        baseline.fingerprint(),
        bindings(with_console)?.fingerprint()
    );

    let mut upgraded = Services::default();
    upgraded.insert::<ClockV3>(17)?;
    upgraded.insert::<CounterKey>(0)?;
    assert_ne!(baseline.fingerprint(), bindings(upgraded)?.fingerprint());
    Ok(())
}
