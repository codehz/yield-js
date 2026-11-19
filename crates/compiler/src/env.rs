//! Lexical environments.
//!
//! The compiler assigns stable [`BindingId`]s and records the scope chain a
//! nested function captures. Bindings read or written from another function
//! stay in a heap environment; the rest become frame slots after lowering.
//! `let` and `const` are not distinguished: this PoC does not track TDZ. That
//! distinction, when added, belongs here rather than in the emitter.

use std::collections::BTreeMap;

use oxc::span::Span;
use yield_vm::BindingId;

use crate::error::CompileError;

/// How a name is introduced. Callers reject `using` before constructing this.
#[derive(Clone, Copy)]
pub(crate) enum BindingKind {
    /// Function-scoped. A redeclaration reuses the existing id.
    Var,
    /// Block-scoped. A clash with a function-scoped name in the function body
    /// is an error; a name already predeclared in the current block is reused.
    Lexical,
}

pub(crate) struct Env {
    function_scope: BTreeMap<String, BindingId>,
    scopes: Vec<BTreeMap<String, BindingId>>,
    outer_scopes: Vec<BTreeMap<String, BindingId>>,
}

impl Env {
    pub(crate) fn new(outer_scopes: Vec<BTreeMap<String, BindingId>>) -> Self {
        Self {
            function_scope: BTreeMap::new(),
            scopes: vec![BTreeMap::new()],
            outer_scopes,
        }
    }

    pub(crate) fn lookup(&self, name: &str) -> Option<BindingId> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).copied())
            .or_else(|| self.function_scope.get(name).copied())
            .or_else(|| {
                self.outer_scopes
                    .iter()
                    .rev()
                    .find_map(|scope| scope.get(name).copied())
            })
    }

    pub(crate) fn has_function_binding(&self, name: &str) -> bool {
        self.function_scope.contains_key(name)
    }

    pub(crate) fn insert_function(&mut self, name: String, binding: BindingId) {
        self.function_scope.insert(name, binding);
    }

    pub(crate) fn enter_block(&mut self) {
        self.scopes.push(BTreeMap::new());
    }

    pub(crate) fn exit_block(&mut self) {
        self.scopes.pop();
    }

    /// Scope chain a nested function closes over, including this function's
    /// parameter/var scope and every open block.
    pub(crate) fn capture(&self) -> Vec<BTreeMap<String, BindingId>> {
        let mut outer = self.outer_scopes.clone();
        outer.push(self.function_scope.clone());
        outer.extend(self.scopes.iter().cloned());
        outer
    }

    pub(crate) fn declare(
        &mut self,
        next_binding: &mut BindingId,
        source: &str,
        name: &str,
        kind: BindingKind,
        span: Span,
    ) -> Result<(BindingId, bool), CompileError> {
        match kind {
            BindingKind::Var => {
                if let Some(binding) = self.function_scope.get(name) {
                    return Ok((*binding, false));
                }
                let binding = alloc_binding(next_binding);
                self.function_scope.insert(name.to_owned(), binding);
                Ok((binding, true))
            }
            BindingKind::Lexical => {
                let conflicts_with_function_scope =
                    self.scopes.len() == 1 && self.function_scope.contains_key(name);
                let current = self.scopes.last().expect("scope stack is never empty");
                if let Some(binding) = current.get(name) {
                    return Ok((*binding, false));
                }
                if conflicts_with_function_scope {
                    return Err(CompileError::new(
                        source,
                        span,
                        format!("duplicate lexical binding {name:?}"),
                    ));
                }
                let binding = alloc_binding(next_binding);
                self.scopes
                    .last_mut()
                    .expect("scope stack is never empty")
                    .insert(name.to_owned(), binding);
                Ok((binding, true))
            }
        }
    }

    /// Install a hoisted name if it is not already present. Duplicate `let`
    /// against the function scope is reported later by [`Self::declare`], when
    /// the declaration statement is lowered.
    pub(crate) fn predeclare(
        &mut self,
        next_binding: &mut BindingId,
        name: &str,
        kind: BindingKind,
    ) {
        match kind {
            BindingKind::Var => {
                if !self.function_scope.contains_key(name) {
                    let binding = alloc_binding(next_binding);
                    self.function_scope.insert(name.to_owned(), binding);
                }
            }
            BindingKind::Lexical => {
                let conflicts_with_function_scope =
                    self.scopes.len() == 1 && self.function_scope.contains_key(name);
                if !conflicts_with_function_scope
                    && !self
                        .scopes
                        .last()
                        .expect("scope stack is never empty")
                        .contains_key(name)
                {
                    let binding = alloc_binding(next_binding);
                    self.scopes
                        .last_mut()
                        .expect("scope stack is never empty")
                        .insert(name.to_owned(), binding);
                }
            }
        }
    }
}

fn alloc_binding(next_binding: &mut BindingId) -> BindingId {
    let binding = *next_binding;
    *next_binding = next_binding
        .checked_add(1)
        .expect("binding id space exhausted");
    binding
}
