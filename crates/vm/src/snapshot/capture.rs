use std::collections::{BTreeMap, BTreeSet};

use crate::binding::{Bindings, CpsBoundary, ExportDecl, NativeTarget, Recovery};
use crate::builtin::date_calendar::TIME_CLIP_BOUND;
use crate::builtin::is_callable_object;
use crate::builtin::roots::BuiltinRoots;
use crate::bytecode::{BYTECODE_VERSION, Build};
use crate::error::VmError;
use crate::heap::{
    ASYNC_DISPOSE_DESCRIPTION, ASYNC_DISPOSE_SYMBOL, ASYNC_ITERATOR_DESCRIPTION,
    ASYNC_ITERATOR_SYMBOL, AsyncCompletion, Attributes, CombineKind, DISPOSE_DESCRIPTION,
    DISPOSE_SYMBOL, EnvRole, HAS_INSTANCE_DESCRIPTION, HAS_INSTANCE_SYMBOL, Heap,
    IS_CONCAT_SPREADABLE_DESCRIPTION, IS_CONCAT_SPREADABLE_SYMBOL, ITERATOR_DESCRIPTION,
    ITERATOR_SYMBOL, IntegrityLevel, MATCH_ALL_DESCRIPTION, MATCH_ALL_SYMBOL, MATCH_DESCRIPTION,
    MATCH_SYMBOL, NativeObject, Object, OwnProperty, OwnedEdge, PrivateElement, PrivateMemberKind,
    PromiseCallback, PromiseState, Property, REPLACE_DESCRIPTION, REPLACE_SYMBOL, ReactionJob,
    RejectionStatus, SEARCH_DESCRIPTION, SEARCH_SYMBOL, SPECIES_DESCRIPTION, SPECIES_SYMBOL,
    SPLIT_DESCRIPTION, SPLIT_SYMBOL, TO_PRIMITIVE_DESCRIPTION, TO_PRIMITIVE_SYMBOL,
    TO_STRING_TAG_DESCRIPTION, TO_STRING_TAG_SYMBOL, WeakKey,
};
use crate::heap::{CollectionIteration, CollectionKind, GeneratorStatus, push_value};
use crate::heap::{DisposableResource, DisposeHint};
use crate::host::Host;
use crate::regexp::compile_pattern;
use crate::runtime::{
    Continuation, ContinuationTarget, HOST_TASK_ID_LIMIT, HostInstance, HostSlot, HostTask,
    HostTaskState, ModuleState, PendingOperation, Settlement,
};
use crate::value::{ContinuationId, ObjectId, PropertyKeyRef, Value, ValueRef};
use crate::vm::{VerifiedBuild, Vm};

use super::record::{valid_string_position, *};

fn valid_date_time(object: &Object) -> bool {
    !matches!(object, Object::Date { time: Some(time), .. }
        if !(-TIME_CLIP_BOUND..=TIME_CLIP_BOUND).contains(time))
}

/// A buffer's length never exceeds its maximum; a view keeps whole elements
/// and a representable range. A buffer that is currently detached still bounds
/// its views by the maximum it was created with, so a detached view record is
/// not rejected for looking out of range. The reason is a located diagnostic.
fn binary_object_error(heap: &Heap, id: ObjectId) -> Option<String> {
    let capacity = |buffer: ObjectId| -> Result<usize, String> {
        let buffer = heap
            .buffer(buffer)
            .ok()
            .flatten()
            .ok_or_else(|| format!("binary object {id} references an invalid buffer"))?;
        Ok(match (buffer.max_byte_length, buffer.byte_length()) {
            (Some(max), _) => max,
            (None, Some(length)) => length,
            // A detached fixed buffer no longer records its original length.
            (None, None) => usize::MAX,
        })
    };
    let view_range =
        |buffer: ObjectId, offset: usize, length: Option<usize>| -> Result<(), String> {
            match length {
                Some(length) => {
                    let capacity = capacity(buffer)?;
                    if offset.checked_add(length).is_none_or(|end| end > capacity) {
                        return Err(format!("binary object {id} exceeds its buffer"));
                    }
                }
                None => {
                    let buffer = heap.buffer(buffer).ok().flatten().ok_or_else(|| {
                        format!("binary object {id} references an invalid buffer")
                    })?;
                    if !buffer.is_resizable() {
                        return Err(format!(
                            "binary object {id} tracks the length of a fixed-length buffer"
                        ));
                    }
                }
            }
            Ok(())
        };
    match heap.get(id) {
        Err(_) => Some(format!("binary object {id} is missing")),
        Ok(Object::ArrayBuffer(buffer)) => buffer
            .byte_length()
            .zip(buffer.max_byte_length)
            .filter(|(length, max)| length > max)
            .map(|_| format!("ArrayBuffer {id} exceeds its maximum byte length")),
        Ok(Object::TypedArray(view)) => {
            let width = view.kind.bytes();
            if view.byte_offset % width != 0 {
                Some(format!("TypedArray {id} has an unaligned byte offset"))
            } else if view.byte_length.is_some_and(|length| length % width != 0) {
                Some(format!("TypedArray {id} has a partial element length"))
            } else {
                view_range(view.buffer, view.byte_offset, view.byte_length).err()
            }
        }
        Ok(Object::DataView(view)) => {
            view_range(view.buffer, view.byte_offset, view.byte_length).err()
        }
        Ok(_) => None,
    }
}

