use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};

use super::object::{OwnedEdge, push_state, push_value};
use super::types::{
    ArrayObject, Attributes, CombineState, DescriptorUpdate, EnvironmentObject, FunctionObject,
    HAS_INSTANCE_DESCRIPTION, HAS_INSTANCE_SYMBOL, ITERATOR_DESCRIPTION, ITERATOR_SYMBOL,
    MATCH_ALL_DESCRIPTION, MATCH_ALL_SYMBOL, MATCH_DESCRIPTION, MATCH_SYMBOL, NativeObject, Object,
    OrderedProperty, OwnProperty, PlainObject, PrivateElement, PrivateMemberKind, PromiseObject,
    PromiseState, Property, PropertyTable, REPLACE_DESCRIPTION, REPLACE_SYMBOL, SEARCH_DESCRIPTION,
    SEARCH_SYMBOL, SPECIES_DESCRIPTION, SPECIES_SYMBOL, SPLIT_DESCRIPTION, SPLIT_SYMBOL,
    TO_PRIMITIVE_DESCRIPTION, TO_PRIMITIVE_SYMBOL, WeakKey, array_index,
};
use crate::error::VmError;
use crate::gc::LiveSet;
use crate::value::{
    BindingId, JsString, ObjectId, PropertyKey, PropertyKeyRef, SymbolId, Value, ValueRef,
};

type PrivateElements = BTreeMap<(ObjectId, u32), PrivateElement>;

#[derive(Debug)]
pub(crate) struct Heap {
    objects: BTreeMap<ObjectId, Object>,
    next_id: ObjectId,
    /// Symbol descriptions. Missing entry and absent description are distinct.
    pub(crate) symbols: BTreeMap<SymbolId, Option<JsString>>,
    /// `Symbol.for` registry. Keys are registry strings; ordinary symbols are
    /// never interned by description.
    pub(crate) symbol_registry: BTreeMap<JsString, SymbolId>,
    next_symbol_id: SymbolId,
    pub(crate) combines: BTreeMap<ObjectId, CombineState>,
    pub(crate) private: BTreeMap<ObjectId, PrivateElements>,
    /// Uncalled resolution objects. Capture conservatively rejects any id in
    /// this set that the tracer did not reach. At a GC safepoint, unreachable
    /// resolutions can be discarded with their owning objects.
    pub(crate) resolutions: BTreeSet<ObjectId>,
    pub(crate) object_prototype: Option<ObjectId>,
    pub(crate) array_prototype: Option<ObjectId>,
    pub(crate) function_prototype: Option<ObjectId>,
    /// Shared internal prototype of every Promise allocated by this heap.
    pub(crate) promise_prototype: Option<ObjectId>,
}

impl Heap {
    pub(crate) fn new() -> Self {
        Self {
            objects: BTreeMap::new(),
            next_id: 1,
            symbols: BTreeMap::from([
                (ITERATOR_SYMBOL, Some(ITERATOR_DESCRIPTION.into())),
                (TO_PRIMITIVE_SYMBOL, Some(TO_PRIMITIVE_DESCRIPTION.into())),
                (SPECIES_SYMBOL, Some(SPECIES_DESCRIPTION.into())),
                (MATCH_SYMBOL, Some(MATCH_DESCRIPTION.into())),
                (MATCH_ALL_SYMBOL, Some(MATCH_ALL_DESCRIPTION.into())),
                (REPLACE_SYMBOL, Some(REPLACE_DESCRIPTION.into())),
                (SEARCH_SYMBOL, Some(SEARCH_DESCRIPTION.into())),
                (SPLIT_SYMBOL, Some(SPLIT_DESCRIPTION.into())),
                (HAS_INSTANCE_SYMBOL, Some(HAS_INSTANCE_DESCRIPTION.into())),
            ]),
            symbol_registry: BTreeMap::new(),
            next_symbol_id: HAS_INSTANCE_SYMBOL + 1,
            combines: BTreeMap::new(),
            private: BTreeMap::new(),
            resolutions: BTreeSet::new(),
            object_prototype: None,
            array_prototype: None,
            function_prototype: None,
            promise_prototype: None,
        }
    }

