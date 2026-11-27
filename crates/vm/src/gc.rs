//! Runtime garbage collection and read-only tracing shared with snapshots.
//!
//! Tracing is read-only; sweeping starts only after all reachable IDs have
//! been validated. Unreachable IDs are never reused within the same VM.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::error::VmError;
use crate::frame::Frame;
use crate::heap::{
    ASYNC_DISPOSE_SYMBOL, ASYNC_ITERATOR_SYMBOL, DISPOSE_SYMBOL, HAS_INSTANCE_SYMBOL, Heap,
    IS_CONCAT_SPREADABLE_SYMBOL, ITERATOR_SYMBOL, MATCH_ALL_SYMBOL, MATCH_SYMBOL, Object,
    OwnedEdge, REPLACE_SYMBOL, SEARCH_SYMBOL, SPECIES_SYMBOL, SPLIT_SYMBOL, TO_PRIMITIVE_SYMBOL,
    TO_STRING_TAG_SYMBOL, WeakKey,
};
use crate::runtime::{Continuation, HostInstance, HostSlot};
use crate::value::{
    ContinuationId, ObjectId, PropertyKey, PropertyKeyRef, SymbolId, Value, ValueRef,
};
use crate::vm::VmCore;

#[derive(Default)]
pub(crate) struct LiveSet {
    pub(crate) objects: BTreeSet<ObjectId>,
    pub(crate) symbols: BTreeSet<SymbolId>,
    pub(crate) continuations_seen: BTreeSet<ContinuationId>,
}

impl LiveSet {
    pub(crate) fn contains_weak_key(&self, key: WeakKey) -> bool {
        match key {
            WeakKey::Object(id) => self.objects.contains(&id),
            WeakKey::Symbol(id) => self.symbols.contains(&id),
        }
    }
}

/// Borrowed views of VM values that are live on the synchronous Rust stack.
/// Adding a view doesn't enumerate or copy any roots; only a collection walks it.
pub(crate) struct ActiveRoots<'a>(Option<(&'a ActiveRoots<'a>, &'a dyn TraceRoots)>);

impl ActiveRoots<'_> {
    pub(crate) const EMPTY: Self = Self(None);

    pub(crate) fn with<'a>(&'a self, view: &'a dyn TraceRoots) -> ActiveRoots<'a> {
        ActiveRoots(Some((self, view)))
    }

    pub(crate) fn frames(&self) -> impl Iterator<Item = &Frame> {
        std::iter::successors(Some(self), |roots| roots.0.map(|(parent, _)| parent))
            .filter_map(|roots| roots.0.and_then(|(_, view)| view.frame()))
    }

    fn trace(&self, tracer: &mut Tracer<'_>) -> Result<(), VmError> {
        let mut current = self;
        while let Some((parent, view)) = current.0 {
            view.trace(tracer)?;
            current = parent;
        }
        Ok(())
    }
}

pub(crate) trait TraceRoots {
    fn frame(&self) -> Option<&Frame> {
        None
    }

    fn trace(&self, tracer: &mut Tracer<'_>) -> Result<(), VmError>;
}

impl TraceRoots for Value {
    fn trace(&self, tracer: &mut Tracer<'_>) -> Result<(), VmError> {
        tracer.mark_value(self)
    }
}

impl TraceRoots for ObjectId {
    fn trace(&self, tracer: &mut Tracer<'_>) -> Result<(), VmError> {
        tracer.mark_object(*self)
    }
}

impl TraceRoots for PropertyKey {
    fn trace(&self, tracer: &mut Tracer<'_>) -> Result<(), VmError> {
        self.borrowed().trace(tracer)
    }
}

impl TraceRoots for PropertyKeyRef<'_> {
    fn trace(&self, tracer: &mut Tracer<'_>) -> Result<(), VmError> {
        if let Self::Symbol(id) = *self {
            tracer.mark_value(&Value::symbol(id))?;
        }
        Ok(())
    }
}

impl<T: TraceRoots + ?Sized> TraceRoots for &T {
    fn frame(&self) -> Option<&Frame> {
        (**self).frame()
    }

    fn trace(&self, tracer: &mut Tracer<'_>) -> Result<(), VmError> {
        (**self).trace(tracer)
    }
}

