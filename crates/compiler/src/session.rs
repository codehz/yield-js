//! Program-wide compile session: host imports, constant pool, binding ids,
//! and the finished function table.
//!
//! No AST. Nested functions enqueue work through [`crate::lower`], which
//! borrows this session only for interning and id allocation.

use std::collections::{BTreeMap, BTreeSet};

use oxc::span::Span;
use yield_vm::BindingId;
use yield_vm::bytecode::{
    Build, Constant, Function as BytecodeFunction, ImportDecl, Instr, Preload, Slot,
};

use crate::CompileOptions;
use crate::error::CompileError;
use crate::names::Tables;

pub(crate) struct Session<'src> {
    pub(crate) source: &'src str,
    import_bindings: BTreeMap<String, ImportDecl>,
    top_level_functions: BTreeMap<String, BindingId>,
    exports: BTreeMap<String, BindingId>,
    preload: Option<Preload>,
    constants: Vec<Constant>,
    string_constants: BTreeMap<yield_vm::JsString, u32>,
    functions: Vec<BytecodeFunction>,
    imports: BTreeMap<String, BTreeSet<String>>,
    next_func_id: u32,
    next_binding_id: BindingId,
}

impl<'src> Session<'src> {
    pub(crate) fn new(source: &'src str, options: &CompileOptions) -> Self {
        Self {
            source,
            top_level_functions: BTreeMap::new(),
            exports: BTreeMap::new(),
            preload: None,
            constants: Vec::new(),
            string_constants: BTreeMap::new(),
            functions: Vec::new(),
            imports: options.imports.clone(),
            import_bindings: BTreeMap::new(),
            next_func_id: 1,
            next_binding_id: 0,
        }
    }

    pub(crate) fn resolve_import(
        &self,
        module: &str,
        module_span: Span,
        export: Option<(&str, Span)>,
    ) -> Result<ImportDecl, CompileError> {
        let (module_index, (_, exports)) = self
            .imports
            .iter()
            .enumerate()
            .find(|(_, (specifier, _))| specifier.as_str() == module)
            .ok_or_else(|| {
                crate::error::unsupported(
                    self.source,
                    module_span,
                    &format!("unknown host module {module:?}"),
                )
            })?;
        let export = export
            .map(|(name, span)| {
                let index = exports
                    .iter()
                    .position(|export| export == name)
                    .ok_or_else(|| {
                        crate::error::unsupported(
                            self.source,
                            span,
                            &format!("host module {module:?} has no export {name:?}"),
                        )
                    })?;
                u32::try_from(index).map_err(|_| {
                    CompileError::new(self.source, span, "too many host module exports")
                })
            })
            .transpose()?;
        let module = u32::try_from(module_index)
            .map_err(|_| CompileError::new(self.source, module_span, "too many host modules"))?;
        Ok(ImportDecl { module, export })
    }

    pub(crate) fn bind_import(&mut self, local: &str, import: ImportDecl) {
        self.import_bindings.insert(local.to_owned(), import);
    }

    pub(crate) fn import(&self, name: &str) -> Option<ImportDecl> {
        self.import_bindings.get(name).copied()
    }

    pub(crate) fn names(&self) -> Tables<'_> {
        Tables {
            imports: &self.import_bindings,
            top_level_functions: &self.top_level_functions,
            preload: self.preload.as_ref().map(|preload| &preload.exports),
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
        if let Constant::String(value) = constant {
            if let Some(&index) = self.string_constants.get(&value) {
                return index;
            }
            let index = self.constants.len() as u32;
            self.string_constants.insert(value.clone(), index);
            self.constants.push(Constant::String(value));
            return index;
        }
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
        if function.func_id == 0 {
            // Main is lowered second, but stays at the head of the build table.
            self.functions.insert(0, function);
        } else {
            self.functions.push(function);
        }
    }

    pub(crate) fn finish_preload(
        &mut self,
        initializer: u32,
        main_source: &'src str,
    ) -> Result<(), CompileError> {
        let exports = std::mem::take(&mut self.exports);
        let mut by_binding = BTreeMap::new();
        let mut getters = BTreeMap::new();
        for (name, &binding) in &exports {
            let func_id = match by_binding.entry(binding) {
                std::collections::btree_map::Entry::Occupied(entry) => *entry.get(),
                std::collections::btree_map::Entry::Vacant(entry) => {
                    let func_id = self.alloc_func_id(Span::default())?;
                    let slot = Slot::transient(0);
                    self.push_function(BytecodeFunction {
                        func_id,
                        name: format!("\0preload_getter@{func_id}"),
                        is_async: false,
                        is_arrow: false,
                        is_method: true,
                        length: 0,
                        params: Vec::new(),
                        rest_param: None,
                        this_param: None,
                        home_param: None,
                        new_target_param: None,
                        arguments_param: None,
                        transient_slots: 1,
                        persistent_slots: 0,
                        code: vec![
                            Instr::GetBinding { dst: slot, binding },
                            Instr::Return { src: slot },
                        ],
                        resumes: Vec::new(),
                        handlers: Vec::new(),
                    });
                    *entry.insert(func_id)
                }
            };
            getters.insert(name.clone(), func_id);
        }
        self.preload = Some(Preload {
            initializer,
            exports,
            getters,
        });
        self.import_bindings.clear();
        self.top_level_functions.clear();
        self.source = main_source;
        Ok(())
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
        build.preload = self.preload;
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
