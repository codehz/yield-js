use std::collections::{BTreeMap, BTreeSet};

use yield_compiler::{CompileOptions, compile};
use yield_vm::bytecode::Build;
use yield_vm::host_data::{HostDataObject, HostDataValue};
use yield_vm::{
    Host, HostException, HostExport, HostReply, HostValue, NativeKey, PendingOperationMeta, Value,
};

mod case;

pub use case::run_case;

const EXPORTS: &[&str] = &[
    "inspect", "seed", "wait", "bind", "setLabel", "expect", "assert",
];

#[derive(Default)]
pub struct TestHost {
    calls: Vec<(String, Vec<HostValue>)>,
    errors: Vec<Value>,
    operations: BTreeMap<String, PendingOperationMeta>,
    natives: BTreeSet<NativeKey>,
    matchers: Vec<HostValue>,
    pub reject_restore_operation: bool,
    pub reject_native: bool,
}

impl TestHost {
    fn imported(module: &str, export: &str) -> bool {
        module == "test" && EXPORTS.contains(&export)
    }

    pub fn count(&self, name: &str) -> usize {
        self.calls.iter().filter(|(call, _)| call == name).count()
    }

    pub fn errors(&self) -> &[Value] {
        &self.errors
    }
}

impl Host for TestHost {
    fn resolve_export(&self, module: &str, export: &str) -> Option<HostExport> {
        Self::imported(module, export).then_some(HostExport::Function)
    }

    fn project_arguments(&self, module: &str, export: &str) -> bool {
        module == "test" && matches!(export, "expect" | "assert")
    }

    fn project_native_arguments(&self, key: &NativeKey) -> bool {
        is_matcher(&key.type_name)
    }

    fn call(
        &mut self,
        module: &str,
        export: &str,
        args: &[HostValue],
    ) -> Result<HostReply, String> {
        if !Self::imported(module, export) {
            return Err(format!("unknown host export {module}:{export}"));
        }
        self.calls.push((export.to_owned(), args.to_vec()));
        let value = match export {
            "inspect" | "setLabel" => HostValue::Undefined,
            "seed" => HostValue::Number(10.0),
            "wait" => {
                let label = string_arg(args, "wait")?;
                let id = format!("wait:{label}");
                self.operations.insert(
                    id.clone(),
                    PendingOperationMeta {
                        id: id.clone(),
                        module: module.to_owned(),
                        export: export.to_owned(),
                    },
                );
                return Ok(HostReply::Operation { operation_id: id });
            }
            "bind" => {
                let key = NativeKey {
                    type_name: "Node".into(),
                    id: string_arg(args, "bind")?,
                };
                self.natives.insert(key.clone());
                HostValue::Native(key)
            }
            "expect" => {
                let id = self.matchers.len().to_string();
                self.matchers
                    .push(args.first().cloned().unwrap_or(HostValue::Undefined));
                HostValue::Native(NativeKey {
                    type_name: "Matcher".into(),
                    id,
                })
            }
            "assert" => {
                let actual = args.first().unwrap_or(&HostValue::Undefined);
                if !truthy(actual) {
                    let message = match args.get(1) {
                        Some(HostValue::String(message)) => format!(": {message}"),
                        Some(message) => format!(": {message:?}"),
                        None => String::new(),
                    };
                    return Ok(HostReply::Throw(format!(
                        "assert: expected truthy, actual {actual:?}{message}"
                    )));
                }
                HostValue::Undefined
            }
            _ => unreachable!(),
        };
        Ok(HostReply::Value(value))
    }

    fn get_property(&self, key: &NativeKey, property: &str) -> Result<HostValue, HostException> {
        if key.type_name == "Matcher" && self.has_native(key) && is_matcher(property) {
            return Ok(HostValue::NativeFunction(NativeKey {
                type_name: property.to_owned(),
                id: key.id.clone(),
            }));
        }
        Ok(HostValue::Undefined)
    }

