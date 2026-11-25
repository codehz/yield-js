//! Runtime garbage collection and read-only tracing shared with snapshots.
//!
//! Tracing is read-only; sweeping starts only after all reachable IDs have
//! been validated. Unreachable IDs are never reused within the same VM.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::rc::Rc;

use crate::error::VmError;
use crate::frame::Frame;
use crate::heap::{
    ASYNC_DISPOSE_SYMBOL, ASYNC_ITERATOR_SYMBOL, DISPOSE_SYMBOL, HAS_INSTANCE_SYMBOL, Heap,
    IS_CONCAT_SPREADABLE_SYMBOL, ITERATOR_SYMBOL, MATCH_ALL_SYMBOL, MATCH_SYMBOL, Object,
    OwnedEdge, REPLACE_SYMBOL, SEARCH_SYMBOL, SPECIES_SYMBOL, SPLIT_SYMBOL, TO_PRIMITIVE_SYMBOL,
    TO_STRING_TAG_SYMBOL, WeakKey,
};
use crate::host::GcRoots;
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
    /// Collects unreachable heap objects, returning the number removed.
    ///
    /// Call only between VM calls, after draining microtasks. No microtasks
    /// are run by this method. Pass every same-VM [`Value`] you intend to use
    /// later that is not otherwise reachable from the VM, including ordinary
    /// symbols. The list may be empty or contain duplicates. Raw IDs do not
    /// identify their VM of origin, so foreign IDs may alias local values.
    /// This does not extend the lifetime of temporary host binding handles.
    ///
    /// Returns [`VmError::InvalidState`] if microtasks are queued or tracing
    /// encounters a missing object, continuation, or symbol. A failed
    /// collection leaves the VM unchanged.
    pub fn collect_garbage(&mut self, roots: &[Value]) -> Result<usize, VmError> {
        if !self.microtasks.is_empty() {
            return Err(VmError::InvalidState(
                "garbage collection requires a drained microtask queue".to_owned(),
            ));
        }
        self.collect_with_roots(|tracer| roots.trace(tracer), None)
    }

    /// Monotonic allocation totals for host GC scheduling, read between VM
    /// calls. `objects` counts every allocation since VM creation; `bytes`
    /// counts the backing storage of byte buffers allocated in the same span.
    /// Compare against values taken at the previous collection; a restored VM
    /// starts both at zero even though its heap is non-empty.
    pub fn allocation_totals(&self) -> (u64, u64) {
        (self.heap.allocations, self.heap.alloc_bytes)
    }

    pub(crate) fn collect_active(
        &mut self,
        host: &crate::Host,
        active: &ActiveRoots<'_>,
    ) -> Result<usize, VmError> {
        self.collect_with_roots(
            |tracer| {
                let mut status = Ok(());
                let Some(roots) = host.services().get::<Rc<dyn GcRoots>>() else {
                    return Ok(());
                };
                roots.visit(&mut |value| {
                    if status.is_ok() {
                        status = tracer.mark_value(value);
                    }
                });
                status
            },
            Some(active),
        )
    }

    fn collect_with_roots(
        &mut self,
        trace_external: impl FnOnce(&mut Tracer<'_>) -> Result<(), VmError>,
        active: Option<&ActiveRoots<'_>>,
    ) -> Result<usize, VmError> {
        let live = self.trace_with_roots(trace_external, active)?;
        for id in &live.symbols {
            if !self.heap.symbols.contains_key(id) {
                return Err(VmError::InvalidState(format!(
                    "garbage collection references missing symbol {id}"
                )));
            }
        }

        let removed = self.heap.sweep(&live);
        for function in &mut self.property_caches {
            function.fill(crate::vm::PropertyCache::Empty);
        }
        self.host_instances
            .retain(|id, _| live.objects.contains(id));
        self.continuations
            .retain(|id, _| live.continuations_seen.contains(id));
        Ok(removed)
    }

    pub(crate) fn trace_roots(&self, roots: &[Value]) -> Result<LiveSet, VmError> {
        self.trace_with_roots(|tracer| roots.trace(tracer), None)
    }

    fn trace_with_roots(
        &self,
        trace_external: impl FnOnce(&mut Tracer<'_>) -> Result<(), VmError>,
        active: Option<&ActiveRoots<'_>>,
    ) -> Result<LiveSet, VmError> {
        let mut tracer = Tracer::new(&self.heap, &self.continuations, &self.host_instances);
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
                ScriptTimer(_) | FetchEvent(..) => {}
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
}

pub(crate) struct Tracer<'a> {
    heap: &'a Heap,
    continuations: &'a BTreeMap<ContinuationId, Continuation>,
    host_instances: &'a BTreeMap<ObjectId, HostInstance>,
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
    ) -> Self {
        Self {
            heap,
            continuations,
            host_instances,
            live: LiveSet::default(),
            weak_maps: Vec::new(),
            object_queue: VecDeque::new(),
            continuation_queue: VecDeque::new(),
            edges: Vec::new(),
        }
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
