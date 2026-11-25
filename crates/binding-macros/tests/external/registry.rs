use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use yield_vm::{
    Bindings, ConsoleEvent, ConsoleKey, ConsoleSink, Decode, Encode, FromBindings, HostFault,
    HostModule, Operation, OperationDriver, OperationToken, Recovery, Registration, ServiceKey,
    Services, host_bindings, host_class, host_module, module_exports, operation,
};

#[host_module(name = "state", operations = [Work])]
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

    fn start(&self, _: OperationToken, _: &Bindings) -> Result<Live, HostFault> {
        Ok(Live)
    }
}

#[host_module(name = "operation-conflict", operations = [ConflictingWork])]
struct OperationConflict;

#[module_exports]
impl OperationConflict {}

#[test]
fn module_dependencies_deduplicate_rust_types_and_stabilize_fingerprints() -> Result<(), String> {
    let repeated = host_bindings! {
        dts: "../dts/empty.d.ts",
        modules: [RepeatedDependencies, Second],
        services: {},
    }?;
    let once_per_module = host_bindings! {
        dts: "../dts/empty.d.ts",
        modules: [First, Second],
        services: {},
    }?;
    let reversed = host_bindings! {
        dts: "../dts/empty.d.ts",
        modules: [Second, First],
        services: {},
    }?;

    assert_eq!(repeated.classes().collect::<Vec<_>>(), [(0, "Shared")]);
    assert_eq!(repeated.operation_count(), 1);
    repeated.decode_operation("work", &[]).unwrap();
    assert_eq!(repeated.fingerprint(), once_per_module.fingerprint());
    assert_eq!(repeated.fingerprint(), reversed.fingerprint());
    Ok(())
}

#[test]
fn only_selected_modules_contribute_dependencies() -> Result<(), String> {
    // ClassConflict and OperationConflict are defined, but selecting Empty
    // alongside First must not pull in either of their conflicting types.
    let bindings = host_bindings! {
        dts: "../dts/empty.d.ts",
        modules: [Empty, First],
        services: {},
    }?;
    assert_eq!(bindings.classes().collect::<Vec<_>>(), [(0, "Shared")]);
    assert_eq!(bindings.operation_count(), 1);
    bindings.decode_operation("work", &[]).unwrap();
    Ok(())
}

#[test]
fn distinct_class_types_with_the_same_tag_are_rejected() {
    let class_collision = host_bindings! {
        dts: "../dts/empty.d.ts",
        modules: [First, ClassConflict],
        services: {},
    };
    assert_eq!(
        class_collision.unwrap_err(),
        "duplicate host class \"Shared\""
    );
}

#[test]
fn distinct_operation_types_with_the_same_name_are_rejected() {
    let operation_collision = host_bindings! {
        dts: "../dts/empty.d.ts",
        modules: [First, OperationConflict],
        services: {},
    };
    assert_eq!(
        operation_collision.unwrap_err(),
        "duplicate host operation \"work\""
    );
}

#[test]
fn duplicate_modules_are_not_deduplicated() {
    let duplicate = host_bindings! {
        dts: "../dts/empty.d.ts",
        modules: [First, First],
        services: {},
    };
    assert_eq!(duplicate.unwrap_err(), "duplicate host module \"first\"");
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

#[test]
fn services_accept_thread_local_values() -> Result<(), String> {
    let bindings = host_bindings! {
        dts: "../dts/state.d.ts",
        modules: [],
        services: {
            LocalKey => Rc::new(Cell::new(9)),
        },
    }?;
    assert_eq!(
        bindings.service::<LocalKey>().map(|value| value.get()),
        Some(9)
    );
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
    let bindings = host_bindings! {
        dts: "../dts/state.d.ts",
        modules: [State],
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
        modules: [],
        services: {
            ClockV2 => 17,
            CounterKey? => None::<u64>,
        },
    }?;
    assert_eq!(absent.service::<CounterKey>(), None);

    let duplicate = host_bindings! {
        dts: "../dts/state.d.ts",
        modules: [],
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
        modules: [],
        services: {
            ClockV2 => 17,
            CounterKey => 0,
        },
    }?;
    let reordered = host_bindings! {
        dts: "../dts/state.d.ts",
        modules: [],
        services: {
            CounterKey => 0,
            ClockV2 => 17,
        },
    }?;
    assert_eq!(baseline.fingerprint(), reordered.fingerprint());

    let other_clock = host_bindings! {
        dts: "../dts/state.d.ts",
        modules: [],
        services: {
            ClockV2 => 42,
            CounterKey => 0,
        },
    }?;
    assert_eq!(baseline.fingerprint(), other_clock.fingerprint());

    let with_console = host_bindings! {
        dts: "../dts/state.d.ts",
        modules: [],
        services: {
            ClockV2 => 17,
            CounterKey => 0,
            ConsoleKey => Arc::new(Sink),
        },
    }?;
    assert_eq!(baseline.fingerprint(), with_console.fingerprint());

    let upgraded = host_bindings! {
        dts: "../dts/state.d.ts",
        modules: [],
        services: {
            ClockV3 => 17,
            CounterKey => 0,
        },
    }?;
    assert_ne!(baseline.fingerprint(), upgraded.fingerprint());
    Ok(())
}
