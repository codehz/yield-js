use std::collections::BTreeMap;
use std::error::Error;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use yield_compiler::{Compiler, MemoryResolver, Project};
use yield_harness::{LabelRegistration, LabelService};
use yield_vm::binding::{JsResult, JsValue, OperationDriver, OperationToken};
use yield_vm::bytecode::Build;
use yield_vm::intrinsics::TypeError;
use yield_vm::{Completion, Host, HostFault, PromiseState, Services, Vm};
use yield_vm::{Decode, Encode, host, host_module, module_exports, operation};

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

#[operation(name = "wait", recovery = resume, services = [labels: Rc<LabelService>])]
#[derive(Encode, Decode)]
struct Wait {
    label: String,
}

impl OperationDriver for Wait {
    type Live = LabelRegistration;

    fn start(
        &self,
        token: OperationToken,
        services: &Services<'_, Self>,
    ) -> Result<Self::Live, HostFault> {
        self.register(token, Self::labels(services), false)
    }

    fn resume(
        &self,
        token: OperationToken,
        services: &Services<'_, Self>,
    ) -> Result<Self::Live, HostFault> {
        self.register(token, Self::labels(services), true)
    }
}

impl Wait {
    fn register(
        &self,
        token: OperationToken,
        labels: &Rc<LabelService>,
        restoring: bool,
    ) -> Result<LabelRegistration, HostFault> {
        labels.register(self.label.clone(), token, restoring)
    }
}

struct DemoHost {
    time: Arc<yield_vm::host::SystemTimeService>,
    counters: Rc<DemoCounters>,
    labels: Rc<LabelService>,
}

impl DemoHost {
    fn new() -> Result<Self, String> {
        let time_zone = iana_time_zone::get_timezone()
            .map_err(|error| format!("failed to determine the system IANA time zone: {error}"))?;
        let counters = Rc::new(DemoCounters::default());
        let labels = Rc::new(LabelService::default());
        Ok(Self {
            time: Arc::new(yield_vm::host::SystemTimeService::new(time_zone)),
            counters,
            labels,
        })
    }

    fn host(&self) -> Result<Host<impl yield_vm::PlatformServices>, String> {
        host! {
            dts: "../demo.d.ts",
            modules: [Demo],
            services: {
                Rc<DemoCounters> => self.counters.clone(),
                Rc<LabelService> => self.labels.clone(),
                time: self.time.clone(),
            },
        }
    }

    fn calls(&self, name: &str) -> usize {
        self.counters.count(name)
    }

    fn token(&self, label: &str) -> Option<OperationToken> {
        self.labels.token(label)
    }

    fn complete(
        &self,
        vm: &mut Vm<impl yield_vm::PlatformServices>,
        label: &str,
        value: JsValue,
    ) -> Result<(), String> {
        let token = self
            .token(label)
            .ok_or_else(|| format!("unknown operation {label:?}"))?;
        vm.complete_operation(token, Completion::Fulfilled(value))
            .map_err(|error| error.to_string())
    }
}

#[host_module(
    name = "demo",
    operations = [Wait],
    services = [counters: Rc<DemoCounters>],
)]
struct Demo;

#[module_exports]
impl Demo {
    #[function(name = "inspect")]
    fn inspect(cx: &mut yield_vm::ModuleCx<'_, Self>, _value: Option<JsValue>) -> JsResult<()> {
        println!("  host call: inspect({:?})", cx.args());
        Self::counters(cx.services()).hit("inspect");
        Ok(())
    }

    #[function(name = "seed")]
    fn seed(cx: &mut yield_vm::ModuleCx<'_, Self>) -> JsResult<JsValue> {
        println!("  host call: seed()");
        Self::counters(cx.services()).hit("seed");
        Ok(JsValue::number(10.0))
    }

    #[function(name = "wait")]
    fn wait(cx: &mut yield_vm::ModuleCx<'_, Self>, arg: JsValue) -> JsResult<JsValue> {
        let Ok(label) = cx.string(&arg)?.to_utf8() else {
            let error =
                cx.construct::<TypeError>(&[JsValue::string("wait label must be valid UTF-16")])?;
            return Err(cx.throw(error));
        };
        println!("  host call: wait({label:?})");
        Self::counters(cx.services()).hit("wait");
        cx.promise(Wait { label })
    }
}

fn compile_options() -> Compiler {
    Compiler::from_dts(include_str!("../demo.d.ts")).expect("demo declarations parse")
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
    let host = DemoHost::new()?;
    let resolver = MemoryResolver::new([("main.js", SOURCE)])?;
    let build: Build = compile_options().compile(Project::new("main.js", resolver))?;
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

    let mut vm = Vm::new(build.clone(), host.host()?, Default::default())?;
    let result = vm.start("run", vec![JsValue::string("input")])?;
    vm.run_until_idle()?;
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

    let mut vm = Vm::restore(first, build.clone(), host.host()?, Default::default())?;
    vm.start_recovered_operations()?;
    println!(
        "restored snapshot 1; inspect/seed calls = {}/{}",
        host.calls("inspect"),
        host.calls("seed")
    );
    host.complete(&mut vm, "a", JsValue::number(2.0))?;
    vm.run_until_idle()?;
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

    let mut vm = Vm::restore(second, build, host.host()?, Default::default())?;
    vm.start_recovered_operations()?;
    host.complete(&mut vm, "b", JsValue::number(3.0))?;
    vm.run_until_idle()?;
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
        yield_vm::ValueRef::BigInt(value) => format!("{value}n"),
        yield_vm::ValueRef::Symbol(_) => "symbol".to_owned(),
        yield_vm::ValueRef::Object(_) => "object".to_owned(),
    }
}
