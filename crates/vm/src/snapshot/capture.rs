use std::collections::{BTreeMap, BTreeSet};

use crate::binding::{Bindings, CpsBoundary, ExportDecl, NativeTarget, Recovery};
use crate::builtin::is_callable_object;
use crate::builtin::roots::BuiltinRoots;
use crate::bytecode::{BYTECODE_VERSION, Build};
use crate::error::VmError;
use crate::heap::{CollectionIteration, CollectionKind, push_value};
use crate::heap::{
    CombineKind, EnvRole, HAS_INSTANCE_DESCRIPTION, HAS_INSTANCE_SYMBOL, Heap,
    IS_CONCAT_SPREADABLE_DESCRIPTION, IS_CONCAT_SPREADABLE_SYMBOL, ITERATOR_DESCRIPTION,
    ITERATOR_SYMBOL, MATCH_ALL_DESCRIPTION, MATCH_ALL_SYMBOL, MATCH_DESCRIPTION, MATCH_SYMBOL,
    NativeObject, Object, OwnProperty, OwnedEdge, PrivateElement, PrivateMemberKind,
    PromiseCallback, PromiseState, Property, REPLACE_DESCRIPTION, REPLACE_SYMBOL, ReactionJob,
    RejectionStatus, SEARCH_DESCRIPTION, SEARCH_SYMBOL, SPECIES_DESCRIPTION, SPECIES_SYMBOL,
    SPLIT_DESCRIPTION, SPLIT_SYMBOL, TO_PRIMITIVE_DESCRIPTION, TO_PRIMITIVE_SYMBOL, WeakKey,
};
use crate::host::Host;
use crate::regexp::compile_pattern;
use crate::runtime::{
    Continuation, HOST_TASK_ID_LIMIT, HostInstance, HostSlot, HostTask, HostTaskState, ModuleState,
    PendingOperation, Settlement,
};
use crate::value::{ContinuationId, ObjectId, PropertyKeyRef, Value, ValueRef};
use crate::vm::Vm;

use super::record::{valid_string_position, *};

impl Vm {
    /// Captures an idle VM without releasing any live registrations.
    /// The caller must hold external logical time and delivery at the same cut;
    /// no event may have been taken out of its source but not yet delivered.
    pub fn snapshot(&self, roots: &[Value]) -> Result<Snapshot, VmError> {
        if !self.microtasks.is_empty() {
            return Err(VmError::InvalidState(
                "snapshot requires a drained microtask queue".to_owned(),
            ));
        }

        let live = self.trace_roots(roots)?;
        if self
            .heap
            .resolutions
            .iter()
            .any(|id| !live.objects.contains(id))
        {
            return Err(VmError::InvalidState(
                "snapshot during pending thenable assimilation".to_owned(),
            ));
        }

        let mut snapshot = Snapshot::new(self.build.build_id.clone());
        snapshot.binding_fingerprint = self.bindings.fingerprint().to_owned();
        snapshot.globals = self.globals.clone();
        snapshot.roots = self.builtins.to_map();
        snapshot.imports = self.imports.iter().map(|import| import.namespace).collect();
        snapshot.pending_rejection_notifications = self
            .pending_rejection_notifications
            .iter()
            .copied()
            .collect();
        for (class, tag) in self.bindings.classes() {
            snapshot
                .class_prototypes
                .insert(tag.to_owned(), self.host_classes[class].prototype);
        }
        snapshot.script_env = self.script_env;
        for id in &live.objects {
            let object = match self.heap.get(*id)? {
                Object::WeakMap { object, entries } => Object::WeakMap {
                    object: object.clone(),
                    entries: entries
                        .iter()
                        .filter(|(key, _)| live.contains_weak_key(**key))
                        .map(|(&key, value)| (key, value.clone()))
                        .collect(),
                },
                Object::WeakSet { object, entries } => Object::WeakSet {
                    object: object.clone(),
                    entries: entries
                        .iter()
                        .copied()
                        .filter(|key| live.contains_weak_key(*key))
                        .collect(),
                },
                object => object.clone(),
            };
            snapshot.objects.push(ObjectRecord { id: *id, object });
            if let Some(elements) = self.heap.private.get(id) {
                snapshot.private_elements.extend(elements.iter().map(
                    |(&(brand, index), element)| PrivateRecord {
                        receiver: *id,
                        brand,
                        index,
                        element: element.clone(),
                    },
                ));
            }
        }
        for (object, instance) in &self.host_instances {
            if !live.objects.contains(object) {
                continue;
            }
            let class = instance.class;
            let slots = instance
                .slots
                .iter()
                .map(|slot| match slot {
                    HostSlot::Js(value) => HostSlotRecord::Js(value.clone()),
                    HostSlot::JsList(values) => HostSlotRecord::JsList(values.clone()),
                })
                .collect();
            let payload = match self.bindings.class_payload_schema(class) {
                Some(schema) => {
                    let payload = instance.payload.as_ref().ok_or_else(|| {
                        VmError::InvalidState(format!(
                            "host instance {object} is missing its payload"
                        ))
                    })?;
                    let mut bytes = Vec::new();
                    self.bindings
                        .class_encode(class, payload.as_ref(), &mut bytes)
                        .map_err(|error| VmError::InvalidState(error.0))?;
                    Some(PayloadRecord { schema, bytes })
                }
                None => None,
            };
            snapshot.host_instances.push(HostInstanceRecord {
                object: *object,
                class: self
                    .bindings
                    .class_tag(class)
                    .unwrap_or_default()
                    .to_owned(),
                slots,
                payload,
            });
        }
        for (module, state) in self.module_states.iter().enumerate() {
            let Some(state) = state else {
                continue;
            };
            let slots = state
                .slots
                .iter()
                .map(|slot| match slot {
                    HostSlot::Js(value) => HostSlotRecord::Js(value.clone()),
                    HostSlot::JsList(values) => HostSlotRecord::JsList(values.clone()),
                })
                .collect();
            let schema = self
                .bindings
                .state_schema(module)
                .ok_or_else(|| VmError::InvalidState("module state is undeclared".to_owned()))?;
            let mut bytes = Vec::new();
            self.bindings
                .state_encode(module, state.payload.as_ref(), &mut bytes)
                .ok_or_else(|| VmError::InvalidState("module state is undeclared".to_owned()))?
                .map_err(|error| VmError::InvalidState(error.0))?;
            snapshot.module_states.push(ModuleStateRecord {
                module: self
                    .bindings
                    .state_name(module)
                    .unwrap_or_default()
                    .to_owned(),
                slots,
                payload: PayloadRecord { schema, bytes },
            });
        }
        for id in live.continuations_seen {
            let continuation = self.continuations.get(&id).ok_or_else(|| {
                VmError::InvalidState(format!("continuation {id} does not exist"))
            })?;
            let waiting_on = continuation.waiting_on.ok_or_else(|| {
                VmError::InvalidState(format!(
                    "continuation {id} waits on a plain value and cannot be snapshotted"
                ))
            })?;
            snapshot.continuations.push(ContinuationRecord {
                id,
                async_promise: continuation.async_promise,
                func_id: continuation.func_id,
                resume_id: continuation.resume_id,
                waiting_on,
                env: continuation.env,
                values: continuation.values.clone(),
            });
        }
        snapshot.next_host_task_id = self.next_host_task_id;
        for (&id, task) in &self.host_tasks {
            let HostTaskState::Waiting(waiting_on) = task.state else {
                return Err(VmError::InvalidState(format!(
                    "host task {id} must be waiting to be snapshotted"
                )));
            };
            snapshot.host_tasks.push(HostTaskRecord {
                id,
                owner: task.owner,
                stage: task.stage,
                receiver: task.receiver.clone(),
                saved: task.saved.clone(),
                waiting_on,
            });
        }
        for (id, operation) in &self.operations {
            snapshot.operations.push(PendingOperationRecord {
                id: *id,
                name: operation.name.clone(),
                schema: operation.schema,
                recovery: operation.recovery,
                state: operation
                    .checkpoint()
                    .map_err(|error| VmError::HostFault(error.0))?,
                target: operation.target,
            });
        }
        for (id, state) in &self.heap.combines {
            if live.objects.contains(id) {
                snapshot.combines.insert(*id, state.clone());
            }
        }
        snapshot.next_symbol_id = self.heap.next_symbol_id();
        snapshot.random_state = self.random_state;
        snapshot.symbol_registry = self.heap.symbol_registry.clone();
        for id in live.symbols {
            if let Some(description) = self.heap.symbols.get(&id) {
                snapshot.symbols.insert(id, description.clone());
            }
        }
        Ok(snapshot)
    }

