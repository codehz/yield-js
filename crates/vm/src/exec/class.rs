//! //! Class construction and private-name access.

use super::{Step, converted_property_key, store_invocation};
use crate::bytecode::{Constant, Instr};
use crate::error::VmError;
use crate::frame::Frame;
use crate::gc::ActiveRoots;
use crate::heap::{Attributes, ClassElement, Object, ScriptClass, ScriptClassSource, object_id};
use crate::host::Host;
use crate::runtime::Invocation;
use crate::value::{PropertyKeyRef, Value, ValueRef};
use crate::vm::VmCore;

impl VmCore {
    pub(super) fn step_class(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        frame: &mut Frame,
        instr: &Instr,
        _constants: &[Constant],
    ) -> Result<Step, VmError> {
        // Reentrant calls borrow the initialized slots; destination writes
        // happen after the last use of this view in each instruction.
        let frame_roots = &roots.with(&*frame);
        match *instr {
            Instr::Closure { dst, function } => self.make_closure(frame, dst, function),
            Instr::NewClassScope { dst, parent } => {
                let parent = self.class_scope(frame, parent)?;
                let env = self
                    .heap
                    .alloc_environment(Some(parent), Default::default());
                frame.write(dst, Value::object(env))?;
                Ok(Step::Next)
            }
            Instr::LoadClassBinding {
                dst,
                scope,
                binding,
            } => {
                let scope = self.class_scope(frame, scope)?;
                let Some(value) = self.heap.find_binding(scope, binding)? else {
                    return Ok(Step::Throw(
                        self.reference_error(frame_roots, "class name is not initialized")?,
                    ));
                };
                frame.write(dst, value)?;
                Ok(Step::Next)
            }
            Instr::InitClassBinding {
                scope,
                binding,
                src,
            } => {
                let scope = self.class_scope(frame, Some(scope))?;
                let value = frame.read(src)?;
                let bindings = &mut self.heap.environment_mut(scope)?.bindings;
                let std::collections::btree_map::Entry::Vacant(entry) = bindings.entry(binding)
                else {
                    return Err(VmError::InvalidState(
                        "class binding initialized twice".into(),
                    ));
                };
                entry.insert(value);
                Ok(Step::Next)
            }
            Instr::CreateClass {
                dst,
                scope,
                constructor,
                heritage,
                name_key,
                default_constructor,
                private_count,
                private_binding,
            } => {
                let env = self.class_scope(frame, scope)?;
                let derived = heritage.is_some();
                let function_root = self.builtins.get("function.prototype");
                let (static_parent, instance_parent) =
                    match heritage.map(|slot| frame.read(slot)).transpose()? {
                        None => (function_root, Some(self.builtins.get("object.prototype"))),
                        Some(parent) if matches!(parent.view(), ValueRef::Null) => {
                            (function_root, None)
                        }
                        Some(parent) => {
                            if !self.is_constructor(&parent)? {
                                return Ok(Step::Throw(self.type_error(
                                    frame_roots,
                                    "class heritage is not a constructor",
                                )?));
                            }
                            let parent_id = object_id(&parent, "class heritage")?;
                            match self.read_property(
                                host,
                                frame_roots,
                                &parent,
                                PropertyKeyRef::String("prototype"),
                            )? {
                                Invocation::Thrown(error) => return Ok(Step::Throw(error)),
                                Invocation::Value(value) => {
                                    match value.view() {
                                        ValueRef::Object(id) => (parent_id, Some(id)),
                                        ValueRef::Null => (parent_id, None),
                                        _ => return Ok(Step::Throw(self.type_error(
                                            frame_roots,
                                            "class heritage prototype must be an object or null",
                                        )?)),
                                    }
                                }
                            }
                        }
                    };
                let dynamic_name = name_key
                    .map(|slot| {
                        Ok::<_, VmError>(self.property_function_name(
                            converted_property_key(frame.read_ref(slot)?)?,
                            None,
                        ))
                    })
                    .transpose()?;
                let class = self.heap.alloc_function(constructor, env);
                let prototype = self.heap.alloc_plain_with(instance_parent);
                let Object::Function(function) = self.heap.get_mut(class)? else {
                    unreachable!()
                };
                function.prototype = Some(static_parent);
                function.dynamic_name = dynamic_name;
                function.class = Some(ScriptClass {
                    prototype,
                    source: ScriptClassSource {
                        derived,
                        default_constructor,
                        elements: Vec::new(),
                        private_count,
                        private_binding,
                        private: Default::default(),
                    },
                });
                function.home = Some(prototype);
                self.install_script_metadata(class, constructor)?;
                self.heap.define_data(
                    class,
                    "prototype",
                    Value::object(prototype),
                    Attributes::FIXED,
                )?;
                self.heap.define_data(
                    prototype,
                    "constructor",
                    Value::object(class),
                    Attributes::METHOD,
                )?;
                frame.write(dst, Value::object(class))?;
                Ok(Step::Next)
            }
            Instr::ScopedClosure {
                dst,
                scope,
                function,
            } => {
                let env = self.class_scope(frame, Some(scope))?;
                let closure = self.materialize_function(function, env)?;
                frame.write(dst, Value::object(closure))?;
                Ok(Step::Next)
            }
            Instr::DefineClassMethod {
                class,
                key,
                src,
                is_static,
                kind,
            } => {
                let class = object_id(&frame.read(class)?, "class method target")?;
                let key = frame.read(key)?;
                let key = converted_property_key(&key)?.to_owned();
                let src = object_id(&frame.read(src)?, "class method closure")?;
                if is_static && key.borrowed().is_str("prototype") {
                    return Ok(Step::Throw(self.type_error(
                        frame_roots,
                        "static class element cannot be named prototype",
                    )?));
                }
                self.define_class_method(class, key, src, is_static, kind)?;
                Ok(Step::Next)
            }
            Instr::AddClassField {
                class,
                key,
                initializer,
                is_static,
            } => {
                let class = object_id(&frame.read(class)?, "class field target")?;
                let key = converted_property_key(frame.read_ref(key)?)?.to_owned();
                if is_static && key.borrowed().is_str("prototype") {
                    return Ok(Step::Throw(self.type_error(
                        frame_roots,
                        "static class element cannot be named prototype",
                    )?));
                }
                let initializer = initializer
                    .map(|slot| object_id(frame.read_ref(slot)?, "field initializer"))
                    .transpose()?;
                self.add_class_element(
                    class,
                    ClassElement::Field {
                        key,
                        initializer,
                        is_static,
                    },
                )?;
                Ok(Step::Next)
            }
            Instr::AddClassBlock { class, body } => {
                let class = object_id(frame.read_ref(class)?, "static block target")?;
                let body = object_id(frame.read_ref(body)?, "static block body")?;
                self.add_class_element(class, ClassElement::Block { body })?;
                Ok(Step::Next)
            }
            Instr::RunClassStatic { class } => {
                let class = object_id(frame.read_ref(class)?, "static initialization target")?;
                match self.initialize_class_elements(host, frame_roots, class, class, true)? {
                    Some(error) => Ok(Step::Throw(error)),
                    None => Ok(Step::Next),
                }
            }
            Instr::AddClassPrivate {
                class,
                index,
                lexical,
                src,
                is_static,
                kind,
            } => {
                let class = object_id(frame.read_ref(class)?, "private element class")?;
                self.check_private_lexical(class, lexical, index)?;
                let closure = src
                    .map(|slot| object_id(frame.read_ref(slot)?, "private element closure"))
                    .transpose()?;
                self.add_class_private(class, index, closure, is_static, kind)?;
                Ok(Step::Next)
            }
            Instr::GetPrivate {
                dst,
                object,
                brand,
                index,
                lexical,
            } => {
                let brand = object_id(frame.read_ref(brand)?, "private brand")?;
                self.check_private_lexical(brand, lexical, index)?;
                let value =
                    self.get_private(host, frame_roots, frame.read_ref(object)?, brand, index)?;
                store_invocation(frame, dst, value)
            }
            Instr::SetPrivate {
                object,
                brand,
                src,
                index,
                lexical,
            } => {
                let brand = object_id(frame.read_ref(brand)?, "private brand")?;
                self.check_private_lexical(brand, lexical, index)?;
                match self.set_private(
                    host,
                    frame_roots,
                    frame.read_ref(object)?,
                    brand,
                    index,
                    frame.read(src)?,
                )? {
                    Invocation::Thrown(error) => Ok(Step::Throw(error)),
                    Invocation::Value(_) => Ok(Step::Next),
                }
            }
            Instr::HasPrivate {
                dst,
                object,
                brand,
                index,
                lexical,
            } => {
                let brand = object_id(frame.read_ref(brand)?, "private brand")?;
                self.check_private_lexical(brand, lexical, index)?;
                let ValueRef::Object(object) = frame.read_ref(object)?.view() else {
                    return Ok(Step::Throw(self.type_error(
                        frame_roots,
                        "private brand check requires an object",
                    )?));
                };
                let present = self
                    .heap
                    .private
                    .get(&object)
                    .is_some_and(|elements| elements.contains_key(&(brand, index)));
                frame.write(dst, Value::boolean(present))?;
                Ok(Step::Next)
            }
            _ => unreachable!("step dispatch routes only these opcodes here"),
        }
    }
}
