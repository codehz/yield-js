use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::JsString;
use crate::binding::{Bindings, Recovery};
use crate::builtins::{
    BuiltinRoots, ROOT_AGGREGATEERROR, ROOT_AGGREGATEERROR_PROTOTYPE, ROOT_ARRAY,
    ROOT_ARRAY_ITERATOR_PROTOTYPE, ROOT_ARRAY_PROTOTYPE, ROOT_ARRAY_VALUES, ROOT_BOOLEAN,
    ROOT_BOOLEAN_PROTOTYPE, ROOT_ERROR, ROOT_ERROR_PROTOTYPE, ROOT_EVALERROR,
    ROOT_EVALERROR_PROTOTYPE, ROOT_FUNCTION_PROTOTYPE, ROOT_GLOBAL_THIS, ROOT_JSON, ROOT_MATH,
    ROOT_NUMBER, ROOT_NUMBER_PROTOTYPE, ROOT_OBJECT, ROOT_OBJECT_PROTOTYPE, ROOT_PROMISE,
    ROOT_PROMISE_PROTOTYPE, ROOT_PROXY, ROOT_QUEUE_MICROTASK, ROOT_RANGEERROR,
    ROOT_RANGEERROR_PROTOTYPE, ROOT_REFERENCEERROR, ROOT_REFERENCEERROR_PROTOTYPE, ROOT_REFLECT,
    ROOT_REGEXP, ROOT_REGEXP_ITERATOR_PROTOTYPE, ROOT_REGEXP_PROTOTYPE, ROOT_STRING,
    ROOT_STRING_ITERATOR, ROOT_STRING_ITERATOR_PROTOTYPE, ROOT_STRING_PROTOTYPE, ROOT_SYMBOL,
    ROOT_SYMBOL_PROTOTYPE, ROOT_SYNTAXERROR, ROOT_SYNTAXERROR_PROTOTYPE, ROOT_TYPEERROR,
    ROOT_TYPEERROR_PROTOTYPE, ROOT_URIERROR, ROOT_URIERROR_PROTOTYPE, ROOT_WEAK_MAP,
    ROOT_WEAK_MAP_PROTOTYPE, is_callable_object,
};
use crate::bytecode::{BYTECODE_VERSION, Build};
use crate::error::VmError;
use crate::heap::{
    Attributes, CombineState, EnvRole, Heap, ITERATOR_DESCRIPTION, ITERATOR_SYMBOL,
    MATCH_ALL_DESCRIPTION, MATCH_ALL_SYMBOL, MATCH_DESCRIPTION, MATCH_SYMBOL, Object, OwnProperty,
    OwnedEdge, PromiseState, Property, REPLACE_DESCRIPTION, REPLACE_SYMBOL, ReactionJob,
    SEARCH_DESCRIPTION, SEARCH_SYMBOL, SPECIES_DESCRIPTION, SPECIES_SYMBOL, SPLIT_DESCRIPTION,
    SPLIT_SYMBOL, TO_PRIMITIVE_DESCRIPTION, TO_PRIMITIVE_SYMBOL,
};
use crate::heap::{CollectionIteration, CollectionKind, push_value};
use crate::host::Host;
use crate::regexp::compile_pattern;
use crate::runtime::{
    Continuation, HostInstance, HostSlot, ModuleState, PendingOperation, Settlement,
};
use crate::value::{ContinuationId, ObjectId, PropertyKeyRef, SymbolId, Value};
use crate::vm::Vm;

// Plain objects preserve the internal Error brand.
pub const SNAPSHOT_FORMAT_VERSION: u32 = 37;

pub(crate) mod ordered_pairs {
    use std::collections::BTreeMap;
    use std::collections::btree_map::Entry;
    use std::fmt;
    use std::marker::PhantomData;

    use serde::de::{Error, SeqAccess, Visitor};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub(crate) fn serialize<K: Ord + Serialize, V: Serialize, S: Serializer>(
        map: &BTreeMap<K, V>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(map)
    }

    pub(crate) fn deserialize<'de, K, V, D>(deserializer: D) -> Result<BTreeMap<K, V>, D::Error>
    where
        K: Ord + Deserialize<'de>,
        V: Deserialize<'de>,
        D: Deserializer<'de>,
    {
        struct Pairs<K, V>(PhantomData<(K, V)>);

        impl<'de, K, V> Visitor<'de> for Pairs<K, V>
        where
            K: Ord + Deserialize<'de>,
            V: Deserialize<'de>,
        {
            type Value = BTreeMap<K, V>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a sequence of key-value pairs")
            }

            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> Result<Self::Value, A::Error> {
                let mut map = BTreeMap::new();
                while let Some((key, value)) = sequence.next_element()? {
                    match map.entry(key) {
                        Entry::Vacant(entry) => {
                            entry.insert(value);
                        }
                        Entry::Occupied(_) => {
                            return Err(A::Error::custom("duplicate key in ordered pairs"));
                        }
                    }
                }
                Ok(map)
            }
        }