    pub fn restore(
        mut snapshot: Snapshot,
        build: Build,
        host: &mut dyn Host,
    ) -> Result<Self, VmError> {
        check_versions(&snapshot)?;
        if snapshot.random_state == 0 {
            return Err(VmError::InvalidSnapshot(
                "invalid Math.random state".to_owned(),
            ));
        }
        build.validate().map_err(VmError::InvalidBuild)?;
        if snapshot.build_id != build.build_id {
            return Err(VmError::InvalidSnapshot(format!(
                "build id mismatch: snapshot {}, build {}",
                snapshot.build_id, build.build_id
            )));
        }
        let bindings = host.bindings();
        if snapshot.binding_fingerprint != bindings.fingerprint() {
            return Err(VmError::InvalidSnapshot(format!(
                "binding fingerprint mismatch: snapshot {}, bindings {}",
                snapshot.binding_fingerprint,
                bindings.fingerprint()
            )));
        }
        let resolved_imports = Self::resolve_imports(&build, &bindings)?;

        let mut heap = Heap::new();
        for ObjectRecord { id, mut object } in std::mem::take(&mut snapshot.objects) {
            if let Object::RegExp {
                source,
                flags,
                compiled,
                ..
            } = &mut object
            {
                // Typed snapshots can be edited without going through deserialization.
                *compiled = compile_pattern(source, &flags.as_string())
                    .map_err(|error| VmError::InvalidSnapshot(format!("RegExp {id}: {error}")))?
                    .1;
            }
            heap.insert_restore(id, object)?;
        }
        validate_rejection_notifications(&heap, &snapshot.pending_rejection_notifications)?;
        heap.combines = snapshot.combines.clone();
        validate_symbol_tables(&snapshot)?;
        heap.restore_symbols(
            snapshot.symbols.clone(),
            snapshot.symbol_registry.clone(),
            snapshot.next_symbol_id,
        );
        load_private_elements(&mut heap, &snapshot.private_elements)?;
        if snapshot.imports.len() != build.imports.len() {
            return Err(VmError::InvalidSnapshot(format!(
                "import namespace count mismatch: snapshot {}, build {}",
                snapshot.imports.len(),
                build.imports.len()
            )));
        }
        validate_native_targets(&bindings, &heap)?;
        let host_classes = load_host_classes(&bindings, &heap, &snapshot.class_prototypes)?;
        let host_instances = load_host_instances(&bindings, &heap, &snapshot.host_instances)?;
        let module_states = load_module_states(&bindings, &heap, &snapshot.module_states)?;
        let continuations = load_continuations(&build, &heap, &snapshot.continuations)?;
        for continuation in continuations.values() {
            require_value_objects(&heap, continuation.values.iter())?;
        }
        let script_env = load_script_env(&build, &heap, snapshot.script_env)?;
        let builtins = BuiltinRoots::load(&snapshot.roots, &heap)?;
        validate_preload_globals(&build, &heap, &builtins, script_env)?;
        let operations = load_operations(&bindings, &heap, snapshot.operations)?;
        let host_tasks = load_host_tasks(
            &bindings,
            &heap,
            &host_instances,
            &operations,
            snapshot.host_tasks,
            snapshot.next_host_task_id,
        )?;
        let mut vm = Self::restored(
            build,
            bindings.clone(),
            crate::vm::RestoredState {
                heap,
                continuations,
                operations,
                host_tasks,
                next_host_task_id: snapshot.next_host_task_id,
                host_classes,
                host_instances,
                module_states,
                globals: snapshot.globals,
                builtins,
                script_env,
            },
        );
        // Recovery can append new notifications; the captured ones must stay first.
        vm.pending_rejection_notifications = snapshot.pending_rejection_notifications.into();
        vm.restore_imports(&bindings, snapshot.imports, resolved_imports)?;
        validate_native_import_objects(&vm.heap, &vm.imports)?;
        check_heap_edges(&vm.build, &vm.heap)?;
        validate_jobs(&vm.heap)?;
        validate_combines(&vm.heap)?;
        vm.settle_restored_operations()?;
        vm.random_state = snapshot.random_state;
        Ok(vm)
    }

    /// Apply the `Pause` and `Reject` strategies. `Resume` / `Reissue`
    /// operations stay pending until [`Vm::start_recovered_operations`] is
    /// called after the restored VM has been published.
    fn settle_restored_operations(&mut self) -> Result<(), VmError> {
        let pending = std::mem::take(&mut self.operations);
        for (id, operation) in pending {
            match operation.recovery {
                Recovery::Pause => {
                    self.deliver_operation(
                        operation.target,
                        Settlement::Fulfilled(Value::UNDEFINED),
                    )?;
                }
                Recovery::Reject => {
                    self.deliver_operation(
                        operation.target,
                        Settlement::Rejected(Value::string(operation.rejection.into())),
                    )?;
                }
                Recovery::Resume | Recovery::Reissue => {
                    self.operations.insert(id, operation);
                }
            }
        }
        Ok(())
    }
}

fn check_versions(snapshot: &Snapshot) -> Result<(), VmError> {
    if snapshot.format_version != SNAPSHOT_FORMAT_VERSION {
        return Err(VmError::InvalidSnapshot(format!(
            "unsupported snapshot format {}, expected {}",
            snapshot.format_version, SNAPSHOT_FORMAT_VERSION
        )));
    }
    if snapshot.bytecode_version != BYTECODE_VERSION {
        return Err(VmError::InvalidSnapshot(format!(
            "unsupported bytecode version {}, expected {}",
            snapshot.bytecode_version, BYTECODE_VERSION
        )));
    }
    Ok(())
}

