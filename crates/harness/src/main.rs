use std::collections::BTreeMap;
use std::error::Error;
use std::sync::{Arc, Mutex};

use yield_compiler::{CompileOptions, MemoryResolver, compile};
use yield_harness::{LabelRegistration, LabelService};
use yield_vm::binding::{
    Bindings, CallCx, CodecError, JsResult, JsValue, OperationDriver, OperationToken,
};
use yield_vm::bytecode::Build;
use yield_vm::{Completion, Host, HostFault, PromiseState, Vm};
use yield_vm::{host_bindings, host_module, operation};

const SOURCE: &str = r#"
import { inspect, seed, wait } from 'demo';
export async function run(input) {
  inspect(input);
  const base = seed();
  const x = await wait("a");
  const sum = base + x;
  const y = await wait("b");
  return sum + y;
}
"#;

#[derive(Default)]
struct DemoCounters(Mutex<BTreeMap<String, usize>>);

impl DemoCounters {
    fn hit(&self, name: &str) {
        *self
            .0
            .lock()
            .expect("counter lock")
            .entry(name.to_owned())
            .or_default() += 1;
    }

    fn count(&self, name: &str) -> usize {
        self.0
            .lock()
            .expect("counter lock")
            .get(name)
            .copied()
            .unwrap_or(0)
    }
}

#[operation(name = "wait", schema = 1, recovery = resume, encode = Wait::encode, decode = Wait::decode)]
struct Wait {
    label: String,
}

impl Wait {
    fn encode(&self, out: &mut Vec<u8>) -> Result<(), CodecError> {
        out.extend_from_slice(self.label.as_bytes());
        Ok(())
    }

    fn decode(input: &[u8], _bindings: &Bindings) -> Result<Self, CodecError> {
        Ok(Self {
            label: std::str::from_utf8(input)
                .map_err(|_| CodecError::new("wait label is not UTF-8"))?
                .to_owned(),
        })
    }
}

impl OperationDriver for Wait {
    type Live = LabelRegistration;

    fn start(&self, token: OperationToken, bindings: &Bindings) -> Result<Self::Live, HostFault> {
        self.register(token, bindings, false)
    }

    fn resume(&self, token: OperationToken, bindings: &Bindings) -> Result<Self::Live, HostFault> {
        self.register(token, bindings, true)
    }
}

impl Wait {
    fn register(
        &self,
        token: OperationToken,
        bindings: &Bindings,
        restoring: bool,
    ) -> Result<LabelRegistration, HostFault> {
        let labels = bindings
            .service::<Arc<LabelService>>("labels")
            .ok_or_else(|| HostFault::new("missing labels service"))?;
        labels.register(self.label.clone(), token, restoring)
    }
}

struct DemoHost {
    bindings: Arc<Bindings>,
    counters: Arc<DemoCounters>,
    labels: Arc<LabelService>,
}

impl DemoHost {
    fn new() -> Result<Self, String> {
        let counters = Arc::new(DemoCounters::default());
        let labels = Arc::new(LabelService::default());
        let bindings = demo_bindings(Arc::clone(&counters), Arc::clone(&labels))?;
        Ok(Self {
            bindings: Arc::new(bindings),
            counters,
            labels,
        })
    }

    fn calls(&self, name: &str) -> usize {
        self.counters.count(name)
    }

    fn token(&self, label: &str) -> Option<OperationToken> {
        self.labels.token(label)
    }

    fn complete(&self, vm: &mut Vm, label: &str, value: JsValue) -> Result<(), String> {
        let token = self
            .token(label)
            .ok_or_else(|| format!("unknown operation {label:?}"))?;
        vm.complete_operation(token, Completion::Fulfilled(value))
            .map_err(|error| error.to_string())
    }
}

impl Host for DemoHost {
    fn bindings(&self) -> Arc<Bindings> {
        self.bindings.clone()
    }

    fn report_error(&mut self, _error: JsValue) {}
}

fn demo_bindings(
    counters: Arc<DemoCounters>,
    labels: Arc<LabelService>,
) -> Result<Bindings, String> {
    host_bindings! {
        modules: [demo],
        classes: [],
        states: [],
        operations: [Wait],
        services: [("demo-counters", 1, counters), ("labels", 1, labels)],
    }
}

#[host_module(name = "demo")]
mod demo {
    use super::*;

    #[function(name = "inspect")]
    fn inspect(cx: &mut CallCx<'_>, _value: Option<JsValue>) -> JsResult<()> {
        println!("  host call: inspect({:?})", cx.args());
        if let Some(counters) = cx.service::<Arc<DemoCounters>>("demo-counters") {
            counters.hit("inspect");
        }
        Ok(())
    }