    pub(crate) fn sweep(&mut self, live: &LiveSet) -> usize {
        let before = self.objects.len();
        self.objects.retain(|id, object| {
            if !live.objects.contains(id) {
                return false;
            }
            match object {
                Object::WeakMap { entries, .. } => {
                    entries.retain(|key, _| live.contains_weak_key(*key));
                }
                Object::WeakSet { entries, .. } => {
                    entries.retain(|key| live.contains_weak_key(*key));
                }
                _ => {}
            }
            true
        });
        self.combines.retain(|id, _| live.objects.contains(id));
        self.private.retain(|id, _| live.objects.contains(id));
        self.resolutions.retain(|id| live.objects.contains(id));
        self.symbols.retain(|id, _| live.symbols.contains(id));
        before - self.objects.len()
    }

    pub(crate) fn alloc(&mut self, object: Object) -> ObjectId {
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .expect("object id space exhausted");
        self.objects.insert(id, object);
        id
    }

    pub(crate) fn weak_key(&self, value: &Value) -> Option<WeakKey> {
        match value.view() {
            ValueRef::Object(id) => Some(WeakKey::Object(id)),
            ValueRef::Symbol(id) => {
                let description = self.symbols.get(&id)?;
                let registered = description
                    .as_ref()
                    .and_then(|key| self.symbol_registry.get(key));
                (registered != Some(&id)).then_some(WeakKey::Symbol(id))
            }
            _ => None,
        }
    }

    /// A fresh `Symbol(desc)`. `None` and an empty string stay distinct.
    pub(crate) fn alloc_symbol(
        &mut self,
        description: Option<JsString>,
    ) -> Result<SymbolId, VmError> {
        let id = self.next_symbol_id;
        self.next_symbol_id = self
            .next_symbol_id
            .checked_add(1)
            .ok_or_else(|| VmError::InvalidState("symbol id space exhausted".to_owned()))?;
        self.symbols.insert(id, description);
        Ok(id)
    }

    /// `Symbol.for(key)`: interns by registry key only.
    pub(crate) fn intern_symbol(&mut self, key: JsString) -> Result<SymbolId, VmError> {
        if let Some(id) = self.symbol_registry.get(&key) {
            return Ok(*id);
        }
        let id = self.alloc_symbol(Some(key.clone()))?;
        self.symbol_registry.insert(key, id);
        Ok(id)
    }

    pub(crate) fn next_symbol_id(&self) -> SymbolId {
        self.next_symbol_id
    }

    /// Binds a validated snapshot's symbol tables. Callers must reject the
    /// snapshot first when any entry is inconsistent.
    pub(crate) fn restore_symbols(
        &mut self,
        symbols: BTreeMap<SymbolId, Option<JsString>>,
        registry: BTreeMap<JsString, SymbolId>,
        next_symbol_id: SymbolId,
    ) {
        self.symbols = symbols;
        self.symbol_registry = registry;
        self.next_symbol_id = next_symbol_id;
    }

    pub(crate) fn alloc_promise(&mut self) -> ObjectId {
        self.alloc(Object::Promise(PromiseObject {
            state: PromiseState::Pending,
            reactions: Vec::new(),
            locked: false,
            adopting: None,
            props: PropertyTable::new(),
            symbol_props: PropertyTable::new(),
            prototype: self.promise_prototype,
            extensible: true,
        }))
    }

    /// Object edges plus combine-state edges. This is the reachability list.
    pub(crate) fn write_edges(
        &self,
        id: ObjectId,
        out: &mut Vec<OwnedEdge>,
    ) -> Result<(), VmError> {
        self.get(id)?.write_edges(out);
        if let Some(state) = self.combines.get(&id) {
            for element in &state.elements {
                out.push(OwnedEdge::Object(*element));
            }
            for value in &state.results {
                push_value(out, value);
            }
            for recorded in &state.states {
                push_state(out, recorded);
            }
            if let Some(value) = &state.first {
                push_value(out, value);
            }
        }
        if let Some(elements) = self.private.get(&id) {
            for (&(brand, _), element) in elements {
                out.push(OwnedEdge::Object(brand));
                if let PrivateElement::Field(value) = element {
                    push_value(out, value);
                }
            }
        }
        Ok(())
    }

    pub(crate) fn install_private_methods(
        &mut self,
        brand: ObjectId,
        receiver: ObjectId,
        is_static: bool,
    ) -> bool {
        let Some(Object::Function(function)) = self.objects.get(&brand) else {
            unreachable!("validated class")
        };
        let class = function.class.as_ref().expect("validated class");
        let mut indices = class
            .source
            .private
            .iter()
            .filter_map(|(&index, member)| {
                (member.is_static == is_static && member.kind != PrivateMemberKind::Field)
                    .then_some(index)
            })
            .peekable();
        if indices.peek().is_none() {
            return true;
        }
        let elements = self.private.entry(receiver).or_default();
        for index in indices {
            let Entry::Vacant(entry) = elements.entry((brand, index)) else {
                return false;
            };
            entry.insert(PrivateElement::Installed);
        }
        true
    }