impl<T: TraceRoots> TraceRoots for Option<T> {
    fn trace(&self, tracer: &mut Tracer<'_>) -> Result<(), VmError> {
        if let Some(value) = self {
            value.trace(tracer)?;
        }
        Ok(())
    }
}

impl<T: TraceRoots> TraceRoots for [T] {
    fn trace(&self, tracer: &mut Tracer<'_>) -> Result<(), VmError> {
        for value in self {
            value.trace(tracer)?;
        }
        Ok(())
    }
}

impl<T: TraceRoots, const N: usize> TraceRoots for [T; N] {
    fn trace(&self, tracer: &mut Tracer<'_>) -> Result<(), VmError> {
        self.as_slice().trace(tracer)
    }
}

impl<T: TraceRoots> TraceRoots for Vec<T> {
    fn trace(&self, tracer: &mut Tracer<'_>) -> Result<(), VmError> {
        self.as_slice().trace(tracer)
    }
}

macro_rules! trace_tuple {
    ($($ty:ident $value:ident),+) => {
        impl<$($ty: TraceRoots),+> TraceRoots for ($($ty,)+) {
            fn trace(&self, tracer: &mut Tracer<'_>) -> Result<(), VmError> {
                let ($($value,)+) = self;
                $($value.trace(tracer)?;)+
                Ok(())
            }
        }
    };
}

trace_tuple!(A a, B b);
trace_tuple!(A a, B b, C c);
trace_tuple!(A a, B b, C c, D d);

impl VmCore {
    pub(crate) fn collect_garbage_with_groups(
        &mut self,
        roots: &[Value],
        groups: &[Vec<ObjectId>],
    ) -> Result<usize, VmError> {
        if !self.microtasks.is_empty() {
            return Err(VmError::InvalidState(
                "garbage collection requires a drained microtask queue".to_owned(),
            ));
        }
        self.collect_with_roots(|tracer| roots.trace(tracer), None, groups)
    }

    /// Monotonic allocation totals for host GC scheduling, read between VM
    /// calls. `objects` counts every allocation since VM creation; `bytes`
    /// counts the backing storage of byte buffers allocated in the same span.
    /// Compare against values taken at the previous collection; a restored VM
    /// starts both at zero even though its heap is non-empty.
    pub fn allocation_totals(&self) -> (u64, u64) {
        (self.heap.allocations, self.heap.alloc_bytes)
    }

    pub(crate) fn collect_active<P: crate::PlatformServices>(
        &mut self,
        host: &crate::Host<P>,
        active: &ActiveRoots<'_>,
    ) -> Result<usize, VmError> {
        use crate::GcRoots as _;

        let Some(roots) = host.platform().gc_roots() else {
            return Ok(0);
        };

        self.collect_with_roots(
            |tracer| {
                let mut status = Ok(());
                roots.visit(&mut |value| {
                    if status.is_ok() {
                        status = tracer.mark_value(value);
                    }
                });
                status
            },
            Some(active),
            &[],
        )
    }

