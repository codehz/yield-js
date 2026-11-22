//! Shared fixtures and lifecycle helpers for snapshot tests.

use yield_compiler::{CompileError, CompileOptions, MemoryResolver, compile};
use yield_harness::{TestHost, build};
use yield_vm::{Completion, JsValue, PromiseState, Snapshot, Value, Vm, VmError};

pub(super) fn compile_main(
    source: &str,
    options: &CompileOptions,
) -> Result<yield_vm::Build, CompileError> {
    let resolver = MemoryResolver::new([("main.js", source)]).expect("valid fixture path");
    compile("main.js", None, &resolver, options)
}

/// Compiler options read from a host declaration file, as a host would build them.
pub(super) fn declared(dts: &str) -> CompileOptions {
    CompileOptions::from_dts(dts).expect("host declarations parse")
}

/// Declaration of the shared `TestHost` modules (`test`, `console`).
pub(super) const TEST_DECLARATIONS: &str = include_str!("../../test.d.ts");

pub(super) fn compile_preloaded(
    source: &str,
    preload: &str,
    options: &CompileOptions,
) -> Result<yield_vm::Build, CompileError> {
    let resolver = MemoryResolver::new([("main.js", source), ("preload.js", preload)])
        .expect("valid fixture paths");
    compile("main.js", Some("preload.js"), &resolver, options)
}

pub(super) const RUN: &str = r#"
import { inspect, seed, wait } from "test";
export async function run(input) {
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
    pub(super) fn start(source: &str, args: Vec<JsValue>) -> Self {
        let mut host = TestHost::default();
        let mut vm = Vm::new(build(source), &mut host).expect("create VM");
        let result = vm.start(&mut host, "run", args).expect("start run");
        vm.run_until_idle(&mut host).expect("drain after start");
        Self { vm, host, result }
    }

    /// The result promise, so callers can collect garbage or capture the VM.
    #[track_caller]
    pub(super) fn roots(&self) -> Vec<Value> {
        vec![self.result.clone()]
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
        self.vm.start_recovered_operations()?;
        Ok(())
    }

    #[track_caller]
    pub(super) fn restore_validated(&mut self, snapshot: Snapshot) -> Result<(), VmError> {
        self.vm = Vm::restore_validated(snapshot, self.vm.build().clone(), &mut self.host)?;
        self.vm.start_recovered_operations()?;
        Ok(())
    }

    #[track_caller]
    pub(super) fn complete(&mut self, id: &str, completion: Completion) {
        self.host
            .complete(&mut self.vm, id, completion)
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

/// `subject - baseline`, panicking instead of wrapping when the subject is smaller.
#[track_caller]
pub(super) fn count_delta(subject: usize, baseline: usize) -> usize {
    subject
        .checked_sub(baseline)
        .unwrap_or_else(|| panic!("subject {subject} is smaller than baseline {baseline}"))
}

/// Subject values that are not accounted for by `baseline`, in subject order.
/// Each baseline value consumes the first `==` match.
#[track_caller]
pub(super) fn values_beyond(subject: &[Value], baseline: &[Value]) -> Vec<Value> {
    let mut remaining = subject.to_vec();
    for expected in baseline {
        let index = remaining
            .iter()
            .position(|value| value == expected)
            .unwrap_or_else(|| {
                panic!("baseline value {expected:?} missing from subject {subject:?}")
            });
        remaining.remove(index);
    }
    remaining
}

/// Always-on VM state at the first await of a script with no live locals.
#[track_caller]
pub(super) fn baseline_at_first_await() -> Snapshot {
    Suspended::start(
        r#"
import { wait } from "test";
export async function run() {
  await wait("a");
}
"#,
        vec![],
    )
    .snapshot()
}

/// Always-on VM state at the second await of a script with no live locals.
#[track_caller]
pub(super) fn baseline_at_second_await() -> Snapshot {
    let mut suspended = Suspended::start(
        r#"
import { wait } from "test";
export async function run() {
  await wait("a");
  await wait("b");
}
"#,
        vec![],
    );
    suspended.complete("wait:a", Completion::Fulfilled(JsValue::number(0.0)));
    suspended.snapshot()
}
