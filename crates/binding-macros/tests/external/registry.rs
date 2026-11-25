use super::start_vm;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use yield_vm::{
    __macro_support::decode_operation, ConsoleEvent, ConsoleKey, ConsoleSink, Decode, Encode,
    FromServices, Host, HostFault, HostModule, Operation, OperationDriver, OperationToken,
    Recovery, Registration, ServiceKey, Services, host, host_class, host_module, module_exports,
    operation,
};

fn fingerprint(host: Host) -> String {
    start_vm(host, Default::default())
        .snapshot(&[])
        .unwrap()
        .binding_fingerprint
}

#[host_module(name = "state", operations = [Work], services = [ClockV2, CounterKey?])]
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

    fn start(&self, _: OperationToken, _: &Services) -> Result<Live, HostFault> {
        Ok(Live)
    }
}

#[host_class(tag = "Shared")]
struct Shared;

type SharedAlias = Shared;
type WorkAlias = Work;

#[host_module(
    name = "first",
    classes = [Shared, SharedAlias, Shared],
    operations = [Work, WorkAlias, Work],
)]
struct RepeatedDependencies;

#[module_exports]
impl RepeatedDependencies {}

#[host_module(name = "first", classes = [Shared], operations = [Work])]
struct First;

#[module_exports]
impl First {}

#[host_module(name = "second", classes = [Shared], operations = [Work])]
struct Second;

#[module_exports]
impl Second {}

#[host_module(name = "empty")]
struct Empty;

#[module_exports]
impl Empty {}

#[host_class(tag = "Shared")]
struct ConflictingClass;

#[host_module(name = "class-conflict", classes = [ConflictingClass])]
struct ClassConflict;

#[module_exports]
impl ClassConflict {}

#[operation(name = "work", recovery = pause)]
#[derive(Encode, Decode)]
struct ConflictingWork;

impl Registration<ConflictingWork> for Live {}

impl OperationDriver for ConflictingWork {
    type Live = Live;

    fn start(&self, _: OperationToken, _: &Services) -> Result<Live, HostFault> {
        Ok(Live)
    }
}

#[host_module(name = "operation-conflict", operations = [ConflictingWork])]
struct OperationConflict;

#[module_exports]
impl OperationConflict {}

#[test]
fn module_dependencies_deduplicate_rust_types_and_stabilize_fingerprints() -> Result<(), String> {
    let repeated = host! {
        dts: "../dts/empty.d.ts",
        modules: [RepeatedDependencies, Second],
        services: {},
    }?;
    let once_per_module = host! {
        dts: "../dts/empty.d.ts",
        modules: [First, Second],
        services: {},
    }?;
    let reversed = host! {
        dts: "../dts/empty.d.ts",
        modules: [Second, First],
        services: {},
    }?;

    decode_operation(&repeated, "work", &[]).unwrap();
    let repeated = fingerprint(repeated);
    assert_eq!(repeated, fingerprint(once_per_module));
    assert_eq!(repeated, fingerprint(reversed));
    Ok(())
}

#[test]
fn only_selected_modules_contribute_dependencies() -> Result<(), String> {
    // ClassConflict and OperationConflict are defined, but selecting Empty
    // alongside First must not pull in either of their conflicting types.
    let host = host! {
        dts: "../dts/empty.d.ts",
        modules: [Empty, First],
        services: {},
    }?;
    decode_operation(&host, "work", &[]).unwrap();
    start_vm(host, Default::default());
    Ok(())
}

#[test]
fn distinct_class_types_with_the_same_tag_are_rejected() {
    let class_collision = host! {
        dts: "../dts/empty.d.ts",
        modules: [First, ClassConflict],
        services: {},
    };
    assert_eq!(
        class_collision.err().unwrap(),
        "duplicate host class \"Shared\""
    );
}

#[test]
fn distinct_operation_types_with_the_same_name_are_rejected() {
    let operation_collision = host! {
        dts: "../dts/empty.d.ts",
        modules: [First, OperationConflict],
        services: {},
    };
    assert_eq!(
        operation_collision.err().unwrap(),
        "duplicate host operation \"work\""
    );
}

