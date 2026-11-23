//! Snapshot capture and restore entry points.

use crate::binding::Recovery;
use crate::builtin::roots::BuiltinRoots;
use crate::error::VmError;
use crate::heap::{Heap, Object};
use crate::host::Host;
use crate::init::{RestoreOptions, SnapshotValidation};
use crate::regexp::compile_pattern;
use crate::runtime::{ContinuationTarget, HostSlot, HostTaskState, Settlement};
use crate::value::Value;
use crate::vm::{VerifiedBuild, Vm};

use super::record::*;

mod check_edges;
mod continuations;
mod jobs;
mod load_modules;
mod load_objects;
mod validate_builtins;
mod validate_tables;

use self::{
    check_edges::*, continuations::*, jobs::*, load_modules::*, load_objects::*,
    validate_builtins::*, validate_tables::*,
};

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
        snapshot.elapsed = self.clock.elapsed();
        snapshot.console = self.console.clone();
        snapshot.base_url = self.base_url.clone();
        snapshot.fetch_environment = self.fetch_environment.clone();
        snapshot.fetch_state = self.fetch_state.clone();
        snapshot.object_urls = self.object_urls.clone();
        snapshot.script_timers = self.script_timers.clone();
        snapshot.next_timer_id = self.next_timer_id;
        snapshot.next_operation_id = self.next_operation_id;
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
        snapshot.pending_rejection_notifications = self.pending_rejection_notifications.clone();
        snapshot.ordinary_tasks = self.ordinary_tasks.iter().cloned().collect();
        snapshot.next_stream_message = self.next_stream_message;
        for (class, tag) in self.bindings.classes() {
            snapshot
                .class_prototypes
                .insert(tag.to_owned(), self.host_classes[class].prototype);
        }
        snapshot.module_envs = self.module_envs.clone();
        snapshot.module_namespaces = self.module_namespaces.clone();
        for id in &live.objects {
            let mut object = match self.heap.get(*id)? {
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
            object.prune_weak_abort_edges(&live.objects);
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
            if matches!(operation.target, DeliveryTarget::Timeout { .. }) {
                // The absolute VM deadline is authoritative, not platform timer state.
                snapshot.operations.push(PendingOperationRecord {
                    id: *id,
                    name: TIMEOUT_OPERATION_NAME.to_owned(),
                    schema: TIMEOUT_OPERATION_SCHEMA,
                    recovery: Recovery::Reissue,
                    state: Vec::new(),
                    target: operation.target,
                    signal: None,
                });
                continue;
            }
            snapshot.operations.push(PendingOperationRecord {
                id: *id,
                name: operation.name.clone(),
                schema: operation.schema,
                recovery: operation.recovery,
                state: operation
                    .checkpoint()
                    .map_err(|error| VmError::HostFault(error.0))?,
                target: operation.target,
                signal: operation.signal,
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
    /// The caller must ensure provenance and integrity. Both validation modes
    /// check format, build and host compatibility. Select
    /// [`SnapshotValidation::Full`] for an additional graph audit.
    pub fn restore(
        snapshot: Snapshot,
        build: impl TryInto<VerifiedBuild, Error: Into<VmError>>,
        host: &mut dyn Host,
        options: RestoreOptions,
    ) -> Result<Self, VmError> {
        check_snapshot_header(&snapshot)?;
        authorize_fetch_environment(
            &snapshot.fetch_environment,
            options.fetch_environment.as_ref(),
        )?;
        let build = build.try_into().map_err(Into::into)?;
        match options.validation {
            SnapshotValidation::Trusted => {
                Self::restore_image::<false>(snapshot, build, host, options.test_clock, |_| {})
            }
            SnapshotValidation::Full => {
                Self::restore_image::<true>(snapshot, build, host, options.test_clock, |_| {})
            }
        }
    }

    /// Restore with full snapshot validation and time its phases.
    #[doc(hidden)]
    pub fn restore_profiled(
        snapshot: Snapshot,
        build: VerifiedBuild,
        host: &mut dyn Host,
    ) -> Result<(Self, crate::perf::Restore), VmError> {
        use std::time::Instant;

        let start = Instant::now();
        let mut last = start;
        let mut profile = crate::perf::Restore::default();
        check_snapshot_header(&snapshot)?;
        authorize_fetch_environment(&snapshot.fetch_environment, None)?;
        let vm = Self::restore_image::<true>(snapshot, build, host, None, |field| {
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
        clock: Option<std::sync::Arc<dyn crate::TestClock>>,
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
            validate_rejection_notifications(
                &heap,
                &snapshot.pending_rejection_notifications,
                &snapshot.ordinary_tasks,
            )?;
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
        if AUDIT {
            snapshot.object_urls.validate(&heap)?;
            snapshot.fetch_state.validate(&heap)?;
            snapshot
                .fetch_state
                .validate_queue(&snapshot.ordinary_tasks)?;
        }
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
        let operations = load_operations(
            &bindings,
            &heap,
            snapshot.elapsed,
            std::mem::take(&mut snapshot.operations),
            &snapshot.fetch_state,
        )?;
        let host_tasks = load_host_tasks(
            &bindings,
            &heap,
            &host_instances,
            &operations,
            snapshot.host_tasks,
            snapshot.next_host_task_id,
        )?;
        if AUDIT {
            snapshot.console.validate(snapshot.elapsed)?;
            crate::builtin::streams::transfer::validate_tasks(
                &heap,
                &snapshot.ordinary_tasks,
                snapshot.next_stream_message,
            )?;
        }
        validate_timers(
            &bindings,
            &heap,
            &snapshot.script_timers,
            &snapshot.ordinary_tasks,
            snapshot.elapsed,
            snapshot.next_timer_id,
            snapshot.next_operation_id,
            &operations,
        )?;
        checkpoint(|profile| &mut profile.state_load);
        let mut vm = Self::restored(
            build,
            bindings.clone(),
            crate::vm::RestoredState {
                heap,
                continuations,
                operations,
                next_operation_id: snapshot.next_operation_id,
                script_timers: snapshot.script_timers,
                next_timer_id: snapshot.next_timer_id,
                next_stream_message: snapshot.next_stream_message,
                clock: crate::runtime::VmClock::new(clock, snapshot.elapsed),
                console: snapshot.console,
                base_url: snapshot.base_url,
                fetch_environment: snapshot.fetch_environment,
                fetch_state: snapshot.fetch_state,
                object_urls: snapshot.object_urls,
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
        vm.pending_rejection_notifications = snapshot.pending_rejection_notifications;
        vm.ordinary_tasks = snapshot.ordinary_tasks.into();
        // Accepted events run first; work that still depends on the old host
        // transport is interrupted locally without starting a new request.
        vm.start_fetch_interruptions(host)?;
        vm.restore_imports(&bindings, snapshot.imports, resolved_imports)?;
        checkpoint(|profile| &mut profile.vm_assembly);
        if AUDIT {
            validate_native_import_objects(&vm.heap, &vm.imports)?;
            check_heap_edges(&vm.build, &vm.heap, vm.builtins.get("globalThis"))?;
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
                    self.unlink_operation_signal(id, operation.signal);
                    self.deliver_operation(
                        operation.target,
                        Settlement::Fulfilled(Value::UNDEFINED),
                    )?;
                }
                Recovery::Reject => {
                    self.unlink_operation_signal(id, operation.signal);
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