/// Symbol tables validate exact reserved well-known records and registry integrity
/// before any heap edge is checked; all ids stay below the high-water mark.
fn validate_symbol_tables(snapshot: &Snapshot) -> Result<(), VmError> {
    if snapshot.next_symbol_id == 0 {
        return Err(VmError::InvalidSnapshot(
            "next symbol id must be positive".to_owned(),
        ));
    }
    for (id, description) in &snapshot.symbols {
        if *id >= snapshot.next_symbol_id {
            return Err(VmError::InvalidSnapshot(format!(
                "symbol {id} is not below the next symbol id {}",
                snapshot.next_symbol_id
            )));
        }
        let reserved = match *id {
            ITERATOR_SYMBOL => Some(ITERATOR_DESCRIPTION),
            TO_PRIMITIVE_SYMBOL => Some(TO_PRIMITIVE_DESCRIPTION),
            SPECIES_SYMBOL => Some(SPECIES_DESCRIPTION),
            MATCH_SYMBOL => Some(MATCH_DESCRIPTION),
            MATCH_ALL_SYMBOL => Some(MATCH_ALL_DESCRIPTION),
            REPLACE_SYMBOL => Some(REPLACE_DESCRIPTION),
            SEARCH_SYMBOL => Some(SEARCH_DESCRIPTION),
            SPLIT_SYMBOL => Some(SPLIT_DESCRIPTION),
            HAS_INSTANCE_SYMBOL => Some(HAS_INSTANCE_DESCRIPTION),
            IS_CONCAT_SPREADABLE_SYMBOL => Some(IS_CONCAT_SPREADABLE_DESCRIPTION),
            _ => None,
        };
        if let Some(expected) = reserved
            && !description
                .as_ref()
                .is_some_and(|description| description.units().eq(expected.encode_utf16()))
        {
            return Err(VmError::InvalidSnapshot(format!(
                "reserved symbol {id} has an invalid description"
            )));
        }
    }
    for (id, description) in [
        (ITERATOR_SYMBOL, ITERATOR_DESCRIPTION),
        (TO_PRIMITIVE_SYMBOL, TO_PRIMITIVE_DESCRIPTION),
        (SPECIES_SYMBOL, SPECIES_DESCRIPTION),
        (MATCH_SYMBOL, MATCH_DESCRIPTION),
        (MATCH_ALL_SYMBOL, MATCH_ALL_DESCRIPTION),
        (REPLACE_SYMBOL, REPLACE_DESCRIPTION),
        (SEARCH_SYMBOL, SEARCH_DESCRIPTION),
        (SPLIT_SYMBOL, SPLIT_DESCRIPTION),
        (HAS_INSTANCE_SYMBOL, HAS_INSTANCE_DESCRIPTION),
        (
            IS_CONCAT_SPREADABLE_SYMBOL,
            IS_CONCAT_SPREADABLE_DESCRIPTION,
        ),
    ] {
        if !snapshot
            .symbols
            .get(&id)
            .and_then(Option::as_ref)
            .is_some_and(|actual| actual.units().eq(description.encode_utf16()))
        {
            return Err(VmError::InvalidSnapshot(format!(
                "snapshot is missing reserved symbol {description}"
            )));
        }
    }
    let mut registered = BTreeSet::new();
    for (key, id) in &snapshot.symbol_registry {
        if let Some(description) = match *id {
            ITERATOR_SYMBOL => Some(ITERATOR_DESCRIPTION),
            TO_PRIMITIVE_SYMBOL => Some(TO_PRIMITIVE_DESCRIPTION),
            SPECIES_SYMBOL => Some(SPECIES_DESCRIPTION),
            MATCH_SYMBOL => Some(MATCH_DESCRIPTION),
            MATCH_ALL_SYMBOL => Some(MATCH_ALL_DESCRIPTION),
            REPLACE_SYMBOL => Some(REPLACE_DESCRIPTION),
            SEARCH_SYMBOL => Some(SEARCH_DESCRIPTION),
            SPLIT_SYMBOL => Some(SPLIT_DESCRIPTION),
            HAS_INSTANCE_SYMBOL => Some(HAS_INSTANCE_DESCRIPTION),
            IS_CONCAT_SPREADABLE_SYMBOL => Some(IS_CONCAT_SPREADABLE_DESCRIPTION),
            _ => None,
        } {
            return Err(VmError::InvalidSnapshot(format!(
                "registry key {key:?} points at reserved symbol {description}"
            )));
        }
        if !registered.insert(*id) {
            return Err(VmError::InvalidSnapshot(format!(
                "registry key {key:?} duplicates symbol {id}"
            )));
        }
        match snapshot.symbols.get(id) {
            Some(Some(description)) if description == key => {}
            _ => {
                return Err(VmError::InvalidSnapshot(format!(
                    "registry key {key:?} does not describe symbol {id}"
                )));
            }
        }
    }
    Ok(())
}

fn load_script_env(
    build: &Build,
    heap: &Heap,
    script_env: Option<ObjectId>,
) -> Result<Option<ObjectId>, VmError> {
    let needs_env = build
        .function(build.script)
        .is_some_and(|function| function.needs_environment());
    match (needs_env, script_env) {
        (false, None) => Ok(None),
        (false, Some(id)) => Err(VmError::InvalidSnapshot(format!(
            "script does not use an environment, found {id}"
        ))),
        (true, None) => Err(VmError::InvalidSnapshot(
            "script environment is missing".to_owned(),
        )),
        (true, Some(id)) => {
            let environment = match heap.get(id) {
                Ok(Object::Environment(environment)) => environment,
                Ok(_) => {
                    return Err(VmError::InvalidSnapshot(format!(
                        "script environment {id} is not an environment"
                    )));
                }
                Err(_) => {
                    return Err(VmError::InvalidSnapshot(format!(
                        "script environment {id} does not exist"
                    )));
                }
            };
            if environment.parent.is_some() {
                return Err(VmError::InvalidSnapshot(format!(
                    "script environment {id} has a parent"
                )));
            }
            for (name, binding) in &build.exports {
                if !environment.bindings.contains_key(binding) {
                    return Err(VmError::InvalidSnapshot(format!(
                        "export {name:?} binding {binding} is missing"
                    )));
                }
            }
            Ok(Some(id))
        }
    }
}

fn validate_preload_globals(
    build: &Build,
    heap: &Heap,
    builtins: &BuiltinRoots,
    script_env: Option<ObjectId>,
) -> Result<(), VmError> {
    let Some(preload) = &build.preload else {
        return Ok(());
    };
    let global = heap.get(builtins.get("globalThis"))?;
    let mut common_env = None;
    let mut closures = BTreeMap::new();
    for (name, binding) in &preload.exports {
        let error = |detail| VmError::InvalidSnapshot(format!("preload global {name:?}: {detail}"));
        let Some(OwnProperty::Stored(
            Property::Accessor {
                get: Some(id),
                set: None,
            },
            attrs,
        )) = global.own_property(PropertyKeyRef::String(name))
        else {
            return Err(error("missing getter-only own property"));
        };
        if attrs != crate::vm::PRELOAD_EXPORT_ATTRIBUTES {
            return Err(error("invalid attributes"));
        }
        let func_id = preload.getters[name];
        let getter = heap
            .function(*id)
            .filter(|getter| getter.func_id == func_id)
            .ok_or_else(|| error("wrong getter function"))?;
        if *closures.entry(func_id).or_insert(*id) != *id {
            return Err(error("aliases do not share a getter"));
        }
        if Some(getter.env) == script_env || *common_env.get_or_insert(getter.env) != getter.env {
            return Err(error("wrong preload environment"));
        }
        if !matches!(heap.get(getter.env), Ok(Object::Environment(env))
            if env.parent.is_none() && env.bindings.contains_key(binding))
        {
            return Err(error("missing top-level export binding"));
        }
    }
    Ok(())
}

fn load_private_elements(heap: &mut Heap, records: &[PrivateRecord]) -> Result<(), VmError> {
    for record in records {
        let invalid = || {
            VmError::InvalidSnapshot(format!(
                "invalid private brand {} index {} on receiver {}",
                record.brand, record.index, record.receiver
            ))
        };
        if matches!(
            heap.get(record.receiver),
            Err(_) | Ok(Object::Environment(_) | Object::ForInIterator { .. })
        ) {
            return Err(invalid());
        }
        let class = heap
            .function(record.brand)
            .and_then(|f| f.class.as_ref())
            .ok_or_else(invalid)?;
        let member = class
            .source
            .private
            .get(&record.index)
            .filter(|_| record.index < class.source.private_count)
            .ok_or_else(invalid)?;
        if (member.is_static && record.brand != record.receiver)
            || matches!(member.kind, PrivateMemberKind::Field)
                != matches!(record.element, PrivateElement::Field(_))
        {
            return Err(invalid());
        }
        if let PrivateElement::Field(value) = &record.element {
            require_value_objects(heap, std::iter::once(value))?;
            if matches!(value.view(), ValueRef::Object(id) if matches!(heap.get(id), Ok(Object::Environment(_) | Object::ForInIterator { .. })))
            {
                return Err(invalid());
            }
        }
        if heap
            .private
            .entry(record.receiver)
            .or_default()
            .insert((record.brand, record.index), record.element.clone())
            .is_some()
        {
            return Err(invalid());
        }
    }
    Ok(())
}

