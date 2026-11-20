//! PoC name-resolution policy.
//!
//! Locals shadow indexed imports, preload globals, and builtins, in that order.
//! `typeof` folds undeclared names and preserves legacy shadowing folds.
use std::collections::BTreeMap;

use crate::env::Env;
use yield_vm::BindingId;
use yield_vm::bytecode::BUILTINS;
use yield_vm::bytecode::ImportDecl;

pub(crate) struct Tables<'a> {
    pub(crate) imports: &'a BTreeMap<String, ImportDecl>,
    pub(crate) top_level_functions: &'a BTreeMap<String, BindingId>,
    pub(crate) preload: Option<&'a BTreeMap<String, BindingId>>,
}

#[derive(Clone, Copy)]
pub(crate) enum ReadOnlyName<'name> {
    Import(ImportDecl),
    Preload(&'name str),
}

pub(crate) enum ValueName<'name> {
    Binding(BindingId),
    ReadOnly(ReadOnlyName<'name>),
    Undefined,
    /// Index into [`BUILTINS`]. Locals shadow this.
    Builtin(usize),
    Unknown,
}

pub(crate) enum Callee<'name> {
    Binding(BindingId),
    ReadOnly(ReadOnlyName<'name>),
    Builtin(usize),
    Unknown,
}

impl Tables<'_> {
    /// Resolves the read-only names after the caller has checked local bindings.
    pub(crate) fn read_only<'name>(&self, name: &'name str) -> Option<ReadOnlyName<'name>> {
        if let Some(import) = self.imports.get(name).copied() {
            Some(ReadOnlyName::Import(import))
        } else if self.has_preload(name) {
            Some(ReadOnlyName::Preload(name))
        } else {
            None
        }
    }

    fn has_preload(&self, name: &str) -> bool {
        self.preload
            .is_some_and(|exports| exports.contains_key(name))
    }

    pub(crate) fn value<'name>(&self, env: &Env, name: &'name str) -> ValueName<'name> {
        if let Some(binding) = env.lookup(name) {
            ValueName::Binding(binding)
        } else if let Some(read_only) = self.read_only(name) {
            ValueName::ReadOnly(read_only)
        } else if name == "undefined" {
            ValueName::Undefined
        } else if let Some(index) = builtin_index(name) {
            ValueName::Builtin(index)
        } else {
            ValueName::Unknown
        }
    }

    pub(crate) fn callee<'name>(&self, env: &Env, name: &'name str) -> Callee<'name> {
        match self.value(env, name) {
            ValueName::Binding(binding) => Callee::Binding(binding),
            ValueName::ReadOnly(read_only) => Callee::ReadOnly(read_only),
            ValueName::Builtin(index) => Callee::Builtin(index),
            ValueName::Undefined | ValueName::Unknown => Callee::Unknown,
        }
    }

    /// `Some(folded)` when `typeof name` must not evaluate `name`.
    /// `None` means lower the operand and emit `UnaryOp::TypeOf`.
    pub(crate) fn typeof_fold(&self, env: &Env, name: &str) -> Option<&'static str> {
        if self.has_preload(name) {
            return None;
        }
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
