use super::start_vm;
use std::any::type_name;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use yield_vm::{
    __macro_support::decode_operation, ConsoleEvent, ConsoleSink, Decode, Encode, FromServices,
    Host, HostFault, HostModule, Operation, OperationDriver, OperationToken, Recovery,
    Registration, Services, host, host_class, host_module, module_exports, operation,
};

fn fingerprint(host: Host) -> String {
    start_vm(host, Default::default())
        .snapshot(&[])
        .unwrap()
        .binding_fingerprint
}

#[host_module(name = "state", operations = [Work], services = [Clock])]
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

#[derive(Debug, PartialEq)]
struct Clock(u64);

#[derive(Debug, PartialEq)]
struct OtherClock(u64);

type ClockAlias = Clock;

#[test]
fn validates_service_dependencies_of_selected_modules() -> Result<(), String> {
    let missing = host! {
        dts: "../dts/state.d.ts",
        modules: [State],
        services: {},
    };
    assert_eq!(
        missing.err().unwrap(),
        format!(
            "host module {:?}: missing host service {:?}",
            "state",
            type_name::<Clock>()
        )
    );

    let wrong_type = host! {
        dts: "../dts/state.d.ts",
        modules: [State],
        services: { OtherClock => OtherClock(17) },
    };
    assert_eq!(
        wrong_type.err().unwrap(),
        format!(
            "host module {:?}: missing host service {:?}",
            "state",
            type_name::<Clock>()
        )
    );

    let host = host! {
        dts: "../dts/state.d.ts",
        modules: [State],
        services: { ClockAlias => Clock(17) },
    }?;
    let vm = start_vm(host, Default::default());
    assert_eq!(vm.service::<Clock>(), Some(&Clock(17)));
    assert_eq!(vm.service::<OtherClock>(), None);
    Ok(())
}

#[test]
fn services_accept_thread_local_values() -> Result<(), String> {
    let local = Rc::new(Cell::new(9u32));
    let host = host! {
        services: {
            Rc<Cell<u32>> => Rc::clone(&local),
        },
    }?;
    let vm = start_vm(host, Default::default());
    local.set(11);
    assert_eq!(
        vm.service::<Rc<Cell<u32>>>().map(|value| value.get()),
        Some(11)
    );
    Ok(())
}

#[test]
fn registers_state_operation_and_services() -> Result<(), String> {
    let mut services = Services::default();
    services.insert(Clock(17))?;
    services.insert(OtherClock(23))?;
    assert_eq!(services.get::<OtherClock>(), Some(&OtherClock(23)));
    assert_eq!(
        services.insert(Clock(42)).unwrap_err(),
        format!("duplicate host service {:?}", type_name::<Clock>())
    );
    assert_eq!(services.get::<ClockAlias>(), Some(&Clock(17)));

    let evaluations = Cell::new(0);
    let host = host! {
        dts: "../dts/state.d.ts",
        modules: [State],
        services: {
            Clock => { evaluations.set(evaluations.get() + 1); Clock(17) },
        },
    }?;
    assert_eq!(evaluations.get(), 1);
    assert_eq!(<State as FromServices>::from_services(&services).count, 3);
    let vm = start_vm(host, Default::default());
    assert_eq!(vm.service::<Clock>(), Some(&Clock(17)));
    assert_eq!(vm.service::<OtherClock>(), None);
    assert_eq!(State::SLOTS[State::SLOT_CURRENT].name, "current");
    assert_eq!(State::SLOTS[State::SLOT_HISTORY].name, "history");
    assert_eq!(Work::RECOVERY, Recovery::Pause);
    Ok(())
}

#[test]
fn rejects_duplicate_service_registrations_through_a_type_alias() {
    let duplicate = host! {
        services: {
            Clock => Clock(17),
            ClockAlias => Clock(23),
        },
    };
    assert_eq!(
        duplicate.err().unwrap(),
        format!("duplicate host service {:?}", type_name::<Clock>())
    );
}

#[test]
fn explicit_service_type_coerces_to_a_trait_object() -> Result<(), String> {
    struct Sink;
    impl ConsoleSink for Sink {
        fn write(&self, _: &ConsoleEvent) {}
    }

    let sink = Arc::new(Sink);
    let host = host! {
        services: { Arc<dyn ConsoleSink> => sink.clone() },
    }?;
    let vm = start_vm(host, Default::default());
    let expected: Arc<dyn ConsoleSink> = sink;
    assert!(Arc::ptr_eq(
        vm.service::<Arc<dyn ConsoleSink>>().unwrap(),
        &expected
    ));
    assert!(vm.service::<Arc<Sink>>().is_none());
    Ok(())
}

#[test]
fn fingerprints_ignore_services() -> Result<(), String> {
    let baseline = fingerprint(host! {
        services: {
            Clock => Clock(17),
            OtherClock => OtherClock(0),
        },
    }?);
    let reordered = fingerprint(host! {
        services: {
            OtherClock => OtherClock(0),
            Clock => Clock(17),
        },
    }?);
    assert_eq!(baseline, reordered);

    let changed_value = fingerprint(host! {
        services: {
            Clock => Clock(42),
            OtherClock => OtherClock(0),
        },
    }?);
    assert_eq!(baseline, changed_value);

    let empty = fingerprint(host! { services: {} }?);
    assert_eq!(baseline, empty);
    Ok(())
}
