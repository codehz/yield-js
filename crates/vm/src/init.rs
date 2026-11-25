//! VM construction: build validation, linking, and module initialization.

#![allow(clippy::too_many_arguments)]

use super::vm::{
    IMPORT_EXPORT_ATTRIBUTES, ImportModule, MODULE_EXPORT_ATTRIBUTES, PRELOAD_EXPORT_ATTRIBUTES,
    ResolvedExport, ResolvedImports, ResolvedModule, VerifiedBuild, Vm, VmCore,
    host_function_metadata, module_getter_capture, next_epoch, property_caches, time_source,
};
use crate::binding::{Bindings, ExportDecl, NativeTarget};
use crate::bytecode::Build;
use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::heap::{Attributes, DescriptorUpdate, Heap, HostClassRecord, IntegrityLevel, Object};
use crate::host::Host;
use crate::runtime::{Arguments, VmClock};
use crate::value::{ObjectId, PropertyKey, Value};
use crate::{JsString, builtin::BuiltinRoots};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

/// Settings applied before evaluating the preload and entry modules.
#[derive(Default)]
pub struct VmOptions<'a> {
    /// Default absolute base for relative URL construction.
    pub base_url: Option<&'a str>,
    /// Origin, referrer, partition and host authorization used by `fetch`.
    /// The default is an opaque origin with no referrer or permissions.
    pub fetch_environment: Option<crate::fetch::FetchEnvironment>,
    #[doc(hidden)]
    pub test_clock: Option<Arc<dyn crate::TestClock>>,
}

/// Settings for restoring a VM. The base URL comes from the snapshot.
#[derive(Default)]
pub struct RestoreOptions {
    /// Additional graph checks to run after the snapshot compatibility checks.
    /// Defaults to [`SnapshotValidation::Trusted`].
    pub validation: SnapshotValidation,
    /// Host authorization for the restored VM. The recorded environment is
    /// retained; this value must identify the same origin, partition and
    /// authorization or restore is refused, so resuming cannot widen access.
    pub fetch_environment: Option<crate::fetch::FetchEnvironment>,
    #[doc(hidden)]
    pub test_clock: Option<Arc<dyn crate::TestClock>>,
}

/// Controls the additional snapshot graph audit performed during restore.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SnapshotValidation {
    /// Check compatibility without auditing the entire graph.
    #[default]
    Trusted,
    /// Audit internal invariants for diagnostics, not for untrusted input.
    Full,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ModuleEvaluation {
    Pending,
    Visiting,
    Done,
}

impl Vm {
    /// Start a fresh VM, linking this host and running module initialization.
    /// A raw [`Build`] is validated; a [`VerifiedBuild`] reuses its validation.
    pub fn new(
        build: impl TryInto<VerifiedBuild, Error: Into<VmError>>,
        host: Host,
        options: VmOptions<'_>,
    ) -> Result<Self, VmError> {
        let base_url = options
            .base_url
            .map(url::Url::parse)
            .transpose()
            .map_err(|error| VmError::InvalidState(format!("invalid base URL: {error}")))?;
        let build = build.try_into().map_err(Into::into)?;
        let mut core = VmCore::prepare(build, &host, options.test_clock)?;
        core.base_url = base_url;
        core.fetch_environment = options.fetch_environment.unwrap_or_default();
        core.initialize_modules(&host)?;
        Ok(Self { core, host })
    }

    /// Construct a VM using the regular startup pipeline and time its phases.
    #[doc(hidden)]
    pub fn new_profiled(build: Build, host: Host) -> Result<(Self, crate::perf::Startup), VmError> {
        let started = std::time::Instant::now();
        let mut digest_checked = None;
        build
            .validate_with_digest_checkpoint::<true>(|timing| {
                digest_checked = timing.map(|timing| (std::time::Instant::now(), timing));
            })
            .map_err(VmError::InvalidBuild)?;
        let validated = std::time::Instant::now();
        let mut core = VmCore::prepare(VerifiedBuild::from_validated(build), &host, None)?;
        let initializing = std::time::Instant::now();
        core.initialize_modules(&host)?;
        let (digest_checked, digest_timing) = digest_checked.expect("recorded digest timing");
        Ok((
            Self { core, host },
            crate::perf::Startup {
                prepare: initializing.duration_since(started),
                build_identity: digest_checked.duration_since(started),
                build_digest: digest_timing,
                build_structure: validated.duration_since(digest_checked),
                link_and_setup: initializing.duration_since(validated),
                initialize: initializing.elapsed(),
            },
        ))
    }
}

