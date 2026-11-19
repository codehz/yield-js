//! Lexical environments.
//!
//! The compiler assigns stable [`BindingId`]s and records the scope chain a
//! nested function captures. Bindings read or written from another function
//! stay in a heap environment; the rest become frame slots after lowering.
//! `let` and `const` are not distinguished. [`Env::pending_lexicals`] tracks
//! bindings that have entered scope in this function but have not finished
//! initializing. [`Env::capture`] copies only the name-to-id chain, so a
//! nested function starts with an empty pending set and does not reject an
//! early read of an outer binding. This is not full ECMAScript TDZ, and loop
//! iterations do not get fresh bindings.

use std::collections::{BTreeMap, BTreeSet};

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
    /// Lexical bindings in this function whose initializer has not run.
    pending_lexicals: BTreeSet<BindingId>,
}

impl Env {
    pub(crate) fn new(outer_scopes: Vec<BTreeMap<String, BindingId>>) -> Self {
        Self {
            function_scope: BTreeMap::new(),
            scopes: vec![BTreeMap::new()],
            outer_scopes,
            pending_lexicals: BTreeSet::new(),
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
        if let Some(scope) = self.scopes.pop() {
            for binding in scope.values() {
                self.pending_lexicals.remove(binding);
            }
        }
    }

    pub(crate) fn current_binding(&self, name: &str) -> Option<BindingId> {
        self.scopes
            .last()
            .and_then(|scope| scope.get(name).copied())
    }

    pub(crate) fn mark_pending(&mut self, binding: BindingId) {
        self.pending_lexicals.insert(binding);
    }

    pub(crate) fn clear_pending(&mut self, binding: BindingId) {
        self.pending_lexicals.remove(&binding);
    }

    /// Reject a use of a lexical binding whose initializer has not been lowered.
    pub(crate) fn check_tdz(
        &self,
        binding: BindingId,
        name: &str,
        source: &str,
        span: Span,
    ) -> Result<(), CompileError> {
        if self.pending_lexicals.contains(&binding) {
            return Err(CompileError::new(
                source,
                span,
                format!("cannot access {name:?} before initialization"),
            ));
        }
        Ok(())
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
    ///
    /// `Some` is the id in the scope the name was installed into. A lexical
    /// name skipped because it clashes with the function scope is `None`.
    pub(crate) fn predeclare(
        &mut self,
        next_binding: &mut BindingId,
        name: &str,
        kind: BindingKind,
    ) -> Option<BindingId> {
        match kind {
            BindingKind::Var => {
                if let Some(binding) = self.function_scope.get(name).copied() {
                    return Some(binding);
                }
                let binding = alloc_binding(next_binding);
                self.function_scope.insert(name.to_owned(), binding);
                Some(binding)
            }
            BindingKind::Lexical => {
                let conflicts_with_function_scope =
                    self.scopes.len() == 1 && self.function_scope.contains_key(name);
                if let Some(binding) = self.current_binding(name) {
                    return Some(binding);
                }
                if conflicts_with_function_scope {
                    return None;
                }
                let binding = alloc_binding(next_binding);
                self.scopes
                    .last_mut()
                    .expect("scope stack is never empty")
                    .insert(name.to_owned(), binding);
                Some(binding)
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
