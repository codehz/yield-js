//! Structural validation of builds before script execution.

use std::collections::{BTreeMap, BTreeSet, btree_map::Entry};
use std::time::Duration;

use super::instr::{self, validate_instruction};
use super::{
    BUILTINS, BYTECODE_VERSION, BinaryOp, Build, Constant, DisposeBuiltin, Function, Instr,
    ModuleExport, ModuleTarget, ParamTarget, PrivateDefinition, Slot, SlotKind,
};
use crate::value::BindingId;

impl Build {
    /// Structural validation performed before any script code runs.
    pub fn validate(&self) -> Result<(), String> {
        self.validate_with_digest_checkpoint::<false>(|_| {})
    }

    pub(crate) fn validate_with_digest_checkpoint<const PROFILE: bool>(
        &self,
        after_digest: impl FnOnce(Option<Duration>),
    ) -> Result<(), String> {
        if self.bytecode_version != BYTECODE_VERSION {
            return Err(format!(
                "unsupported bytecode version {}, expected {}",
                self.bytecode_version, BYTECODE_VERSION
            ));
        }
        let (id, timing) = self.compute_id_profiled::<PROFILE>();
        if self.build_id != id {
            return Err("build_id does not match the bytecode content".to_owned());
        }
        after_digest(timing);
        let mut func_ids = std::collections::BTreeSet::new();
        let mut func_names = std::collections::BTreeSet::new();
        for function in &self.functions {
            if function.is_method && function.is_arrow {
                return Err(format!(
                    "function {} cannot be both a method and an arrow",
                    function.func_id
                ));
            }
            if !func_ids.insert(function.func_id) {
                return Err(format!("duplicate func_id {}", function.func_id));
            }
            if function.name.is_empty() {
                return Err(format!("function {} has an empty name", function.func_id));
            }
            if !func_names.insert(function.name.as_str()) {
                return Err(format!("duplicate function name {:?}", function.name));
            }
        }
        for (index, site) in self.templates.iter().enumerate() {
            if site.raw.is_empty() || site.cooked.len() != site.raw.len() {
                return Err(format!(
                    "template {index} has mismatched cooked and raw chunks"
                ));
            }
        }
        let mut private_limits = BTreeMap::new();
        for instr in self.functions.iter().flat_map(|f| &f.code) {
            if let Instr::CreateClass {
                private_count,
                private_binding,
                ..
            } = instr
            {
                match private_binding {
                    None if *private_count == 0 => {}
                    Some(binding)
                        if *private_count != 0 && *binding < crate::builtin::call::DERIVED_THIS =>
                    {
                        if private_limits.insert(*binding, *private_count).is_some() {
                            return Err(format!("duplicate private class binding {binding}"));
                        }
                    }
                    _ => return Err("invalid private class binding".into()),
                }
            }
        }

        let (binding_owners, getters) = self.validate_modules()?;
        for function in &self.functions {
            self.validate_function(function, &private_limits, &binding_owners, &getters)?;
        }
        Ok(())
    }

