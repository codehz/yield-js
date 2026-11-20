//! PoC name-resolution policy.
//!
//! Static imports are indexed operands; locals shadow imports and builtins
//! at value and call positions. `typeof` folds an undeclared name to
//! `"undefined"`, and preserves the legacy `"function"` fold for a local
//! shadowing an import. Unshadowed imports evaluate their actual value.
use std::collections::BTreeMap;

use crate::env::Env;
use yield_vm::BindingId;
use yield_vm::bytecode::BUILTINS;
use yield_vm::bytecode::ImportDecl;

pub(crate) struct Tables<'a> {
    pub(crate) imports: &'a BTreeMap<String, ImportDecl>,
    pub(crate) top_level_functions: &'a BTreeMap<String, BindingId>,
}

pub(crate) enum ValueName {
    Binding(BindingId),
    Import(ImportDecl),
    Undefined,
    /// Index into [`BUILTINS`]. Locals shadow this.
    Builtin(usize),
    Unknown,
}

pub(crate) enum Callee {
    Binding(BindingId),
    Import(ImportDecl),
    Builtin(usize),
    Unknown,
}

impl Tables<'_> {
    pub(crate) fn value(&self, env: &Env, name: &str) -> ValueName {
        if let Some(binding) = env.lookup(name) {
            ValueName::Binding(binding)
        } else if let Some(import) = self.imports.get(name).copied() {
            ValueName::Import(import)
        } else if name == "undefined" {
            ValueName::Undefined
        } else if let Some(index) = builtin_index(name) {
            ValueName::Builtin(index)
        } else {
            ValueName::Unknown
        }
    }

    pub(crate) fn callee(&self, env: &Env, name: &str) -> Callee {
        if let Some(binding) = env.lookup(name) {
            Callee::Binding(binding)
        } else if let Some(import) = self.imports.get(name).copied() {
            Callee::Import(import)
        } else if let Some(index) = builtin_index(name) {
            Callee::Builtin(index)
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
        if self.imports.contains_key(name) && binding.is_some() {
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