impl VmCore {
    fn prepare(
        build: VerifiedBuild,
        host: &Host,
        clock: Option<Arc<dyn crate::TestClock>>,
    ) -> Result<Self, VmError> {
        let bindings = host.bindings();
        let resolved = Self::resolve_imports(&build, bindings)?;
        let clock = VmClock::new(clock, std::time::Duration::ZERO);
        let mut vm = Self::empty(build, bindings, clock);
        vm.init_builtins(
            host.services()
                .get::<crate::host::UserAgentKey>()
                .map(String::as_str),
        );
        vm.install_templates();
        vm.install_classes(bindings)?;
        vm.install_module_states(bindings);
        vm.install_imports(bindings, resolved)?;
        Ok(vm)
    }

    fn initialize_modules(&mut self, host: &Host) -> Result<(), VmError> {
        self.instantiate_modules(host)?;
        let mut states = vec![ModuleEvaluation::Pending; self.build.modules.len()];
        if let Some(preload) = self.build.preload {
            self.evaluate_module(host, preload, &mut states)?;
            self.install_preload_globals(preload)?;
        }
        self.evaluate_module(host, self.build.entry, &mut states)?;
        for env in self.module_envs.iter().flatten() {
            if !self.heap.environment(*env)?.uninitialized.is_empty() {
                return Err(VmError::InvalidState(
                    "module evaluation left uninitialized bindings".into(),
                ));
            }
        }
        self.run_until_idle(host)
    }

    fn empty(build: VerifiedBuild, bindings: &Bindings, clock: VmClock) -> Self {
        let time = time_source(bindings);
        let module_states = Vec::new();
        let caches = property_caches(&build);
        Self {
            property_caches: caches,
            build,
            heap: Heap::new(),
            host_tasks: BTreeMap::new(),
            next_host_task_id: 1,
            continuations: BTreeMap::new(),
            imports: Vec::new(),
            operations: BTreeMap::new(),
            recovered_operations: BTreeSet::new(),
            ordinary_tasks: VecDeque::new(),
            next_stream_message: 0,
            script_timers: BTreeMap::new(),
            console: Default::default(),
            next_timer_id: 1,
            timer_nesting: 0,
            timer_deadlines: BTreeMap::new(),
            microtasks: VecDeque::new(),
            stack_registrations: Vec::new(),
            pending_rejection_notifications: Vec::new(),
            globals: BTreeMap::new(),
            host_classes: Vec::new(),
            host_instances: BTreeMap::new(),
            module_states,
            epoch: next_epoch(),
            next_operation_id: 1,
            next_host_call: 1,
            next_continuation_id: 1,
            active_array_joins: BTreeSet::new(),
            active_event_paths: Vec::new(),
            reporting_exception: false,
            builtins: BuiltinRoots::default(),
            templates: Vec::new(),
            random_state: crate::builtin::math::RANDOM_SEED,
            clock,
            time,
            base_url: None,
            fetch_environment: Default::default(),
            fetch_state: Default::default(),
            fetch_live: Default::default(),
            subtle_state: Default::default(),
            object_urls: Default::default(),
            module_envs: Vec::new(),
            module_namespaces: Vec::new(),
        }
    }