    pub(crate) fn alloc_plain(&mut self) -> ObjectId {
        self.alloc_plain_with(self.object_prototype)
    }

    pub(crate) fn alloc_plain_with(&mut self, prototype: Option<ObjectId>) -> ObjectId {
        self.alloc(Object::Plain(PlainObject::new(prototype)))
    }

    pub(crate) fn alloc_for_in_iterator(&mut self, source: &Value) -> ObjectId {
        let current = match source.view() {
            ValueRef::Undefined | ValueRef::Null => None,
            ValueRef::Object(id) => Some(id),
            // The primitive wrapper prototypes do not exist in this subset.
            _ => self.object_prototype,
        };
        let string_length = match source.view() {
            ValueRef::String(string) => string.unit_len(),
            _ => 0,
        };
        self.alloc(Object::ForInIterator {
            current,
            pending: None,
            visited: BTreeSet::new(),
            string_indices: 0..string_length,
        })
    }

    pub(crate) fn alloc_array(&mut self, elements: Vec<Value>) -> ObjectId {
        self.alloc(Object::Array(ArrayObject {
            length: elements.len(),
            length_writable: true,
            elements: elements
                .into_iter()
                .enumerate()
                .map(|(index, value)| (index, (Property::Data(value), Attributes::ORDINARY)))
                .collect(),
            props: PropertyTable::new(),
            symbol_props: PropertyTable::new(),
            prototype: self.array_prototype,
            extensible: true,
        }))
    }

    pub(crate) fn alloc_function(&mut self, func_id: u32, env: ObjectId) -> ObjectId {
        self.alloc(Object::Function(FunctionObject {
            func_id,
            env,
            home: None,
            dynamic_name: None,
            class: None,
            props: PropertyTable::new(),
            symbol_props: PropertyTable::new(),
            prototype: self.function_prototype,
            extensible: true,
        }))
    }

    pub(crate) fn alloc_intrinsic(&mut self, intrinsic: crate::intrinsic::IntrinsicId) -> ObjectId {
        let def = intrinsic.definition();
        self.alloc(Object::Native(NativeObject::Intrinsic {
            intrinsic,
            object: PlainObject::callable(
                self.function_prototype,
                f64::from(def.length),
                def.key.initial_name(),
            ),
        }))
    }

    pub(crate) fn alloc_environment(
        &mut self,
        parent: Option<ObjectId>,
        bindings: BTreeMap<BindingId, Value>,
    ) -> ObjectId {
        self.alloc(Object::Environment(EnvironmentObject { parent, bindings }))
    }

    pub(crate) fn insert_restore(&mut self, id: ObjectId, object: Object) -> Result<(), VmError> {
        if !object.valid_property_tables() {
            return Err(VmError::InvalidSnapshot(format!(
                "object {id} has invalid property tables"
            )));
        }
        if self.objects.insert(id, object).is_some() {
            return Err(VmError::InvalidSnapshot(format!(
                "duplicate object id {id}"
            )));
        }
        Ok(())
    }

    pub(crate) fn finish_restore(&mut self) {
        self.next_id = self.objects.keys().next_back().map_or(1, |id| id + 1);
        self.resolutions = self
            .objects
            .iter()
            .filter_map(|(id, object)| match object {
                Object::Resolution(resolution)
                    if resolution.track_unreachable && !resolution.called =>
                {
                    Some(*id)
                }
                _ => None,
            })
            .collect();
    }

    pub(crate) fn contains(&self, id: ObjectId) -> bool {
        self.objects.contains_key(&id)
    }

    pub(crate) fn object_count(&self) -> usize {
        self.objects.len()
    }

    pub(crate) fn get(&self, id: ObjectId) -> Result<&Object, VmError> {
        self.objects.get(&id).ok_or_else(|| missing_object(id))
    }

    /// The ultimate target for operations with no Proxy trap.
    pub(crate) fn proxy_target(&self, mut id: ObjectId) -> Result<ObjectId, VmError> {
        let mut remaining = self.objects.len();
        while let Object::Proxy { target, .. } = self.get(id)? {
            if remaining == 0 {
                return Err(VmError::InvalidState(format!(
                    "proxy target chain at object {id} is cyclic"
                )));
            }
            remaining -= 1;
            id = *target;
        }
        Ok(id)
    }

