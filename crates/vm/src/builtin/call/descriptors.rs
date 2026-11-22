//! Property-key enumeration, descriptors, define/assign machinery.

use super::UNDEFINED_VALUE;
use crate::builtin::string::{utf16_len, utf16_unit_string};
use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::heap::{
    Attributes, DescriptorUpdate, Object, OwnProperty, Property, string_own_property,
};
use crate::host::Host;
use crate::ops;
use crate::runtime::{Arguments, Invocation};
use crate::value::{ObjectId, PropertyKey, PropertyKeyRef, Value, ValueRef};
use crate::vm::Vm;
impl Vm {
    /// `Object.hasOwn(O, P)`.
    ///
    /// ToObject runs before ToPropertyKey, so a nullish target throws without
    /// evaluating a key conversion. The lookup is [[GetOwnProperty]], which
    /// gives proxy traps their invariants and exposes string indices.
    pub(crate) fn object_has_own(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let target = args.first().unwrap_or(&UNDEFINED_VALUE);
        if matches!(target.view(), ValueRef::Undefined | ValueRef::Null) {
            return self.throw_type_error(roots, "Object.hasOwn requires a non-nullish value");
        }
        let object = self.coercible_object(target);
        let roots = &roots.with(&object);
        let key =
            match self.coerce_property_key(host, roots, args.get(1).unwrap_or(&UNDEFINED_VALUE))? {
                Ok(key) => key,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
        let roots = &roots.with(&key);
        match self.own_descriptor(host, roots, object, key.borrowed())? {
            Ok(descriptor) => Ok(Invocation::Value(Value::boolean(descriptor.is_some()))),
            Err(error) => Ok(Invocation::Thrown(error)),
        }
    }

    pub(crate) fn object_keys(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let keys = match args.first().map(Value::view) {
            None | Some(ValueRef::Undefined | ValueRef::Null) => {
                return self.throw_type_error(roots, "Object.keys requires a non-nullish value");
            }
            Some(ValueRef::Object(id)) => match self.enumerable_string_keys(host, roots, id)? {
                Ok(keys) => keys.into_iter().map(Value::string).collect(),
                Err(error) => return Ok(Invocation::Thrown(error)),
            },
            Some(ValueRef::String(text)) => (0..utf16_len(text))
                .map(|index| Value::string(index.to_string().into()))
                .collect(),
            _ => Vec::new(),
        };
        Ok(Invocation::Value(Value::object(
            self.heap.alloc_array(keys),
        )))
    }

    /// `Object.getOwnPropertyNames` / `Object.getOwnPropertySymbols`.
    ///
    /// [[OwnPropertyKeys]] order is preserved, so proxy traps run once and the
    /// filter keeps string keys ahead of symbol keys. Primitive strings expose
    /// their UTF-16 index keys and `length`; other primitives have none.
    pub(crate) fn object_own_property_keys(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
        symbols: bool,
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let mut values = Vec::new();
        match args.first().map(Value::view) {
            None | Some(ValueRef::Undefined | ValueRef::Null) => {
                return self.throw_type_error(
                    roots,
                    if symbols {
                        "Object.getOwnPropertySymbols requires a non-nullish value"
                    } else {
                        "Object.getOwnPropertyNames requires a non-nullish value"
                    },
                );
            }
            Some(ValueRef::Object(id)) => {
                let keys = match self.own_keys(host, roots, id, false)? {
                    Ok(keys) => keys,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                };
                for key in keys {
                    match key {
                        PropertyKey::String(name) if !symbols => values.push(Value::string(name)),
                        PropertyKey::Symbol(symbol) if symbols => {
                            values.push(Value::symbol(symbol))
                        }
                        _ => {}
                    }
                }
            }
            Some(ValueRef::String(text)) if !symbols => {
                for index in 0..utf16_len(text) {
                    values.push(Value::string(index.to_string().into()));
                }
                values.push(Value::string("length".into()));
            }
            Some(_) => {}
        }
        Ok(Invocation::Value(Value::object(
            self.heap.alloc_array(values),
        )))
    }

    pub(crate) fn object_values_or_entries(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
        entries: bool,
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let mut result = Vec::new();
        match args.first().map(|value| (value, value.view())) {
            None | Some((_, ValueRef::Undefined | ValueRef::Null)) => {
                return self
                    .throw_type_error(roots, "Object.values/entries requires a non-nullish value");
            }
            Some((target, ValueRef::Object(id))) => {
                let keys = match self.own_keys(host, roots, id, false)? {
                    Ok(keys) => keys,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                };
                let roots = &roots.with(&keys);
                for key in &keys {
                    let PropertyKey::String(name) = key else {
                        continue;
                    };
                    let roots = &roots.with(&result);
                    match self.own_descriptor(host, roots, id, key.borrowed())? {
                        Ok(Some((_, attrs))) if attrs.enumerable => {}
                        Ok(_) => continue,
                        Err(error) => return Ok(Invocation::Thrown(error)),
                    }
                    let value = match self.read_property(host, roots, target, key.borrowed())? {
                        Invocation::Value(value) => value,
                        thrown @ Invocation::Thrown(_) => return Ok(thrown),
                    };
                    result.push(if entries {
                        Value::object(
                            self.heap
                                .alloc_array(vec![Value::string(name.clone()), value]),
                        )
                    } else {
                        value
                    });
                }
            }
            Some((_, ValueRef::String(text))) => {
                for (index, unit) in text.units().enumerate() {
                    let value = Value::string(utf16_unit_string(unit));
                    result.push(if entries {
                        Value::object(
                            self.heap
                                .alloc_array(vec![Value::string(index.to_string().into()), value]),
                        )
                    } else {
                        value
                    });
                }
            }
            _ => {}
        }
        Ok(Invocation::Value(Value::object(
            self.heap.alloc_array(result),
        )))
    }

    pub(crate) fn object_from_entries(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let result = self.heap.alloc_plain();
        let root_view = (args, result);
        let roots = &roots.with(&root_view);
        let iterable = args.first().unwrap_or(&UNDEFINED_VALUE);
        let iterator_pair = match self.get_iterator(host, roots, iterable)? {
            Ok(pair) => pair,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let roots = &roots.with(&iterator_pair);
        let (iterator, next) = &iterator_pair;
        loop {
            let item = match self.iterator_step(host, roots, iterator, next)? {
                Ok(Some(item)) => item,
                Ok(None) => return Ok(Invocation::Value(Value::object(result))),
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
            let roots = &roots.with(&item);
            if let Err(error) = self.add_object_entry(host, roots, result, &item)? {
                // IteratorClose preserves the original throw over a return-method throw.
                self.close_iterator(host, &roots.with(&error), iterator)?;
                return Ok(Invocation::Thrown(error));
            }
        }
    }

    fn add_object_entry(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        result: ObjectId,
        item: &Value,
    ) -> Result<Result<(), Value>, VmError> {
        if !matches!(item.view(), ValueRef::Object(_)) {
            return Ok(Err(self.type_error(
                roots,
                "Object.fromEntries entry must be an object",
            )?));
        }
        let key = match self.read_property(host, roots, item, PropertyKeyRef::String("0"))? {
            Invocation::Value(key) => key,
            Invocation::Thrown(error) => return Ok(Err(error)),
        };
        let roots = &roots.with(&key);
        let value = match self.read_property(host, roots, item, PropertyKeyRef::String("1"))? {
            Invocation::Value(value) => value,
            Invocation::Thrown(error) => return Ok(Err(error)),
        };
        let key = match self.coerce_property_key(host, &roots.with(&value), &key)? {
            Ok(key) => key,
            Err(error) => return Ok(Err(error)),
        };
        self.heap.set_own(result, key, value)?;
        Ok(Ok(()))
    }

    /// `Object.groupBy(items, callback)`.
    ///
    /// Groups keep the first occurrence order of their coerced property keys.
    /// The result is a null-prototype object whose group arrays are ordinary
    /// writable, enumerable, configurable data properties. Every abrupt exit
    /// after GetIterator closes the iterator and keeps the original throw.
    pub(crate) fn object_group_by(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let items = args.first().unwrap_or(&UNDEFINED_VALUE);
        if matches!(items.view(), ValueRef::Undefined | ValueRef::Null) {
            return self.throw_type_error(roots, "Object.groupBy requires a non-nullish value");
        }
        let callback = args.get(1).unwrap_or(&UNDEFINED_VALUE);
        if !self.is_callable(callback) {
            return self.throw_type_error(roots, "Object.groupBy requires a callable callback");
        }
        let iterator_pair = match self.get_iterator(host, roots, items)? {
            Ok(pair) => pair,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let roots = &roots.with(&iterator_pair);
        let (iterator, next) = &iterator_pair;
        let mut groups: Vec<(PropertyKey, Vec<Value>)> = Vec::new();
        let mut index = 0u64;
        loop {
            let roots = &roots.with(&groups);
            if index >= (1u64 << 53) - 1 {
                let error =
                    self.type_error(roots, "Object.groupBy exceeded the maximum group count")?;
                self.close_iterator(host, &roots.with(&error), iterator)?;
                return Ok(Invocation::Thrown(error));
            }
            let item = match self.iterator_step(host, roots, iterator, next)? {
                Ok(Some(item)) => item,
                Ok(None) => break,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
            let roots = &roots.with(&item);
            let call_args = [item.clone(), Value::number(index as f64)];
            let roots = &roots.with(&call_args);
            let key = match self.invoke(
                host,
                roots,
                callback,
                Arguments::Values(&call_args),
                &UNDEFINED_VALUE,
            )? {
                Invocation::Value(key) => key,
                Invocation::Thrown(error) => {
                    self.close_iterator(host, &roots.with(&error), iterator)?;
                    return Ok(Invocation::Thrown(error));
                }
            };
            let roots = &roots.with(&key);
            let key = match self.coerce_property_key(host, roots, &key)? {
                Ok(key) => key,
                Err(error) => {
                    self.close_iterator(host, &roots.with(&error), iterator)?;
                    return Ok(Invocation::Thrown(error));
                }
            };
            match groups.iter_mut().find(|(existing, _)| *existing == key) {
                Some((_, elements)) => elements.push(item),
                None => groups.push((key, vec![item])),
            }
            index += 1;
        }
        let result = self.heap.alloc_plain_with(None);
        for (key, elements) in groups {
            let array = self.heap.alloc_array(elements);
            self.heap
                .define_data(result, key, Value::object(array), Attributes::ORDINARY)?;
        }
        Ok(Invocation::Value(Value::object(result)))
    }

    pub(crate) fn get_own_property_descriptor(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
        reflect: bool,
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let target = args.first().unwrap_or(&UNDEFINED_VALUE);
        if reflect && !matches!(target.view(), ValueRef::Object(_)) {
            return self
                .throw_type_error(roots, "Reflect.getOwnPropertyDescriptor requires an object");
        }
        if matches!(target.view(), ValueRef::Null | ValueRef::Undefined) {
            return self.throw_type_error(
                roots,
                "Object.getOwnPropertyDescriptor requires a non-nullish value",
            );
        }
        let key =
            match self.coerce_property_key(host, roots, args.get(1).unwrap_or(&UNDEFINED_VALUE))? {
                Ok(key) => key,
                Err(thrown) => return Ok(Invocation::Thrown(thrown)),
            };
        let descriptor = match target.view() {
            ValueRef::Object(id) => match self.own_descriptor(host, roots, id, key.borrowed())? {
                Ok(descriptor) => descriptor,
                Err(error) => return Ok(Invocation::Thrown(error)),
            },
            ValueRef::String(text) => {
                return Ok(Invocation::Value(
                    match string_own_property(text, key.borrowed()) {
                        Some(property) => Value::object(self.string_descriptor(property)?),
                        None => Value::UNDEFINED,
                    },
                ));
            }
            _ => None,
        };
        Ok(Invocation::Value(match descriptor {
            Some((property, attributes)) => {
                Value::object(self.descriptor_object(property, attributes)?)
            }
            None => Value::UNDEFINED,
        }))
    }

    pub(crate) fn descriptor_object(
        &mut self,
        property: Property,
        attributes: Attributes,
    ) -> Result<ObjectId, VmError> {
        let result = self.heap.alloc_plain();
        match property {
            Property::Data(value) => {
                self.heap.set_own_string(result, "value", value)?;
                self.heap.set_own_string(
                    result,
                    "writable",
                    Value::boolean(attributes.writable),
                )?;
            }
            Property::Accessor { get, set } => {
                self.heap.set_own_string(
                    result,
                    "get",
                    get.map_or(Value::UNDEFINED, Value::object),
                )?;
                self.heap.set_own_string(
                    result,
                    "set",
                    set.map_or(Value::UNDEFINED, Value::object),
                )?;
            }
        }
        self.heap
            .set_own_string(result, "enumerable", Value::boolean(attributes.enumerable))?;
        self.heap.set_own_string(
            result,
            "configurable",
            Value::boolean(attributes.configurable),
        )?;
        Ok(result)
    }

    pub(crate) fn get_own_property_descriptors(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let target = args.first().unwrap_or(&UNDEFINED_VALUE);
        let target = match target.view() {
            ValueRef::Object(id) => id,
            ValueRef::Null | ValueRef::Undefined => {
                return self.throw_type_error(
                    roots,
                    "Object.getOwnPropertyDescriptors requires a non-nullish value",
                );
            }
            primitive => {
                let result = self.heap.alloc_plain();
                if let ValueRef::String(text) = primitive {
                    for (index, unit) in text.units().enumerate() {
                        let descriptor = self.string_descriptor(OwnProperty::StringIndex(unit))?;
                        self.heap.set_own_string(
                            result,
                            &index.to_string(),
                            Value::object(descriptor),
                        )?;
                    }
                    let length =
                        self.string_descriptor(OwnProperty::StringLength(text.unit_len()))?;
                    self.heap
                        .set_own_string(result, "length", Value::object(length))?;
                }
                return Ok(Invocation::Value(Value::object(result)));
            }
        };
        let keys = match self.own_keys(host, roots, target, false)? {
            Ok(keys) => keys,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let result = self.heap.alloc_plain();
        for key in &keys {
            let view = (result, &keys);
            let roots = &roots.with(&view);
            let (property, attributes) =
                match self.own_descriptor(host, roots, target, key.borrowed())? {
                    Ok(Some(descriptor)) => descriptor,
                    Ok(None) => continue,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                };
            let descriptor = self.descriptor_object(property, attributes)?;
            self.heap
                .set_own(result, key.clone(), Value::object(descriptor))?;
        }
        Ok(Invocation::Value(Value::object(result)))
    }

    fn string_descriptor(&mut self, property: OwnProperty<'_>) -> Result<ObjectId, VmError> {
        let attributes = property.attributes();
        self.descriptor_object(property.into_owned(), attributes)
    }

    pub(crate) fn define_property_builtin(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
        reflect: bool,
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let Some(ValueRef::Object(target)) = args.first().map(Value::view) else {
            return self.throw_type_error(
                roots,
                if reflect {
                    "Reflect.defineProperty requires an object target"
                } else {
                    "Object.defineProperty requires an object target"
                },
            );
        };
        let key = match self.coerce_property_key(
            host,
            roots,
            args.get(1).unwrap_or(&Value::UNDEFINED),
        )? {
            Ok(key) => key,
            Err(thrown) => return Ok(Invocation::Thrown(thrown)),
        };
        let update = match self.read_property_descriptor(
            host,
            &roots.with(&key),
            args.get(2).unwrap_or(&Value::UNDEFINED),
        )? {
            Ok(update) => update,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        if reflect {
            return Ok(
                match self.try_apply_property_update(host, roots, target, key, update)? {
                    Ok(applied) => Invocation::Value(Value::boolean(applied)),
                    Err(error) => Invocation::Thrown(error),
                },
            );
        }
        self.apply_property_update(host, roots, target, key, update)
    }

    pub(crate) fn object_assign(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let target = args.first().unwrap_or(&UNDEFINED_VALUE);
        if matches!(target.view(), ValueRef::Null | ValueRef::Undefined) {
            return self.throw_type_error(roots, "Object.assign requires a non-nullish target");
        }
        let target_id = self.coercible_object(target);
        let target = Value::object(target_id);
        let roots = &roots.with(&target);
        for source in args.iter().skip(1) {
            let ValueRef::Object(source_id) = source.view() else {
                if let ValueRef::String(text) = source.view() {
                    for (index, unit) in text.units().enumerate() {
                        let invocation = self.set_property(
                            host,
                            roots,
                            target_id,
                            &target,
                            PropertyKey::String(index.to_string().into()),
                            Value::string(utf16_unit_string(unit)),
                            true,
                        )?;
                        if let thrown @ Invocation::Thrown(_) = invocation {
                            return Ok(thrown);
                        }
                    }
                }
                continue;
            };
            let keys = match self.own_keys(host, roots, source_id, false)? {
                Ok(keys) => keys,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
            for key in &keys {
                let roots = &roots.with(&keys);
                match self.own_descriptor(host, roots, source_id, key.borrowed())? {
                    Ok(Some((_, attributes))) if attributes.enumerable => {}
                    Ok(_) => continue,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                }
                let value = match self.read_property(host, roots, source, key.borrowed())? {
                    Invocation::Value(value) => value,
                    thrown @ Invocation::Thrown(_) => return Ok(thrown),
                };
                let invocation =
                    self.set_property(host, roots, target_id, &target, key.clone(), value, true)?;
                if let thrown @ Invocation::Thrown(_) = invocation {
                    return Ok(thrown);
                }
            }
        }
        Ok(Invocation::Value(target))
    }

    pub(crate) fn define_properties(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        target: ObjectId,
        properties: Option<&Value>,
    ) -> Result<Invocation, VmError> {
        let root_view = (target, properties);
        let roots = &roots.with(&root_view);
        let source = match properties.map(|value| (value, value.view())) {
            Some((_, ValueRef::Object(source))) => source,
            None | Some((_, ValueRef::Undefined | ValueRef::Null)) => {
                return self.throw_type_error(
                    roots,
                    "Object.defineProperties requires non-nullish properties",
                );
            }
            Some((_, ValueRef::String(text))) if !text.is_empty() => {
                return self.throw_type_error(roots, "property descriptor must be an object");
            }
            _ => return Ok(Invocation::Value(Value::object(target))),
        };
        let keys = match self.own_keys(host, roots, source, false)? {
            Ok(keys) => keys,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let mut updates = Vec::new();
        let roots = &roots.with(&keys);
        for key in &keys {
            let roots = &roots.with(&updates);
            // Getters can change the enumerability of keys still to be visited.
            match self.own_descriptor(host, roots, source, key.borrowed())? {
                Ok(Some((_, attributes))) if attributes.enumerable => {}
                Ok(_) => continue,
                Err(error) => return Ok(Invocation::Thrown(error)),
            }
            let descriptor =
                match self.read_property(host, roots, properties.unwrap(), key.borrowed())? {
                    Invocation::Value(value) => value,
                    thrown => return Ok(thrown),
                };
            match self.read_property_descriptor(host, roots, &descriptor)? {
                Ok(update) => updates.push((key.clone(), update)),
                Err(error) => return Ok(Invocation::Thrown(error)),
            }
        }
        // Conversion errors precede all definitions; application errors keep earlier writes.
        updates.reverse();
        while let Some((key, update)) = updates.pop() {
            let roots = &roots.with(&updates);
            if let thrown @ Invocation::Thrown(_) =
                self.apply_property_update(host, roots, target, key, update)?
            {
                return Ok(thrown);
            }
        }
        Ok(Invocation::Value(Value::object(target)))
    }

    pub(crate) fn read_property_descriptor(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        descriptor: &Value,
    ) -> Result<Result<DescriptorUpdate, Value>, VmError> {
        let roots = &roots.with(&descriptor);
        let ValueRef::Object(id) = descriptor.view() else {
            return Ok(Err(
                self.type_error(roots, "property descriptor must be an object")?
            ));
        };
        let mut update = DescriptorUpdate::default();
        // ToPropertyDescriptor observes inherited fields, and checks/reads each
        // in order: an earlier getter can affect the fields read afterward.
        for name in [
            "enumerable",
            "configurable",
            "value",
            "writable",
            "get",
            "set",
        ] {
            let roots = &roots.with(&update);
            let key = PropertyKeyRef::String(name);
            match self.has_property_key(host, roots, id, key)? {
                Invocation::Value(value) if value.truthy() => {}
                Invocation::Value(_) => continue,
                Invocation::Thrown(error) => return Ok(Err(error)),
            }
            let value = match self.read_property(host, roots, descriptor, key)? {
                Invocation::Value(value) => value,
                Invocation::Thrown(error) => return Ok(Err(error)),
            };
            match name {
                "enumerable" => update.enumerable = Some(value.truthy()),
                "configurable" => update.configurable = Some(value.truthy()),
                "value" => update.value = Some(value),
                "writable" => update.writable = Some(value.truthy()),
                "get" | "set" => {
                    let handler = match value.view() {
                        ValueRef::Undefined => None,
                        ValueRef::Object(id) if self.is_callable(&value) => Some(id),
                        _ => {
                            return Ok(Err(self.type_error(
                                roots,
                                "descriptor accessor must be callable or undefined",
                            )?));
                        }
                    };
                    if name == "get" {
                        update.get = Some(handler);
                    } else {
                        update.set = Some(handler);
                    }
                }
                _ => unreachable!(),
            }
        }
        if update.is_data() && update.is_accessor() {
            return Ok(Err(self.type_error(
                roots,
                "descriptor cannot mix data and accessor fields",
            )?));
        }
        Ok(Ok(update))
    }

    pub(crate) fn apply_property_update(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        target: ObjectId,
        key: PropertyKey,
        update: DescriptorUpdate,
    ) -> Result<Invocation, VmError> {
        match self.try_apply_property_update(host, &roots.with(&target), target, key, update)? {
            Ok(true) => Ok(Invocation::Value(Value::object(target))),
            Ok(false) => self.throw_type_error(roots, "Cannot define property"),
            Err(error) => Ok(Invocation::Thrown(error)),
        }
    }

    pub(crate) fn try_apply_property_update(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        target: ObjectId,
        key: PropertyKey,
        mut update: DescriptorUpdate,
    ) -> Result<Result<bool, Value>, VmError> {
        let target = self.heap.proxy_target(target)?;
        let roots = &roots.with(&target);
        // Integer-indexed exotic [[DefineOwnProperty]].
        match self.heap.typed_index(target, key.borrowed())? {
            crate::heap::TypedIndex::Ordinary => {}
            crate::heap::TypedIndex::Invalid => return Ok(Ok(false)),
            crate::heap::TypedIndex::Valid(index) => {
                if update.configurable == Some(false)
                    || update.enumerable == Some(false)
                    || update.writable == Some(false)
                    || update.is_accessor()
                {
                    return Ok(Ok(false));
                }
                if let Some(value) = update.value {
                    match self.typed_set_element(host, roots, target, index, &value)? {
                        Ok(()) => {}
                        Err(error) => return Ok(Err(error)),
                    }
                }
                return Ok(Ok(true));
            }
        }
        if key.borrowed().is_str("length")
            && matches!(self.heap.get(target)?, Object::Array(_))
            && let Some(value) = &mut update.value
        {
            // ArraySetLength performs ToUint32 and then a separate ToNumber.
            // Each may call script and observe changes made by the previous one.
            let length = match self.coerce_number(host, roots, value)? {
                Ok(number) => ops::number_to_uint32(number),
                Err(error) => return Ok(Err(error)),
            };
            let number = match self.coerce_number(host, roots, value)? {
                Ok(number) => number,
                Err(error) => return Ok(Err(error)),
            };
            if f64::from(length) != number {
                return Ok(Err(self.make_range_error(roots, "Invalid array length")?));
            }
            *value = Value::number(f64::from(length));
        }
        Ok(Ok(self.heap.apply_descriptor(target, key, update)?))
    }
}
