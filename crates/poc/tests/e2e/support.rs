//! Shared fixtures and lifecycle helpers for snapshot tests.

use yield_poc::{TestHost, build};
use yield_vm::{Completion, HostValue, PromiseState, Snapshot, Value, Vm, VmError};

pub(super) const RUN: &str = r#"
async function run(input) {
  inspect(input);
  const base = seed();
  const x = await wait("a");
  const sum = base + x;
  const y = await wait("b");
  return sum + y;
}
"#;

pub(super) struct Suspended {
    pub(super) vm: Vm,
    pub(super) host: TestHost,
    result: Value,
}

impl Suspended {
    #[track_caller]
    pub(super) fn start(source: &str, args: Vec<HostValue>) -> Self {
        let mut host = TestHost::default();
        let mut vm = Vm::new(build(source), &host).expect("create VM");
        let result = vm.start(&mut host, "run", args).expect("start run");
        vm.run_until_idle(&mut host).expect("drain after start");
        Self { vm, host, result }
    }

    #[track_caller]
    pub(super) fn snapshot(&self) -> Snapshot {
        self.vm
            .snapshot(std::slice::from_ref(&self.result))
            .expect("snapshot VM with the result root")
    }

    #[track_caller]
    pub(super) fn restore(&mut self, snapshot: Snapshot) -> Result<(), VmError> {
        self.vm = Vm::restore(snapshot, self.vm.build().clone(), &mut self.host)?;
        Ok(())
    }

    #[track_caller]
    pub(super) fn complete(&mut self, id: &str, completion: Completion) {
        self.vm
            .complete_operation(id, completion)
            .map_err(|error| (id, error))
            .expect("complete operation");
        self.vm
            .run_until_idle(&mut self.host)
            .map_err(|error| (id, error))
            .expect("drain after completing operation");
    }

    #[track_caller]
    pub(super) fn state(&self) -> PromiseState {
        self.vm
            .promise_state(&self.result)
            .expect("read result promise state")
    }
}

#[track_caller]
pub(super) fn json_round_trip(snapshot: &Snapshot) -> Snapshot {
    let json = snapshot.to_json().expect("encode snapshot JSON");
    Snapshot::from_json(&json).expect("decode snapshot JSON")
}