#[test]
fn duplicate_modules_are_not_deduplicated() {
    let duplicate = host! {
        dts: "../dts/empty.d.ts",
        modules: [First, First],
        services: {},
    };
    assert_eq!(duplicate.err().unwrap(), "duplicate host module \"first\"");
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

struct LocalKey;
impl ServiceKey for LocalKey {
    type Value = Rc<Cell<u32>>;
    const ID: &'static str = "local";
    const VERSION: u32 = 1;
}

type ClockAlias = ClockV2;

#[test]
fn validates_service_dependencies_of_selected_modules() -> Result<(), String> {
    let missing = host! {
        dts: "../dts/state.d.ts",
        modules: [State],
        services: { ClockAlias? => None::<u64> },
    };
    assert_eq!(
        missing.err().unwrap(),
        "host module \"state\": missing host service \"clock\""
    );

    let wrong_key = host! {
        dts: "../dts/state.d.ts",
        modules: [State],
        services: { OtherClock => 17 },
    };
    assert_eq!(
        wrong_key.err().unwrap(),
        "host module \"state\": wrong key type for host service \"clock\""
    );

    let host = host! {
        dts: "../dts/state.d.ts",
        modules: [State],
        services: { ClockAlias? => Some(17), CounterKey? => None::<u64> },
    }?;
    let vm = start_vm(host, Default::default());
    assert_eq!(vm.service::<ClockV2>(), Some(&17));
    assert_eq!(vm.service::<CounterKey>(), None);
    Ok(())
}

#[test]
fn services_accept_thread_local_values() -> Result<(), String> {
    let host = host! {
        services: {
            LocalKey => Rc::new(Cell::new(9)),
        },
    }?;
    let vm = start_vm(host, Default::default());
    assert_eq!(vm.service::<LocalKey>().map(|value| value.get()), Some(9));
    Ok(())
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
    let host = host! {
        dts: "../dts/state.d.ts",
        modules: [State],
        services: {
            ClockV2 => { evaluations.set(evaluations.get() + 1); 17 },
        },
    }?;
    assert_eq!(evaluations.get(), 1);
    assert_eq!(<State as FromServices>::from_services(&services).count, 3);
    let vm = start_vm(host, Default::default());
    assert_eq!(vm.service::<ClockV2>(), Some(&17));
    assert_eq!(vm.service::<OtherClock>(), None);
    assert_eq!(State::SLOTS[State::SLOT_CURRENT].name, "current");
    assert_eq!(State::SLOTS[State::SLOT_HISTORY].name, "history");
    assert_eq!(Work::RECOVERY, Recovery::Pause);

    let absent = host! {
        modules: [],
        services: {
            ClockV2 => 17,
            CounterKey? => None::<u64>,
        },
    }?;
    let vm = start_vm(absent, Default::default());
    assert_eq!(vm.service::<CounterKey>(), None);

    let duplicate = host! {
        services: {
            ClockV2 => 17,
            ClockV3 => 23,
        },
    };
    assert_eq!(duplicate.err().unwrap(), "duplicate host service \"clock\"");
    Ok(())
}

#[test]
fn fingerprints_only_stable_service_contracts() -> Result<(), String> {
    struct Sink;
    impl ConsoleSink for Sink {
        fn write(&self, _: &ConsoleEvent) {}
    }

    let baseline = fingerprint(host! {
        services: {
            ClockV2 => 17,
            CounterKey => 0,
        },
    }?);
    let reordered = fingerprint(host! {
        services: {
            CounterKey => 0,
            ClockV2 => 17,
        },
    }?);
    assert_eq!(baseline, reordered);

    let other_clock = fingerprint(host! {
        services: {
            ClockV2 => 42,
            CounterKey => 0,
        },
    }?);
    assert_eq!(baseline, other_clock);

    let with_console = fingerprint(host! {
        services: {
            ClockV2 => 17,
            CounterKey => 0,
            ConsoleKey => Arc::new(Sink),
        },
    }?);
    assert_eq!(baseline, with_console);

    let upgraded = fingerprint(host! {
        services: {
            ClockV3 => 17,
            CounterKey => 0,
        },
    }?);
    assert_ne!(baseline, upgraded);
    Ok(())
}