fn validate_weak_key(heap: &Heap, owner: &str, id: ObjectId, key: WeakKey) -> Result<(), VmError> {
    let valid = match key {
        WeakKey::Object(target) => !matches!(
            heap.get(target),
            Err(_) | Ok(Object::Environment(_) | Object::ForInIterator { .. })
        ),
        WeakKey::Symbol(target) => heap.weak_key(&Value::symbol(target)).is_some(),
    };
    if !valid {
        return Err(VmError::InvalidSnapshot(format!(
            "{owner} {id} has an invalid key {key}"
        )));
    }
    Ok(())
}

fn check_heap_edges(build: &Build, heap: &Heap) -> Result<(), VmError> {
    let mut edges = Vec::new();
    let mut prototypes = BTreeMap::new();
    use crate::bytecode::Instr;
    let definitions = build
        .functions
        .iter()
        .flat_map(|f| &f.code)
        .filter_map(|instr| {
            if let Instr::CreateClass {
                constructor,
                heritage,
                default_constructor,
                private_count,
                private_binding,
                ..
            } = instr
            {
                Some((
                    *constructor,
                    (
                        heritage.is_some(),
                        *default_constructor,
                        *private_count,
                        *private_binding,
                    ),
                ))
            } else {
                None
            }
        })
        .collect::<BTreeMap<_, _>>();
    for id in heap.ids() {
        use crate::builtin::call::{DERIVED_CLASS, DERIVED_TARGET, DERIVED_THIS};
        if let Object::Environment(env) = heap.get(id)?
            && env.bindings.range(DERIVED_THIS..).next().is_some()
        {
            let valid_class = matches!(env.bindings.get(&DERIVED_CLASS).map(Value::view), Some(ValueRef::Object(class))
                if heap.function(class).and_then(|f| f.class.as_ref()).is_some_and(|c| c.source.derived && !c.source.default_constructor));
            let valid_target = matches!(env.bindings.get(&DERIVED_TARGET).map(Value::view), Some(ValueRef::Object(target))
                if heap.get(target).is_ok_and(crate::builtin::is_callable_object));
            let valid_this = match env.bindings.get(&DERIVED_THIS).map(Value::view) {
                None => true,
                Some(ValueRef::Object(receiver)) => !matches!(
                    heap.get(receiver),
                    Err(_) | Ok(Object::Environment(_) | Object::ForInIterator { .. })
                ),
                _ => false,
            };
            if env.parent.is_some()
                || env.bindings.keys().any(|key| *key < DERIVED_THIS)
                || !valid_class
                || !valid_target
                || !valid_this
            {
                return Err(VmError::InvalidSnapshot(format!(
                    "invalid derived constructor context {id}"
                )));
            }
        }
        if let Object::Function(function) = heap.get(id)? {
            let code = build
                .function(function.func_id)
                .filter(|_| !build.is_initializer(function.func_id));
            let Some(code) = code else {
                return Err(VmError::InvalidSnapshot(format!(
                    "function object {id} references an invalid function"
                )));
            };
            if (code.home_param.is_some() && function.home.is_none())
                || function.home.is_some_and(|home| {
                    !matches!(heap.get(home), Ok(Object::Function(_) | Object::Plain(_)))
                })
            {
                return Err(VmError::InvalidSnapshot(format!(
                    "function object {id} has an invalid home object"
                )));
            }
            if let Some(class) = &function.class
                && (!code.is_constructable()
                    || definitions.get(&function.func_id) != Some(&(class.source.derived, class.source.default_constructor, class.source.private_count, class.source.private_binding))
                    || function.home != Some(class.prototype)
                    || class.source.private_binding.is_some_and(|binding| !matches!(heap.environment(function.env), Ok(env) if env.bindings.get(&binding) == Some(&Value::object(id))))
                    || !matches!(heap.get(class.prototype), Ok(Object::Plain(_)))
                    || !matches!(heap.get(id)?.own_property(PropertyKeyRef::String("prototype")),
                        Some(OwnProperty::Stored(Property::Data(prototype), attributes))
                            if matches!(prototype.view(), ValueRef::Object(id) if id == class.prototype)
                                && attributes == crate::heap::Attributes::FIXED))
            {
                return Err(VmError::InvalidSnapshot(format!(
                    "function object {id} has an invalid class record"
                )));
            }
            if let Some(class) = &function.class {
                let mut fields = BTreeSet::new();
                for (&index, member) in &class.source.private {
                    let home = Some(if member.is_static {
                        id
                    } else {
                        class.prototype
                    });
                    let valid_closure = |target, accessor: bool| {
                        heap.function(target).is_some_and(|f| {
                            f.home == home
                                && build.function(f.func_id).is_some_and(|code| {
                                    !build.is_initializer(code.func_id)
                                        && code.is_method
                                        && (!accessor || code.is_accessor_method())
                                })
                        })
                    };
                    let valid = match member.kind {
                        PrivateMemberKind::Field => {
                            fields.insert(index);
                            true
                        }
                        PrivateMemberKind::Method(target) => valid_closure(target, false),
                        PrivateMemberKind::Accessor { get, set } => {
                            (get.is_some() || set.is_some())
                                && get
                                    .into_iter()
                                    .chain(set)
                                    .all(|target| valid_closure(target, true))
                        }
                    };
                    if index >= class.source.private_count || !valid {
                        return Err(VmError::InvalidSnapshot(format!(
                            "class {id} has an invalid private declaration {index}"
                        )));
                    }
                }
                for element in &class.source.elements {
                    if let crate::heap::ClassElement::PrivateField { index, .. } = element
                        && !fields.remove(index)
                    {
                        return Err(VmError::InvalidSnapshot(format!(
                            "class {id} has an invalid private field {index}"
                        )));
                    }
                    if let Some(closure) = element.closure()
                        && !heap
                            .function(closure)
                            .and_then(|f| build.function(f.func_id))
                            .is_some_and(|f| {
                                !build.is_initializer(f.func_id) && f.is_accessor_method()
                            })
                    {
                        return Err(VmError::InvalidSnapshot(format!(
                            "class {id} has an invalid initializer closure"
                        )));
                    }
                }
                if !fields.is_empty() {
                    return Err(VmError::InvalidSnapshot(format!(
                        "class {id} has missing private field definitions"
                    )));
                }
            }
        }
        // The shared edge walk below checks the bound receiver and arguments.
        if let Object::Bound(bound) = heap.get(id)? {
            let callable = heap
                .get(bound.target)
                .is_ok_and(crate::builtin::is_callable_object);
            let valid = callable
                && match heap.function(bound.target) {
                    Some(function) => build.function(function.func_id).is_some(),
                    None => true,
                };
            if !valid {
                return Err(VmError::InvalidSnapshot(format!(
                    "bound target {} is missing or not a function",
                    bound.target
                )));
            }
        }
        if let Object::CollectionIterator {
            kind,
            iteration,
            target,
            next_index,
            ..
        } = heap.get(id)?
        {
            let length = match (kind, target) {
                (_, None) => Some(0),
                (CollectionKind::Map, Some(target)) => match heap.get(*target) {
                    Ok(Object::Map { entries, .. }) => Some(entries.len()),
                    _ => None,
                },
                (CollectionKind::Set, Some(target)) => match heap.get(*target) {
                    Ok(Object::Set { elements, .. }) => Some(elements.len()),
                    _ => None,
                },
            };
            if length.is_none_or(|length| *next_index > length)
                || matches!(
                    (kind, iteration),
                    (CollectionKind::Set, CollectionIteration::Keys)
                )
            {
                return Err(VmError::InvalidSnapshot(format!(
                    "collection iterator {id} has an invalid target, kind, or position"
                )));
            }
        }
        if let Object::ArrayIterator {
            array: Some(array), ..
        } = heap.get(id)?
            && heap.get(*array).is_err()
        {
            return Err(VmError::InvalidSnapshot(format!(
                "array iterator {id} references missing source {array}"
            )));
        }
        if let Object::StringIterator {
            string, position, ..
        } = heap.get(id)?
            && !valid_string_position(string, *position)
        {
            return Err(VmError::InvalidSnapshot(format!(
                "string iterator {id} has an invalid UTF-16 position"
            )));
        }
        if let Object::RegExpStringIterator { regexp, .. } = heap.get(id)?
            && heap.get(*regexp).is_err()
        {
            return Err(VmError::InvalidSnapshot(format!(
                "RegExp string iterator {id} references missing regexp {regexp}"
            )));
        }
        if let regexp @ Object::RegExp { .. } = heap.get(id)?
            && !regexp
                .own_property(PropertyKeyRef::String("lastIndex"))
                .is_some_and(|property| {
                    matches!(property, OwnProperty::Stored(Property::Data(_), attributes)
                        if !attributes.enumerable && !attributes.configurable)
                })
        {
            return Err(VmError::InvalidSnapshot(format!(
                "RegExp {id} has an invalid lastIndex property"
            )));
        }
        if let Object::ForInIterator {
            current,
            pending,
            visited,
            string_indices,
        } = heap.get(id)?
            && (current.is_some_and(|source| {
                matches!(
                    heap.get(source),
                    Err(_) | Ok(Object::Environment(_) | Object::ForInIterator { .. })
                )
            }) || (current.is_none() && pending.is_some())
                || string_indices.start > string_indices.end
                || (!string_indices.is_empty()
                    && (pending.is_some()
                        || visited.len() != 1
                        || !visited.first().is_some_and(|key| key == "length"))))
        {
            return Err(VmError::InvalidSnapshot(format!(
                "for-in iterator {id} has invalid cursor state"
            )));
        }
        heap.write_edges(id, &mut edges)?;
        match heap.get(id)? {
            Object::WeakMap { entries, .. } => {
                for (&key, value) in entries {
                    validate_weak_key(heap, "WeakMap", id, key)?;
                    push_value(&mut edges, value);
                }
            }
            Object::WeakSet { entries, .. } => {
                for &key in entries {
                    validate_weak_key(heap, "WeakSet", id, key)?;
                }
            }
            _ => {}
        }
        for edge in edges.drain(..) {
            match edge {
                OwnedEdge::Object(target) => {
                    if !heap.contains(target) {
                        return Err(VmError::InvalidSnapshot(format!(
                            "value references missing object {target}"
                        )));
                    }
                }
                OwnedEdge::Accessor(target) => match heap.get(target) {
                    Ok(object) if is_callable_object(object) => {}
                    Ok(_) => {
                        return Err(VmError::InvalidSnapshot(format!(
                            "object {id} accessor has invalid target {target}"
                        )));
                    }
                    Err(_) => {
                        return Err(VmError::InvalidSnapshot(format!(
                            "object {id} accessor references missing object {target}"
                        )));
                    }
                },
                OwnedEdge::Prototype(target) => match heap.is_environment(target) {
                    None => {
                        return Err(VmError::InvalidSnapshot(format!(
                            "object {id} prototype {target} does not exist"
                        )));
                    }
                    Some(true) => {
                        return Err(VmError::InvalidSnapshot(format!(
                            "object {id} prototype {target} is an environment"
                        )));
                    }
                    Some(false) => {
                        prototypes.insert(id, target);
                    }
                },
                OwnedEdge::Environment { id: env, role } => {
                    require_environment(heap, id, env, role)?;
                }
                OwnedEdge::Continuation(_) => {}
                OwnedEdge::Symbol(symbol) => {
                    if !heap.symbols.contains_key(&symbol) {
                        return Err(VmError::InvalidSnapshot(format!(
                            "object {id} references missing symbol {symbol}"
                        )));
                    }
                }
            }
        }
        if let Object::Proxy {
            target, handler, ..
        } = heap.get(id)?
        {
            if !matches!(
                heap.get(*target),
                Ok(Object::Plain(_)
                    | Object::Array(_)
                    | Object::Boxed { .. }
                    | Object::Proxy { .. })
            ) {
                return Err(VmError::InvalidSnapshot(format!(
                    "proxy {id} has invalid target {target}"
                )));
            }
            if matches!(
                heap.get(*handler),
                Err(_) | Ok(Object::Environment(_) | Object::ForInIterator { .. })
            ) {
                return Err(VmError::InvalidSnapshot(format!(
                    "proxy {id} has invalid handler {handler}"
                )));
            }
            // Follow the target before reading its prototype. This checks both
            // target-only cycles and cycles involving ordinary prototype links.
            prototypes.insert(id, *target);
        }
    }

    let mut completed = BTreeSet::new();
    let mut path = BTreeSet::new();
    for &id in prototypes.keys() {
        let mut current = id;
        while !completed.contains(&current) {
            if !path.insert(current) {
                return Err(VmError::InvalidSnapshot(format!(
                    "prototype chain of object {id} is cyclic"
                )));
            }
            let Some(&next) = prototypes.get(&current) else {
                break;
            };
            current = next;
        }
        completed.extend(std::mem::take(&mut path));
    }
    Ok(())
}
fn valid_native_target(bindings: &Bindings, target: crate::binding::NativeTarget) -> bool {
    match target {
        crate::binding::NativeTarget::Export(id) => matches!(
            bindings.export_decl(id),
            Some(crate::binding::ExportDecl::Function(_))
        ),
        crate::binding::NativeTarget::Method(id) => bindings.method(id).is_some(),
        crate::binding::NativeTarget::Accessor {
            class,
            accessor,
            set,
        } => bindings
            .accessor(class as usize, accessor as usize)
            .is_some_and(|declaration| {
                if set {
                    declaration.set.is_some()
                } else {
                    declaration.get.is_some()
                }
            }),
    }
}

