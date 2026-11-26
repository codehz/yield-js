use super::{guest_build, start_vm};
use std::any::type_name;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use yield_vm::binding::CodecError;
use yield_vm::{
    __macro_support::decode_operation, Decode, Diagnostic, Diagnostics, Encode, Host, HostFault,
    HostModule, Operation, OperationDriver, OperationToken, PlatformServices, Recovery,
    Registration, Services, Vm, host, host_class, host_module, module_exports, operation,
};

fn fingerprint(host: Host<impl PlatformServices>) -> String {
    start_vm(host, Default::default())
        .snapshot(&[])
        .unwrap()
        .binding_fingerprint
}

#[host_module(name = "state", operations = [Work], services = [clock: Clock])]
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

    fn start(&self, _: OperationToken, _: &Services<'_, Self>) -> Result<Live, HostFault> {
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

    fn start(&self, _: OperationToken, _: &Services<'_, Self>) -> Result<Live, HostFault> {
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
    assert_eq!(vm.bind_service::<Clock>(), Some(&Clock(17)));
    assert_eq!(vm.bind_service::<OtherClock>(), None);
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
        vm.bind_service::<Rc<Cell<u32>>>().map(|value| value.get()),
        Some(11)
    );
    Ok(())
}

#[test]
fn registers_state_operation_and_services() -> Result<(), String> {
    let evaluations = Cell::new(0);
    let host = host! {
        dts: "../dts/state.d.ts",
        modules: [State],
        services: {
            Clock => { evaluations.set(evaluations.get() + 1); Clock(17) },
            OtherClock => OtherClock(23),
        },
    }?;
    assert_eq!(evaluations.get(), 1);
    let vm = start_vm(host, Default::default());
    assert_eq!(vm.bind_service::<ClockAlias>(), Some(&Clock(17)));
    let snapshot = vm.snapshot(&[]).unwrap();
    let state = snapshot
        .module_states
        .iter()
        .find(|state| state.module == "state")
        .expect("snapshot includes the linked module state");
    assert_eq!(state.bytes.as_slice(), &[3]);
    assert_eq!(vm.bind_service::<OtherClock>(), Some(&OtherClock(23)));
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
    trait Service {}
    struct Sink;
    impl Service for Sink {}

    let sink = Arc::new(Sink);
    let host = host! {
        services: { Arc<dyn Service> => sink.clone() },
    }?;
    let vm = start_vm(host, Default::default());
    let expected: Arc<dyn Service> = sink;
    assert!(Arc::ptr_eq(
        vm.bind_service::<Arc<dyn Service>>().unwrap(),
        &expected
    ));
    assert!(vm.bind_service::<Arc<Sink>>().is_none());
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

#[repr(align(128))]
struct AlignedService(Rc<Cell<u32>>);

impl Drop for AlignedService {
    fn drop(&mut self) {
        self.0.set(self.0.get() + 1);
    }
}

struct Marker;

#[host_module(name = "layout", operations = [ProbeLayout])]
struct LayoutModule;

#[module_exports]
impl LayoutModule {}

#[operation(
    name = "probe-layout",
    recovery = pause,
    services = [clock: Clock, aligned: AlignedService, marker: Marker],
)]
#[derive(Encode)]
struct ProbeLayout;

impl Decode<ProbeLayout> for ProbeLayout {
    fn decode(input: &mut &[u8], services: &Services<'_, Self>) -> Result<Self, CodecError> {
        let clock = u64::decode(input, services)?;
        assert_eq!(Self::clock(services), &Clock(clock));
        let aligned = Self::aligned(services);
        assert_eq!(std::ptr::from_ref(aligned).addr() % 128, 0);
        assert_eq!(aligned.0.get(), clock as u32);
        let marker = std::hint::black_box(Self::marker(services));
        assert_eq!(std::mem::size_of_val(marker), 0);
        Ok(Self)
    }
}

impl Registration<ProbeLayout> for () {}

impl OperationDriver for ProbeLayout {
    type Live = ();

    fn start(&self, _: OperationToken, _: &Services<'_, Self>) -> Result<(), HostFault> {
        Ok(())
    }
}