    fn validate_modules(&self) -> Result<(BTreeMap<BindingId, u32>, BTreeSet<u32>), String> {
        let mut pending = vec![self.entry];
        pending.extend(self.preload);
        for &root in &pending {
            self.check_module(root)?;
        }

        let mut module_ids = BTreeSet::new();
        let mut initializers = BTreeSet::new();
        let mut binding_owners = BTreeMap::new();
        for (index, module) in self.modules.iter().enumerate() {
            let index = u32::try_from(index).map_err(|_| "too many source modules")?;
            if module.id.is_empty() || !module_ids.insert(module.id.as_str()) {
                return Err(format!(
                    "empty or duplicate source module ID {:?}",
                    module.id
                ));
            }
            for initializer in [module.declaration, module.evaluation] {
                if !initializers.insert(initializer) {
                    return Err(format!("module initializer {initializer} is not distinct"));
                }
                let function = self.function(initializer).ok_or_else(|| {
                    format!(
                        "module {:?} references missing initializer {initializer}",
                        module.id
                    )
                })?;
                if !function.is_constructable()
                    || function.param_targets().next().is_some()
                    || !function.resumes.is_empty()
                {
                    return Err(format!("invalid module initializer {initializer}"));
                }
                if initializer == module.declaration
                    && (!function.handlers.is_empty()
                        || function.code.iter().any(|instruction| {
                            !matches!(
                                instruction,
                                Instr::Undefined { .. }
                                    | Instr::Move { .. }
                                    | Instr::DeclareBinding { .. }
                                    | Instr::SetBinding { .. }
                                    | Instr::SetModuleBindingDirect { .. }
                                    | Instr::Closure { .. }
                                    | Instr::Return { .. }
                            )
                        }))
                {
                    return Err(format!(
                        "invalid module declaration initializer {initializer}"
                    ));
                }
            }
            let mut dependencies = BTreeSet::new();
            for &dependency in &module.dependencies {
                self.check_module(dependency)?;
                if !dependencies.insert(dependency) {
                    return Err(format!(
                        "duplicate dependency {dependency} in module {:?}",
                        module.id
                    ));
                }
            }
            for binding in self.module_bindings(index) {
                if binding_owners.insert(binding, index).is_some() {
                    return Err(format!("duplicate declaration of module binding {binding}"));
                }
            }
        }

        let mut reachable = vec![false; self.modules.len()];
        while let Some(index) = pending.pop() {
            if !std::mem::replace(&mut reachable[index as usize], true) {
                pending.extend(&self.modules[index as usize].dependencies);
            }
        }
        if let Some(index) = reachable.iter().position(|seen| !seen) {
            return Err(format!("unreachable module {:?}", self.modules[index].id));
        }

        let mut by_target = BTreeMap::new();
        let mut getters = BTreeSet::new();
        for module in &self.modules {
            for (name, export) in &module.exports {
                let ModuleExport { target, getter } = *export;
                match by_target.entry(target) {
                    Entry::Occupied(entry) if *entry.get() == getter => continue,
                    Entry::Occupied(_) => {
                        return Err(format!("exports of {target:?} must share a getter"));
                    }
                    Entry::Vacant(entry) => entry.insert(getter),
                };
                self.check_module_target(target, &binding_owners)?;
                let function = self.function(getter).ok_or_else(|| {
                    format!(
                        "missing getter {getter} for module {:?} export {name:?}",
                        module.id
                    )
                })?;
                if initializers.contains(&getter)
                    || !getters.insert(getter)
                    || !function.is_accessor_method()
                    || function.param_targets().next().is_some()
                    || !function.resumes.is_empty()
                    || !function.handlers.is_empty()
                    || Self::getter_target(&function.code) != Some(target)
                {
                    return Err(format!(
                        "invalid getter {getter} for module {:?} export {name:?}",
                        module.id
                    ));
                }
            }
        }
        Ok((binding_owners, getters))
    }

    fn getter_target(code: &[Instr]) -> Option<ModuleTarget> {
        let [read, Instr::Return { src }] = code else {
            return None;
        };
        match *read {
            Instr::GetModuleBinding {
                dst,
                module,
                binding,
            } if dst == *src => Some(ModuleTarget::Binding { module, binding }),
            Instr::GetModuleNamespace { dst, module } if dst == *src => {
                Some(ModuleTarget::Namespace { module })
            }
            Instr::GetImport { dst, import } if dst == *src => Some(ModuleTarget::Host(import)),
            _ => None,
        }
    }

    fn check_module(&self, module: u32) -> Result<(), String> {
        if module as usize >= self.modules.len() {
            return Err(format!("invalid source module index {module}"));
        }
        Ok(())
    }

    fn check_module_target(
        &self,
        target: ModuleTarget,
        binding_owners: &BTreeMap<BindingId, u32>,
    ) -> Result<(), String> {
        match target {
            ModuleTarget::Binding { module, binding } => {
                self.check_module(module)?;
                if binding_owners.get(&binding) != Some(&module) {
                    return Err(format!(
                        "binding {binding} is not declared in module {module}"
                    ));
                }
            }
            ModuleTarget::Namespace { module } => self.check_module(module)?,
            ModuleTarget::Host(import) => {
                let exports = self
                    .imports
                    .values()
                    .nth(import.module as usize)
                    .ok_or_else(|| format!("invalid host module index {}", import.module))?;
                if let Some(export) = import.export
                    && export as usize >= exports.len()
                {
                    return Err(format!(
                        "invalid host export index {export} for module {}",
                        import.module
                    ));
                }
            }
        }
        Ok(())
    }

    fn check_builtin(
        &self,
        builtin: u32,
        location: &str,
        pc: usize,
        what: &str,
    ) -> Result<(), String> {
        if builtin as usize >= BUILTINS.len() && DisposeBuiltin::offset(builtin).is_none() {
            return Err(format!(
                "{location}: {what} at pc {pc} references builtin {builtin}"
            ));
        }
        Ok(())
    }