    /// Allocates every tagged-template call site's frozen `cooked` array, its
    /// frozen `raw` array, and the FIXED `raw` own property once per VM.
    fn install_templates(&mut self) {
        let mut templates = Vec::with_capacity(self.build.templates.len());
        for site in &self.build.templates {
            let cooked = site
                .cooked
                .iter()
                .map(|text| match text {
                    Some(text) => Value::string(text.clone()),
                    None => Value::UNDEFINED,
                })
                .collect();
            let raw = site
                .raw
                .iter()
                .map(|text| Value::string(text.clone()))
                .collect();
            let cooked = self.heap.alloc_array(cooked);
            let raw_array = self.heap.alloc_array(raw);
            self.heap
                .get_mut(raw_array)
                .expect("new raw array")
                .set_integrity_level(IntegrityLevel::Frozen);
            self.heap
                .define_data(
                    cooked,
                    PropertyKey::from("raw"),
                    Value::object(raw_array),
                    Attributes::FIXED,
                )
                .expect("template array accepts the raw property");
            self.heap
                .get_mut(cooked)
                .expect("new cooked array")
                .set_integrity_level(IntegrityLevel::Frozen);
            templates.push(cooked);
        }
        self.templates = templates;
    }

    pub(crate) fn template_object(&self, template: u32) -> Result<Value, VmError> {
        self.templates
            .get(template as usize)
            .copied()
            .map(Value::object)
            .ok_or_else(|| {
                VmError::InvalidState(format!("template call site {template} is not installed"))
            })
    }

    pub(crate) fn resolve_imports(
        build: &Build,
        bindings: &Bindings,
    ) -> Result<ResolvedImports, VmError> {
        let mut modules = Vec::with_capacity(build.imports.len());
        for (module, exports) in &build.imports {
            let mut resolved = Vec::with_capacity(exports.len());
            for export in exports {
                let (id, declaration) = bindings
                    .export(module, export)
                    .ok_or_else(|| VmError::MissingImport(format!("{module}:{export}")))?;
                let kind = match declaration {
                    ExportDecl::Function(_) => ResolvedExport::Function(id),
                    ExportDecl::Constant(_) => ResolvedExport::Constant,
                };
                resolved.push((export.clone(), kind));
            }
            modules.push(ResolvedModule {
                module: module.clone(),
                exports: resolved,
            });
        }
        Ok(ResolvedImports { modules })
    }

    fn install_classes(&mut self, bindings: &Bindings) -> Result<(), VmError> {
        for (class, _) in bindings.classes() {
            let prototype = self.heap.alloc_plain_with(None);
            self.host_classes.push(HostClassRecord {
                prototype,
                source: class,
            });
            for method in 0..bindings.method_count(class) {
                let target = NativeTarget::Method(crate::binding::MethodId {
                    class: class as u32,
                    method: method as u32,
                });
                let (name, arity) =
                    host_function_metadata(bindings, target).expect("method exists");
                let function = self.heap.intern_native(target, false, arity, name);
                let name = bindings
                    .method(crate::binding::MethodId {
                        class: class as u32,
                        method: method as u32,
                    })
                    .expect("method exists")
                    .name;
                self.heap.define_data(
                    prototype,
                    name.key(),
                    Value::object(function),
                    crate::binding::PROTOTYPE_METHOD_ATTRIBUTES,
                )?;
            }
            for accessor in 0..bindings.accessor_count(class) {
                let declaration = bindings.accessor(class, accessor).expect("accessor exists");
                let get = declaration.get.as_ref().map(|_| {
                    let target = NativeTarget::Accessor {
                        class: class as u32,
                        accessor: accessor as u32,
                        set: false,
                    };
                    let (name, arity) =
                        host_function_metadata(bindings, target).expect("getter exists");
                    self.heap.intern_native(target, false, arity, name)
                });
                let set = declaration.set.as_ref().map(|_| {
                    let target = NativeTarget::Accessor {
                        class: class as u32,
                        accessor: accessor as u32,
                        set: true,
                    };
                    let (name, arity) =
                        host_function_metadata(bindings, target).expect("setter exists");
                    self.heap.intern_native(target, false, arity, name)
                });
                self.heap.define_property(
                    prototype,
                    declaration.name.key(),
                    crate::heap::Property::Accessor { get, set },
                )?;
            }
        }
        for (class, _) in bindings.classes() {
            let parent = if let Some(parent) = bindings.class_parent(class) {
                self.host_classes[parent].prototype
            } else if bindings.class_is_event_target(class) {
                self.builtins.get(crate::builtin::event::TARGET_PROTOTYPE)
            } else {
                continue;
            };
            self.heap
                .set_prototype(self.host_classes[class].prototype, Some(parent))?;
        }
        Ok(())
    }