    fn collect_with_roots(
        &mut self,
        trace_external: impl FnOnce(&mut Tracer<'_>) -> Result<(), VmError>,
        active: Option<&ActiveRoots<'_>>,
        groups: &[Vec<ObjectId>],
    ) -> Result<usize, VmError> {
        let live = self.trace_with_roots(trace_external, active, groups)?;
        for id in &live.symbols {
            if !self.heap.symbols.contains_key(id) {
                return Err(VmError::InvalidState(format!(
                    "garbage collection references missing symbol {id}"
                )));
            }
        }

        let removed = self.heap.sweep(&live);
        self.schedule_finalization_cleanup(&live.objects, self.delivering_finalization);
        for function in &mut self.property_caches {
            function.fill(crate::vm::PropertyCache::Empty);
        }
        self.host_instances
            .retain(|id, _| live.objects.contains(id));
        self.continuations
            .retain(|id, _| live.continuations_seen.contains(id));
        Ok(removed)
    }

    pub(crate) fn trace_roots(
        &self,
        roots: &[Value],
        groups: &[Vec<ObjectId>],
    ) -> Result<LiveSet, VmError> {
        self.trace_with_roots(|tracer| roots.trace(tracer), None, groups)
    }

    fn trace_with_roots(
        &self,
        trace_external: impl FnOnce(&mut Tracer<'_>) -> Result<(), VmError>,
        active: Option<&ActiveRoots<'_>>,
        groups: &[Vec<ObjectId>],
    ) -> Result<LiveSet, VmError> {
        let mut tracer = Tracer::new(
            &self.heap,
            &self.continuations,
            &self.host_instances,
            groups,
        )?;
        trace_external(&mut tracer)?;
        if let Some(active) = active {
            active.trace(&mut tracer)?;
            self.stack_registrations.trace(&mut tracer)?;
            for job in &self.microtasks {
                job.trace(&mut tracer)?;
            }
        }
        for &promise in &self.pending_rejection_notifications {
            tracer.mark_object(promise)?;
        }
        for value in self.globals.values() {
            tracer.mark_value(value)?;
        }
        for id in self.object_urls.ids() {
            tracer.mark_object(id)?;
        }
        for id in self.fetch_state.ids() {
            tracer.mark_object(id)?;
        }
        for id in self.subtle_state.ids() {
            tracer.mark_object(id)?;
        }
        for import in &self.imports {
            tracer.mark_object(import.namespace)?;
        }
        for operation in self.operations.values() {
            if let Some(signal) = operation.signal {
                tracer.mark_object(signal)?;
            }
            match operation.target {
                crate::snapshot::DeliveryTarget::Promise(promise) => tracer.mark_object(promise)?,
                crate::snapshot::DeliveryTarget::Timeout {
                    target: crate::snapshot::TimerTarget::Abort(signal),
                    ..
                } => {
                    tracer.mark_object(signal)?;
                }
                crate::snapshot::DeliveryTarget::Task(_)
                | crate::snapshot::DeliveryTarget::Timeout { .. } => {}
            }
        }
        for task in &self.ordinary_tasks {
            use crate::snapshot::OrdinaryTask::*;
            match task {
                AbortTimeout(id) | RejectionHandled(id) => id.trace(&mut tracer)?,
                StreamMessage(id, _) => id.trace(&mut tracer)?,
                UnhandledBatch(batch) => batch.trace(&mut tracer)?,
                ScriptTimer(_) | FetchEvent(..) | FinalizationCleanup(_) => {}
            }
        }
        for key in &self.kept_objects {
            match *key {
                WeakKey::Object(id) => tracer.mark_object(id)?,
                WeakKey::Symbol(id) => {
                    tracer.live.symbols.insert(id);
                }
            }
        }
        for timer in self.script_timers.values() {
            timer.callback.trace(&mut tracer)?;
            timer.arguments.as_ref().trace(&mut tracer)?;
        }
        for task in self.host_tasks.values() {
            task.trace(&mut tracer)?;
        }
        for class in &self.host_classes {
            tracer.mark_object(class.prototype)?;
        }
        for state in &self.module_states {
            for slot in &state.slots {
                tracer.mark_slot(slot)?;
            }
        }
        for id in self.builtins.ids() {
            tracer.mark_object(id)?;
        }
        for &id in &self.templates {
            tracer.mark_object(id)?;
        }
        if self.module_envs.len() != self.build.modules.len()
            || self.module_namespaces.len() != self.build.modules.len()
        {
            return Err(VmError::InvalidState(
                "source module root count mismatch".to_owned(),
            ));
        }
        for (module, env) in self.module_envs.iter().enumerate() {
            if env.is_some() != self.module_needs_environment(module) {
                return Err(VmError::InvalidState(format!(
                    "wrong environment presence for source module {module}"
                )));
            }
        }
        for &id in self
            .module_envs
            .iter()
            .flatten()
            .chain(&self.module_namespaces)
        {
            tracer.mark_object(id)?;
        }
        tracer.live.symbols.extend([
            ITERATOR_SYMBOL,
            TO_PRIMITIVE_SYMBOL,
            SPECIES_SYMBOL,
            MATCH_SYMBOL,
            MATCH_ALL_SYMBOL,
            REPLACE_SYMBOL,
            SEARCH_SYMBOL,
            SPLIT_SYMBOL,
            HAS_INSTANCE_SYMBOL,
            IS_CONCAT_SPREADABLE_SYMBOL,
            ASYNC_ITERATOR_SYMBOL,
            TO_STRING_TAG_SYMBOL,
            DISPOSE_SYMBOL,
            ASYNC_DISPOSE_SYMBOL,
        ]);
        tracer
            .live
            .symbols
            .extend(self.heap.symbol_registry.values().copied());
        tracer.drain()?;
        tracer.mark_ephemerons()?;
        Ok(tracer.live)
    }

    pub(crate) fn schedule_finalization_cleanup(
        &mut self,
        live_objects: &BTreeSet<ObjectId>,
        skip: Option<ObjectId>,
    ) {
        let pending = pending_finalization_ids(
            live_objects
                .iter()
                .filter_map(|&id| self.heap.get(id).ok().map(|object| (id, object))),
            skip,
        );
        self.ordinary_tasks.retain(|task| match task {
            crate::snapshot::OrdinaryTask::FinalizationCleanup(id) => pending.contains(id),
            _ => true,
        });
        let queued: BTreeSet<ObjectId> = self
            .ordinary_tasks
            .iter()
            .filter_map(|task| match task {
                crate::snapshot::OrdinaryTask::FinalizationCleanup(id) => Some(*id),
                _ => None,
            })
            .collect();
        for id in pending {
            if !queued.contains(&id) {
                self.ordinary_tasks
                    .push_back(crate::snapshot::OrdinaryTask::FinalizationCleanup(id));
            }
        }
    }
}

pub(crate) fn pending_finalization_ids<'a>(
    objects: impl Iterator<Item = (ObjectId, &'a Object)>,
    skip: Option<ObjectId>,
) -> BTreeSet<ObjectId> {
    objects
        .filter_map(|(id, object)| {
            if Some(id) == skip {
                return None;
            }
            match object {
                Object::FinalizationRegistry { cells, .. }
                    if cells.iter().any(|cell| cell.target.is_none()) =>
                {
                    Some(id)
                }
                _ => None,
            }
        })
        .collect()
}

pub(crate) struct Tracer<'a> {
    heap: &'a Heap,
    continuations: &'a BTreeMap<ContinuationId, Continuation>,
    host_instances: &'a BTreeMap<ObjectId, HostInstance>,
    groups: &'a [Vec<ObjectId>],
    group_index: BTreeMap<ObjectId, usize>,
    live: LiveSet,
    weak_maps: Vec<&'a BTreeMap<WeakKey, Value>>,
    object_queue: VecDeque<(ObjectId, &'a Object)>,
    continuation_queue: VecDeque<ContinuationId>,
    edges: Vec<OwnedEdge>,
}

impl<'a> Tracer<'a> {
    fn new(
        heap: &'a Heap,
        continuations: &'a BTreeMap<ContinuationId, Continuation>,
        host_instances: &'a BTreeMap<ObjectId, HostInstance>,
        groups: &'a [Vec<ObjectId>],
    ) -> Result<Self, VmError> {
        let mut group_index = BTreeMap::new();
        for (index, group) in groups.iter().enumerate() {
            for &id in group {
                match group_index.entry(id) {
                    Entry::Occupied(entry) => {
                        if *entry.get() != index {
                            return Err(VmError::InvalidState(format!(
                                "object {id} belongs to multiple host groups"
                            )));
                        }
                        continue;
                    }
                    Entry::Vacant(entry) => entry.insert(index),
                };
                heap.get(id).map_err(|_| {
                    VmError::InvalidState(format!("host group references missing object {id}"))
                })?;
            }
        }
        Ok(Self {
            heap,
            continuations,
            host_instances,
            groups,
            group_index,
            live: LiveSet::default(),
            weak_maps: Vec::new(),
            object_queue: VecDeque::new(),
            continuation_queue: VecDeque::new(),
            edges: Vec::new(),
        })
    }

    fn mark_value(&mut self, value: &Value) -> Result<(), VmError> {
        match value.view() {
            ValueRef::Object(id) => self.mark_object(id)?,
            ValueRef::Symbol(id) => {
                self.live.symbols.insert(id);
            }
            _ => {}
        }
        Ok(())
    }

    fn mark_slot(&mut self, slot: &HostSlot) -> Result<(), VmError> {
        match slot {
            HostSlot::Js(value) => self.mark_value(value)?,
            HostSlot::JsList(values) => {
                for value in values {
                    self.mark_value(value)?;
                }
            }
        }
        Ok(())
    }

    fn mark_object(&mut self, id: ObjectId) -> Result<(), VmError> {
        if self.live.objects.insert(id) {
            let object = self.heap.get(id).map_err(|_| {
                VmError::InvalidState(format!("snapshot root references missing object {id}"))
            })?;
            self.object_queue.push_back((id, object));
        }
        Ok(())
    }

    fn mark_continuation(&mut self, promise: ObjectId, id: ContinuationId) -> Result<(), VmError> {
        self.mark_pending_continuation(id).map_err(|_| {
            VmError::InvalidState(format!(
                "Promise {promise} references missing continuation {id}"
            ))
        })
    }

    pub(crate) fn mark_pending_continuation(&mut self, id: ContinuationId) -> Result<(), VmError> {
        if !self.continuations.contains_key(&id) {
            return Err(VmError::InvalidState(format!(
                "continuation {id} does not exist"
            )));
        }
        if self.live.continuations_seen.insert(id) {
            self.continuation_queue.push_back(id);
        }
        Ok(())
    }

    fn drain(&mut self) -> Result<(), VmError> {
        loop {
            let mut progressed = false;
            while let Some((id, object)) = self.object_queue.pop_front() {
                progressed = true;
                self.walk_object(id, object)?;
            }
            while let Some(id) = self.continuation_queue.pop_front() {
                progressed = true;
                self.walk_continuation(id)?;
            }
            if !progressed {
                break;
            }
        }
        Ok(())
    }

    fn mark_ephemerons(&mut self) -> Result<(), VmError> {
        loop {
            // Values can reveal more keys (or more WeakMaps), even without queuing objects.
            let before = (self.live.objects.len(), self.live.symbols.len());
            for index in 0..self.weak_maps.len() {
                for (key, value) in self.weak_maps[index] {
                    if self.live.contains_weak_key(*key) {
                        self.mark_value(value)?;
                    }
                }
            }
            self.drain()?;
            if before == (self.live.objects.len(), self.live.symbols.len()) {
                return Ok(());
            }
        }
    }

    fn walk_object(&mut self, id: ObjectId, object: &'a Object) -> Result<(), VmError> {
        if let Some(&index) = self.group_index.get(&id) {
            let group = &self.groups[index];
            // Removing the first entry records that this group was expanded.
            if group.len() > 1 && self.group_index.remove(&group[0]).is_some() {
                for &member in group {
                    self.mark_object(member)?;
                }
            }
        }
        if let Object::WeakMap { entries, .. } = object {
            self.weak_maps.push(entries);
        }
        // Only a live, unaborted source can keep an otherwise unreachable
        // dependent alive. Walking these here also closes over WeakMap values:
        // either kind of conditional edge may discover a new live source.
        if let Object::AbortSignal { signal, .. } = object
            && signal.reason.is_none()
        {
            for &dependent in &signal.dependents {
                if let Ok(Object::AbortSignal { target, signal, .. }) = self.heap.get(dependent)
                    && signal.dependent
                    && signal.reason.is_none()
                    && (!signal.steps.is_empty()
                        || target
                            .listeners
                            .iter()
                            .any(|listener| listener.ty == "abort"))
                {
                    self.mark_object(dependent)?;
                }
            }
        }
        let mut edges = std::mem::take(&mut self.edges);
        self.heap.write_edges(id, object, &mut edges);
        for edge in &edges {
            match *edge {
                OwnedEdge::Object(target)
                | OwnedEdge::Accessor(target)
                | OwnedEdge::Prototype(target)
                | OwnedEdge::Environment { id: target, .. } => {
                    self.mark_object(target)?;
                }
                OwnedEdge::Continuation(continuation) => {
                    self.mark_continuation(id, continuation)?;
                }
                OwnedEdge::Symbol(symbol) => {
                    self.live.symbols.insert(symbol);
                }
            }
        }
        edges.clear();
        self.edges = edges;
        if let Some(instance) = self.host_instances.get(&id) {
            for slot in &instance.slots {
                self.mark_slot(slot)?;
            }
        }
        Ok(())
    }

    fn walk_continuation(&mut self, id: ContinuationId) -> Result<(), VmError> {
        let continuation = self
            .continuations
            .get(&id)
            .ok_or_else(|| VmError::InvalidState(format!("continuation {id} does not exist")))?;
        for object in continuation.linked_objects() {
            self.mark_object(object)?;
        }
        for value in &continuation.values {
            self.mark_value(value)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytecode::{Build, Function, Instr, Slot, SourceModule};
    use crate::heap::PlainObject;
    use crate::{Host, Vm, VmOptions};

    fn empty_vm() -> Vm {
        let initializer = |func_id| Function {
            func_id,
            name: format!("init{func_id}"),
            initial_name: String::new(),
            is_async: false,
            is_arrow: false,
            is_method: false,
            is_generator: false,
            length: 0,
            params: vec![],
            rest_param: None,
            this_param: None,
            new_target_param: None,
            home_param: None,
            arguments_param: None,
            transient_slots: 1,
            persistent_slots: 0,
            code: vec![
                Instr::Undefined {
                    dst: Slot::transient(0),
                },
                Instr::Return {
                    src: Slot::transient(0),
                },
            ],
            resumes: vec![],
            generator_start: None,
            generator_resumes: vec![],
            handlers: vec![],
            module: Some(0),
            stack_sites: vec![],
        };
        let mut build = Build::new(
            vec![],
            vec![],
            BTreeMap::new(),
            vec![initializer(0), initializer(1)],
            vec![SourceModule {
                id: "entry".to_owned(),
                declaration: 0,
                evaluation: 1,
                dependencies: vec![],
                exports: BTreeMap::new(),
            }],
            0,
            None,
        );
        build.finalize();
        Vm::new(build, Host::default(), VmOptions::default()).unwrap()
    }

    #[test]
    fn host_groups_are_not_roots() {
        let mut vm = empty_vm();
        let a = vm.core.heap.alloc_plain();
        let b = vm.core.heap.alloc_plain();

        vm.collect_garbage_with_groups(&[], &[vec![], vec![a, b]])
            .unwrap();
        assert!(vm.core.heap.get(a).is_err());
        assert!(vm.core.heap.get(b).is_err());
    }

    #[test]
    fn host_groups_close_over_weak_map_ephemerons() {
        let mut vm = empty_vm();
        let heap = &mut vm.core.heap;
        let key = heap.alloc_plain();
        let a = heap.alloc_plain();
        let b = heap.alloc_plain();
        let c = heap.alloc_plain();
        let d = heap.alloc_plain();
        let mut weak_map = |key, value| {
            heap.alloc(Object::WeakMap {
                object: PlainObject::new(None),
                entries: BTreeMap::from([(WeakKey::Object(key), Value::object(value))]),
            })
        };
        let first = weak_map(key, a);
        let second = weak_map(b, c);
        // Put the second map first so it must be revisited after A expands.
        let roots = [second, first, key].map(Value::object);
        let groups = [vec![b, a], vec![d, c]];

        let mut prepared = false;
        vm.snapshot_with_live_objects_and_groups(&roots, &groups, |live| {
            assert!([a, b, c, d].iter().all(|id| live.contains(id)));
            prepared = true;
        })
        .unwrap();
        assert!(prepared);

        vm.collect_garbage_with_groups(&roots, &groups).unwrap();
        assert!(
            [a, b, c, d]
                .into_iter()
                .all(|id| vm.core.heap.get(id).is_ok())
        );

        vm.collect_garbage(&roots).unwrap();
        assert!(vm.core.heap.get(a).is_ok());
        assert!(
            [b, c, d]
                .into_iter()
                .all(|id| vm.core.heap.get(id).is_err())
        );
    }

    #[test]
    fn host_groups_invalid_input_leaves_heap_unswept() {
        let mut vm = empty_vm();
        let a = vm.core.heap.alloc_plain();
        let b = vm.core.heap.alloc_plain();
        for groups in [[vec![a], vec![ObjectId::MAX]], [vec![a], vec![a, b]]] {
            assert!(matches!(
                vm.collect_garbage_with_groups(&[], &groups),
                Err(VmError::InvalidState(_))
            ));
            assert!(vm.core.heap.get(a).is_ok());
            assert!(vm.core.heap.get(b).is_ok());
        }
    }

    #[test]
    fn host_groups_active_gc_requires_root_service() {
        let mut vm = empty_vm();
        let object = vm.core.heap.alloc_plain();
        let removed = vm
            .core
            .collect_active(
                &Host::<crate::host::Platform>::default(),
                &ActiveRoots::EMPTY,
            )
            .unwrap();
        assert_eq!(removed, 0);
        assert!(vm.core.heap.get(object).is_ok());
    }
}