        deserializer.deserialize_seq(Pairs(PhantomData))
    }
}

fn valid_string_position(string: &JsString, position: usize) -> bool {
    position <= string.unit_len()
        && (position == 0
            || !(matches!(string.unit_at(position - 1), Some(0xd800..=0xdbff))
                && matches!(string.unit_at(position), Some(0xdc00..=0xdfff))))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObjectRecord {
    pub id: ObjectId,
    pub object: Object,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContinuationRecord {
    pub id: ContinuationId,
    /// Async-function result Promise.
    pub async_promise: ObjectId,
    pub func_id: u32,
    pub resume_id: u32,
    /// Promise the continuation waits on. Only recoverable, unsettled promises
    /// are persisted, so this is required for every restored continuation.
    pub waiting_on: ObjectId,
    /// Invocation environment of the suspended frame. Required exactly when
    /// [`crate::bytecode::Function::needs_environment`] is true.
    pub env: Option<ObjectId>,
    /// Values in `ResumeEntry::saved_slots` order.
    pub values: Vec<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingOperationRecord {
    pub id: u64,
    pub name: String,
    pub schema: u32,
    pub recovery: Recovery,
    pub state: Vec<u8>,
    pub promise: ObjectId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostSlotRecord {
    Js(Value),
    JsList(Vec<Value>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayloadRecord {
    pub schema: u32,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostInstanceRecord {
    pub object: ObjectId,
    pub class: String,
    pub slots: Vec<HostSlotRecord>,
    pub payload: Option<PayloadRecord>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModuleStateRecord {
    pub module: String,
    pub slots: Vec<HostSlotRecord>,
    pub payload: PayloadRecord,
}

/// Host-held script roots live outside the VM. The snapshot deliberately does
/// not contain native pointers, host callbacks, or pending microtasks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub format_version: u32,
    pub bytecode_version: u32,
    pub build_id: String,
    pub binding_fingerprint: String,
    pub objects: Vec<ObjectRecord>,
    pub continuations: Vec<ContinuationRecord>,
    pub operations: Vec<PendingOperationRecord>,
    #[serde(with = "ordered_pairs")]
    pub globals: BTreeMap<JsString, Value>,
    pub roots: BTreeMap<String, ObjectId>,
    /// VM-managed module namespaces corresponding to the sorted build manifest.
    pub imports: Vec<ObjectId>,
    /// Class tag to the VM-owned prototype saved with the object graph.
    pub class_prototypes: BTreeMap<String, ObjectId>,
    pub host_instances: Vec<HostInstanceRecord>,
    pub module_states: Vec<ModuleStateRecord>,
    pub script_env: Option<ObjectId>,
    pub combines: BTreeMap<ObjectId, CombineState>,
    pub random_state: u64,
    /// Descriptions of the symbols the snapshot keeps alive, keyed by id.
    pub symbols: BTreeMap<SymbolId, Option<JsString>>,
    /// `Symbol.for` registry entries kept alive by the snapshot.
    #[serde(with = "ordered_pairs")]
    pub symbol_registry: BTreeMap<JsString, SymbolId>,
    /// Allocation high-water mark, preserved so restored ids stay fresh.
    pub next_symbol_id: SymbolId,
}

impl Snapshot {
    pub fn new(build_id: String) -> Self {
        Self {
            format_version: SNAPSHOT_FORMAT_VERSION,
            bytecode_version: BYTECODE_VERSION,
            build_id,
            binding_fingerprint: String::new(),
            objects: Vec::new(),
            continuations: Vec::new(),
            operations: Vec::new(),
            imports: Vec::new(),
            class_prototypes: BTreeMap::new(),
            host_instances: Vec::new(),
            module_states: Vec::new(),
            globals: BTreeMap::new(),
            roots: BTreeMap::new(),
            script_env: None,
            combines: BTreeMap::new(),
            random_state: crate::math::RANDOM_SEED,
            symbols: BTreeMap::new(),
            symbol_registry: BTreeMap::new(),
            next_symbol_id: 0,
        }
    }

    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string(self).map_err(|error| error.to_string())
    }

    pub fn from_json(source: &str) -> Result<Self, String> {
        serde_json::from_str(source).map_err(|error| error.to_string())
    }
}

impl Vm {
    pub fn snapshot(&self, roots: &[Value]) -> Result<Snapshot, VmError> {
        if !self.microtasks.is_empty() {
            return Err(VmError::InvalidState(
                "snapshot requires a drained microtask queue".to_owned(),
            ));
        }

        let mut tracer = Tracer::new(&self.heap, &self.continuations, &self.host_instances);
        for root in roots {
            tracer.mark_value(root)?;
        }
        for value in self.globals.values() {
            tracer.mark_value(value)?;
        }
        for import in &self.imports {
            tracer.mark_object(import.namespace)?;
        }
        for operation in self.operations.values() {
            tracer.mark_object(operation.promise)?;
        }
        for prototype in &self.class_prototypes {
            tracer.mark_object(*prototype)?;
        }
        for state in self.module_states.iter().flatten() {
            for slot in &state.slots {
                match slot {
                    HostSlot::Js(value) => tracer.mark_value(value)?,
                    HostSlot::JsList(values) => {
                        for value in values {
                            tracer.mark_value(value)?;
                        }
                    }
                }
            }
        }
        for id in self.builtins.to_map().values() {
            tracer.mark_object(*id)?;
        }
        let script_needs_env = self
            .build
            .function(self.build.script)
            .is_some_and(|function| function.needs_environment());
        match (script_needs_env, self.script_env) {
            (true, Some(id)) => tracer.mark_object(id)?,
            (false, None) => {}
            (true, None) => {
                return Err(VmError::InvalidState(
                    "script environment is missing".to_owned(),
                ));
            }
            (false, Some(id)) => {
                return Err(VmError::InvalidState(format!(
                    "script environment {id} is unexpected"
                )));
            }
        }
        tracer.drain()?;
        tracer.mark_ephemerons()?;
        if self
            .heap
            .resolutions
            .iter()
            .any(|id| !tracer.objects.contains(id))
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
        for (class, tag) in self.bindings.classes() {
            snapshot
                .class_prototypes
                .insert(tag.to_owned(), self.class_prototypes[class]);
        }
        snapshot.script_env = self.script_env;
        for id in &tracer.objects {
            let object = match self.heap.get(*id)? {
                Object::WeakMap { object, entries } => Object::WeakMap {
                    object: object.clone(),
                    entries: entries
                        .iter()
                        .filter(|(key, _)| tracer.objects.contains(key))
                        .map(|(&key, value)| (key, value.clone()))
                        .collect(),
                },
                object => object.clone(),
            };
            snapshot.objects.push(ObjectRecord { id: *id, object });
        }
        for (object, instance) in &self.host_instances {
            if !tracer.objects.contains(object) {
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
        for id in tracer.continuations_seen {
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
        for (id, operation) in &self.operations {
            snapshot.operations.push(PendingOperationRecord {
                id: *id,
                name: operation.name.clone(),
                schema: operation.schema,
                recovery: operation.recovery,
                state: operation.state.clone(),
                promise: operation.promise,
            });
        }
        for (id, state) in &self.heap.combines {
            if tracer.objects.contains(id) {
                snapshot.combines.insert(*id, state.clone());
            }
        }
        snapshot.next_symbol_id = self.heap.next_symbol_id();
        snapshot.random_state = self.random_state;
        snapshot.symbol_registry = self.heap.symbol_registry.clone();
        let mut symbol_ids = tracer.symbols;
        symbol_ids.insert(ITERATOR_SYMBOL);
        symbol_ids.insert(TO_PRIMITIVE_SYMBOL);
        symbol_ids.insert(SPECIES_SYMBOL);
        symbol_ids.extend(self.heap.symbol_registry.values().copied());
        for id in symbol_ids {
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
        heap.combines = snapshot.combines.clone();
        validate_symbol_tables(&snapshot)?;
        heap.restore_symbols(
            snapshot.symbols.clone(),
            snapshot.symbol_registry.clone(),
            snapshot.next_symbol_id,
        );
        if snapshot.imports.len() != build.imports.len() {
            return Err(VmError::InvalidSnapshot(format!(
                "import namespace count mismatch: snapshot {}, build {}",
                snapshot.imports.len(),
                build.imports.len()
            )));
        }
        validate_native_targets(&bindings, &heap)?;
        let class_prototypes = load_class_prototypes(&bindings, &heap, &snapshot.class_prototypes)?;
        let host_instances = load_host_instances(
            &bindings,
            &heap,
            &class_prototypes,
            &snapshot.host_instances,
        )?;
        let module_states = load_module_states(&bindings, &heap, &snapshot.module_states)?;
        let continuations = load_continuations(&build, &heap, &snapshot.continuations)?;
        for continuation in continuations.values() {
            require_value_objects(&heap, continuation.values.iter())?;
        }
        let script_env = load_script_env(&build, &heap, snapshot.script_env)?;
        let builtins = load_roots(&heap, &snapshot.roots)?;
        let operations = load_operations(&bindings, &heap, &snapshot.operations)?;
        let mut vm = Self::restored(
            build,
            bindings.clone(),
            crate::vm::RestoredState {
                heap,
                continuations,
                operations,
                class_prototypes,
                host_instances,
                module_states,
                globals: snapshot.globals,
                builtins,
                script_env,
            },
        );
        vm.restore_imports(&bindings, snapshot.imports, resolved_imports)?;
        validate_native_import_objects(&vm.heap, &vm.imports)?;
        check_heap_edges(&vm.build, &vm.heap, &bindings)?;
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
                    self.settle(operation.promise, Settlement::Fulfilled(Value::Undefined))?;
                }
                Recovery::Reject => {
                    self.settle(
                        operation.promise,
                        Settlement::Rejected(Value::String(operation.rejection.into())),
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

struct Tracer<'a> {
    heap: &'a Heap,
    continuations: &'a BTreeMap<ContinuationId, Continuation>,
    host_instances: &'a BTreeMap<ObjectId, HostInstance>,
    objects: BTreeSet<ObjectId>,
    symbols: BTreeSet<SymbolId>,
    continuations_seen: BTreeSet<ContinuationId>,
    weak_maps: Vec<ObjectId>,
    object_queue: VecDeque<ObjectId>,
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
            objects: BTreeSet::new(),
            symbols: BTreeSet::new(),
            continuations_seen: BTreeSet::new(),
            weak_maps: Vec::new(),
            object_queue: VecDeque::new(),
            continuation_queue: VecDeque::new(),
            edges: Vec::new(),
        }
    }

    fn mark_value(&mut self, value: &Value) -> Result<(), VmError> {
        match value {
            Value::Object(id) => self.mark_object(*id)?,
            Value::Symbol(id) => {
                self.symbols.insert(*id);
            }
            _ => {}
        }
        Ok(())
    }

    fn mark_object(&mut self, id: ObjectId) -> Result<(), VmError> {
        if !self.heap.contains(id) {
            return Err(VmError::InvalidState(format!(
                "snapshot root references missing object {id}"
            )));
        }
        if self.objects.insert(id) {
            self.object_queue.push_back(id);
        }
        Ok(())
    }

    fn mark_continuation(&mut self, promise: ObjectId, id: ContinuationId) -> Result<(), VmError> {
        if !self.continuations.contains_key(&id) {
            return Err(VmError::InvalidState(format!(
                "Promise {promise} references missing continuation {id}"
            )));
        }
        if self.continuations_seen.insert(id) {
            self.continuation_queue.push_back(id);
        }
        Ok(())
    }

    fn drain(&mut self) -> Result<(), VmError> {
        loop {
            let mut progressed = false;
            while let Some(id) = self.object_queue.pop_front() {
                progressed = true;
                self.walk_object(id)?;
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
        let heap = self.heap;
        loop {
            // Values can reveal more keys (or more WeakMaps) through strong edges.
            // A scan that queues no new objects cannot unlock more entries.
            for index in 0..self.weak_maps.len() {
                let Object::WeakMap { entries, .. } = heap.get(self.weak_maps[index])? else {
                    unreachable!("only WeakMaps are deferred");
                };
                for (key, value) in entries {
                    if self.objects.contains(key) {
                        self.mark_value(value)?;
                    }
                }
            }
            if self.object_queue.is_empty() {
                return Ok(());
            }
            self.drain()?;
        }
    }

    fn walk_object(&mut self, id: ObjectId) -> Result<(), VmError> {
        if matches!(self.heap.get(id)?, Object::WeakMap { .. }) {
            self.weak_maps.push(id);
        }
        let mut edges = std::mem::take(&mut self.edges);
        self.heap.write_edges(id, &mut edges)?;
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
                    self.symbols.insert(symbol);
                }
            }
        }
        edges.clear();
        self.edges = edges;
        if let Some(instance) = self.host_instances.get(&id) {
            let mut values = Vec::new();
            for slot in &instance.slots {
                match slot {
                    HostSlot::Js(value) => values.push(value.clone()),
                    HostSlot::JsList(items) => values.extend(items.iter().cloned()),
                }
            }
            for value in values {
                self.mark_value(&value)?;
            }
        }
        Ok(())
    }

    fn walk_continuation(&mut self, id: ContinuationId) -> Result<(), VmError> {
        let (objects, values) = {
            let continuation = self.continuations.get(&id).ok_or_else(|| {
                VmError::InvalidState(format!("continuation {id} does not exist"))
            })?;
            let mut objects = Vec::new();
            continuation.for_each_object(|object| objects.push(object));
            (objects, continuation.values.clone())
        };
        for object in objects {
            self.mark_object(object)?;
        }
        for value in &values {
            self.mark_value(value)?;
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

fn load_roots(heap: &Heap, roots: &BTreeMap<String, ObjectId>) -> Result<BuiltinRoots, VmError> {
    let expected = BuiltinRoots::default().to_map().len();
    if roots.len() != expected {
        return Err(VmError::InvalidSnapshot(format!(
            "snapshot has {} builtin roots, expected {expected}",
            roots.len()
        )));
    }
    let promise = required_root(roots, ROOT_PROMISE)?;
    let prototype = required_root(roots, ROOT_PROMISE_PROTOTYPE)?;
    let queue = required_root(roots, ROOT_QUEUE_MICROTASK)?;
    match heap.get(promise)? {
        Object::BuiltinStatic(function)
            if function.name == "Promise" && function.member.is_none() => {}
        _ => {
            return Err(VmError::InvalidSnapshot(
                "promise root is not the Promise constructor".to_owned(),
            ));
        }
    }
    if !matches!(heap.get(prototype)?, Object::Plain(_)) {
        return Err(VmError::InvalidSnapshot(
            "promise.prototype root is not a plain object".to_owned(),
        ));
    }
    match heap.get(queue)? {
        Object::BuiltinStatic(function)
            if function.name == "queueMicrotask" && function.member.is_none() => {}
        _ => {
            return Err(VmError::InvalidSnapshot(
                "queueMicrotask root is not the builtin".to_owned(),
            ));
        }
    }
    require_fixed_prototype(heap, promise, prototype, "Promise")?;
    let plain_root = |name| {
        let id = required_root(roots, name)?;
        if !matches!(heap.get(id), Ok(Object::Plain(_))) {
            return Err(VmError::InvalidSnapshot(format!(
                "{name} root is not a plain object"
            )));
        }
        Ok(id)
    };
    let builtin_root = |key, name| {
        let id = required_root(roots, key)?;
        if !matches!(heap.get(id), Ok(Object::BuiltinStatic(f)) if f.name == name && f.member.is_none())
        {
            return Err(VmError::InvalidSnapshot(format!(
                "{key} root is not the {name} constructor"
            )));
        }
        Ok(id)
    };
    let object_prototype = plain_root(ROOT_OBJECT_PROTOTYPE)?;
    if matches!(heap.get(object_prototype)?, Object::Plain(plain) if plain.prototype.is_some()) {
        return Err(VmError::InvalidSnapshot(
            "Object.prototype must have a null prototype".to_owned(),
        ));
    }
    let json = plain_root(ROOT_JSON)?;
    let object = builtin_root(ROOT_OBJECT, "Object")?;
    require_fixed_prototype(heap, object, object_prototype, "Object")?;
    let map = builtin_root("map", "Map")?;
    let map_prototype = plain_root("map.prototype")?;
    let map_iterator_prototype = plain_root("mapIterator.prototype")?;
    require_fixed_prototype(heap, map, map_prototype, "Map")?;
    let set = builtin_root("set", "Set")?;
    let set_prototype = plain_root("set.prototype")?;
    let set_iterator_prototype = plain_root("setIterator.prototype")?;
    require_fixed_prototype(heap, set, set_prototype, "Set")?;
    let weakmap = builtin_root(ROOT_WEAK_MAP, "WeakMap")?;
    let weakmap_prototype = plain_root(ROOT_WEAK_MAP_PROTOTYPE)?;
    require_fixed_prototype(heap, weakmap, weakmap_prototype, "WeakMap")?;
    let array_prototype = required_root(roots, ROOT_ARRAY_PROTOTYPE)?;
    if !matches!(heap.get(array_prototype)?, Object::Array(_)) {
        return Err(VmError::InvalidSnapshot(
            "Array.prototype root is not an array".to_owned(),
        ));
    }
    let array = builtin_root(ROOT_ARRAY, "Array")?;
    require_fixed_prototype(heap, array, array_prototype, "Array")?;
    let array_values = required_root(roots, ROOT_ARRAY_VALUES)?;
    if !matches!(heap.get(array_values), Ok(Object::BuiltinStatic(f)) if f.name == "Array" && f.member.as_deref() == Some("values"))
    {
        return Err(VmError::InvalidSnapshot(
            "array.values root is not the builtin".to_owned(),
        ));
    }
    let string_prototype = plain_root(ROOT_STRING_PROTOTYPE)?;
    let string = builtin_root(ROOT_STRING, "String")?;
    require_fixed_prototype(heap, string, string_prototype, "String")?;
    let regexp = builtin_root(ROOT_REGEXP, "RegExp")?;
    let regexp_prototype = plain_root(ROOT_REGEXP_PROTOTYPE)?;
    require_fixed_prototype(heap, regexp, regexp_prototype, "RegExp")?;
    if roots
        .iter()
        .any(|(name, id)| name != ROOT_REGEXP_PROTOTYPE && *id == regexp_prototype)
    {
        return Err(VmError::InvalidSnapshot(
            "RegExp.prototype aliases another builtin root".to_owned(),
        ));
    }
    let number_prototype = plain_root(ROOT_NUMBER_PROTOTYPE)?;
    let number = builtin_root(ROOT_NUMBER, "Number")?;
    require_fixed_prototype(heap, number, number_prototype, "Number")?;
    if !heap
        .get(number_prototype)?
        .own_property(PropertyKeyRef::String("constructor"))
        .is_some_and(|property| property.into_owned() == Property::Data(Value::Object(number)))
    {
        return Err(VmError::InvalidSnapshot(
            "Number prototype constructor is invalid".to_owned(),
        ));
    }
    let boolean_prototype = plain_root(ROOT_BOOLEAN_PROTOTYPE)?;
    let boolean_constructor = builtin_root(ROOT_BOOLEAN, "Boolean")?;
    require_fixed_prototype(heap, boolean_constructor, boolean_prototype, "Boolean")?;
    let symbol_prototype = plain_root(ROOT_SYMBOL_PROTOTYPE)?;
    let symbol = builtin_root(ROOT_SYMBOL, "Symbol")?;
    require_fixed_prototype(heap, symbol, symbol_prototype, "Symbol")?;
    let array_iterator_prototype = plain_root(ROOT_ARRAY_ITERATOR_PROTOTYPE)?;
    let string_iterator_prototype = plain_root(ROOT_STRING_ITERATOR_PROTOTYPE)?;
    let regexp_iterator_prototype = plain_root(ROOT_REGEXP_ITERATOR_PROTOTYPE)?;
    let string_iterator = required_root(roots, ROOT_STRING_ITERATOR)?;
    if !matches!(heap.get(string_iterator), Ok(Object::BuiltinStatic(f)) if f.name == "String" && f.member.as_deref() == Some("iterator"))
    {
        return Err(VmError::InvalidSnapshot(
            "string.iterator root is not the builtin".to_owned(),
        ));
    }
    let function_prototype = plain_root(ROOT_FUNCTION_PROTOTYPE)?;
    let error_prototype = plain_root(ROOT_ERROR_PROTOTYPE)?;
    let typeerror_prototype = plain_root(ROOT_TYPEERROR_PROTOTYPE)?;
    let rangeerror_prototype = plain_root(ROOT_RANGEERROR_PROTOTYPE)?;
    let referenceerror_prototype = plain_root(ROOT_REFERENCEERROR_PROTOTYPE)?;
    let syntaxerror_prototype = plain_root(ROOT_SYNTAXERROR_PROTOTYPE)?;
    let evalerror_prototype = plain_root(ROOT_EVALERROR_PROTOTYPE)?;
    let urierror_prototype = plain_root(ROOT_URIERROR_PROTOTYPE)?;
    let aggregateerror_prototype = plain_root(ROOT_AGGREGATEERROR_PROTOTYPE)?;
    let error_types = [
        (ROOT_TYPEERROR, "TypeError", typeerror_prototype),
        (ROOT_RANGEERROR, "RangeError", rangeerror_prototype),
        (
            ROOT_REFERENCEERROR,
            "ReferenceError",
            referenceerror_prototype,
        ),
        (ROOT_SYNTAXERROR, "SyntaxError", syntaxerror_prototype),
        (ROOT_EVALERROR, "EvalError", evalerror_prototype),
        (ROOT_URIERROR, "URIError", urierror_prototype),
        (
            ROOT_AGGREGATEERROR,
            "AggregateError",
            aggregateerror_prototype,
        ),
    ];
    let mut error_prototypes = BTreeSet::from([error_prototype]);
    for (root, name, prototype) in error_types {
        if !error_prototypes.insert(prototype)
            || !matches!(heap.get(prototype)?, Object::Plain(plain) if plain.prototype == Some(error_prototype))
        {
            return Err(VmError::InvalidSnapshot(format!(
                "{root} prototype has invalid identity or inheritance"
            )));
        }
        let constructor = builtin_root(root, name)?;
        require_fixed_prototype(heap, constructor, prototype, name)?;
        if !heap
            .get(prototype)?
            .own_property(PropertyKeyRef::String("constructor"))
            .is_some_and(|property| {
                property.attributes() == Attributes::METHOD
                    && property.into_owned() == Property::Data(Value::Object(constructor))
            })
        {
            return Err(VmError::InvalidSnapshot(format!(
                "{root} prototype constructor is invalid"
            )));
        }
    }
    let error = builtin_root(ROOT_ERROR, "Error")?;
    if !heap
        .get(error_prototype)?
        .own_property(PropertyKeyRef::String("constructor"))
        .is_some_and(|property| {
            property.attributes() == Attributes::METHOD
                && property.into_owned() == Property::Data(Value::Object(error))
        })
    {
        return Err(VmError::InvalidSnapshot(
            "Error prototype constructor is invalid".to_owned(),
        ));
    }
    require_fixed_prototype(heap, error, error_prototype, "Error")?;
    let rangeerror = builtin_root(ROOT_RANGEERROR, "RangeError")?;
    let referenceerror = builtin_root(ROOT_REFERENCEERROR, "ReferenceError")?;
    let syntaxerror = builtin_root(ROOT_SYNTAXERROR, "SyntaxError")?;
    let evalerror = builtin_root(ROOT_EVALERROR, "EvalError")?;
    let urierror = builtin_root(ROOT_URIERROR, "URIError")?;
    let aggregateerror = builtin_root(ROOT_AGGREGATEERROR, "AggregateError")?;
    let mut prototypes = BTreeSet::from([object_prototype]);
    for (name, id) in [
        (ROOT_STRING_PROTOTYPE, string_prototype),
        (ROOT_NUMBER_PROTOTYPE, number_prototype),
        (ROOT_BOOLEAN_PROTOTYPE, boolean_prototype),
        (ROOT_SYMBOL_PROTOTYPE, symbol_prototype),
        (ROOT_ARRAY_PROTOTYPE, array_prototype),
        (ROOT_ARRAY_ITERATOR_PROTOTYPE, array_iterator_prototype),
        (ROOT_STRING_ITERATOR_PROTOTYPE, string_iterator_prototype),
        (ROOT_REGEXP_ITERATOR_PROTOTYPE, regexp_iterator_prototype),
        (ROOT_FUNCTION_PROTOTYPE, function_prototype),
    ] {
        if !prototypes.insert(id) {
            return Err(VmError::InvalidSnapshot(format!(
                "{name} aliases another prototype root"
            )));
        }
    }
    let global_this = plain_root(ROOT_GLOBAL_THIS)?;
    // Its properties and prototype are mutable, but the root must be distinct.
    if roots
        .iter()
        .any(|(name, id)| name != ROOT_GLOBAL_THIS && *id == global_this)
    {
        return Err(VmError::InvalidSnapshot(
            "globalThis aliases another builtin root".to_owned(),
        ));
    }
    Ok(BuiltinRoots {
        global_this,
        map,
        map_prototype,
        map_iterator_prototype,
        set,
        set_prototype,
        set_iterator_prototype,
        weakmap,
        weakmap_prototype,
        promise,
        proxy: builtin_root(ROOT_PROXY, "Proxy")?,
        promise_prototype: prototype,
        queue_microtask: queue,
        object_prototype,
        array,
        array_prototype,
        array_values,
        string,
        string_prototype,
        regexp,
        regexp_prototype,
        number,
        number_prototype,
        boolean_constructor,
        boolean_prototype,
        array_iterator_prototype,
        string_iterator,
        string_iterator_prototype,
        regexp_iterator_prototype,
        function_prototype,
        error_prototype,
        typeerror_prototype,
        rangeerror_prototype,
        referenceerror_prototype,
        syntaxerror_prototype,
        evalerror_prototype,
        urierror_prototype,
        aggregateerror_prototype,
        error,
        typeerror: builtin_root(ROOT_TYPEERROR, "TypeError")?,
        rangeerror,
        referenceerror,
        syntaxerror,
        evalerror,
        urierror,
        aggregateerror,
        symbol,
        symbol_prototype,
        reflect: plain_root(ROOT_REFLECT)?,
        object,
        math: plain_root(ROOT_MATH)?,
        json,
    })
}

fn require_fixed_prototype(
    heap: &Heap,
    constructor: ObjectId,
    prototype: ObjectId,
    name: &str,
) -> Result<(), VmError> {
    if heap
        .get(constructor)?
        .own_property(PropertyKeyRef::String("prototype"))
        .is_some_and(|property| {
            property.attributes() == Attributes::FIXED
                && property.into_owned() == Property::Data(Value::Object(prototype))
        })
    {
        Ok(())
    } else {
        Err(VmError::InvalidSnapshot(format!(
            "{name}.prototype does not match the fixed prototype root"
        )))
    }
}

fn required_root(roots: &BTreeMap<String, ObjectId>, name: &str) -> Result<ObjectId, VmError> {
    roots
        .get(name)
        .copied()
        .ok_or_else(|| VmError::InvalidSnapshot(format!("snapshot is missing builtin root {name}")))
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

fn check_heap_edges(build: &Build, heap: &Heap, bindings: &Bindings) -> Result<(), VmError> {
    let mut edges = Vec::new();
    let mut prototypes = BTreeMap::new();
    for id in heap.ids() {
        if let Object::Native(native) = heap.get(id)?
            && !valid_native_target(bindings, native.target)
        {
            return Err(VmError::InvalidSnapshot(format!(
                "native object {id} references an undeclared binding"
            )));
        }
        if let Object::Function(function) = heap.get(id)?
            && function.func_id == build.script
        {
            return Err(VmError::InvalidSnapshot(format!(
                "function object {id} references the script function"
            )));
        }
        // The shared edge walk below checks the bound receiver and arguments.
        if let Object::Bound(bound) = heap.get(id)? {
            let callable = heap
                .get(bound.target)
                .is_ok_and(crate::builtins::is_callable_object);
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
                || (!string_indices.is_empty() && (pending.is_some() || !visited.is_empty())))
        {
            return Err(VmError::InvalidSnapshot(format!(
                "for-in iterator {id} has invalid cursor state"
            )));
        }
        heap.write_edges(id, &mut edges)?;
        if let Object::WeakMap { entries, .. } = heap.get(id)? {
            for (key, value) in entries {
                if matches!(
                    heap.get(*key),
                    Err(_) | Ok(Object::Environment(_) | Object::ForInIterator { .. })
                ) {
                    return Err(VmError::InvalidSnapshot(format!(
                        "WeakMap {id} has an invalid key {key}"
                    )));
                }
                push_value(&mut edges, value);
            }
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
    let mut identities = BTreeSet::new();
    for id in heap.ids() {
        if let Object::Native(native) = heap.get(id)? {
            if !valid_native_target(bindings, native.target) {
                return Err(VmError::InvalidSnapshot(format!(
                    "native object {id} references an undeclared binding"
                )));
            }
            if !identities.insert((native.target, native.constructable)) {
                return Err(VmError::InvalidSnapshot(format!(
                    "duplicate native object identity at {id}"
                )));
            }
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
            if let Value::Object(id) = value
                && let Object::Native(native) = heap.get(*id)?
                && !identities.insert((native.target, native.constructable))
            {
                return Err(VmError::InvalidSnapshot(format!(
                    "duplicate native import identity at {id}"
                )));
            }
        }
    }
    Ok(())
}

fn load_class_prototypes(
    bindings: &Bindings,
    heap: &Heap,
    records: &BTreeMap<String, ObjectId>,
) -> Result<Vec<ObjectId>, VmError> {
    if records.len() != bindings.classes_len() {
        return Err(VmError::InvalidSnapshot(format!(
            "class prototype count mismatch: snapshot {}, bindings {}",
            records.len(),
            bindings.classes_len()
        )));
    }
    let mut prototypes = vec![0; bindings.classes_len()];
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
        prototypes[class] = id;
    }
    Ok(prototypes)
}

fn load_host_instances(
    bindings: &Bindings,
    heap: &Heap,
    class_prototypes: &[ObjectId],
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
        if heap.prototype_of(record.object)? != Some(class_prototypes[class]) {
            return Err(VmError::InvalidSnapshot(format!(
                "host instance {} does not use the {} prototype",
                record.object, record.class
            )));
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
        match value {
            Value::Object(id) if !heap.contains(*id) => {
                return Err(VmError::InvalidSnapshot(format!(
                    "value references missing object {id}"
                )));
            }
            Value::Symbol(id) if !heap.symbols.contains_key(id) => {
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
    if record.func_id == build.script {
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
                                if !matches!(callback, Value::Object(id) if heap.get(*id).is_ok_and(crate::builtins::is_callable_object))
                                {
                                    return Err(VmError::InvalidSnapshot(format!(
                                        "Promise {id} reaction capability is not callable"
                                    )));
                                }
                            }
                        }
                        ReactionJob::Adopt { result } | ReactionJob::Combine { result, .. } => {
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
        let promise = heap.promise(*id).map_err(|_| {
            VmError::InvalidSnapshot(format!("combine key {id} is not a pending Promise"))
        })?;
        if !matches!(promise.state, PromiseState::Pending) {
            return Err(VmError::InvalidSnapshot(format!(
                "combine key {id} is not a pending Promise"
            )));
        }
        if state.remaining > state.count
            || state.results.len() != state.count
            || state.elements.len() != state.count
        {
            return Err(VmError::InvalidSnapshot(format!(
                "combine {id} has inconsistent remaining/count/results"
            )));
        }
        let states_ok = match state.kind {
            crate::heap::CombineKind::AllSettled => state.states.len() == state.count,
            _ => state.states.is_empty(),
        };
        if !states_ok {
            return Err(VmError::InvalidSnapshot(format!(
                "combine {id} has inconsistent state records"
            )));
        }
        for element in &state.elements {
            require_promise(heap, *element, &format!("combine {id} element"))?;
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
    records: &[PendingOperationRecord],
) -> Result<BTreeMap<u64, PendingOperation>, VmError> {
    let mut operations = BTreeMap::new();
    let mut operation_promises = BTreeSet::new();
    for record in records {
        if record.id == 0 || operations.contains_key(&record.id) {
            return Err(VmError::InvalidSnapshot(format!(
                "duplicate or invalid host operation id {}",
                record.id
            )));
        }
        let (_, recovery, rejection) =
            bindings
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
        match heap.get(record.promise) {
            Ok(Object::Promise(promise)) if matches!(promise.state, PromiseState::Pending) => {}
            _ => {
                return Err(VmError::InvalidSnapshot(format!(
                    "operation {:?} references non-pending Promise {}",
                    record.name, record.promise
                )));
            }
        }
        if !operation_promises.insert(record.promise) {
            return Err(VmError::InvalidSnapshot(format!(
                "Promise {} is associated with more than one host operation",
                record.promise
            )));
        }
        operations.insert(
            record.id,
            PendingOperation {
                name: record.name.clone(),
                schema: record.schema,
                recovery,
                rejection,
                state: record.state.clone(),
                promise: record.promise,
            },
        );
    }
    Ok(operations)
}