    fn install_module_states(&mut self, bindings: &Bindings) {
        self.module_states = (0..bindings.module_count())
            .map(|module| {
                let slots = bindings
                    .state_slots(module)
                    .iter()
                    .map(|slot| match slot.kind {
                        crate::binding::SlotKind::Js => {
                            crate::runtime::HostSlot::Js(Value::UNDEFINED)
                        }
                        crate::binding::SlotKind::JsList => {
                            crate::runtime::HostSlot::JsList(Vec::new())
                        }
                    })
                    .collect();
                crate::runtime::ModuleState {
                    slots,
                    payload: bindings.state_new(module),
                }
            })
            .collect();
    }

    fn import_export_value(
        &mut self,
        bindings: &Bindings,
        resolved: ResolvedExport,
    ) -> Result<Value, VmError> {
        match resolved {
            ResolvedExport::Constant => {
                // Constants are looked up through the bindings below.
                let _ = bindings;
                Err(VmError::InvalidState(
                    "constant import needs a declaration".to_owned(),
                ))
            }
            ResolvedExport::Function(id) => {
                let constructable = matches!(
                    bindings.export_decl(id),
                    Some(ExportDecl::Function(function)) if function.constructable
                );
                let target = NativeTarget::Export(id);
                let (name, arity) = host_function_metadata(bindings, target).ok_or_else(|| {
                    VmError::InvalidState("missing host function declaration".into())
                })?;
                Ok(Value::object(self.heap.intern_native(
                    target,
                    constructable,
                    arity,
                    name,
                )))
            }
        }
    }

    fn install_imports(
        &mut self,
        bindings: &Bindings,
        resolved: ResolvedImports,
    ) -> Result<(), VmError> {
        for module in resolved.modules {
            let namespace = self.heap.alloc_plain_with(None);
            let mut values = Vec::with_capacity(module.exports.len());
            for (name, export) in module.exports {
                let value = match export {
                    ResolvedExport::Constant => {
                        let (_, declaration) =
                            bindings.export(&module.module, &name).ok_or_else(|| {
                                VmError::MissingImport(format!("{}:{name}", module.module))
                            })?;
                        let ExportDecl::Constant(constant) = declaration else {
                            return Err(VmError::MissingImport(format!(
                                "{}:{name}",
                                module.module
                            )));
                        };
                        constant.value.value()
                    }
                    ResolvedExport::Function(_) => self.import_export_value(bindings, export)?,
                };
                self.heap.define_data(
                    namespace,
                    name.as_str(),
                    value.clone(),
                    IMPORT_EXPORT_ATTRIBUTES,
                )?;
                values.push(value);
            }
            self.heap.get_mut(namespace)?.prevent_extensions();
            self.imports.push(ImportModule {
                module: module.module,
                namespace,
                values,
            });
        }
        Ok(())
    }