    fn call_native(
        &mut self,
        key: &NativeKey,
        args: &[HostValue],
    ) -> Result<HostReply, HostException> {
        let actual = self
            .matcher(key)
            .filter(|_| is_matcher(&key.type_name))
            .ok_or_else(|| HostException(HostValue::String("missing matcher binding".into())))?;
        let expected = args.first().unwrap_or(&HostValue::Undefined);
        let passed = match key.type_name.as_str() {
            "toBe" => same_value(actual, expected),
            "toEqual" => structural_equal(actual, expected),
            "toBeTruthy" => truthy(actual),
            _ => unreachable!(),
        };
        if passed {
            Ok(HostReply::Value(HostValue::Undefined))
        } else {
            let expected = if key.type_name == "toBeTruthy" {
                "truthy".to_owned()
            } else {
                format!("{expected:?}")
            };
            Err(HostException(HostValue::String(
                format!("{}: expected {expected}, actual {actual:?}", key.type_name).into(),
            )))
        }
    }

    fn report_error(&mut self, error: &Value) {
        self.errors.push(error.clone());
    }

    fn has_native(&self, key: &NativeKey) -> bool {
        !self.reject_native
            && (self.natives.contains(key)
                || ((key.type_name == "Matcher" || is_matcher(&key.type_name))
                    && self.matcher(key).is_some()))
    }

    fn rebuild_operation(&mut self, operation: &PendingOperationMeta) -> bool {
        !self.reject_restore_operation
            && self
                .operations
                .get(&operation.id)
                .is_some_and(|known| known == operation)
    }
}

impl TestHost {
    fn matcher(&self, key: &NativeKey) -> Option<&HostValue> {
        self.matchers.get(key.id.parse::<usize>().ok()?)
    }
}

fn string_arg(args: &[HostValue], export: &str) -> Result<String, String> {
    match args {
        [HostValue::String(value)] => value
            .to_utf8()
            .map_err(|_| format!("{export} requires valid UTF-16")),
        _ => Err(format!("{export} expects one string")),
    }
}

fn is_matcher(name: &str) -> bool {
    matches!(name, "toBe" | "toEqual" | "toBeTruthy")
}

fn truthy(value: &HostValue) -> bool {
    match value {
        HostValue::Undefined | HostValue::Null | HostValue::Bool(false) => false,
        HostValue::Number(n) => *n != 0.0 && !n.is_nan(),
        HostValue::String(s) => !s.is_empty(),
        _ => true,
    }
}

fn same_value(a: &HostValue, b: &HostValue) -> bool {
    match (a, b) {
        (HostValue::Data(a), HostValue::Data(b)) => a.identity == b.identity,
        (HostValue::Number(a), HostValue::Number(b)) => {
            a.to_bits() == b.to_bits() || a.is_nan() && b.is_nan()
        }
        _ => a == b,
    }
}

fn structural_equal(a: &HostValue, b: &HostValue) -> bool {
    let (HostValue::Data(a), HostValue::Data(b)) = (a, b) else {
        return same_value(a, b);
    };
    let mut pending = vec![(a.root, b.root)];
    let mut seen = BTreeSet::new();
    while let Some((left, right)) = pending.pop() {
        if !seen.insert((left, right)) {
            continue;
        }
        let (Some(left_object), Some(right_object)) = (a.objects.get(&left), b.objects.get(&right))
        else {
            return false;
        };
        match (left_object, right_object) {
            (HostDataObject::Opaque { identity: a }, HostDataObject::Opaque { identity: b })
                if a == b => {}
            (
                HostDataObject::Data {
                    array_length: al,
                    properties: ap,
                },
                HostDataObject::Data {
                    array_length: bl,
                    properties: bp,
                },
            ) if al == bl && ap.len() == bp.len() => {
                for ((ak, av), (bk, bv)) in ap.iter().zip(bp) {
                    if ak != bk {
                        return false;
                    }
                    match (av, bv) {
                        (HostDataValue::Object(left), HostDataValue::Object(right)) => {
                            pending.push((*left, *right))
                        }
                        (HostDataValue::Scalar(a), HostDataValue::Scalar(b))
                            if same_value(a, b) => {}
                        (HostDataValue::Symbol(a), HostDataValue::Symbol(b)) if a == b => (),
                        _ => return false,
                    }
                }
            }
            _ => return false,
        }
    }
    true
}

pub(crate) fn compile_options() -> CompileOptions {
    CompileOptions {
        imports: BTreeMap::from([(
            "test".to_owned(),
            EXPORTS.iter().map(|name| (*name).to_owned()).collect(),
        )]),
    }
}

pub fn build(source: &str) -> Build {
    compile(source, &compile_options()).expect("source compiles")
}

pub fn number(value: f64) -> HostValue {
    HostValue::Number(value)
}