    /// Read an internal prototype with no Proxy trap.
    pub(crate) fn prototype_of(&self, id: ObjectId) -> Result<Option<ObjectId>, VmError> {
        Ok(self.get(self.proxy_target(id)?)?.prototype())
    }

    pub(crate) fn get_mut(&mut self, id: ObjectId) -> Result<&mut Object, VmError> {
        self.objects.get_mut(&id).ok_or_else(|| missing_object(id))
    }

    /// `None` if missing, otherwise whether the object is an environment.
    pub(crate) fn is_environment(&self, id: ObjectId) -> Option<bool> {
        self.objects
            .get(&id)
            .map(|object| matches!(object, Object::Environment(_)))
    }

    pub(crate) fn promise(&self, id: ObjectId) -> Result<&PromiseObject, VmError> {
        self.get(id)?.as_promise().ok_or_else(|| not_promise(id))
    }

    pub(crate) fn promise_mut(&mut self, id: ObjectId) -> Result<&mut PromiseObject, VmError> {
        self.get_mut(id)?
            .as_promise_mut()
            .ok_or_else(|| not_promise(id))
    }

    pub(crate) fn environment(&self, id: ObjectId) -> Result<&EnvironmentObject, VmError> {
        self.get(id)?
            .as_environment()
            .ok_or_else(|| not_environment(id))
    }

    pub(crate) fn environment_mut(
        &mut self,
        id: ObjectId,
    ) -> Result<&mut EnvironmentObject, VmError> {
        self.get_mut(id)?
            .as_environment_mut()
            .ok_or_else(|| not_environment(id))
    }