    fn validate_function(
        &self,
        function: &Function,
        private_limits: &BTreeMap<BindingId, u32>,
        binding_owners: &BTreeMap<BindingId, u32>,
        getters: &BTreeSet<u32>,
    ) -> Result<(), String> {
        let check_private = |lexical, index| {
            if private_limits
                .get(&lexical)
                .is_some_and(|limit| index < *limit)
            {
                Ok(())
            } else {
                Err(format!(
                    "invalid class private index {index} for binding {lexical}"
                ))
            }
        };
        let location = format!("function {} ({})", function.func_id, function.name);
        if function.code.is_empty() {
            return Err(format!("{location}: empty code"));
        }
        match function.module {
            Some(module) if module as usize >= self.modules.len() => {
                return Err(format!(
                    "{location}: invalid defining module index {module}"
                ));
            }
            None if !function.stack_sites.is_empty() => {
                return Err(format!("{location}: stack sites without a defining module"));
            }
            _ => {}
        }
        let mut previous = None;
        for site in &function.stack_sites {
            if site.pc as usize >= function.code.len() || previous.is_some_and(|pc| pc >= site.pc) {
                return Err(format!("{location}: invalid stack site PC {}", site.pc));
            }
            if (site.line == 0) != (site.column == 0) {
                return Err(format!(
                    "{location}: invalid stack site coordinates at PC {}",
                    site.pc
                ));
            }
            previous = Some(site.pc);
        }
        if function.resumes.len() > u32::MAX as usize {
            return Err(format!("{location}: too many resume entries"));
        }
        // An async generator carries both tables: `resumes` for its own
        // awaits and `generator_resumes` for the three injected completions.
        // Each table is validated on its own below.
        if function.is_generator && function.is_arrow {
            return Err(format!("{location}: generator cannot be an arrow"));
        }
        if function.is_generator != function.generator_start.is_some() {
            return Err(format!("{location}: invalid generator start table"));
        }
        if !function.is_generator && !function.generator_resumes.is_empty() {
            return Err(format!(
                "{location}: generator resume table on a non-generator"
            ));
        }
        if function.is_generator && !function.is_async && !function.resumes.is_empty() {
            return Err(format!(
                "{location}: synchronous generator cannot have await resumes"
            ));
        }
        if function.generator_resumes.len() > u32::MAX as usize {
            return Err(format!("{location}: too many generator resume entries"));
        }

        let check_slot = |slot: Slot, what: &str| -> Result<(), String> {
            let limit = match slot.kind {
                SlotKind::Transient => function.transient_slots,
                SlotKind::Persistent => function.persistent_slots,
            };
            if slot.index >= limit {
                return Err(format!(
                    "{location}: {what} references out-of-range {:?} slot {} (size {limit})",
                    slot.kind, slot.index
                ));
            }
            Ok(())
        };
        let check_binding = |binding: BindingId| {
            if binding >= crate::builtin::call::DERIVED_THIS {
                Err(format!("{location}: reserved binding {binding}"))
            } else {
                Ok(())
            }
        };

        if function.params.len() > u16::MAX as usize {
            return Err(format!("{location}: too many parameters"));
        }
        if usize::from(function.length) > function.params.len() {
            return Err(format!(
                "{location}: length exceeds positional parameter count"
            ));
        }
        if function.is_arrow && function.arguments_param.is_some() {
            return Err(format!("{location}: arrow cannot have an arguments target"));
        }
        if function.is_arrow && function.new_target_param.is_some() {
            return Err(format!(
                "{location}: arrow cannot have a new.target destination"
            ));
        }
        if function.is_arrow && function.home_param.is_some() {
            return Err(format!("{location}: arrow cannot have a home destination"));
        }
        let mut seen_bindings = std::collections::BTreeSet::new();
        let mut seen_slots = Vec::new();
        for param in function.param_targets() {
            match param {
                ParamTarget::Binding(binding) => {
                    check_binding(*binding)?;
                    if binding_owners.contains_key(binding) {
                        return Err(format!(
                            "{location}: parameter shadows module binding {binding}"
                        ));
                    }
                    if !seen_bindings.insert(*binding) {
                        return Err(format!("{location}: duplicate parameter binding {binding}"));
                    }
                }
                ParamTarget::Slot(slot) => {
                    check_slot(*slot, "parameter")?;
                    if seen_slots.contains(slot) {
                        return Err(format!("{location}: duplicate parameter slot {slot:?}"));
                    }
                    seen_slots.push(*slot);
                }
            }
        }

        for (pc, instr) in function.code.iter().enumerate() {
            let check_target = |target: u32, what: &str| -> Result<(), String> {
                if target as usize >= function.code.len() {
                    return Err(format!(
                        "{location}: {what} at pc {pc} targets pc {target}, code len {}",
                        function.code.len()
                    ));
                }
                Ok(())
            };
            let check_comparison = |op: BinaryOp| -> Result<(), String> {
                if matches!(
                    op,
                    BinaryOp::Lt
                        | BinaryOp::Le
                        | BinaryOp::Gt
                        | BinaryOp::Ge
                        | BinaryOp::Eq
                        | BinaryOp::Ne
                        | BinaryOp::StrictEq
                        | BinaryOp::StrictNe
                ) {
                    Ok(())
                } else {
                    Err(format!(
                        "{location}: branch comparison at pc {pc} uses a non-comparison operator"
                    ))
                }
            };
            // Each `custom` entry in the table must match one of these rules.
            macro_rules! validate_custom {
                (Const { $dst:ident, $constant:ident }) => {{
                    check_slot(*$dst, "Const.dst")?;
                    if *$constant as usize >= self.constants.len() {
                        return Err(format!(
                            "{location}: Const at pc {pc} references constant {constant}",
                            constant = $constant
                        ));
                    }
                }};
                (BinaryConst { $dst:ident, $_op:ident, $lhs:ident, $constant:ident }) => {{
                    check_slot(*$dst, "BinaryConst.dst")?;
                    check_slot(*$lhs, "BinaryConst.lhs")?;
                    if *$constant as usize >= self.constants.len() {
                        return Err(format!(
                            "{location}: BinaryConst at pc {pc} references constant {constant}",
                            constant = $constant
                        ));
                    }
                }};
                (GetImport { $dst:ident, $import:ident }) => {{
                    check_slot(*$dst, "GetImport.dst")?;
                    self.check_module_target(ModuleTarget::Host(*$import), binding_owners)
                        .map_err(|err| format!("{location}: GetImport at pc {pc}: {err}"))?;
                }};
                (GetModuleBinding { $dst:ident, $module:ident, $binding:ident }) => {{
                    check_slot(*$dst, "GetModuleBinding.dst")?;
                    self.check_module_target(
                        ModuleTarget::Binding {
                            module: *$module,
                            binding: *$binding,
                        },
                        binding_owners,
                    )
                    .map_err(|err| format!("{location}: GetModuleBinding at pc {pc}: {err}"))?;
                }};
                (GetModuleNamespace { $dst:ident, $module:ident }) => {{
                    check_slot(*$dst, "GetModuleNamespace.dst")?;
                    self.check_module(*$module).map_err(|err| {
                        format!("{location}: GetModuleNamespace at pc {pc}: {err}")
                    })?;
                }};
                (ImportModule { $dst:ident, $module:ident }) => {{
                    check_slot(*$dst, "ImportModule.dst")?;
                    self.check_module(*$module).map_err(|err| {
                        format!("{location}: ImportModule at pc {pc}: {err}")
                    })?;
                }};
                (SuperConstruct { $dst:ident, $context:ident, $callee:ident, $args:ident }) => {{
                    check_slot(*$dst, "derived constructor result")?;
                    check_slot(*$context, "derived constructor context")?;
                    check_slot(*$callee, "SuperConstruct.callee")?;
                    for arg in $args.slots() {
                        check_slot(*arg, "SuperConstruct.arg")?;
                    }
                }};
                (Closure { $dst:ident, $callee:ident }) => {{
                    check_slot(*$dst, "Closure.dst")?;
                    if self.is_initializer(*$callee) || getters.contains($callee) {
                        return Err(format!(
                            "{location}: Closure at pc {pc} references reserved function {callee}",
                            callee = $callee
                        ));
                    }
                    if self.function(*$callee).is_none() {
                        return Err(format!(
                            "{location}: Closure at pc {pc} references function {callee}",
                            callee = $callee
                        ));
                    }
                }};
                (LoadClassBinding { $dst:ident, $scope:ident, $binding:ident }) => {{
                    check_binding(*$binding)?;
                    check_slot(*$dst, "class scope result")?;
                    if let Some(slot) = $scope {
                        check_slot(*slot, "class scope")?;
                    }
                }};
                (InitClassBinding { $scope:ident, $binding:ident, $src:ident }) => {{
                    check_binding(*$binding)?;
                    check_slot(*$scope, "InitClassBinding.scope")?;
                    check_slot(*$src, "InitClassBinding.src")?;
                }};
                (CreateClass {
                    $dst:ident, $scope:ident, $heritage:ident, $name_key:ident, $constructor:ident,
                    $_default_constructor:ident, $_private_count:ident, $_private_binding:ident
                }) => {{
                    check_slot(*$dst, "CreateClass.dst")?;
                    if let Some(slot) = $scope {
                        check_slot(*slot, "CreateClass.scope")?;
                    }
                    if let Some(slot) = $heritage {
                        check_slot(*slot, "CreateClass.heritage")?;
                    }
                    if let Some(key) = $name_key {
                        check_slot(*key, "CreateClass.name_key")?;
                    }
                    if self.is_initializer(*$constructor)
                        || !self
                            .function(*$constructor)
                            .is_some_and(Function::is_constructable)
                    {
                        return Err(format!(
                            "{location}: invalid class constructor {constructor} at pc {pc}",
                            constructor = $constructor
                        ));
                    }
                }};
                (ScopedClosure { $dst:ident, $scope:ident, $function:ident }) => {{
                    check_slot(*$dst, "ScopedClosure.dst")?;
                    check_slot(*$scope, "ScopedClosure.scope")?;
                    if self.is_initializer(*$function)
                        || getters.contains($function)
                        || self.function(*$function).is_none()
                    {
                        return Err(format!(
                            "{location}: invalid scoped closure function {function} at pc {pc}",
                            function = $function
                        ));
                    }
                }};
                (AddClassPrivate {
                    $class:ident, $index:ident, $lexical:ident, $src:ident, $_is_static:ident, $kind:ident
                }) => {{
                    check_slot(*$class, "AddClassPrivate.class")?;
                    if let Some(slot) = $src {
                        check_slot(*slot, "AddClassPrivate.src")?;
                    } else if *$kind != PrivateDefinition::Field {
                        return Err(format!(
                            "{location}: private method definition is missing its closure at pc {pc}"
                        ));
                    }
                    check_private(*$lexical, *$index)?;
                }};
                (GetPrivate { $value:ident, $object:ident, $brand:ident, $index:ident, $lexical:ident }) => {{
                    validate_custom!(@private_element { $value, $object, $brand, $index, $lexical });
                }};
                (SetPrivate { $object:ident, $brand:ident, $value:ident, $index:ident, $lexical:ident }) => {{
                    validate_custom!(@private_element { $value, $object, $brand, $index, $lexical });
                }};
                (HasPrivate { $value:ident, $object:ident, $brand:ident, $index:ident, $lexical:ident }) => {{
                    validate_custom!(@private_element { $value, $object, $brand, $index, $lexical });
                }};
                (@private_element { $value:ident, $object:ident, $brand:ident, $index:ident, $lexical:ident }) => {{
                    check_slot(*$value, "private element value")?;
                    check_slot(*$object, "private element receiver")?;
                    check_slot(*$brand, "private element brand")?;
                    check_private(*$lexical, *$index)?;
                }};
                (NewRegExp { $dst:ident, $source:ident, $flags:ident }) => {{
                    check_slot(*$dst, "NewRegExp.dst")?;
                    crate::regexp::validate_regexp_string($source, $flags).map_err(|message| {
                        format!("{location}: invalid NewRegExp at pc {pc}: {message}")
                    })?;
                }};
                (GetMemberConstKey { $dst:ident, $object:ident, $key:ident }) => {{
                    check_slot(*$dst, "GetMemberConstKey.dst")?;
                    check_slot(*$object, "GetMemberConstKey.object")?;
                    if !matches!(self.constants.get(*$key as usize), Some(Constant::String(_))) {
                        return Err(format!(
                            "{location}: GetMemberConstKey at pc {pc} requires a string constant, got index {key}",
                            key = $key
                        ));
                    }
                }};
                (GetIterator { $iterator:ident, $next:ident, $iterable:ident }) => {{
                    check_slot(*$iterator, "GetIterator.iterator")?;
                    check_slot(*$next, "GetIterator.next")?;
                    if $iterator == $next {
                        return Err(format!(
                            "{location}: GetIterator destinations overlap at pc {pc}"
                        ));
                    }
                    check_slot(*$iterable, "GetIterator.iterable")?;
                }};
                (IteratorNext { $done:ident, $value:ident, $iterator:ident, $next:ident }) => {{
                    check_slot(*$done, "IteratorNext.done")?;
                    check_slot(*$value, "IteratorNext.value")?;
                    if $done == $value {
                        return Err(format!(
                            "{location}: IteratorNext destinations overlap at pc {pc}"
                        ));
                    }
                    check_slot(*$iterator, "IteratorNext.iterator")?;
                    check_slot(*$next, "IteratorNext.next")?;
                }};
                (ForInNext { $done:ident, $value:ident, $iterator:ident }) => {{
                    check_slot(*$done, "ForInNext.done")?;
                    check_slot(*$value, "ForInNext.value")?;
                    if $done == $value {
                        return Err(format!(
                            "{location}: ForInNext destinations overlap at pc {pc}"
                        ));
                    }
                    check_slot(*$iterator, "ForInNext.iterator")?;
                }};
                (GetBinding { $dst:ident, $binding:ident }) => {{
                    check_binding(*$binding)?;
                    check_slot(*$dst, "GetBinding.dst")?;
                }};
                (SetBinding { $binding:ident, $src:ident }) => {{
                    check_binding(*$binding)?;
                    check_slot(*$src, "SetBinding.src")?;
                }};
                (GetBindingDirect { $dst:ident, $_depth:ident, $binding:ident }) => {{
                    check_binding(*$binding)?;
                    check_slot(*$dst, "GetBindingDirect.dst")?;
                }};
                (SetBindingDirect { $_depth:ident, $binding:ident, $src:ident }) => {{
                    check_binding(*$binding)?;
                    check_slot(*$src, "SetBindingDirect.src")?;
                }};
                (GetModuleBindingDirect { $dst:ident, $module:ident, $binding:ident }) => {{
                    check_slot(*$dst, "GetModuleBindingDirect.dst")?;
                    self.check_module_target(
                        ModuleTarget::Binding {
                            module: *$module,
                            binding: *$binding,
                        },
                        binding_owners,
                    )
                    .map_err(|err| {
                        format!("{location}: GetModuleBindingDirect at pc {pc}: {err}")
                    })?;
                }};
                (SetModuleBindingDirect { $module:ident, $binding:ident, $src:ident }) => {{
                    check_slot(*$src, "SetModuleBindingDirect.src")?;
                    self.check_module_target(
                        ModuleTarget::Binding {
                            module: *$module,
                            binding: *$binding,
                        },
                        binding_owners,
                    )
                    .map_err(|err| {
                        format!("{location}: SetModuleBindingDirect at pc {pc}: {err}")
                    })?;
                }};
                (DeclareBinding { $binding:ident }) => {{
                    check_binding(*$binding)?;
                    if let Some(&module) = binding_owners.get($binding)
                        && self.modules[module as usize].declaration != function.func_id
                    {
                        return Err(format!(
                            "{location}: redeclares module binding {binding}",
                            binding = $binding
                        ));
                    }
                }};
                (NewIterationEnv { $parent:ident, $bindings:ident }) => {{
                    check_slot(*$parent, "NewIterationEnv.parent")?;
                    let mut seen = BTreeSet::new();
                    for binding in $bindings {
                        check_binding(*binding)?;
                        if !seen.insert(*binding) {
                            return Err(format!(
                                "{location}: NewIterationEnv repeats binding {binding}"
                            ));
                        }
                    }
                }};
                (Await { $dst:ident, $input:ident, $resume:ident }) => {{
                    if !function.is_async {
                        return Err(format!("{location}: Await in a synchronous function"));
                    }
                    check_slot(*$dst, "Await.dst")?;
                    check_slot(*$input, "Await.input")?;
                    if *$resume as usize >= function.resumes.len() {
                        return Err(format!(
                            "{location}: Await at pc {pc} references resume {resume}",
                            resume = $resume
                        ));
                    }
                }};
                (Yield { $input:ident, $generator:ident, $delegate:ident }) => {{
                    if !function.is_generator {
                        return Err(format!("{location}: Yield in a non-generator function"));
                    }
                    if *$delegate && function.is_async {
                        return Err(format!(
                            "{location}: async generator cannot hand back a delegate result object"
                        ));
                    }
                    check_slot(*$input, "Yield.input")?;
                    if *$generator as usize >= function.generator_resumes.len() {
                        return Err(format!(
                            "{location}: Yield at pc {pc} references generator {generator}",
                            generator = $generator
                        ));
                    }
                }};
                (GeneratorStart) => {{
                    if !function.is_generator {
                        return Err(format!(
                            "{location}: GeneratorStart in a non-generator function"
                        ));
                    }
                }};
                (Jump { $target:ident }) => {{
                    check_target(*$target, "Jump")?;
                }};
                (BranchFalse { $cond:ident, $target:ident }) => {{
                    check_slot(*$cond, "BranchFalse.cond")?;
                    check_target(*$target, "BranchFalse")?;
                }};
                (BranchTrue { $cond:ident, $target:ident }) => {{
                    check_slot(*$cond, "BranchTrue.cond")?;
                    check_target(*$target, "BranchTrue")?;
                }};
                (BranchCompareFalse { $lhs:ident, $op:ident, $rhs:ident, $target:ident }) => {{
                    check_slot(*$lhs, "BranchCompareFalse.lhs")?;
                    check_slot(*$rhs, "BranchCompareFalse.rhs")?;
                    check_comparison(*$op)?;
                    check_target(*$target, "BranchCompareFalse")?;
                }};
                (BranchCompareConstFalse { $lhs:ident, $op:ident, $constant:ident, $target:ident }) => {{
                    check_slot(*$lhs, "BranchCompareConstFalse.lhs")?;
                    check_comparison(*$op)?;
                    if *$constant as usize >= self.constants.len() {
                        return Err(format!(
                            "{location}: BranchCompareConstFalse at pc {pc} references constant {constant}",
                            constant = $constant
                        ));
                    }
                    check_target(*$target, "BranchCompareConstFalse")?;
                }};
                (BranchCompareTrue { $lhs:ident, $op:ident, $rhs:ident, $target:ident }) => {{
                    check_slot(*$lhs, "BranchCompareTrue.lhs")?;
                    check_slot(*$rhs, "BranchCompareTrue.rhs")?;
                    check_comparison(*$op)?;
                    check_target(*$target, "BranchCompareTrue")?;
                }};
                (BranchCompareConstTrue { $lhs:ident, $op:ident, $constant:ident, $target:ident }) => {{
                    check_slot(*$lhs, "BranchCompareConstTrue.lhs")?;
                    check_comparison(*$op)?;
                    if *$constant as usize >= self.constants.len() {
                        return Err(format!(
                            "{location}: BranchCompareConstTrue at pc {pc} references constant {constant}",
                            constant = $constant
                        ));
                    }
                    check_target(*$target, "BranchCompareConstTrue")?;
                }};
                (BranchBinary { $_op:ident, $lhs:ident, $rhs:ident, $target:ident }) => {{
                    check_slot(*$lhs, "BranchBinary.lhs")?;
                    check_slot(*$rhs, "BranchBinary.rhs")?;
                    check_target(*$target, "BranchBinary")?;
                }};
                (BranchBinaryConst { $_op:ident, $lhs:ident, $constant:ident, $target:ident }) => {{
                    check_slot(*$lhs, "BranchBinaryConst.lhs")?;
                    if *$constant as usize >= self.constants.len() {
                        return Err(format!(
                            "{location}: BranchBinaryConst at pc {pc} references constant {constant}",
                            constant = $constant
                        ));
                    }
                    check_target(*$target, "BranchBinaryConst")?;
                }};
                (BranchUnary { $_op:ident, $src:ident, $target:ident }) => {{
                    check_slot(*$src, "BranchUnary.src")?;
                    check_target(*$target, "BranchUnary")?;
                }};
                (GetBuiltin { $dst:ident, $builtin:ident }) => {{
                    check_slot(*$dst, "GetBuiltin.dst")?;
                    self.check_builtin(*$builtin, &location, pc, "GetBuiltin")?;
                }};
                (GetTemplate { $dst:ident, $template:ident }) => {{
                    check_slot(*$dst, "GetTemplate.dst")?;
                    if *$template as usize >= self.templates.len() {
                        return Err(format!(
                            "{location}: GetTemplate at pc {pc} references template {template}",
                            template = $template
                        ));
                    }
                }};
            }

            instr::instruction_table!(validate_instruction; instr, check_slot, validate_custom;);
        }
        validate_continuations(function, &location, check_slot)
    }
}

