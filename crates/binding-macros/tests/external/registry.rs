use std::cell::Cell;
use std::sync::Arc;
use yield_vm::{
    Bindings, ConsoleEvent, ConsoleKey, ConsoleSink, Decode, Encode, FromBindings, HostFault,
    HostModule, Operation, OperationDriver, OperationToken, Recovery, Registration, ServiceKey,
    Services, host_bindings, host_module, module_exports, operation,
};

#[host_module(name = "state")]
#[derive(Encode, Decode)]
struct State {
    #[js_slot]
    current: JsValue,
    #[js_list]
    history: Vec<JsValue>,
    count: u8,
}

impl Default for State {
    fn default() -> Self {
        Self { count: 3 }
    }
}

#[module_exports]
impl State {}

#[operation(name = "work", recovery = pause)]
#[derive(Encode, Decode)]
struct Work;

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
        dts: "../dts/state.d.ts",
        modules: [State], classes: [], operations: [Work],
        services: {
            ClockV2 => { evaluations.set(evaluations.get() + 1); 17 },
        },
    }?;
    assert_eq!(evaluations.get(), 1);
    assert_eq!(<State as FromBindings>::from_bindings(&bindings).count, 3);
    assert_eq!(bindings.service::<ClockV2>(), Some(&17));
    assert_eq!(bindings.service::<OtherClock>(), None);
    assert_eq!(State::SLOTS[State::SLOT_CURRENT].name, "current");
    assert_eq!(State::SLOTS[State::SLOT_HISTORY].name, "history");
    assert_eq!(Work::RECOVERY, Recovery::Pause);

    let absent = host_bindings! {
        dts: "../dts/state.d.ts",
        modules: [], classes: [], operations: [],
        services: {
            ClockV2 => 17,
            CounterKey? => None::<u64>,
        },
    }?;
    assert_eq!(absent.service::<CounterKey>(), None);

    let duplicate = host_bindings! {
        dts: "../dts/state.d.ts",
        modules: [], classes: [], operations: [],
        services: {
            ClockV2 => 17,
            ClockV3 => 23,
        },
    };
    assert_eq!(duplicate.unwrap_err(), "duplicate host service \"clock\"");
    Ok(())
}

#[test]
fn fingerprints_only_stable_service_contracts() -> Result<(), String> {
    struct Sink;
    impl ConsoleSink for Sink {
        fn write(&self, _: &ConsoleEvent) {}
    }

    let baseline = host_bindings! {
        dts: "../dts/state.d.ts",
        modules: [], classes: [], operations: [],
        services: {
            ClockV2 => 17,
            CounterKey => 0,
        },
    }?;
    let reordered = host_bindings! {
        dts: "../dts/state.d.ts",
        modules: [], classes: [], operations: [],
        services: {
            CounterKey => 0,
            ClockV2 => 17,
        },
    }?;
    assert_eq!(baseline.fingerprint(), reordered.fingerprint());

    let other_clock = host_bindings! {
        dts: "../dts/state.d.ts",
        modules: [], classes: [], operations: [],
        services: {
            ClockV2 => 42,
            CounterKey => 0,
        },
    }?;
    assert_eq!(baseline.fingerprint(), other_clock.fingerprint());

    let with_console = host_bindings! {
        dts: "../dts/state.d.ts",
        modules: [], classes: [], operations: [],
        services: {
            ClockV2 => 17,
            CounterKey => 0,
            ConsoleKey => Arc::new(Sink),
        },
    }?;
    assert_eq!(baseline.fingerprint(), with_console.fingerprint());

    let upgraded = host_bindings! {
        dts: "../dts/state.d.ts",
        modules: [], classes: [], operations: [],
        services: {
            ClockV3 => 17,
            CounterKey => 0,
        },
    }?;
    assert_ne!(baseline.fingerprint(), upgraded.fingerprint());
    Ok(())
}