fn validate_native_targets(bindings: &Bindings, heap: &Heap) -> Result<(), VmError> {
    let mut host_identities = BTreeSet::new();
    let mut intrinsic_identities = BTreeSet::new();
    for id in heap.ids() {
        let unique = match heap.get(id)? {
            Object::Native(NativeObject::Host {
                target,
                constructable,
                ..
            }) => {
                if !valid_native_target(bindings, *target) {
                    return Err(VmError::InvalidSnapshot(format!(
                        "native object {id} references an undeclared binding"
                    )));
                }
                host_identities.insert((*target, *constructable))
            }
            Object::Native(NativeObject::Intrinsic { intrinsic, .. }) => {
                // IntrinsicId deserialization has already linked a declared key.
                intrinsic_identities.insert(*intrinsic)
            }
            _ => continue,
        };
        if !unique {
            return Err(VmError::InvalidSnapshot(format!(
                "duplicate native object identity at {id}"
            )));
        }
    }
    Ok(())
}

fn validate_native_import_objects(
    heap: &Heap,
    imports: &[crate::vm::ImportModule],
) -> Result<(), VmError> {
    let mut identities = BTreeSet::new();
    for import in imports {
        for value in &import.values {
            if let ValueRef::Object(id) = value.view()
                && let Object::Native(NativeObject::Host {
                    target,
                    constructable,
                    ..
                }) = heap.get(id)?
                && !identities.insert((*target, *constructable))
            {
                return Err(VmError::InvalidSnapshot(format!(
                    "duplicate native import identity at {id}"
                )));
            }
        }
    }
    Ok(())
}

fn load_host_classes(
    bindings: &Bindings,
    heap: &Heap,
    records: &BTreeMap<String, ObjectId>,
) -> Result<Vec<crate::heap::HostClassRecord>, VmError> {
    if records.len() != bindings.classes_len() {
        return Err(VmError::InvalidSnapshot(format!(
            "class prototype count mismatch: snapshot {}, bindings {}",
            records.len(),
            bindings.classes_len()
        )));
    }
    let mut classes = Vec::with_capacity(bindings.classes_len());
    for (class, tag) in bindings.classes() {
        let id = *records
            .get(tag)
            .ok_or_else(|| VmError::InvalidSnapshot(format!("snapshot is missing class {tag}")))?;
        match heap.get(id) {
            Ok(Object::Plain(_)) => {}
            Ok(_) => {
                return Err(VmError::InvalidSnapshot(format!(
                    "class {tag} prototype {id} is not a plain object"
                )));
            }
            Err(_) => {
                return Err(VmError::InvalidSnapshot(format!(
                    "class {tag} prototype {id} does not exist"
                )));
            }
        }
        classes.push(crate::heap::HostClassRecord {
            prototype: id,
            source: class,
        });
    }
    Ok(classes)
}