impl Vm {
    /// Captures an idle VM without releasing any live registrations.
    /// The caller must hold external logical time and delivery at the same cut;
    /// no event may have been taken out of its source but not yet delivered.
    pub fn snapshot(&self, roots: &[Value]) -> Result<Snapshot, VmError> {
        self.capture_image(roots, |_| {})
    }

    /// Capture through the same path as `snapshot`, timing its phases.
    #[doc(hidden)]
    pub fn snapshot_profiled(
        &self,
        roots: &[Value],
    ) -> Result<(Snapshot, crate::perf::Capture), VmError> {
        use std::time::Instant;

        let start = Instant::now();
        let mut last = start;
        let mut profile = crate::perf::Capture::default();
        let snapshot = self.capture_image(roots, |field| {
            let now = Instant::now();
            *field(&mut profile) += now.duration_since(last);
            last = now;
        })?;
        let end = Instant::now();
        profile.records += end.duration_since(last);
        profile.total = end.duration_since(start);
        Ok((snapshot, profile))
    }

    fn capture_image(
        &self,
        roots: &[Value],
        mut checkpoint: impl FnMut(fn(&mut crate::perf::Capture) -> &mut std::time::Duration),
    ) -> Result<Snapshot, VmError> {
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
        checkpoint(|profile| &mut profile.trace);

        let mut snapshot = Snapshot::new(self.build.build_id.clone());
        snapshot.binding_fingerprint = self.bindings.fingerprint().to_owned();
        snapshot.globals = self.globals.clone();
        snapshot.roots = self.builtins.to_map();
        if self.templates.len() != self.build.templates.len() {
            return Err(VmError::InvalidState(
                "template object table does not match the build".to_owned(),
            ));
        }
        snapshot.templates = self.templates.clone();
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
        snapshot.module_envs = self.module_envs.clone();
        snapshot.module_namespaces = self.module_namespaces.clone();
        for id in &live.objects {
            let object = match self.heap.get(*id)? {
                object if !valid_date_time(object) => {
                    return Err(VmError::InvalidState(format!(
                        "Date {id} has a time outside the TimeClip range"
                    )));
                }
                object
                    if matches!(
                        object,
                        Object::ArrayBuffer(_) | Object::TypedArray(_) | Object::DataView(_)
                    ) && binary_object_error(&self.heap, *id).is_some() =>
                {
                    return Err(VmError::InvalidState(
                        binary_object_error(&self.heap, *id).expect("checked above"),
                    ));
                }
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
                Object::Environment(env) if !env.uninitialized.is_empty() => {
                    return Err(VmError::InvalidState(format!(
                        "environment {id} has uninitialized module bindings"
                    )));
                }
                object => object.clone(),
            };
            snapshot.objects.push(ObjectRecord { id: *id, object });
            if let Some(generator) = self.heap.generator(*id) {
                snapshot.generators.insert(*id, generator.clone());
            }
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
        checkpoint(|profile| &mut profile.records);
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
        checkpoint(|profile| &mut profile.host_records);
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
                target: match continuation.target {
                    ContinuationTarget::Promise(promise) => {
                        ContinuationTargetRecord::Promise(promise)
                    }
                    ContinuationTarget::Generator(generator) => {
                        ContinuationTargetRecord::Generator(generator)
                    }
                },
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
        checkpoint(|profile| &mut profile.records);
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
        checkpoint(|profile| &mut profile.host_records);
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

    /// Restore a trusted, unmodified snapshot produced by [`Vm::snapshot`].
    ///
    /// The caller must ensure provenance and integrity. This checks format,
    /// build and host compatibility, but does not audit the snapshot graph.
    /// Use [`Vm::restore_validated`] to diagnose invalid snapshot state.
    pub fn restore(snapshot: Snapshot, build: Build, host: &mut dyn Host) -> Result<Self, VmError> {
        check_snapshot_header(&snapshot)?;
        Self::restore_image::<false>(snapshot, VerifiedBuild::new(build)?, host, |_| {})
    }

    /// Restore a trusted snapshot with an already validated program.
    /// The same snapshot requirements as [`Vm::restore`] apply.
    pub fn restore_verified(
        snapshot: Snapshot,
        build: VerifiedBuild,
        host: &mut dyn Host,
    ) -> Result<Self, VmError> {
        check_snapshot_header(&snapshot)?;
        Self::restore_image::<false>(snapshot, build, host, |_| {})
    }

    /// Restore with full snapshot validation for development and diagnostics.
    /// This is not a sandbox or a resource limit for untrusted input.
    pub fn restore_validated(
        snapshot: Snapshot,
        build: Build,
        host: &mut dyn Host,
    ) -> Result<Self, VmError> {
        check_snapshot_header(&snapshot)?;
        Self::restore_image::<true>(snapshot, VerifiedBuild::new(build)?, host, |_| {})
    }

    /// Restore with full snapshot validation and time its phases.
    #[doc(hidden)]
    pub fn restore_verified_profiled(
        snapshot: Snapshot,
        build: VerifiedBuild,
        host: &mut dyn Host,
    ) -> Result<(Self, crate::perf::Restore), VmError> {
        use std::time::Instant;

        let start = Instant::now();
        let mut last = start;
        let mut profile = crate::perf::Restore::default();
        check_snapshot_header(&snapshot)?;
        let vm = Self::restore_image::<true>(snapshot, build, host, |field| {
            let now = Instant::now();
            *field(&mut profile) = now.duration_since(last);
            last = now;
        })?;
        let end = Instant::now();
        profile.finalize = end.duration_since(last);
        profile.total = end.duration_since(start);
        Ok((vm, profile))
    }

    fn restore_image<const AUDIT: bool>(
        mut snapshot: Snapshot,
        build: VerifiedBuild,
        host: &mut dyn Host,
        mut checkpoint: impl FnMut(fn(&mut crate::perf::Restore) -> &mut std::time::Duration),
    ) -> Result<Self, VmError> {
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
        checkpoint(|profile| &mut profile.compatibility);

        let mut heap = Heap::new();
        for ObjectRecord { id, mut object } in std::mem::take(&mut snapshot.objects) {
            if !valid_date_time(&object) {
                return Err(VmError::InvalidSnapshot(format!(
                    "Date {id} has a time outside the TimeClip range"
                )));
            }
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
        checkpoint(|profile| &mut profile.heap_insert);
        if AUDIT {
            validate_rejection_notifications(&heap, &snapshot.pending_rejection_notifications)?;
        }
        heap.combines = snapshot.combines.clone();
        heap.generators = load_generators(&build, &heap, snapshot.generators.clone())?;
        if AUDIT {
            validate_symbol_tables(&snapshot)?;
        }
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
        checkpoint(|profile| &mut profile.heap_side_tables);
        validate_native_targets(&bindings, &heap)?;
        checkpoint(|profile| &mut profile.native_validation);
        let host_classes = load_host_classes(&bindings, &heap, &snapshot.class_prototypes)?;
        let host_instances = load_host_instances(&bindings, &heap, &snapshot.host_instances)?;
        let module_states = load_module_states(&bindings, &heap, &snapshot.module_states)?;
        let continuations = load_continuations(&build, &heap, &snapshot.continuations)?;
        if AUDIT {
            for continuation in continuations.values() {
                require_value_objects(&heap, continuation.values.iter())?;
            }
        }
        let builtins = BuiltinRoots::load(&snapshot.roots, &heap)?;
        let templates = load_templates(&build, &heap, std::mem::take(&mut snapshot.templates))?;
        if AUDIT {
            validate_source_modules(
                &build,
                &heap,
                &builtins,
                &snapshot.module_envs,
                &snapshot.module_namespaces,
                &snapshot.imports,
            )?;
        }
        let operations = load_operations(&bindings, &heap, snapshot.operations)?;
        let host_tasks = load_host_tasks(
            &bindings,
            &heap,
            &host_instances,
            &operations,
            snapshot.host_tasks,
            snapshot.next_host_task_id,
        )?;
        checkpoint(|profile| &mut profile.state_load);
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
                templates,
                module_envs: snapshot.module_envs,
                module_namespaces: snapshot.module_namespaces,
            },
        );
        // Recovery can append new notifications; the captured ones must stay first.
        vm.pending_rejection_notifications = snapshot.pending_rejection_notifications.into();
        vm.restore_imports(&bindings, snapshot.imports, resolved_imports)?;
        checkpoint(|profile| &mut profile.vm_assembly);
        if AUDIT {
            validate_native_import_objects(&vm.heap, &vm.imports)?;
            check_heap_edges(&vm.build, &vm.heap)?;
            validate_jobs(&vm.heap)?;
            validate_combines(&vm.heap)?;
            checkpoint(|profile| &mut profile.graph_validation);
        }
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

fn check_snapshot_header(snapshot: &Snapshot) -> Result<(), VmError> {
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
    if snapshot.random_state == 0 {
        return Err(VmError::InvalidSnapshot(
            "invalid Math.random state".to_owned(),
        ));
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
            ASYNC_ITERATOR_SYMBOL => Some(ASYNC_ITERATOR_DESCRIPTION),
            TO_STRING_TAG_SYMBOL => Some(TO_STRING_TAG_DESCRIPTION),
            DISPOSE_SYMBOL => Some(DISPOSE_DESCRIPTION),
            ASYNC_DISPOSE_SYMBOL => Some(ASYNC_DISPOSE_DESCRIPTION),
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
        (ASYNC_ITERATOR_SYMBOL, ASYNC_ITERATOR_DESCRIPTION),
        (TO_STRING_TAG_SYMBOL, TO_STRING_TAG_DESCRIPTION),
        (DISPOSE_SYMBOL, DISPOSE_DESCRIPTION),
        (ASYNC_DISPOSE_SYMBOL, ASYNC_DISPOSE_DESCRIPTION),
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
            ASYNC_ITERATOR_SYMBOL => Some(ASYNC_ITERATOR_DESCRIPTION),
            TO_STRING_TAG_SYMBOL => Some(TO_STRING_TAG_DESCRIPTION),
            DISPOSE_SYMBOL => Some(DISPOSE_DESCRIPTION),
            ASYNC_DISPOSE_SYMBOL => Some(ASYNC_DISPOSE_DESCRIPTION),
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

fn validate_source_modules(
    build: &Build,
    heap: &Heap,
    builtins: &BuiltinRoots,
    envs: &[Option<ObjectId>],
    namespaces: &[ObjectId],
    host_namespaces: &[ObjectId],
) -> Result<(), VmError> {
    if envs.len() != build.modules.len() || namespaces.len() != build.modules.len() {
        return Err(VmError::InvalidSnapshot(
            "source module root count mismatch".to_owned(),
        ));
    }
    if host_namespaces.iter().copied().chain(builtins.ids()).any(
        |id| matches!(heap.get(id), Ok(Object::Plain(object)) if object.module_namespace.is_some()),
    ) {
        return Err(VmError::InvalidSnapshot(
            "a host or builtin root aliases a source module namespace".to_owned(),
        ));
    }

    // Identity tags also distinguish modules with identical (or empty) shape.
    let mut sentinel = None;
    for (id, object) in heap.iter() {
        match object {
            Object::Environment(env) => {
                if !env.uninitialized.is_empty() {
                    return Err(VmError::InvalidSnapshot(format!(
                        "environment {id} has uninitialized module bindings"
                    )));
                }
                if let Some(module) = env.module
                    && envs.get(module as usize) != Some(&Some(id))
                {
                    return Err(VmError::InvalidSnapshot(format!(
                        "environment {id} is not the root for source module {module}"
                    )));
                }
                if env.module_getter_sentinel
                    && (env.module.is_some()
                        || env.parent.is_some()
                        || !env.bindings.is_empty()
                        || sentinel.replace(id).is_some())
                {
                    return Err(VmError::InvalidSnapshot(format!(
                        "invalid module getter sentinel environment {id}"
                    )));
                }
            }
            Object::Plain(object) => {
                if let Some(module) = object.module_namespace
                    && namespaces.get(module as usize) != Some(&id)
                {
                    return Err(VmError::InvalidSnapshot(format!(
                        "namespace {id} is not the root for source module {module}"
                    )));
                }
            }
            _ => {}
        }
    }

    let mut getters = BTreeMap::new();
    let mut uses_sentinel = false;
    let mut validate_getter = |object: &Object,
                               name: &str,
                               export: &crate::bytecode::ModuleExport,
                               attributes: crate::heap::Attributes,
                               owner: &str| {
        let invalid = |detail| VmError::InvalidSnapshot(format!("{owner} {name:?}: {detail}"));
        let Some(OwnProperty::Stored(
            Property::Accessor {
                get: Some(id),
                set: None,
            },
            actual_attributes,
        )) = object.own_property(PropertyKeyRef::String(name))
        else {
            return Err(invalid("missing getter-only own property"));
        };
        if actual_attributes != attributes {
            return Err(invalid("invalid attributes"));
        }
        let getter = heap
            .function(*id)
            .filter(|getter| {
                getter.func_id == export.getter && getter.home.is_none() && getter.class.is_none()
            })
            .ok_or_else(|| invalid("wrong getter function"))?;
        let capture = match crate::vm::module_getter_capture(export.target) {
            Some(module) => envs[module as usize],
            None => {
                uses_sentinel = true;
                sentinel
            }
        };
        if capture != Some(getter.env) {
            return Err(invalid("wrong getter environment"));
        }
        if *getters.entry(export.target).or_insert(*id) != *id {
            return Err(invalid("aliases do not share a getter"));
        }
        Ok(())
    };

    for (index, module) in build.modules.iter().enumerate() {
        let module_index = index as u32;
        let env = envs[index];
        if env.is_some() != build.module_needs_environment(module_index) {
            return Err(VmError::InvalidSnapshot(format!(
                "wrong environment presence for source module {:?}",
                module.id
            )));
        }
        if let Some(id) = env {
            let invalid = || {
                VmError::InvalidSnapshot(format!(
                    "invalid environment {id} for source module {:?}",
                    module.id
                ))
            };
            let Ok(Object::Environment(env)) = heap.get(id) else {
                return Err(invalid());
            };
            if env.module != Some(module_index) || env.parent.is_some() {
                return Err(invalid());
            }
            let declared: BTreeSet<_> = build.module_bindings(module_index).collect();
            if declared
                .iter()
                .any(|binding| !env.bindings.contains_key(binding))
            {
                return Err(invalid());
            }
            // Evaluation can add captured block bindings to the same environment.
            // Conditional blocks need not have run, unlike module declarations.
            let evaluation = build
                .function(module.evaluation)
                .expect("validated evaluation");
            let block_bindings: BTreeSet<_> = evaluation
                .code
                .iter()
                .filter_map(|instr| match instr {
                    crate::bytecode::Instr::DeclareBinding { binding } => Some(*binding),
                    _ => None,
                })
                .collect();
            if env
                .bindings
                .keys()
                .any(|binding| !declared.contains(binding) && !block_bindings.contains(binding))
            {
                return Err(invalid());
            }
        }

        let id = namespaces[index];
        let owner = format!("namespace for source module {:?}", module.id);
        let invalid = || VmError::InvalidSnapshot(format!("invalid {owner}: object {id}"));
        let Ok(object @ Object::Plain(plain)) = heap.get(id) else {
            return Err(invalid());
        };
        if plain.module_namespace != Some(module_index)
            || plain.is_error
            || object.prototype().is_some()
            || object.is_extensible()
            || plain.props.iter().count() != module.exports.len()
            || plain.symbol_props.iter().next().is_some()
        {
            return Err(invalid());
        }
        for (name, export) in &module.exports {
            validate_getter(
                object,
                name,
                export,
                crate::vm::MODULE_EXPORT_ATTRIBUTES,
                &owner,
            )?;
        }
    }

    if let Some(preload) = build.preload {
        let global = heap.get(builtins.get("globalThis"))?;
        for (name, export) in &build.modules[preload as usize].exports {
            validate_getter(
                global,
                name,
                export,
                crate::vm::PRELOAD_EXPORT_ATTRIBUTES,
                "preload global",
            )?;
        }
    }
    if sentinel.is_some() != uses_sentinel {
        return Err(VmError::InvalidSnapshot(
            "unexpected module getter sentinel environment".to_owned(),
        ));
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
            Err(_)
                | Ok(Object::Environment(_)
                    | Object::ForInIterator { .. }
                    | Object::DisposeState(_))
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
            if matches!(value.view(), ValueRef::Object(id) if matches!(heap.get(id), Ok(Object::Environment(_) | Object::ForInIterator { .. } | Object::DisposeState(_))))
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

/// A restored generator must name a real generator function, a resume point its
/// saved value count matches, and a frame state that could exist at a snapshot
/// boundary. Executing generators have a native frame and cannot be restored.
fn load_generators(
    build: &Build,
    heap: &Heap,
    records: BTreeMap<ObjectId, crate::heap::GeneratorObject>,
) -> Result<BTreeMap<ObjectId, crate::heap::GeneratorObject>, VmError> {
    for (id, generator) in &records {
        let invalid = |detail: &str| VmError::InvalidSnapshot(format!("generator {id} {detail}"));
        if !matches!(heap.get(*id), Ok(Object::Plain(_))) {
            return Err(invalid("is not an ordinary object"));
        }
        let function = build
            .function(generator.func_id)
            .filter(|function| function.is_generator)
            .ok_or_else(|| invalid("references a non-generator function"))?;
        let async_generator = function.is_async;
        match generator.status {
            GeneratorStatus::Executing => {
                return Err(invalid("was captured while executing"));
            }
            GeneratorStatus::Completed => {
                if generator.env.is_some() || !generator.values.is_empty() {
                    return Err(invalid("is completed but keeps a frame"));
                }
                if !generator.requests.is_empty() {
                    return Err(invalid("is completed with unsettled requests"));
                }
            }
            GeneratorStatus::Awaiting => {
                if !async_generator {
                    return Err(invalid("awaits inside a synchronous generator"));
                }
                if generator.env.is_some() || !generator.values.is_empty() {
                    return Err(invalid("is awaiting but keeps a frame"));
                }
                if generator.requests.is_empty() {
                    return Err(invalid("is awaiting without a request"));
                }
            }
            GeneratorStatus::Closing => {
                if !async_generator {
                    return Err(invalid("closes inside a synchronous generator"));
                }
                if generator.env.is_some() || !generator.values.is_empty() {
                    return Err(invalid("is closing but keeps a frame"));
                }
                match generator.requests.front() {
                    Some(head) if head.completion == AsyncCompletion::Return => {}
                    _ => return Err(invalid("is closing without a return request")),
                }
            }
            GeneratorStatus::Start => {
                if !generator.requests.is_empty() {
                    return Err(invalid("is unstarted with unsettled requests"));
                }
                let start = function
                    .generator_start
                    .as_ref()
                    .ok_or_else(|| invalid("has no start entry"))?;
                if start.saved_slots.len() != generator.values.len() {
                    return Err(invalid("has the wrong number of start values"));
                }
            }
            GeneratorStatus::Suspended(index) => {
                if !generator.requests.is_empty() {
                    return Err(invalid("is suspended with unsettled requests"));
                }
                let entry = function
                    .generator_resumes
                    .get(index as usize)
                    .ok_or_else(|| invalid("has an invalid resume index"))?;
                if entry.saved_slots.len() != generator.values.len() {
                    return Err(invalid("has the wrong number of saved values"));
                }
            }
        }
        if !matches!(
            generator.status,
            GeneratorStatus::Completed | GeneratorStatus::Awaiting | GeneratorStatus::Closing
        ) && generator.env.is_some() != function.needs_environment()
        {
            return Err(invalid("has the wrong environment presence"));
        }
        if let Some(env) = generator.env {
            require_environment(heap, *id, env, EnvRole::Closure)
                .map_err(|_| invalid("references an invalid environment"))?;
        }
        require_value_objects(heap, generator.values.iter())?;
        let mut promises = BTreeSet::new();
        for request in &generator.requests {
            if !matches!(
                heap.get(request.promise),
                Ok(Object::Promise(promise)) if matches!(promise.state, PromiseState::Pending)
            ) {
                return Err(invalid("has a request that is not a pending Promise"));
            }
            if !promises.insert(request.promise) {
                return Err(invalid("has two requests for one Promise"));
            }
            if let ValueRef::Object(target) = request.argument.view()
                && !heap.contains(target)
            {
                return Err(invalid("has a request argument that is not an object"));
            }
        }
    }
    Ok(records)
}

fn validate_weak_key(heap: &Heap, owner: &str, id: ObjectId, key: WeakKey) -> Result<(), VmError> {
    let valid = match key {
        WeakKey::Object(target) => !matches!(
            heap.get(target),
            Err(_)
                | Ok(Object::Environment(_)
                    | Object::ForInIterator { .. }
                    | Object::DisposeState(_))
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
        if let Some(error) = binary_object_error(heap, id) {
            return Err(VmError::InvalidSnapshot(error));
        }
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
                    Err(_)
                        | Ok(Object::Environment(_)
                            | Object::ForInIterator { .. }
                            | Object::DisposeState(_))
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
        if let Object::TextDecoder(decoder) = heap.get(id)? {
            let prefix_len = usize::from(decoder.prefix_len);
            // A saved prefix must be the beginning of one scalar, not a
            // complete or plainly invalid sequence.
            let prefix_valid = prefix_len <= decoder.prefix.len()
                && (prefix_len == 0
                    || std::str::from_utf8(&decoder.prefix[..prefix_len]).is_err_and(|error| {
                        error.valid_up_to() == 0 && error.error_len().is_none()
                    }));
            // Only an open stream can leave a prefix, and only a fatal error
            // in an open stream retains unprocessed bytes.
            let stream_valid = decoder.streaming || prefix_len == 0 && decoder.pending.is_empty();
            let pending_valid = decoder.pending.is_empty() || decoder.fatal && decoder.streaming;
            if !prefix_valid || !stream_valid || !pending_valid {
                return Err(VmError::InvalidSnapshot(format!(
                    "TextDecoder {id} has invalid stream state"
                )));
            }
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
        let object = heap.get(id)?;
        if let Object::ForInIterator {
            current,
            pending,
            visited,
            string_indices,
        } = object
            && (current.is_some_and(|source| {
                matches!(
                    heap.get(source),
                    Err(_)
                        | Ok(Object::Environment(_)
                            | Object::ForInIterator { .. }
                            | Object::DisposeState(_))
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
        heap.write_edges(id, object, &mut edges);
        match object {
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
                    Some(false) if matches!(heap.get(target), Ok(Object::DisposeState(_))) => {
                        return Err(VmError::InvalidSnapshot(format!(
                            "object {id} prototype {target} is an internal disposal state"
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
                    | Object::Date { .. }
                    | Object::ArrayBuffer(_)
                    | Object::TypedArray(_)
                    | Object::DataView(_)
                    | Object::Proxy { .. })
            ) {
                return Err(VmError::InvalidSnapshot(format!(
                    "proxy {id} has invalid target {target}"
                )));
            }
            if matches!(
                heap.get(*handler),
                Err(_)
                    | Ok(Object::Environment(_)
                        | Object::ForInIterator { .. }
                        | Object::DisposeState(_))
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

/// Template objects are permanent roots: the snapshot must name exactly one
/// frozen cooked array per build call site, in order, with a FIXED `raw`
/// property that points at the matching frozen raw array.
fn load_templates(
    build: &Build,
    heap: &Heap,
    ids: Vec<ObjectId>,
) -> Result<Vec<ObjectId>, VmError> {
    if ids.len() != build.templates.len() {
        return Err(VmError::InvalidSnapshot(format!(
            "template count mismatch: snapshot {}, build {}",
            ids.len(),
            build.templates.len()
        )));
    }
    let mut seen = BTreeSet::new();
    for (index, (&id, site)) in ids.iter().zip(&build.templates).enumerate() {
        let invalid = |detail: &str| {
            VmError::InvalidSnapshot(format!("template {index} object {id}: {detail}"))
        };
        if !seen.insert(id) {
            return Err(VmError::InvalidSnapshot(format!(
                "duplicate template object {id}"
            )));
        }
        let Ok(object) = heap.get(id) else {
            return Err(invalid("missing object"));
        };
        let Object::Array(array) = object else {
            return Err(invalid("not an array"));
        };
        if !object.test_integrity_level(IntegrityLevel::Frozen) {
            return Err(invalid("not frozen"));
        }
        if array.length != site.cooked.len() {
            return Err(invalid("cooked array length mismatch"));
        }
        for (position, expected) in site.cooked.iter().enumerate() {
            let Some(OwnProperty::Stored(Property::Data(value), attributes)) =
                array.own_index(position)
            else {
                return Err(invalid("cooked element is not a data property"));
            };
            let valid = attributes.enumerable
                && !attributes.writable
                && !attributes.configurable
                && matches!(
                    (expected, value.view()),
                    (None, ValueRef::Undefined) | (Some(_), ValueRef::String(_))
                );
            let matched = match (expected, value.view()) {
                (None, ValueRef::Undefined) => true,
                (Some(expected), ValueRef::String(actual)) => actual == expected,
                _ => false,
            };
            if !valid || !matched {
                return Err(invalid("cooked element value mismatch"));
            }
        }
        let Some(raw_property) = object.own_property(PropertyKeyRef::String("raw")) else {
            return Err(invalid("missing raw property"));
        };
        if raw_property.attributes() != Attributes::FIXED {
            return Err(invalid("raw property is not fixed"));
        }
        let Some(ValueRef::Object(raw_id)) = raw_property.data_value().map(Value::view) else {
            return Err(invalid("raw property is not an array"));
        };
        let Ok(raw_object) = heap.get(raw_id) else {
            return Err(invalid("raw array is missing"));
        };
        let Object::Array(raw_array) = raw_object else {
            return Err(invalid("raw property is not an array"));
        };
        if !raw_object.test_integrity_level(IntegrityLevel::Frozen) {
            return Err(invalid("raw array is not frozen"));
        }
        if raw_array.length != site.raw.len() {
            return Err(invalid("raw array length mismatch"));
        }
        for (position, expected) in site.raw.iter().enumerate() {
            let Some(OwnProperty::Stored(Property::Data(value), attributes)) =
                raw_array.own_index(position)
            else {
                return Err(invalid("raw element is not a data property"));
            };
            let valid = attributes.enumerable
                && !attributes.writable
                && !attributes.configurable
                && matches!(value.view(), ValueRef::String(_));
            let matched = matches!(value.view(), ValueRef::String(actual) if actual == expected);
            if !valid || !matched {
                return Err(invalid("raw element value mismatch"));
            }
        }
    }
    Ok(ids)
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
    let mut awaiting = BTreeMap::new();
    for record in indexed.values() {
        if let ContinuationTargetRecord::Generator(generator) = record.target {
            *awaiting.entry(generator).or_insert(0usize) += 1;
        }
    }
    for (id, generator) in &heap.generators {
        if matches!(generator.status, GeneratorStatus::Awaiting) && awaiting.get(id) != Some(&1) {
            return Err(VmError::InvalidSnapshot(format!(
                "generator {id} is awaiting but does not own exactly one continuation"
            )));
        }
    }
    Ok(indexed
        .into_values()
        .map(|record| {
            (
                record.id,
                Continuation {
                    target: match record.target {
                        ContinuationTargetRecord::Promise(promise) => {
                            ContinuationTarget::Promise(promise)
                        }
                        ContinuationTargetRecord::Generator(generator) => {
                            ContinuationTarget::Generator(generator)
                        }
                    },
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
    match record.target {
        ContinuationTargetRecord::Promise(promise) => {
            if !function.is_async || function.is_generator {
                return Err(VmError::InvalidSnapshot(format!(
                    "continuation {} targets a Promise from a non-async function",
                    record.id
                )));
            }
            match heap.get(promise) {
                Ok(Object::Promise(promise)) if matches!(promise.state, PromiseState::Pending) => {}
                _ => {
                    return Err(VmError::InvalidSnapshot(format!(
                        "continuation {} async result {promise} is not a pending Promise",
                        record.id
                    )));
                }
            }
        }
        ContinuationTargetRecord::Generator(generator) => {
            if !(function.is_async && function.is_generator) {
                return Err(VmError::InvalidSnapshot(format!(
                    "continuation {} targets a generator from a non-generator function",
                    record.id
                )));
            }
            let target = heap.generator(generator).ok_or_else(|| {
                VmError::InvalidSnapshot(format!(
                    "continuation {} targets missing generator {generator}",
                    record.id
                ))
            })?;
            if target.func_id != record.func_id {
                return Err(VmError::InvalidSnapshot(format!(
                    "continuation {} targets a generator of another function",
                    record.id
                )));
            }
            if !matches!(target.status, GeneratorStatus::Awaiting) {
                return Err(VmError::InvalidSnapshot(format!(
                    "continuation {} targets generator {generator} that is not awaiting",
                    record.id
                )));
            }
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

fn valid_disposal_record(heap: &Heap, record: &DisposableResource, async_: Option<bool>) -> bool {
    let valid_object = |id| {
        heap.get(id).is_ok_and(|object| {
            !matches!(
                object,
                Object::Environment(_) | Object::ForInIterator { .. } | Object::DisposeState(_)
            )
        })
    };
    let (method, hint) = match record {
        DisposableResource::Method {
            value,
            method,
            hint,
        } => {
            if !matches!(value.view(), ValueRef::Object(id) if valid_object(id)) {
                return false;
            }
            (method, hint)
        }
        DisposableResource::Adopt {
            value,
            method,
            hint,
        } => {
            if *hint == DisposeHint::AsyncFromSync
                || matches!(value.view(), ValueRef::Object(id) if !valid_object(id))
            {
                return false;
            }
            (method, hint)
        }
        DisposableResource::Defer { method, hint } => {
            if *hint == DisposeHint::AsyncFromSync {
                return false;
            }
            (method, hint)
        }
        DisposableResource::AsyncEmpty => return async_ != Some(false),
    };
    async_.is_none_or(|async_| async_ == (*hint != DisposeHint::Sync))
        && matches!(method.view(), ValueRef::Object(id) if heap.get(id).is_ok_and(is_callable_object))
}

fn validate_jobs(heap: &Heap) -> Result<(), VmError> {
    let mut adopts: BTreeMap<ObjectId, usize> = BTreeMap::new();
    let mut closes: BTreeMap<ObjectId, Vec<(ObjectId, ObjectId)>> = BTreeMap::new();
    let mut disposals = BTreeMap::new();
    for (id, object) in heap.iter() {
        match object {
            Object::Promise(promise) => {
                if let Some(state) = promise.disposal
                    && (!matches!(promise.state, PromiseState::Pending)
                        || promise.adopting.is_some()
                        || !matches!(heap.get(state), Ok(Object::DisposeState(state)) if state.result == Some(id)))
                {
                    return Err(VmError::InvalidSnapshot(format!(
                        "invalid disposal backlink on Promise {id}"
                    )));
                }
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
                        ReactionJob::GeneratorClose { generator, request } => {
                            require_promise(heap, *request, &format!("Promise {id} reaction"))?;
                            closes.entry(*generator).or_default().push((id, *request));
                        }
                        ReactionJob::DisposeNext { state, result } => {
                            if !matches!(promise.state, PromiseState::Pending)
                                || disposals.insert(*state, (id, *result)).is_some()
                            {
                                return Err(VmError::InvalidSnapshot(format!(
                                    "invalid disposal reaction on Promise {id}"
                                )));
                            }
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
            Object::DisposableStack {
                async_,
                disposed,
                records,
                ..
            } if (*disposed && !records.is_empty())
                || !records
                    .iter()
                    .all(|record| valid_disposal_record(heap, record, Some(*async_))) =>
            {
                return Err(VmError::InvalidSnapshot(format!(
                    "invalid disposable stack {id}"
                )));
            }
            _ => {}
        }
    }
    for (id, object) in heap.iter() {
        if let Object::DisposeState(state) = object {
            let reaction = disposals.remove(&id);
            let valid_links = match (state.result, state.waiting) {
                (None, None) => reaction.is_none(),
                (Some(result), Some(source)) => {
                    reaction == Some((source, result))
                        && matches!(heap.get(result), Ok(Object::Promise(promise)) if promise.disposal == Some(id))
                }
                _ => false,
            };
            if !valid_links
                || !state
                    .records
                    .iter()
                    .all(|record| valid_disposal_record(heap, record, state.result.map(|_| true)))
            {
                return Err(VmError::InvalidSnapshot(format!(
                    "invalid disposal state {id}"
                )));
            }
        }
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
    if !disposals.is_empty() {
        return Err(VmError::InvalidSnapshot(
            "disposal reaction has no disposal state".to_owned(),
        ));
    }
    for (generator, waits) in &closes {
        let record = heap.generator(*generator).ok_or_else(|| {
            VmError::InvalidSnapshot(format!(
                "Promise reaction targets missing generator {generator}"
            ))
        })?;
        let Some(head) = record.requests.front() else {
            return Err(VmError::InvalidSnapshot(format!(
                "generator {generator} has a close reaction without a request"
            )));
        };
        if waits.len() != 1 || waits[0].1 != head.promise {
            return Err(VmError::InvalidSnapshot(format!(
                "generator {generator} must own exactly one close reaction for its head request"
            )));
        }
        if record.status != GeneratorStatus::Closing {
            return Err(VmError::InvalidSnapshot(format!(
                "generator {generator} has a close reaction without closing"
            )));
        }
    }
    for (id, generator) in &heap.generators {
        if generator.status == GeneratorStatus::Closing && !closes.contains_key(id) {
            return Err(VmError::InvalidSnapshot(format!(
                "generator {id} is closing without a close reaction"
            )));
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