    pub(crate) fn function(&self, id: ObjectId) -> Option<&FunctionObject> {
        self.objects.get(&id).and_then(Object::as_function)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (ObjectId, &Object)> {
        self.objects.iter().map(|(id, object)| (*id, object))
    }

    pub(crate) fn ids(&self) -> Vec<ObjectId> {
        self.objects.keys().copied().collect()
    }

    pub(crate) fn intern_native(
        &mut self,
        target: crate::binding::NativeTarget,
        constructable: bool,
        length: usize,
        name: JsString,
    ) -> ObjectId {
        if let Some(id) = self.objects.iter().find_map(|(id, object)| match object {
            Object::Native(NativeObject::Host {
                target: current,
                constructable: flag,
                ..
            }) if *current == target && *flag == constructable => Some(*id),
            _ => None,
        }) {
            return id;
        }
        self.alloc(Object::Native(NativeObject::Host {
            target,
            constructable,
            object: PlainObject::callable(self.function_prototype, length as f64, name),
        }))
    }

    pub(crate) fn set_prototype(
        &mut self,
        id: ObjectId,
        prototype: Option<ObjectId>,
    ) -> Result<bool, VmError> {
        let id = self.proxy_target(id)?;
        let object = self.get(id)?;
        if object.prototype() == prototype {
            return Ok(true);
        }
        if self.object_prototype == Some(id) || !object.is_extensible() {
            return Ok(false);
        }
        // All existing links were checked at restore or installed here. A new
        // link can only form a cycle if its chain leads back to the receiver.
        let mut current = prototype;
        while let Some(ancestor) = current {
            let ancestor = self.proxy_target(ancestor)?;
            if ancestor == id {
                return Ok(false);
            }
            current = self.get(ancestor)?.prototype();
        }
        *self
            .get_mut(id)?
            .prototype_mut()
            .expect("every extensible object has a prototype slot") = prototype;
        Ok(true)
    }

    pub(crate) fn has_own(&self, id: ObjectId, key: &PropertyKey) -> Result<bool, VmError> {
        Ok(self.get(id)?.own_property(key.borrowed()).is_some())
    }

    pub(crate) fn delete_own(
        &mut self,
        id: ObjectId,
        key: PropertyKeyRef<'_>,
    ) -> Result<bool, VmError> {
        let object = self.get_mut(id)?;
        if let Object::Array(array) = &mut *object {
            if key.is_str("length") {
                return Ok(false);
            }
            if let Some(index) = array_index(key) {
                if let Entry::Occupied(entry) = array.elements.entry(index) {
                    if !entry.get().1.configurable {
                        return Ok(false);
                    }
                    entry.remove();
                }
                return Ok(true);
            }
        }
        if object.string_own_property(key).is_some() {
            return Ok(false);
        }
        let Some((props, symbols)) = object.tables_mut() else {
            // Opaque objects expose no own script properties.
            return Ok(true);
        };
        Ok(match key {
            PropertyKeyRef::String(key) => props.remove_configurable(&JsString::from(key)),
            PropertyKeyRef::JsString(key) => props.remove_configurable(key),
            PropertyKeyRef::Symbol(id) => symbols.remove_configurable(&id),
        })
    }

    /// String-keyed convenience for internal builtin setup.
    pub(crate) fn set_own_string(
        &mut self,
        id: ObjectId,
        key: &str,
        value: Value,
    ) -> Result<(), VmError> {
        self.set_own(id, key.into(), value)
    }

    pub(crate) fn define_data(
        &mut self,
        id: ObjectId,
        key: impl Into<PropertyKey>,
        value: Value,
        attributes: Attributes,
    ) -> Result<(), VmError> {
        self.write_own_property(id, key.into(), Property::Data(value), Some(attributes))
    }

    /// Writes an own data property. Does not walk the chain or apply readonly checks.
    pub(crate) fn set_own(
        &mut self,
        id: ObjectId,
        key: PropertyKey,
        value: Value,
    ) -> Result<(), VmError> {
        self.write_own_property(id, key, Property::Data(value), None)
    }

    pub(crate) fn apply_descriptor(
        &mut self,
        id: ObjectId,
        key: PropertyKey,
        update: DescriptorUpdate,
    ) -> Result<bool, VmError> {
        let object = self.get(id)?;
        if object.tables().is_none() {
            return Ok(false);
        }
        if let Object::Array(array) = object
            && !array.length_writable
            && array_index(key.borrowed()).is_some_and(|index| index >= array.length)
        {
            return Ok(false);
        }
        let current = object.own_property(key.borrowed());
        if let Some(property @ (OwnProperty::StringLength(_) | OwnProperty::StringIndex(_))) =
            current
        {
            return Ok(update.allows_fixed_string(property));
        }
        let Some((property, attrs)) = update.complete(object.is_extensible(), current) else {
            return Ok(false);
        };
        if key.borrowed().is_str("length")
            && let Object::Array(array) = self.get_mut(id)?
        {
            let Property::Data(length) = property else {
                unreachable!("validated array length descriptor")
            };
            let ValueRef::Number(length) = length.view() else {
                unreachable!("validated array length descriptor")
            };
            return Ok(array.set_length(length as usize, attrs.writable));
        }
        self.write_own_property(id, key, property, Some(attrs))?;
        Ok(true)
    }

    /// Storage only: script callers validate assignment or descriptors first.
    fn write_own_property(
        &mut self,
        id: ObjectId,
        key: PropertyKey,
        property: Property,
        attributes: Option<Attributes>,
    ) -> Result<(), VmError> {
        let object = self.get_mut(id)?;
        if object.string_own_property(key.borrowed()).is_some() {
            return Err(VmError::InvalidState(format!(
                "object {id} has a fixed virtual string property"
            )));
        }
        if let Object::Array(array) = &mut *object
            && let Some(index) = array_index(key.borrowed())
        {
            let entry = array
                .elements
                .entry(index)
                .or_insert((Property::Data(Value::UNDEFINED), Attributes::ORDINARY));
            entry.0 = property;
            if let Some(attributes) = attributes {
                entry.1 = attributes;
            }
            array.length = array.length.max(index + 1);
            return Ok(());
        }
        let (props, symbols) = object.tables_mut().ok_or_else(|| {
            VmError::InvalidState(format!("object {id} does not expose script properties"))
        })?;
        insert_table(props, symbols, key, attributes, |_| property);
        Ok(())
    }

    pub(crate) fn define_property(
        &mut self,
        id: ObjectId,
        key: PropertyKey,
        property: Property,
    ) -> Result<(), VmError> {
        let plain = match self.get_mut(id)? {
            Object::Plain(plain)
            | Object::Map { object: plain, .. }
            | Object::Set { object: plain, .. }
            | Object::WeakMap { object: plain, .. }
            | Object::WeakSet { object: plain, .. }
            | Object::CollectionIterator { object: plain, .. }
            | Object::ArrayIterator { object: plain, .. }
            | Object::StringIterator { object: plain, .. }
            | Object::RegExpStringIterator { object: plain, .. } => plain,
            _ => {
                return Err(VmError::InvalidState(
                    "property definition requires a plain object".to_owned(),
                ));
            }
        };
        if !plain.extensible {
            return Err(VmError::InvalidState(
                "literal definition requires an extensible object".to_owned(),
            ));
        }
        let attributes = match property {
            Property::Data(_) => Attributes::ORDINARY,
            Property::Accessor { .. } => Attributes::ACCESSOR,
        };
        insert_table(
            &mut plain.props,
            &mut plain.symbol_props,
            key,
            Some(attributes),
            |old| {
                let mut property = property;
                if let Property::Accessor { get, set } = &mut property
                    && let Some(Property::Accessor {
                        get: old_get,
                        set: old_set,
                    }) = old
                {
                    *get = get.or(*old_get);
                    *set = set.or(*old_set);
                }
                property
            },
        );
        Ok(())
    }

    pub(crate) fn get_binding(&self, env: ObjectId, binding: BindingId) -> Result<Value, VmError> {
        let Some(owner) = self.binding_owner(env, binding)? else {
            return Err(VmError::InvalidState(format!(
                "binding {binding} is not initialized"
            )));
        };
        self.environment(owner)?
            .bindings
            .get(&binding)
            .cloned()
            .ok_or_else(|| VmError::InvalidState(format!("binding {binding} is not initialized")))
    }

    pub(crate) fn find_binding(
        &self,
        env: ObjectId,
        binding: BindingId,
    ) -> Result<Option<Value>, VmError> {
        Ok(match self.binding_owner(env, binding)? {
            Some(owner) => self.environment(owner)?.bindings.get(&binding).cloned(),
            None => None,
        })
    }

    pub(crate) fn set_binding(
        &mut self,
        env: ObjectId,
        binding: BindingId,
        value: Value,
    ) -> Result<(), VmError> {
        let Some(owner) = self.binding_owner(env, binding)? else {
            return Err(VmError::InvalidState(format!(
                "binding {binding} is not declared"
            )));
        };
        self.environment_mut(owner)?.bindings.insert(binding, value);
        Ok(())
    }

    pub(crate) fn declare_binding(
        &mut self,
        env: ObjectId,
        binding: BindingId,
    ) -> Result<(), VmError> {
        self.environment_mut(env)?
            .bindings
            .entry(binding)
            .or_insert(Value::UNDEFINED);
        Ok(())
    }

    fn binding_owner(
        &self,
        env: ObjectId,
        binding: BindingId,
    ) -> Result<Option<ObjectId>, VmError> {
        let mut current = Some(env);
        while let Some(id) = current {
            let environment = self.environment(id)?;
            if environment.bindings.contains_key(&binding) {
                return Ok(Some(id));
            }
            current = environment.parent;
        }
        Ok(None)
    }
}

pub(super) fn lookup_tables<'m, V>(
    props: &'m PropertyTable<JsString, V>,
    symbol_props: &'m PropertyTable<SymbolId, V>,
    key: PropertyKeyRef<'_>,
) -> Option<&'m OrderedProperty<V>> {
    match key {
        PropertyKeyRef::String(key) => props.entry(&JsString::from(key)),
        PropertyKeyRef::JsString(key) => props.entry(key),
        PropertyKeyRef::Symbol(id) => symbol_props.entry(&id),
    }
}

fn insert_table<V>(
    props: &mut PropertyTable<JsString, V>,
    symbol_props: &mut PropertyTable<SymbolId, V>,
    key: PropertyKey,
    attributes: Option<Attributes>,
    value: impl FnOnce(Option<&V>) -> V,
) {
    match key {
        PropertyKey::String(key) => props.update(key, attributes, value),
        PropertyKey::Symbol(id) => symbol_props.update(id, attributes, value),
    }
}

pub(crate) fn object_id(value: &Value, what: &str) -> Result<ObjectId, VmError> {
    match value.view() {
        ValueRef::Object(id) => Ok(id),
        _ => Err(VmError::InvalidState(what.to_owned())),
    }
}

fn missing_object(id: ObjectId) -> VmError {
    VmError::InvalidState(format!("object {id} does not exist"))
}

fn not_promise(id: ObjectId) -> VmError {
    VmError::InvalidState(format!("object {id} is not a Promise"))
}

fn not_environment(id: ObjectId) -> VmError {
    VmError::InvalidState(format!("object {id} is not an environment"))
}
