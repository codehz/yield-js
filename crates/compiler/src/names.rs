//! PoC name-resolution policy.
//!
//! Locals shadow everything at value and call positions. Host imports are
//! callable, but not first-class values. Direct script functions are ordinary
//! mutable bindings. `typeof` still folds an undeclared name to `"undefined"`,
//! an import to `"function"` even when a local shadows it, and a local that
//! shadows a direct script function to `"function"`. The live script binding
//! itself is evaluated so a later assignment is visible. Callers check TDZ
//! before folding.
use std::collections::BTreeMap;

use crate::env::Env;
use yield_vm::BindingId;
use yield_vm::bytecode::BUILTINS;

pub(crate) struct Tables<'a> {
    pub(crate) imports: &'a BTreeMap<String, u32>,
    pub(crate) top_level_functions: &'a BTreeMap<String, BindingId>,
}

pub(crate) enum ValueName {
    Binding(BindingId),
    Undefined,
    /// Host import. The PoC rejects reading it as a value.
    NotFirstClass,
    /// Index into [`BUILTINS`]. Locals shadow this; the builtin shadows imports.
    Builtin(usize),
    Unknown,
}

pub(crate) enum Callee {
    Binding(BindingId),
    Import(u32),
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
        } else {
            Callee::Unknown
        }
    }

    /// `Some(folded)` when `typeof name` must not evaluate `name`.
    /// `None` means lower the operand and emit `UnaryOp::TypeOf`.
    pub(crate) fn typeof_fold(&self, env: &Env, name: &str) -> Option<&'static str> {
        let binding = env.lookup(name);
        let known = name == "undefined"
            || binding.is_some()
            || self.imports.contains_key(name)
            || self.top_level_functions.contains_key(name)
            || builtin_index(name).is_some();
        if !known {
            return Some("undefined");
        }
        if self.imports.contains_key(name) {
            return Some("function");
        }
        if let Some(function) = self.top_level_functions.get(name).copied()
            && binding != Some(function)
        {
            return Some("function");
        }
        None
    }
}

pub(crate) fn builtin_index(name: &str) -> Option<usize> {
    BUILTINS.iter().position(|builtin| *builtin == name)
}