    pub(crate) fn restore_imports(
        &mut self,
        bindings: &Bindings,
        namespaces: Vec<ObjectId>,
        resolved: ResolvedImports,
    ) -> Result<(), VmError> {
        if namespaces.len() != resolved.modules.len() {
            return Err(VmError::InvalidSnapshot(
                "import namespace count mismatch".to_owned(),
            ));
        }
        for (namespace, module) in namespaces.into_iter().zip(resolved.modules) {
            let object = self.heap.get(namespace)?;
            if !matches!(object, Object::Plain(_))
                || object.prototype().is_some()
                || object.is_extensible()
            {
                return Err(VmError::InvalidSnapshot(format!(
                    "import namespace {} is not frozen with a null prototype",
                    module.module
                )));
            }
            let mut actual = BTreeMap::new();
            for key in object.own_keys(false) {
                let crate::value::PropertyKey::String(name) = key else {
                    return Err(VmError::InvalidSnapshot(format!(
                        "import namespace {} has a symbol key",
                        module.module
                    )));
                };
                let Some(property) =
                    object.own_property(crate::value::PropertyKeyRef::JsString(&name))
                else {
                    return Err(VmError::InvalidSnapshot(format!(
                        "import namespace {} lost key {:?}",
                        module.module, name
                    )));
                };
                if property.attributes() != IMPORT_EXPORT_ATTRIBUTES {
                    return Err(VmError::InvalidSnapshot(format!(
                        "import namespace {} export {name:?} is not frozen",
                        module.module
                    )));
                }
                let Some(value) = property.data_value().cloned() else {
                    return Err(VmError::InvalidSnapshot(format!(
                        "import namespace {} export {name:?} is not a data property",
                        module.module
                    )));
                };
                if actual.insert(name, value).is_some() {
                    return Err(VmError::InvalidSnapshot(format!(
                        "import namespace {} has a duplicate key",
                        module.module
                    )));
                }
            }
            if actual.len() != module.exports.len() {
                return Err(VmError::InvalidSnapshot(format!(
                    "import namespace {} does not have exactly the resolved exports",
                    module.module
                )));
            }
            let mut values = Vec::with_capacity(module.exports.len());
            for (name, export) in module.exports {
                let expected = match export {
                    ResolvedExport::Function(_) => self.import_export_value(bindings, export)?,
                    ResolvedExport::Constant => {
                        let (_, declaration) =
                            bindings.export(&module.module, &name).ok_or_else(|| {
                                VmError::MissingImport(format!("{}:{name}", module.module))
                            })?;
                        let ExportDecl::Constant(constant) = declaration else {
                            return Err(VmError::MissingImport(format!(
                                "{}:{name}",
                                module.module
                            )));
                        };
                        constant.value.value()
                    }
                };
                let key = JsString::from(name.as_str());
                let Some(value) = actual.get(&key) else {
                    return Err(VmError::InvalidSnapshot(format!(
                        "import namespace {} is missing export {name:?}",
                        module.module
                    )));
                };
                if value != &expected {
                    return Err(VmError::InvalidSnapshot(format!(
                        "import namespace {} export {name:?} does not match the bindings",
                        module.module
                    )));
                }
                values.push(value.clone());
            }
            self.imports.push(ImportModule {
                module: module.module,
                namespace,
                values,
            });
        }
        Ok(())
    }

    fn instantiate_modules(&mut self, host: &Host) -> Result<(), VmError> {
        let count = self.build.modules.len();
        self.module_envs.reserve(count);
        self.module_namespaces.reserve(count);
        for index in 0..count {
            let module = index as u32;
            let env = if self.module_needs_environment(index) {
                let bindings = self.build.module_bindings(module);
                let env = self
                    .heap
                    .alloc_environment(None, bindings.map(|id| (id, Value::UNDEFINED)).collect());
                let environment = self.heap.environment_mut(env)?;
                environment.module = Some(module);
                environment
                    .uninitialized
                    .extend(environment.bindings.keys().copied());
                Some(env)
            } else {
                None
            };
            self.module_envs.push(env);
            self.module_namespaces
                .push(self.heap.alloc_module_namespace(module));
        }

        let mut getters = BTreeMap::new();
        let mut sentinel = None;
        for (module, &namespace) in self.build.modules.iter().zip(&self.module_namespaces) {
            for (name, export) in &module.exports {
                let getter = match getters.entry(export.target) {
                    std::collections::btree_map::Entry::Occupied(entry) => *entry.get(),
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        let env = match module_getter_capture(export.target) {
                            Some(module) => self.module_environment(module)?,
                            None => match sentinel {
                                Some(env) => env,
                                None => {
                                    let env = self.heap.alloc_environment(None, BTreeMap::new());
                                    self.heap.environment_mut(env)?.module_getter_sentinel = true;
                                    sentinel = Some(env);
                                    env
                                }
                            },
                        };
                        *entry.insert(self.heap.alloc_function(export.getter, env))
                    }
                };
                let descriptor = DescriptorUpdate {
                    get: Some(Some(getter)),
                    set: Some(None),
                    enumerable: Some(MODULE_EXPORT_ATTRIBUTES.enumerable),
                    configurable: Some(MODULE_EXPORT_ATTRIBUTES.configurable),
                    ..DescriptorUpdate::default()
                };
                if !self
                    .heap
                    .apply_descriptor(namespace, name.as_str().into(), descriptor)?
                {
                    return Err(VmError::InvalidState(format!(
                        "cannot install module export {name:?}"
                    )));
                }
            }
            self.heap.get_mut(namespace)?.prevent_extensions();
        }

