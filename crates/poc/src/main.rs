use std::collections::BTreeMap;
use std::error::Error;

use yield_compiler::{CompileOptions, compile};
use yield_vm::{
    Completion, Host, HostReply, HostValue, NativeKey, PendingOperationMeta, PromiseState, Vm,
};

const SOURCE: &str = r#"
export async function run(input) {
  inspect(input);
  const base = seed();
  const x = await wait("a");
  const sum = base + x;
  const y = await wait("b");
  return sum + y;
}
"#;

struct DemoHost {
    calls: Vec<(String, Vec<HostValue>)>,
    operations: BTreeMap<String, PendingOperationMeta>,
}

impl DemoHost {
    fn new() -> Self {
        Self {
            calls: Vec::new(),
            operations: BTreeMap::new(),
        }
    }

    fn calls(&self, name: &str) -> usize {
        self.calls.iter().filter(|(call, _)| call == name).count()
    }
}

impl Host for DemoHost {
    fn has_import(&self, name: &str) -> bool {
        matches!(name, "inspect" | "seed" | "wait")
    }

    fn call(&mut self, import: &str, args: &[HostValue]) -> Result<HostReply, String> {
        println!("  host call: {import}({args:?})");
        self.calls.push((import.to_owned(), args.to_vec()));
        match import {
            "inspect" => Ok(HostReply::Value(HostValue::Undefined)),
            "seed" => Ok(HostReply::Value(HostValue::Number(10.0))),
            "wait" => {
                let label = match args {
                    [HostValue::String(label)] => label
                        .to_utf8()
                        .map_err(|_| "wait label must be valid UTF-16".to_owned())?,
                    _ => return Err("wait expects one string".to_owned()),
                };
                let id = format!("wait:{label}");
                self.operations.insert(
                    id.clone(),
                    PendingOperationMeta {
                        id: id.clone(),
                        import: import.to_owned(),
                    },
                );
                Ok(HostReply::Operation { operation_id: id })
            }
            _ => Err(format!("unknown import {import}")),
        }
    }

    fn has_native(&self, _key: &NativeKey) -> bool {
        true
    }

    fn rebuild_operation(&mut self, operation: &PendingOperationMeta) -> bool {
        self.operations
            .get(&operation.id)
            .is_some_and(|known| known == operation)
    }
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
    let build = compile(
        SOURCE,
        &CompileOptions {
            imports: vec!["inspect".into(), "seed".into(), "wait".into()],
        },
    )?;
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

    let mut host = DemoHost::new();
    let mut vm = Vm::new(build.clone(), &mut host)?;
    let result = vm.start(&mut host, "run", vec![HostValue::String("input".into())])?;
    vm.run_until_idle(&mut host)?;
    println!(
        "\nafter first await: pending {:?}",
        vm.pending_operation_ids()
    );
    let first = vm.snapshot(std::slice::from_ref(&result))?;
    println!(
        "snapshot 1: {}, {} continuation(s)",
        describe_continuations(&first),
        first.continuations.len()
    );
    drop(vm);

    let mut vm = Vm::restore(first, build.clone(), &mut host)?;
    println!(
        "restored snapshot 1; inspect/seed calls = {}/{}",
        host.calls("inspect"),
        host.calls("seed")
    );
    vm.complete_operation("wait:a", Completion::Fulfilled(HostValue::Number(2.0)))?;
    vm.run_until_idle(&mut host)?;
    println!(
        "after second await: pending {:?}",
        vm.pending_operation_ids()
    );
    let second = vm.snapshot(std::slice::from_ref(&result))?;
    println!(
        "snapshot 2: {}, {} continuation(s)",
        describe_continuations(&second),
        second.continuations.len()
    );
    drop(vm);

    let mut vm = Vm::restore(second, build, &mut host)?;
    vm.complete_operation("wait:b", Completion::Fulfilled(HostValue::Number(3.0)))?;
    vm.run_until_idle(&mut host)?;
    match vm.promise_state(&result)? {
        PromiseState::Pending => println!("result: pending (unexpected)"),
        PromiseState::Fulfilled(value) => {
            println!("result: {}", render_value(&vm.to_host_value(&value)?))
        }
        PromiseState::Rejected(value) => {
            println!(
                "result: rejected {}",
                render_value(&vm.to_host_value(&value)?)
            )
        }
    }
    println!(
        "audit: inspect={} seed={} wait={} (restore did not re-run effects)",
        host.calls("inspect"),
        host.calls("seed"),
        host.calls("wait")
    );
    Ok(())
}

fn render_value(value: &HostValue) -> String {
    match value {
        HostValue::Undefined => "undefined".to_owned(),
        HostValue::Null => "null".to_owned(),
        HostValue::Bool(value) => value.to_string(),
        HostValue::Number(value) => value.to_string(),
        HostValue::String(value) => format!("{value:?}"),
        HostValue::Native(key) => key.to_string(),
    }
}
