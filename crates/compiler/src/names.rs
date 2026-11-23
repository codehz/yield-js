//! PoC name-resolution policy.
//!
//! Locals shadow indexed imports, preload globals, and builtins, in that order.
//! `typeof` folds undeclared names and preserves legacy shadowing folds.
use std::collections::BTreeMap;

use crate::env::Env;
use yield_vm::BindingId;
use yield_vm::bytecode::BUILTINS;
use yield_vm::bytecode::ModuleTarget;

pub(crate) struct Tables<'a> {
    pub(crate) imports: &'a BTreeMap<String, ModuleTarget>,
    pub(crate) preload: Option<&'a BTreeMap<String, crate::frontend::link::Target>>,
}

#[derive(Clone, Copy)]
pub(crate) enum ReadOnlyName<'name> {
    Import(ModuleTarget),
    Preload(&'name str),
}

pub(crate) enum ValueName<'name> {
    Binding(BindingId),
    ReadOnly(ReadOnlyName<'name>),
    DynamicGlobal(&'name str),
    Undefined,
    /// Index into [`BUILTINS`]. Locals shadow this.
    Builtin(usize),
    Unknown,
}

pub(crate) enum Callee<'name> {
    Binding(BindingId),
    ReadOnly(ReadOnlyName<'name>),
    DynamicGlobal(&'name str),
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
        } else if is_dynamic_global(name) {
            ValueName::DynamicGlobal(name)
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
            ValueName::DynamicGlobal(name) => Callee::DynamicGlobal(name),
            ValueName::Builtin(index) => Callee::Builtin(index),
            ValueName::Undefined | ValueName::Unknown => Callee::Unknown,
        }
    }

    /// `Some(folded)` when `typeof name` must not evaluate `name`.
    /// `None` means lower the operand and emit `UnaryOp::TypeOf`.
    ///
    /// Only a genuinely undeclared identifier folds, to `"undefined"`. A
    /// declared identifier (local, host import, preload global, top-level
    /// function, or builtin) evaluates like any other identifier: shadowing
    /// must not fold to `"function"`, so `typeof` of a shadowed import or
    /// top-level function reports the runtime value of the inner binding.
    pub(crate) fn typeof_fold(&self, env: &Env, name: &str) -> Option<&'static str> {
        matches!(self.value(env, name), ValueName::Unknown).then_some("undefined")
    }
}

pub(crate) fn builtin_index(name: &str) -> Option<usize> {
    BUILTINS.iter().position(|builtin| *builtin == name)
}

/// Known names whose values come from mutable global object properties.
pub(crate) fn is_dynamic_global(name: &str) -> bool {
    matches!(
        name,
        "onerror" | "onunhandledrejection" | "onrejectionhandled" | "console"
    )
}