fn load_host_instances(
    bindings: &Bindings,
    heap: &Heap,
    records: &[HostInstanceRecord],
) -> Result<BTreeMap<ObjectId, HostInstance>, VmError> {
    let mut instances = BTreeMap::new();
    for record in records {
        if instances.contains_key(&record.object) {
            return Err(VmError::InvalidSnapshot(format!(
                "duplicate host instance object {}",
                record.object
            )));
        }
        match heap.get(record.object) {
            Ok(Object::Plain(_)) => {}
            Ok(_) => {
                return Err(VmError::InvalidSnapshot(format!(
                    "host instance {} is not a plain object",
                    record.object
                )));
            }
            Err(_) => {
                return Err(VmError::InvalidSnapshot(format!(
                    "host instance {} does not exist",
                    record.object
                )));
            }
        }
        let class = bindings.class_by_tag(&record.class).ok_or_else(|| {
            VmError::InvalidSnapshot(format!("unknown host class {:?}", record.class))
        })?;
        let expected_slots = bindings.class_slots(class);
        if record.slots.len() != expected_slots.len() {
            return Err(VmError::InvalidSnapshot(format!(
                "class {} slot count mismatch: snapshot {}, bindings {}",
                record.class,
                record.slots.len(),
                expected_slots.len()
            )));
        }
        let mut slots = Vec::with_capacity(record.slots.len());
        for (slot, declaration) in record.slots.iter().zip(expected_slots) {
            let slot = match (slot, declaration.kind) {
                (HostSlotRecord::Js(value), crate::binding::SlotKind::Js) => {
                    HostSlot::Js(value.clone())
                }
                (HostSlotRecord::JsList(values), crate::binding::SlotKind::JsList) => {
                    HostSlot::JsList(values.clone())
                }
                _ => {
                    return Err(VmError::InvalidSnapshot(format!(
                        "class {} slot {:?} kind mismatch",
                        record.class, declaration.name
                    )));
                }
            };
            for value in match &slot {
                HostSlot::Js(value) => std::slice::from_ref(value),
                HostSlot::JsList(values) => values.as_slice(),
            } {
                require_value_objects(heap, std::iter::once(value))?;
            }
            slots.push(slot);
        }
        let payload = match bindings.class_payload_schema(class) {
            Some(schema) => {
                let payload = record.payload.as_ref().ok_or_else(|| {
                    VmError::InvalidSnapshot(format!(
                        "class {} is missing its payload",
                        record.class
                    ))
                })?;
                if payload.schema != schema {
                    return Err(VmError::InvalidSnapshot(format!(
                        "class {} payload schema mismatch: snapshot {}, bindings {}",
                        record.class, payload.schema, schema
                    )));
                }
                Some(
                    bindings
                        .class_decode(class, &payload.bytes)
                        .map_err(|error| {
                            VmError::InvalidSnapshot(format!(
                                "class {} payload: {error}",
                                record.class
                            ))
                        })?,
                )
            }
            None => {
                if record.payload.is_some() {
                    return Err(VmError::InvalidSnapshot(format!(
                        "class {} has no payload declaration",
                        record.class
                    )));
                }
                None
            }
        };
        instances.insert(
            record.object,
            HostInstance {
                class,
                slots,
                payload,
            },
        );
    }
    Ok(instances)
}

fn load_module_states(
    bindings: &Bindings,
    heap: &Heap,
    records: &[ModuleStateRecord],
) -> Result<Vec<Option<ModuleState>>, VmError> {
    let declared = bindings.state_modules();
    let mut by_name: BTreeMap<&str, &ModuleStateRecord> = BTreeMap::new();
    for record in records {
        if by_name.insert(record.module.as_str(), record).is_some() {
            return Err(VmError::InvalidSnapshot(format!(
                "duplicate module state {:?}",
                record.module
            )));
        }
    }
    if by_name.len() != declared.len() {
        return Err(VmError::InvalidSnapshot(format!(
            "module state count mismatch: snapshot {}, bindings {}",
            by_name.len(),
            declared.len()
        )));
    }
    let mut states = (0..bindings.module_count())
        .map(|_| None)
        .collect::<Vec<_>>();
    for module in declared {
        let name = bindings
            .state_name(module)
            .ok_or_else(|| VmError::InvalidSnapshot("module state is undeclared".to_owned()))?;
        let record = by_name.get(name).ok_or_else(|| {
            VmError::InvalidSnapshot(format!("snapshot is missing module state {name:?}"))
        })?;
        let expected_slots = bindings.state_slots(module).unwrap_or_default();
        if record.slots.len() != expected_slots.len() {
            return Err(VmError::InvalidSnapshot(format!(
                "module {name} state slot count mismatch"
            )));
        }
        let mut slots = Vec::with_capacity(record.slots.len());
        for (slot, declaration) in record.slots.iter().zip(expected_slots) {
            let slot = match (slot, declaration.kind) {
                (HostSlotRecord::Js(value), crate::binding::SlotKind::Js) => {
                    HostSlot::Js(value.clone())
                }
                (HostSlotRecord::JsList(values), crate::binding::SlotKind::JsList) => {
                    HostSlot::JsList(values.clone())
                }
                _ => {
                    return Err(VmError::InvalidSnapshot(format!(
                        "module {name} state slot {:?} kind mismatch",
                        declaration.name
                    )));
                }
            };
            for value in match &slot {
                HostSlot::Js(value) => std::slice::from_ref(value),
                HostSlot::JsList(values) => values.as_slice(),
            } {
                require_value_objects(heap, std::iter::once(value))?;
            }
            slots.push(slot);
        }
        let schema = bindings
            .state_schema(module)
            .ok_or_else(|| VmError::InvalidSnapshot("module state is undeclared".to_owned()))?;
        if record.payload.schema != schema {
            return Err(VmError::InvalidSnapshot(format!(
                "module {name} state payload schema mismatch: snapshot {}, bindings {}",
                record.payload.schema, schema
            )));
        }
        let payload = bindings
            .state_decode(module, &record.payload.bytes)
            .ok_or_else(|| VmError::InvalidSnapshot("module state is undeclared".to_owned()))?
            .map_err(|error| {
                VmError::InvalidSnapshot(format!("module {name} state payload: {error}"))
            })?;
        states[module] = Some(ModuleState { slots, payload });
    }
    Ok(states)
}

fn require_environment(
    heap: &Heap,
    owner: ObjectId,
    env: ObjectId,
    role: EnvRole,
) -> Result<(), VmError> {
    match heap.is_environment(env) {
        Some(true) => Ok(()),
        Some(false) => Err(VmError::InvalidSnapshot(match role {
            EnvRole::Closure => {
                format!("function object {owner} references non-environment {env}")
            }
            EnvRole::Parent => {
                format!("environment object {owner} references non-environment parent {env}")
            }
        })),
        None => Err(VmError::InvalidSnapshot(match role {
            EnvRole::Closure => {
                format!("function object {owner} references missing environment {env}")
            }
            EnvRole::Parent => {
                format!("environment object {owner} references missing parent {env}")
            }
        })),
    }
}