#[test]
fn resolves_the_same_dependencies_against_each_host_layout() -> Result<(), String> {
    let wide_drops = Rc::new(Cell::new(17));
    let narrow_drops = Rc::new(Cell::new(23));
    let wide = host! {
        dts: "../dts/empty.d.ts",
        modules: [LayoutModule],
        services: {
            OtherClock => OtherClock(99),
            AlignedService => AlignedService(Rc::clone(&wide_drops)),
            Marker => Marker,
            Clock => Clock(17),
        },
    }?;
    let narrow = host! {
        dts: "../dts/empty.d.ts",
        modules: [LayoutModule],
        services: {
            Clock => Clock(23),
            Marker => Marker,
            AlignedService => AlignedService(Rc::clone(&narrow_drops)),
        },
    }?;
    let decode = |host: &Host, clock: u64| {
        decode_operation(host, "probe-layout", &clock.to_le_bytes()).unwrap();
    };
    decode(&wide, 17);
    decode(&narrow, 23);
    decode(&wide, 17);
    assert_eq!(wide_drops.get(), 17);
    assert_eq!(narrow_drops.get(), 23);
    drop(wide);
    assert_eq!(wide_drops.get(), 18);
    decode(&narrow, 23);
    drop(narrow);
    assert_eq!(narrow_drops.get(), 24);
    Ok(())
}

#[test]
fn rejects_a_missing_required_layout_service_during_linking() {
    let missing = host! {
        dts: "../dts/empty.d.ts",
        modules: [LayoutModule],
        services: {
            Clock => Clock(17),
            AlignedService => AlignedService(Rc::new(Cell::new(0))),
        },
    };
    assert_eq!(
        missing.err().unwrap(),
        format!(
            "host operation {:?}: missing host service {:?}",
            "probe-layout",
            type_name::<Marker>(),
        ),
    );
}

struct DiagnosticCounter(Rc<Cell<u32>>);

impl Diagnostics for DiagnosticCounter {
    fn report(&self, event: Diagnostic<'_>) {
        assert!(matches!(event, Diagnostic::StartupException { .. }));
        self.0.set(self.0.get() + 1);
    }
}

#[test]
fn non_clone_diagnostics_and_same_type_custom_service_are_independent() -> Result<(), String> {
    let platform_count = Rc::new(Cell::new(0));
    let custom_count = Rc::new(Cell::new(0));
    let order = Cell::new(0);
    let make = || {
        host! {
            services: {
                DiagnosticCounter => {
                    assert_eq!(order.replace(1), 0);
                    DiagnosticCounter(Rc::clone(&custom_count))
                },
                diagnostics: {
                    assert_eq!(order.replace(2), 1);
                    DiagnosticCounter(Rc::clone(&platform_count))
                },
                Clock => {
                    assert_eq!(order.replace(3), 2);
                    Clock(17)
                },
            },
        }
    };
    let host = make()?;
    assert_eq!(order.replace(0), 3);
    assert!(host.platform().console().is_none());
    assert!(host.platform().user_agent().is_none());
    assert!(Rc::ptr_eq(
        &host.platform().diagnostics().unwrap().0,
        &platform_count
    ));
    let vm = start_vm(host, Default::default());
    let custom = vm.bind_service::<DiagnosticCounter>().unwrap();
    assert!(Rc::ptr_eq(&custom.0, &custom_count));

    let throwing_guest = || guest_build(Default::default(), true);
    assert!(Vm::new(throwing_guest(), make()?, Default::default()).is_err());
    assert_eq!(order.get(), 3);
    assert_eq!(platform_count.get(), 1);
    assert_eq!(custom_count.get(), 0);

    let custom_only = host! {
        services: { DiagnosticCounter => DiagnosticCounter(Rc::clone(&custom_count)) },
    }?;
    assert!(custom_only.platform().diagnostics().is_none());
    assert!(Vm::new(throwing_guest(), custom_only, Default::default()).is_err());
    assert_eq!(custom_count.get(), 0);

    let platform_only = host! {
        services: { diagnostics: DiagnosticCounter(Rc::clone(&platform_count)) },
    }?;
    let vm = start_vm(platform_only, Default::default());
    assert!(vm.bind_service::<DiagnosticCounter>().is_none());
    assert!(host! { services: {} }?.platform().diagnostics().is_none());
    Ok(())
}