        for index in 0..count {
            let declaration = self.build.modules[index].declaration;
            self.run_module_phase(host, index, declaration)?;
        }
        Ok(())
    }

    fn install_preload_globals(&mut self, preload: u32) -> Result<(), VmError> {
        let namespace = self.module_namespaces[preload as usize];
        let global = self.builtins.get("globalThis");
        for name in self.build.modules[preload as usize].exports.keys() {
            let Some(crate::heap::OwnProperty::Stored(
                crate::heap::Property::Accessor {
                    get: Some(getter),
                    set: None,
                },
                _,
            )) = self
                .heap
                .get(namespace)?
                .own_property(crate::value::PropertyKeyRef::String(name))
            else {
                return Err(VmError::InvalidState(format!(
                    "missing preload getter {name:?}"
                )));
            };
            let descriptor = DescriptorUpdate {
                get: Some(Some(*getter)),
                set: Some(None),
                enumerable: Some(PRELOAD_EXPORT_ATTRIBUTES.enumerable),
                configurable: Some(PRELOAD_EXPORT_ATTRIBUTES.configurable),
                ..DescriptorUpdate::default()
            };
            if !self
                .heap
                .apply_descriptor(global, name.as_str().into(), descriptor)?
            {
                return Err(VmError::InvalidState(format!(
                    "cannot install preload global {name:?}"
                )));
            }
        }
        Ok(())
    }

    fn evaluate_module(
        &mut self,
        host: &Host,
        root: u32,
        states: &mut [ModuleEvaluation],
    ) -> Result<(), VmError> {
        let root = root as usize;
        if states[root] == ModuleEvaluation::Done {
            return Ok(());
        }
        let mut stack = vec![(root, 0)];
        states[root] = ModuleEvaluation::Visiting;
        while let Some((index, next_dependency)) = stack.last_mut() {
            let module = &self.build.modules[*index];
            if let Some(&dependency) = module.dependencies.get(*next_dependency) {
                *next_dependency += 1;
                let dependency = dependency as usize;
                if states[dependency] == ModuleEvaluation::Pending {
                    states[dependency] = ModuleEvaluation::Visiting;
                    stack.push((dependency, 0));
                }
            } else {
                let index = *index;
                self.run_module_phase(host, index, module.evaluation)?;
                states[index] = ModuleEvaluation::Done;
                stack.pop();
            }
        }
        Ok(())
    }

    fn run_module_phase(
        &mut self,
        host: &Host,
        module: usize,
        func_id: u32,
    ) -> Result<(), VmError> {
        let function_index = self.function_index(func_id)?;
        let prepared = self.module_envs[module];
        let result = self
            .activate(
                host,
                &ActiveRoots::EMPTY,
                function_index,
                None,
                prepared,
                Arguments::EMPTY,
                (&Value::UNDEFINED, &Value::UNDEFINED, None),
                None,
            )?
            .into_result();
        if let Err(VmError::Thrown(error)) = &result
            && let Some(sink) = host.services().get::<crate::host::DiagnosticsKey>()
        {
            sink.report(crate::host::Diagnostic::StartupException {
                diagnostic: &self.exception_diagnostic(error),
            });
        }
        result.map(|_| ())
    }
}