fn require_value_objects<'a>(
    heap: &Heap,
    values: impl IntoIterator<Item = &'a Value>,
) -> Result<(), VmError> {
    for value in values {
        match value.view() {
            ValueRef::Object(id) if !heap.contains(id) => {
                return Err(VmError::InvalidSnapshot(format!(
                    "value references missing object {id}"
                )));
            }
            ValueRef::Symbol(id) if !heap.symbols.contains_key(&id) => {
                return Err(VmError::InvalidSnapshot(format!(
                    "value references missing symbol {id}"
                )));
            }
            _ => {}
        }
    }
    Ok(())
}

fn load_continuations(
    build: &Build,
    heap: &Heap,
    records: &[ContinuationRecord],
) -> Result<BTreeMap<ContinuationId, Continuation>, VmError> {
    let mut indexed = BTreeMap::new();
    for record in records {
        if indexed.insert(record.id, record.clone()).is_some() {
            return Err(VmError::InvalidSnapshot(format!(
                "duplicate continuation id {}",
                record.id
            )));
        }
        validate_continuation(build, heap, record)?;
    }
    for (id, object) in heap.iter() {
        let Object::Promise(promise) = object else {
            continue;
        };
        for job in &promise.reactions {
            let Some(continuation) = job.await_id() else {
                continue;
            };
            let Some(record) = indexed.get(&continuation) else {
                return Err(VmError::InvalidSnapshot(format!(
                    "Promise {id} references missing continuation {continuation}"
                )));
            };
            if record.waiting_on != id {
                return Err(VmError::InvalidSnapshot(format!(
                    "Promise {id} reaction {continuation} waits on {}",
                    record.waiting_on
                )));
            }
        }
    }
    Ok(indexed
        .into_values()
        .map(|record| {
            (
                record.id,
                Continuation {
                    async_promise: record.async_promise,
                    func_id: record.func_id,
                    resume_id: record.resume_id,
                    waiting_on: Some(record.waiting_on),
                    env: record.env,
                    values: record.values,
                },
            )
        })
        .collect())
}

fn validate_continuation(
    build: &Build,
    heap: &Heap,
    record: &ContinuationRecord,
) -> Result<(), VmError> {
    let function = build.function(record.func_id).ok_or_else(|| {
        VmError::InvalidSnapshot(format!(
            "continuation {} references missing function {}",
            record.id, record.func_id
        ))
    })?;
    if build.is_initializer(record.func_id) {
        return Err(VmError::InvalidSnapshot(format!(
            "continuation {} references the script function",
            record.id
        )));
    }
    let resume = function
        .resumes
        .get(record.resume_id as usize)
        .ok_or_else(|| {
            VmError::InvalidSnapshot(format!(
                "continuation {} references missing resume {}",
                record.id, record.resume_id
            ))
        })?;
    if resume.saved_slots.len() != record.values.len() {
        return Err(VmError::InvalidSnapshot(format!(
            "continuation {} has {} values, expected {}",
            record.id,
            record.values.len(),
            resume.saved_slots.len()
        )));
    }
    if function.needs_environment() {
        let Some(env) = record.env else {
            return Err(VmError::InvalidSnapshot(format!(
                "continuation {} has no invocation environment",
                record.id
            )));
        };
        if !matches!(heap.is_environment(env), Some(true)) {
            let missing = heap.is_environment(env).is_none();
            return Err(VmError::InvalidSnapshot(if missing {
                format!(
                    "continuation {} references missing environment {env}",
                    record.id
                )
            } else {
                format!(
                    "continuation {} environment {env} is not an environment",
                    record.id
                )
            }));
        }
    } else if let Some(env) = record.env {
        return Err(VmError::InvalidSnapshot(format!(
            "continuation {} does not use an invocation environment, found {env}",
            record.id
        )));
    }
    match heap.get(record.async_promise) {
        Ok(Object::Promise(promise)) if matches!(promise.state, PromiseState::Pending) => {}
        _ => {
            return Err(VmError::InvalidSnapshot(format!(
                "continuation {} async result {} is not a pending Promise",
                record.id, record.async_promise
            )));
        }
    }
    let waiting = match heap.get(record.waiting_on) {
        Ok(Object::Promise(promise)) => promise,
        _ => {
            return Err(VmError::InvalidSnapshot(format!(
                "continuation {} waits on non-Promise object {}",
                record.id, record.waiting_on
            )));
        }
    };
    if !matches!(waiting.state, PromiseState::Pending) {
        return Err(VmError::InvalidSnapshot(format!(
            "continuation {} waits on a settled Promise",
            record.id
        )));
    }
    let occurrences = waiting
        .reactions
        .iter()
        .filter(|job| job.await_id() == Some(record.id))
        .count();
    if occurrences != 1 {
        return Err(VmError::InvalidSnapshot(format!(
            "continuation {} must appear exactly once in Promise {} reactions, found {occurrences}",
            record.id, record.waiting_on
        )));
    }
    Ok(())
}

fn validate_rejection_notifications(heap: &Heap, queue: &[ObjectId]) -> Result<(), VmError> {
    use RejectionStatus::{Handled, PendingHandled, PendingReport, Reported, Unobserved};

    let mut queued = BTreeSet::new();
    for &id in queue {
        if !queued.insert(id) {
            return Err(VmError::InvalidSnapshot(format!(
                "duplicate pending rejection notification for Promise {id}"
            )));
        }
        if !matches!(heap.get(id), Ok(Object::Promise(promise))
            if matches!(promise.state, PromiseState::Rejected(_))
                && matches!(promise.rejection_status, PendingReport | PendingHandled))
        {
            return Err(VmError::InvalidSnapshot(format!(
                "pending rejection notification {id} must reference a rejected Promise with a pending notification"
            )));
        }
    }

    for (id, object) in heap.iter() {
        let Object::Promise(promise) = object else {
            continue;
        };
        let valid = match promise.rejection_status {
            PendingReport | PendingHandled => queued.contains(&id),
            Reported => matches!(promise.state, PromiseState::Rejected(_)),
            Unobserved => match promise.state {
                PromiseState::Pending => promise.reactions.is_empty(),
                PromiseState::Fulfilled(_) => true,
                PromiseState::Rejected(_) => false,
            },
            Handled => true,
        };
        if !valid {
            return Err(VmError::InvalidSnapshot(format!(
                "Promise {id} has inconsistent rejection status {:?}",
                promise.rejection_status
            )));
        }
    }
    Ok(())
}

fn validate_jobs(heap: &Heap) -> Result<(), VmError> {
    let mut adopts: BTreeMap<ObjectId, usize> = BTreeMap::new();
    for (id, object) in heap.iter() {
        match object {
            Object::Promise(promise) => {
                for job in &promise.reactions {
                    match job {
                        ReactionJob::Await(_) => {}
                        ReactionJob::Script {
                            resolve, reject, ..
                        } => {
                            for callback in [resolve, reject] {
                                if !matches!(callback.view(), ValueRef::Object(id) if heap.get(id).is_ok_and(crate::builtin::is_callable_object))
                                {
                                    return Err(VmError::InvalidSnapshot(format!(
                                        "Promise {id} reaction capability is not callable"
                                    )));
                                }
                            }
                        }
                        ReactionJob::Adopt { result } => {
                            require_promise(heap, *result, &format!("Promise {id} reaction"))?;
                        }
                    }
                    if let ReactionJob::Adopt { result } = job {
                        *adopts.entry(*result).or_default() += 1;
                    }
                }
            }
            Object::Resolution(resolution) => {
                require_promise(heap, resolution.promise, &format!("Resolution {id}"))?;
                if !matches!(heap.get(resolution.pair), Ok(Object::Resolution(_))) {
                    return Err(VmError::InvalidSnapshot(format!(
                        "Resolution {id} pair {} is not a resolution function",
                        resolution.pair
                    )));
                }
            }
            _ => {}
        }
    }
    for (id, object) in heap.iter() {
        let Object::Promise(promise) = object else {
            continue;
        };
        if promise.locked && matches!(promise.state, PromiseState::Pending) {
            let count = adopts.get(&id).copied().unwrap_or(0);
            if count != 1 {
                return Err(VmError::InvalidSnapshot(format!(
                    "locked Promise {id} must have exactly one Adopt reaction, found {count}"
                )));
            }
        }
        if let Some(source) = promise.adopting {
            require_promise(heap, source, &format!("Promise {id} adopting"))?;
            let linked = heap
                .promise(source)?
                .reactions
                .iter()
                .any(|job| matches!(job, ReactionJob::Adopt { result } if *result == id));
            if !linked && matches!(heap.promise(source)?.state, PromiseState::Pending) {
                return Err(VmError::InvalidSnapshot(format!(
                    "Promise {id} adopts pending Promise {source} without an Adopt reaction"
                )));
            }
        }
    }
    Ok(())
}