    #[function(name = "seed")]
    fn seed(cx: &mut CallCx<'_>) -> JsResult<JsValue> {
        println!("  host call: seed()");
        if let Some(counters) = cx.service::<Arc<DemoCounters>>("demo-counters") {
            counters.hit("seed");
        }
        Ok(JsValue::number(10.0))
    }

    #[function(name = "wait")]
    fn wait(cx: &mut CallCx<'_>, arg: JsValue) -> JsResult<JsValue> {
        let label = cx
            .string(&arg)?
            .to_utf8()
            .map_err(|_| cx.type_error("wait label must be valid UTF-16"))?;
        println!("  host call: wait({label:?})");
        if let Some(counters) = cx.service::<Arc<DemoCounters>>("demo-counters") {
            counters.hit("wait");
        }
        cx.promise(Wait { label })
    }
}

fn compile_options() -> CompileOptions {
    let bindings = demo_bindings(
        Arc::new(DemoCounters::default()),
        Arc::new(LabelService::default()),
    )
    .expect("demo bindings");
    CompileOptions::from_bindings(&bindings)
}

fn describe_continuations(snapshot: &yield_vm::Snapshot) -> String {
    snapshot
        .continuations
        .iter()
        .map(|continuation| {
            format!(
                "func {} resume {} env {:?} values {:?}",
                continuation.func_id, continuation.resume_id, continuation.env, continuation.values
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn main() -> Result<(), Box<dyn Error>> {
    let resolver = MemoryResolver::new([("main.js", SOURCE)])?;
    let build: Build = compile("main.js", None, &resolver, &compile_options())?;
    let function = build.function_named("run").expect("demo entry function");
    println!("build {} ({})", build.build_id, function.name);
    println!(
        "slots: {} transient, {} persistent",
        function.transient_slots, function.persistent_slots
    );
    for (index, resume) in function.resumes.iter().enumerate() {
        println!(
            "resume {index}: saved {:?}, fulfill pc {}, reject pc {}",
            resume.saved_slots, resume.fulfill_pc, resume.reject_pc
        );
    }

    let mut host = DemoHost::new()?;
    let mut vm = Vm::new(build.clone(), &mut host)?;
    let result = vm.start(&mut host, "run", vec![JsValue::string("input")])?;
    vm.run_until_idle(&mut host)?;
    println!(
        "\nafter first await: pending {:?}",
        host.labels.pending_labels()
    );
    let first = vm.snapshot(std::slice::from_ref(&result))?;
    println!(
        "snapshot 1: {}, {} continuation(s)",
        describe_continuations(&first),
        first.continuations.len()
    );
    drop(vm);

    let mut vm = Vm::restore(first, build.clone(), &mut host)?;
    vm.start_recovered_operations()?;
    println!(
        "restored snapshot 1; inspect/seed calls = {}/{}",
        host.calls("inspect"),
        host.calls("seed")
    );
    host.complete(&mut vm, "a", JsValue::number(2.0))?;
    vm.run_until_idle(&mut host)?;
    println!(
        "after second await: pending {:?}",
        host.labels.pending_labels()
    );
    let second = vm.snapshot(std::slice::from_ref(&result))?;
    println!(
        "snapshot 2: {}, {} continuation(s)",
        describe_continuations(&second),
        second.continuations.len()
    );
    drop(vm);

    let mut vm = Vm::restore(second, build, &mut host)?;
    vm.start_recovered_operations()?;
    host.complete(&mut vm, "b", JsValue::number(3.0))?;
    vm.run_until_idle(&mut host)?;
    match vm.promise_state(&result)? {
        PromiseState::Pending => println!("result: pending (unexpected)"),
        PromiseState::Fulfilled(value) => println!("result: {}", render_value(&value)),
        PromiseState::Rejected(value) => println!("result: rejected {}", render_value(&value)),
    }
    println!(
        "audit: inspect={} seed={} wait={} (restore did not re-run effects)",
        host.calls("inspect"),
        host.calls("seed"),
        host.calls("wait")
    );
    Ok(())
}

fn render_value(value: &yield_vm::Value) -> String {
    match value.view() {
        yield_vm::ValueRef::Undefined => "undefined".to_owned(),
        yield_vm::ValueRef::Null => "null".to_owned(),
        yield_vm::ValueRef::Bool(value) => value.to_string(),
        yield_vm::ValueRef::Number(value) => value.to_string(),
        yield_vm::ValueRef::String(value) => format!("{value:?}"),
        yield_vm::ValueRef::Symbol(_) => "symbol".to_owned(),
        yield_vm::ValueRef::Object(_) => "object".to_owned(),
    }
}
