use std::cell::Cell;
use yield_vm::binding::{CodecError, DecodeCx};
use yield_vm::{
    Bindings, HostFault, HostModuleState, Operation, OperationDriver, OperationToken, Recovery,
    Registration, host_bindings, host_module, host_state, operation,
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

#[test]
fn registers_state_operation_and_services() -> Result<(), String> {
    let evaluations = Cell::new(0);
    let first = host_bindings! {
        dts: "tests/dts/state.d.ts",
        modules: [state], classes: [], states: [State], operations: [Work],
        services: [("clock", 2, { evaluations.set(evaluations.get() + 1); 17u64 })],
    }?;
    let second = host_bindings! {
        dts: "tests/dts/state.d.ts",
        modules: [state], classes: [], states: [State], operations: [Work],
        services: [("clock", 2, String::from("different resource"))],
    }?;
    let third = host_bindings! {
        dts: "tests/dts/state.d.ts",
        modules: [state], classes: [], states: [State], operations: [Work],
        services: [("clock", 3, 17u64)],
    }?;
    assert_eq!(evaluations.get(), 1);
    assert_eq!(first.fingerprint(), second.fingerprint());
    assert_ne!(first.fingerprint(), third.fingerprint());
    assert_eq!(State::SLOTS[State::SLOT_CURRENT].name, "current");
    assert_eq!(State::SLOTS[State::SLOT_HISTORY].name, "history");
    assert_eq!(Work::RECOVERY, Recovery::Pause);
    Ok(())
}

#[test]
fn rejects_duplicate_service_ids_even_across_versions() {
    let duplicate = host_bindings! {
        dts: "tests/dts/state.d.ts",
        modules: [], classes: [], states: [], operations: [],
        services: [("clock", 2, 17u64), ("clock", 3, "other")],
    };
    assert_eq!(duplicate.unwrap_err(), "duplicate host service \"clock\"");
    assert!(
        host_bindings! {
            dts: "tests/dts/state.d.ts",
            modules: [], classes: [], states: [], operations: [], services: []
        }
        .is_ok()
    );
}
