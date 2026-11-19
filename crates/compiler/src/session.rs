//! Program-wide compile session: host imports, constant pool, binding ids,
//! and the finished function table.
//!
//! No AST. Nested functions enqueue work through [`crate::lower`], which
//! borrows this session only for interning and id allocation.

use std::collections::BTreeMap;

use oxc::span::Span;
use yield_vm::BindingId;
use yield_vm::bytecode::{Build, Constant, Function as BytecodeFunction, ImportDecl};

use crate::CompileOptions;
use crate::error::CompileError;
use crate::names::Tables;

pub(crate) struct Session<'src> {
    pub(crate) source: &'src str,
    import_names: BTreeMap<String, u32>,
    /// Direct program function names, for the local `typeof` policy only.
    top_level_functions: BTreeMap<String, BindingId>,
    exports: BTreeMap<String, BindingId>,
    constants: Vec<Constant>,
    functions: Vec<BytecodeFunction>,
    imports: Vec<ImportDecl>,
    next_func_id: u32,
    next_binding_id: BindingId,
}

impl<'src> Session<'src> {
    pub(crate) fn new(source: &'src str, options: &CompileOptions) -> Result<Self, CompileError> {
        let mut imports = Vec::with_capacity(options.imports.len());
        let mut import_names = BTreeMap::new();
        for name in &options.imports {
            if import_names.contains_key(name) {
                return Err(CompileError::new(
                    source,
                    Span::default(),
                    format!("duplicate host import {name:?}"),
                ));
            }
            import_names.insert(name.clone(), imports.len() as u32);
            imports.push(ImportDecl { name: name.clone() });
        }
        Ok(Self {
            source,
            import_names,
            top_level_functions: BTreeMap::new(),
            exports: BTreeMap::new(),
            constants: Vec::new(),
            functions: Vec::new(),
            imports,
            // Function 0 is the synthetic script.
            next_func_id: 1,
            next_binding_id: 0,
        })
    }

    pub(crate) fn names(&self) -> Tables<'_> {
        Tables {
            imports: &self.import_names,
            top_level_functions: &self.top_level_functions,
        }
    }

    pub(crate) fn record_top_level_function(&mut self, name: String, binding: BindingId) {
        self.top_level_functions.insert(name, binding);
    }

    pub(crate) fn insert_export(
        &mut self,
        name: &str,
        binding: BindingId,
        span: Span,
    ) -> Result<(), CompileError> {
        if self.exports.contains_key(name) {
            return Err(CompileError::new(
                self.source,
                span,
                format!("duplicate export name {name:?}"),
            ));
        }
        self.exports.insert(name.to_owned(), binding);
        Ok(())
    }

    pub(crate) fn exports(&self) -> &BTreeMap<String, BindingId> {
        &self.exports
    }

    pub(crate) fn intern(&mut self, constant: Constant) -> u32 {
        let index = self.constants.len() as u32;
        self.constants.push(constant);
        index
    }

    pub(crate) fn binding_ids(&mut self) -> &mut BindingId {
        &mut self.next_binding_id
    }

    pub(crate) fn alloc_func_id(&mut self, span: Span) -> Result<u32, CompileError> {
        let func_id = self.next_func_id;
        self.next_func_id = self
            .next_func_id
            .checked_add(1)
            .ok_or_else(|| CompileError::new(self.source, span, "too many functions"))?;
        Ok(func_id)
    }

    pub(crate) fn push_function(&mut self, function: BytecodeFunction) {
        self.functions.push(function);
    }

    pub(crate) fn finish(self) -> Result<Build, CompileError> {
        let source = self.source;
        let mut build = Build::new(
            self.constants,
            self.imports,
            self.functions,
            0,
            self.exports,
        );
        build.finalize();
        build.validate().map_err(|error| {
            CompileError::new(
                source,
                Span::default(),
                format!("internal bytecode validation failed: {error}"),
            )
        })?;
        Ok(build)
    }
}
