//! Program-wide compile session: host imports, constant pool, binding ids,
//! and the finished function table.
//!
//! No AST. Nested functions enqueue work through [`crate::lower`], which
//! borrows this session only for interning and id allocation.

use std::collections::BTreeMap;

use oxc::span::Span;
use yield_vm::bytecode::{
    Build, Constant, Function as BytecodeFunction, Instr, ModuleTarget, Slot, SourceModule,
    TemplateSite,
};
use yield_vm::{BindingId, JsString};

use crate::CompileOptions;
use crate::error::CompileError;

pub(crate) struct Session {
    constants: Vec<Constant>,
    string_constants: BTreeMap<yield_vm::JsString, u32>,
    templates: Vec<TemplateSite>,
    functions: Vec<BytecodeFunction>,
    next_func_id: u32,
    next_binding_id: BindingId,
}

impl Session {
    pub(crate) fn new() -> Self {
        Self {
            constants: Vec::new(),
            string_constants: BTreeMap::new(),
            templates: Vec::new(),
            functions: Vec::new(),
            next_func_id: 1,
            next_binding_id: 0,
        }
    }

    /// Isolate scratch lowering while keeping its IDs disjoint from reserved IDs.
    pub(crate) fn fork_for_probe(&self) -> Self {
        Self {
            next_func_id: self.next_func_id,
            next_binding_id: self.next_binding_id,
            ..Self::new()
        }
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

    pub(crate) fn constants(&self) -> &[Constant] {
        &self.constants
    }

    pub(crate) fn add_template(
        &mut self,
        cooked: Vec<Option<JsString>>,
        raw: Vec<JsString>,
    ) -> Result<u32, CompileError> {
        let index = u32::try_from(self.templates.len())
            .map_err(|_| CompileError::internal("too many tagged-template call sites"))?;
        self.templates.push(TemplateSite { cooked, raw });
        Ok(index)
    }

    pub(crate) fn binding_ids(&mut self) -> &mut BindingId {
        &mut self.next_binding_id
    }

    pub(crate) fn alloc_func_id(&mut self, source: &str, span: Span) -> Result<u32, CompileError> {
        let func_id = self.next_func_id;
        self.next_func_id = self
            .next_func_id
            .checked_add(1)
            .ok_or_else(|| CompileError::new(source, span, "too many functions"))?;
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

    pub(crate) fn getter(&mut self, target: ModuleTarget) -> Result<u32, CompileError> {
        let func_id = self.alloc_func_id("", Span::default())?;
        let slot = Slot::transient(0);
        self.push_function(BytecodeFunction {
            func_id,
            name: format!("\0module_getter@{func_id}"),
            initial_name: String::new(),
            is_async: false,
            is_arrow: false,
            is_method: true,
            is_generator: false,
            length: 0,
            params: Vec::new(),
            rest_param: None,
            this_param: None,
            home_param: None,
            new_target_param: None,
            arguments_param: None,
            transient_slots: 1,
            persistent_slots: 0,
            code: vec![read_target(slot, target), Instr::Return { src: slot }],
            resumes: Vec::new(),
            generator_start: None,
            generator_resumes: Vec::new(),
            handlers: Vec::new(),
        });
        Ok(func_id)
    }

    pub(crate) fn finish(
        self,
        modules: Vec<SourceModule>,
        entry: u32,
        preload: Option<u32>,
        options: &CompileOptions,
    ) -> Result<Build, CompileError> {
        let mut build = Build::new(
            self.constants,
            self.templates,
            options.imports.clone(),
            self.functions,
            modules,
            entry,
            preload,
        );
        build.finalize();
        build.validate().map_err(|error| {
            CompileError::internal(format!("internal bytecode validation failed: {error}"))
                .in_module(&build.modules[entry as usize].id)
        })?;
        Ok(build)
    }
}

pub(crate) fn read_target(dst: Slot, target: ModuleTarget) -> Instr {
    match target {
        ModuleTarget::Binding { module, binding } => Instr::GetModuleBinding {
            dst,
            module,
            binding,
        },
        ModuleTarget::Namespace { module } => Instr::GetModuleNamespace { dst, module },
        ModuleTarget::Host(import) => Instr::GetImport { dst, import },
    }
}