fn validate_combines(heap: &Heap) -> Result<(), VmError> {
    for (id, state) in &heap.combines {
        if !matches!(heap.get(*id), Ok(Object::Plain(_))) {
            return Err(VmError::InvalidSnapshot(format!(
                "combine key {id} is not a plain object"
            )));
        }
        if state.kind == CombineKind::Race {
            return Err(VmError::InvalidSnapshot(format!(
                "combine {id} has invalid Race kind"
            )));
        }
        if !matches!(state.capability.view(), ValueRef::Object(target) if heap.get(target).is_ok_and(is_callable_object))
        {
            return Err(VmError::InvalidSnapshot(format!(
                "combine {id} capability is not callable"
            )));
        }
        let uncalled = state.called.iter().filter(|&&called| !called).count();
        // An abrupt iteration leaves the sentinel in place after suspension.
        if state.results.len() != state.called.len()
            || !matches!(state.remaining.checked_sub(uncalled), Some(0 | 1))
        {
            return Err(VmError::InvalidSnapshot(format!(
                "combine {id} has inconsistent results, called flags, or remaining count"
            )));
        }
    }
    for (id, object) in heap.iter() {
        let Object::PromiseCallback {
            callback:
                PromiseCallback::CombineElement {
                    state,
                    index,
                    rejected,
                },
            ..
        } = object
        else {
            continue;
        };
        let combine = heap.combines.get(state).ok_or_else(|| {
            VmError::InvalidSnapshot(format!(
                "combine element callback {id} has missing state {state}"
            ))
        })?;
        if *index >= combine.called.len() {
            return Err(VmError::InvalidSnapshot(format!(
                "combine element callback {id} index {index} is out of bounds for state {state}"
            )));
        }
        let direction_ok = match combine.kind {
            CombineKind::All => !*rejected,
            CombineKind::AllSettled => true,
            CombineKind::Any => *rejected,
            CombineKind::Race => false,
        };
        if !direction_ok {
            return Err(VmError::InvalidSnapshot(format!(
                "combine element callback {id} has invalid direction for state {state}"
            )));
        }
    }
    Ok(())
}

fn require_promise(heap: &Heap, id: ObjectId, what: &str) -> Result<(), VmError> {
    if heap.promise(id).is_err() {
        return Err(VmError::InvalidSnapshot(format!(
            "{what} references non-Promise object {id}"
        )));
    }
    Ok(())
}

fn load_operations(
    bindings: &Bindings,
    heap: &Heap,
    records: Vec<PendingOperationRecord>,
) -> Result<BTreeMap<u64, PendingOperation>, VmError> {
    let mut operations = BTreeMap::new();
    let mut operation_promises = BTreeSet::new();
    for record in records {
        if record.id == 0 || record.id == u64::MAX || operations.contains_key(&record.id) {
            return Err(VmError::InvalidSnapshot(format!(
                "duplicate or invalid host operation id {}",
                record.id
            )));
        }
        let (declaration, recovery, rejection) = bindings
            .operation(&record.name, record.schema)
            .ok_or_else(|| {
                VmError::InvalidSnapshot(format!(
                    "operation {} schema {} is not declared",
                    record.name, record.schema
                ))
            })?;
        if recovery != record.recovery {
            return Err(VmError::InvalidSnapshot(format!(
                "operation {} schema {} recovery mismatch",
                record.name, record.schema
            )));
        }
        bindings
            .decode_operation(&record.name, record.schema, &record.state)
            .map_err(|error| {
                VmError::InvalidSnapshot(format!(
                    "operation {} schema {} state: {error}",
                    record.name, record.schema
                ))
            })?;
        if let DeliveryTarget::Promise(promise) = record.target {
            let pending = matches!(heap.get(promise), Ok(Object::Promise(p)) if matches!(p.state, PromiseState::Pending));
            if !pending {
                return Err(VmError::InvalidSnapshot(format!(
                    "operation {:?} references non-pending Promise {promise}",
                    record.name
                )));
            }
            if !operation_promises.insert(promise) {
                return Err(VmError::InvalidSnapshot(format!(
                    "Promise {promise} is associated with more than one host operation"
                )));
            }
        }
        operations.insert(
            record.id,
            PendingOperation {
                name: record.name,
                schema: record.schema,
                recovery,
                rejection,
                state: record.state,
                target: record.target,
                declaration,
                live: None,
            },
        );
    }
    Ok(operations)
}

fn load_host_tasks(
    bindings: &Bindings,
    heap: &Heap,
    instances: &BTreeMap<ObjectId, HostInstance>,
    operations: &BTreeMap<u64, PendingOperation>,
    records: Vec<HostTaskRecord>,
    next_id: u64,
) -> Result<BTreeMap<u64, HostTask>, VmError> {
    if !(1..=HOST_TASK_ID_LIMIT).contains(&next_id) {
        return Err(VmError::InvalidSnapshot(format!(
            "invalid next host task id {next_id}"
        )));
    }
    let mut tasks = BTreeMap::new();
    for record in records {
        let id = record.id;
        let invalid =
            |message: &str| VmError::InvalidSnapshot(format!("host task {id}: {message}"));
        if !(1..next_id).contains(&id) || tasks.contains_key(&id) {
            return Err(invalid("duplicate id or id outside its high-water mark"));
        }
        match operations.get(&record.waiting_on) {
            Some(operation) if operation.target == DeliveryTarget::Task(id) => {}
            _ => return Err(invalid("waiting operation does not target this task")),
        }
        match record.owner {
            NativeTarget::Export(export) if matches!(bindings.export_decl(export), Some(ExportDecl::Function(f)) if !f.constructable) =>
                {}
            NativeTarget::Method(method) if bindings.method(method).is_some() => {
                let valid_receiver = match record.receiver.view() {
                    ValueRef::Object(object) => instances.get(&object).is_some_and(|instance| {
                        bindings.class_is_a(instance.class, method.class as usize)
                    }),
                    _ => false,
                };
                if !valid_receiver {
                    return Err(invalid("receiver is not an instance of the method's class"));
                }
            }
            _ => return Err(invalid("invalid CPS owner")),
        }
        let stage = bindings
            .cps(record.owner)
            .and_then(|stages| stages.get(record.stage as usize))
            .filter(|stage| matches!(stage.kind, CpsBoundary::External))
            .ok_or_else(|| invalid("stage is not an external CPS boundary"))?;
        if record.saved.len() != stage.slots.len() {
            return Err(invalid("saved value count does not match CPS stage"));
        }
        require_value_objects(heap, std::iter::once(&record.receiver).chain(&record.saved))?;
        tasks.insert(
            id,
            HostTask {
                owner: record.owner,
                stage: record.stage,
                receiver: record.receiver,
                saved: record.saved,
                state: HostTaskState::Waiting(record.waiting_on),
            },
        );
    }
    for (&id, operation) in operations {
        if let DeliveryTarget::Task(task) = operation.target {
            let state = tasks.get(&task).map(|task| task.state);
            if state != Some(HostTaskState::Waiting(id)) {
                return Err(VmError::InvalidSnapshot(format!(
                    "operation {id} targets host task {task}, which does not wait on it"
                )));
            }
        }
    }
    Ok(tasks)
}
