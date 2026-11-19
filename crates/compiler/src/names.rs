//! PoC name-resolution policy.
//!
//! Locals shadow everything at value and call positions. Host imports are
//! callable, but not first-class values. Top-level functions are callable and,
//! when not shadowed by a local, import, or builtin, also values. `typeof` is
//! the exception: an import or top-level function of the same name is reported
//! as `"function"` even when a local binding shadows it. That quirk is
//! intentional compatibility with the current PoC, not an accident of the walker.
use std::collections::BTreeMap;

use crate::env::Env;
use yield_vm::BindingId;
use yield_vm::bytecode::BUILTINS;

pub(crate) struct Tables<'a> {
    pub(crate) imports: &'a BTreeMap<String, u32>,
    pub(crate) top_level: &'a BTreeMap<String, u32>,
}

pub(crate) enum ValueName {
    Binding(BindingId),
    Undefined,
    /// Host import. The PoC rejects reading it as a value.
    NotFirstClass,
    /// Index into [`BUILTINS`]. Locals shadow this; the builtin shadows imports.
    Builtin(usize),
    /// Top-level function id. Locals, builtins, and imports win first.
    TopFunction(u32),
    Unknown,
}

pub(crate) enum Callee {
    Binding(BindingId),
    Import(u32),
    Function(u32),
    Builtin(usize),
    Unknown,
}

impl Tables<'_> {
    pub(crate) fn value(&self, env: &Env, name: &str) -> ValueName {
        if let Some(binding) = env.lookup(name) {
            ValueName::Binding(binding)
        } else if name == "undefined" {
            ValueName::Undefined
        } else if let Some(index) = builtin_index(name) {
            ValueName::Builtin(index)
        } else if self.imports.contains_key(name) {
            ValueName::NotFirstClass
        } else if let Some(function) = self.top_level.get(name).copied() {
            ValueName::TopFunction(function)
        } else {
            ValueName::Unknown
        }
    }

    pub(crate) fn callee(&self, env: &Env, name: &str) -> Callee {
        if let Some(binding) = env.lookup(name) {
            Callee::Binding(binding)
        } else if let Some(index) = builtin_index(name) {
            Callee::Builtin(index)
        } else if let Some(import) = self.imports.get(name).copied() {
            Callee::Import(import)
        } else if let Some(function) = self.top_level.get(name).copied() {
            Callee::Function(function)
        } else {
            Callee::Unknown
        }
    }

    /// `Some(folded)` when `typeof name` must not evaluate `name`.
    /// `None` means lower the operand and emit `UnaryOp::TypeOf`.
    pub(crate) fn typeof_fold(&self, env: &Env, name: &str) -> Option<&'static str> {
        let known = name == "undefined"
            || env.lookup(name).is_some()
            || self.is_import_or_top_level(name)
            || builtin_index(name).is_some();
        if !known {
            return Some("undefined");
        }
        if self.is_import_or_top_level(name) {
            return Some("function");
        }
        None
    }

    fn is_import_or_top_level(&self, name: &str) -> bool {
        self.imports.contains_key(name) || self.top_level.contains_key(name)
    }
}

pub(crate) fn builtin_index(name: &str) -> Option<usize> {
    BUILTINS.iter().position(|builtin| *builtin == name)
}
