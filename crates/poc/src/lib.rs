use std::collections::{BTreeMap, BTreeSet};

use yield_compiler::{CompileOptions, compile};
use yield_vm::bytecode::Build;
use yield_vm::{Host, HostReply, HostValue, NativeKey, PendingOperationMeta, Value};

mod case;

pub use case::run_case;

const IMPORTS: [&str; 5] = ["inspect", "seed", "wait", "bind", "setLabel"];

#[derive(Default)]
pub struct TestHost {
    calls: Vec<(String, Vec<HostValue>)>,
    errors: Vec<Value>,
    operations: BTreeMap<String, PendingOperationMeta>,
    natives: BTreeSet<NativeKey>,
    pub reject_restore_operation: bool,
    pub reject_native: bool,
    fail_import: bool,
}

impl TestHost {
    fn imported(name: &str) -> bool {
        IMPORTS.contains(&name)
    }

    pub fn count(&self, name: &str) -> usize {
        self.calls.iter().filter(|(call, _)| call == name).count()
    }

    pub fn errors(&self) -> &[Value] {
        &self.errors
    }
}

impl Host for TestHost {
    fn has_import(&self, name: &str) -> bool {
        Self::imported(name) && !self.fail_import
    }

    fn call(&mut self, import: &str, args: &[HostValue]) -> Result<HostReply, String> {
        self.calls.push((import.to_owned(), args.to_vec()));
        match import {
            "inspect" => Ok(HostReply::Value(HostValue::Undefined)),
            "seed" => Ok(HostReply::Value(HostValue::Number(10.0))),
            "wait" => {
                let label = match args {
                    [HostValue::String(label)] => label.clone(),
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
            "bind" => {
                let id = match args {
                    [HostValue::String(id)] => id.clone(),
                    _ => return Err("bind expects one string".to_owned()),
                };
                let key = NativeKey {
                    type_name: "Node".to_owned(),
                    id,
                };
                self.natives.insert(key.clone());
                Ok(HostReply::Value(HostValue::Native(key)))
            }
            "setLabel" => Ok(HostReply::Value(HostValue::Undefined)),
            _ => Err(format!("unknown import {import}")),
        }
    }

    fn report_error(&mut self, error: &Value) {
        self.errors.push(error.clone());
    }

    fn has_native(&self, key: &NativeKey) -> bool {
        !self.reject_native && self.natives.contains(key)
    }

    fn rebuild_operation(&mut self, operation: &PendingOperationMeta) -> bool {
        !self.reject_restore_operation
            && self
                .operations
                .get(&operation.id)
                .is_some_and(|known| known == operation)
    }
}

pub(crate) fn compile_options() -> CompileOptions {
    CompileOptions {
        imports: IMPORTS.iter().copied().map(str::to_owned).collect(),
    }
}

pub fn build(source: &str) -> Build {
    compile(source, &compile_options()).expect("source compiles")
}

pub fn number(value: f64) -> HostValue {
    HostValue::Number(value)
}