fn validate_continuations(
    function: &Function,
    location: &str,
    check_slot: impl Fn(Slot, &str) -> Result<(), String>,
) -> Result<(), String> {
    let check_saved = |saved: &[u16], what: std::fmt::Arguments<'_>| -> Result<(), String> {
        let mut previous = None;
        for slot in saved {
            if *slot >= function.persistent_slots {
                return Err(format!(
                    "{location}: {what} saves persistent slot {slot}, size {}",
                    function.persistent_slots
                ));
            }
            if previous.is_some_and(|prev| *slot <= prev) {
                return Err(format!(
                    "{location}: {what} saved_slots are not strictly ascending"
                ));
            }
            previous = Some(*slot);
        }
        Ok(())
    };

    for (resume_id, resume) in function.resumes.iter().enumerate() {
        let check_pc = |pc: u32, what: &str| -> Result<(), String> {
            if pc as usize >= function.code.len() {
                return Err(format!(
                    "{location}: resume {resume_id} {what} pc {pc} is out of range"
                ));
            }
            Ok(())
        };
        check_pc(resume.fulfill_pc, "fulfill")?;
        check_pc(resume.reject_pc, "reject")?;
        check_slot(resume.fulfill_dst, "fulfill_dst")?;
        check_slot(resume.reject_dst, "reject_dst")?;
        check_saved(&resume.saved_slots, format_args!("resume {resume_id}"))?;
    }

    if let Some(start) = &function.generator_start {
        let mut starts = function
            .code
            .iter()
            .enumerate()
            .filter(|(_, instruction)| matches!(instruction, Instr::GeneratorStart))
            .map(|(pc, _)| pc);
        let (Some(start_pc), None) = (starts.next(), starts.next()) else {
            return Err(format!(
                "{location}: generator needs exactly one start boundary"
            ));
        };
        if start.body_pc as usize != start_pc + 1 || start.body_pc as usize >= function.code.len() {
            return Err(format!(
                "{location}: generator body pc {} does not follow its start boundary",
                start.body_pc
            ));
        }
        if has_control_flow_target(function, start.body_pc) {
            return Err(format!(
                "{location}: ordinary control flow enters generator body pc {}",
                start.body_pc
            ));
        }
        check_saved(&start.saved_slots, format_args!("generator start"))?;
    }
    let mut yield_pcs = vec![None; function.generator_resumes.len()];
    for (pc, instruction) in function.code.iter().enumerate() {
        if let Instr::Yield { generator, .. } = instruction
            && let Some(slot) = yield_pcs.get_mut(*generator as usize)
            && slot.replace(pc as u32).is_some()
        {
            return Err(format!(
                "{location}: generator {generator} is resumed from two yields"
            ));
        }
    }
    for (generator, entry) in function.generator_resumes.iter().enumerate() {
        let Some(yield_pc) = yield_pcs[generator] else {
            return Err(format!(
                "{location}: generator entry {generator} has no Yield"
            ));
        };
        if entry.next_pc != yield_pc + 1 {
            return Err(format!(
                "{location}: generator {generator} next pc {} is not the instruction after its yield",
                entry.next_pc
            ));
        }
        for (pc, what) in [
            (entry.next_pc, "next"),
            (entry.throw_pc, "throw"),
            (entry.return_pc, "return"),
        ] {
            if pc as usize >= function.code.len() {
                return Err(format!(
                    "{location}: generator {generator} {what} pc {pc} is out of range"
                ));
            }
        }
        check_slot(entry.next_dst, "next_dst")?;
        check_slot(entry.throw_dst, "throw_dst")?;
        for (pc, what) in [(entry.throw_pc, "throw"), (entry.return_pc, "return")] {
            if has_control_flow_target(function, pc) {
                return Err(format!(
                    "{location}: ordinary control flow enters generator {generator} {what} pc {pc}"
                ));
            }
            if pc > 0 && falls_through(&function.code[pc as usize - 1]) {
                return Err(format!(
                    "{location}: generator {generator} {what} pc {pc} has a fall-through predecessor"
                ));
            }
        }
        if !matches!(
            function.code[entry.return_pc as usize],
            Instr::LoadGeneratorReturn { .. }
        ) {
            return Err(format!(
                "{location}: generator {generator} return pc {} does not read the return payload",
                entry.return_pc
            ));
        }
        check_saved(&entry.saved_slots, format_args!("generator {generator}"))?;
    }

    for (handler_id, handler) in function.handlers.iter().enumerate() {
        if handler.start_pc >= handler.end_pc {
            return Err(format!(
                "{location}: handler {handler_id} has empty or inverted range {}..{}",
                handler.start_pc, handler.end_pc
            ));
        }
        if handler.end_pc as usize > function.code.len() {
            return Err(format!(
                "{location}: handler {handler_id} ends at pc {}, code len {}",
                handler.end_pc,
                function.code.len()
            ));
        }
        if handler.target_pc as usize >= function.code.len() {
            return Err(format!(
                "{location}: handler {handler_id} targets pc {}, code len {}",
                handler.target_pc,
                function.code.len()
            ));
        }
        if handler.target_pc >= handler.start_pc && handler.target_pc < handler.end_pc {
            return Err(format!(
                "{location}: handler {handler_id} targets a pc inside its own protected range"
            ));
        }
        check_slot(handler.dst, &format!("handler {handler_id}.dst"))?;
    }

    Ok(())
}

/// Whether an ordinary edge names `target`: a jump, a conditional jump, or an
/// exception landing. Externally injected generator entries must avoid all of them.
fn has_control_flow_target(function: &Function, target: u32) -> bool {
    function.code.iter().any(|instruction| match instruction {
        Instr::Jump { target: slot }
        | Instr::BranchFalse { target: slot, .. }
        | Instr::BranchTrue { target: slot, .. }
        | Instr::BranchCompareFalse { target: slot, .. }
        | Instr::BranchCompareConstFalse { target: slot, .. }
        | Instr::BranchCompareTrue { target: slot, .. }
        | Instr::BranchCompareConstTrue { target: slot, .. }
        | Instr::BranchBinary { target: slot, .. }
        | Instr::BranchBinaryConst { target: slot, .. }
        | Instr::BranchUnary { target: slot, .. } => *slot == target,
        _ => false,
    }) || function
        .handlers
        .iter()
        .any(|handler| handler.target_pc == target)
}

/// Whether execution continues at the next instruction after `instruction`.
fn falls_through(instruction: &Instr) -> bool {
    !matches!(
        instruction,
        Instr::Jump { .. } | Instr::Return { .. } | Instr::Throw { .. } | Instr::GeneratorStart
    )
}
