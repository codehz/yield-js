//! Private elements are receiver-owned state, indexed by lexical class identity.

use std::collections::btree_map::Entry;

use crate::bytecode::PrivateDefinition;
use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::heap::{
    ClassElement, Object, PrivateElement, PrivateMember, PrivateMemberKind, ScriptClass,
};
use crate::host::Host;
use crate::runtime::{Arguments, Invocation};
use crate::value::{ObjectId, Value, ValueRef};
use crate::vm::Vm;

impl Vm {
    pub(crate) fn private_class(
        &self,
        brand: ObjectId,
        index: u32,
    ) -> Result<&ScriptClass, VmError> {
        self.heap
            .function(brand)
            .and_then(|f| f.class.as_ref())
            .filter(|record| index < record.source.private_count)
            .ok_or_else(|| VmError::InvalidState("invalid private class reference".into()))
    }

    pub(crate) fn check_private_lexical(
        &self,
        brand: ObjectId,
        lexical: crate::BindingId,
        index: u32,
    ) -> Result<(), VmError> {
        if self.private_class(brand, index)?.source.private_binding != Some(lexical) {
            return Err(VmError::InvalidState(
                "private brand does not match its lexical class".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn stamp_private(
        &mut self,
        receiver: ObjectId,
        brand: ObjectId,
        index: u32,
        value: PrivateElement,
    ) -> bool {
        let Entry::Vacant(entry) = self
            .heap
            .private
            .entry(receiver)
            .or_default()
            .entry((brand, index))
        else {
            return false;
        };
        entry.insert(value);
        true
    }

    pub(crate) fn add_class_private(
        &mut self,
        brand: ObjectId,
        index: u32,
        closure: Option<ObjectId>,
        is_static: bool,
        definition: PrivateDefinition,
    ) -> Result<(), VmError> {
        let class = self.private_class(brand, index)?;
        let home = if is_static { brand } else { class.prototype };
        let previous = class
            .source
            .private
            .get(&index)
            .map(|member| (member.is_static, member.kind));
        let kind = match (definition, closure, previous) {
            (PrivateDefinition::Field, _, None) => PrivateMemberKind::Field,
            (PrivateDefinition::Method, Some(id), None) => PrivateMemberKind::Method(id),
            (PrivateDefinition::Get, Some(id), None) => PrivateMemberKind::Accessor {
                get: Some(id),
                set: None,
            },
            (PrivateDefinition::Set, Some(id), None) => PrivateMemberKind::Accessor {
                get: None,
                set: Some(id),
            },
            (
                PrivateDefinition::Get,
                Some(id),
                Some((s, PrivateMemberKind::Accessor { get: None, set })),
            ) if s == is_static => PrivateMemberKind::Accessor { get: Some(id), set },
            (
                PrivateDefinition::Set,
                Some(id),
                Some((s, PrivateMemberKind::Accessor { get, set: None })),
            ) if s == is_static => PrivateMemberKind::Accessor { get, set: Some(id) },
            _ => return Err(VmError::InvalidState("invalid private declaration".into())),
        };
        if let Some(id) = closure {
            let Object::Function(function) = self.heap.get_mut(id)? else {
                return Err(VmError::InvalidState(
                    "private declaration requires a closure".into(),
                ));
            };
            if !self.build.function(function.func_id).is_some_and(|code| {
                code.is_method
                    && (definition == PrivateDefinition::Method || code.is_accessor_method())
            }) {
                return Err(VmError::InvalidState(
                    "invalid private declaration closure".into(),
                ));
            }
            function.home = Some(home);
        }
        let Object::Function(function) = self.heap.get_mut(brand)? else {
            unreachable!()
        };
        let class = function.class.as_mut().expect("validated class");
        class
            .source
            .private
            .insert(index, PrivateMember { is_static, kind });
        if definition == PrivateDefinition::Field {
            class.source.elements.push(ClassElement::PrivateField {
                index,
                initializer: closure,
            });
        }
        Ok(())
    }

    pub(crate) fn get_private(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        brand: ObjectId,
        index: u32,
    ) -> Result<Invocation, VmError> {
        let class = self.private_class(brand, index)?;
        let ValueRef::Object(id) = receiver.view() else {
            return self.throw_type_error(roots, "private member requires an object");
        };
        let Some(element) = self
            .heap
            .private
            .get(&id)
            .and_then(|elements| elements.get(&(brand, index)))
        else {
            return self.throw_type_error(roots, "private member is not installed");
        };
        if let PrivateElement::Field(value) = element {
            return Ok(Invocation::Value(value.clone()));
        }
        match class.source.private[&index].kind {
            PrivateMemberKind::Method(method) => Ok(Invocation::Value(Value::object(method))),
            PrivateMemberKind::Accessor { get: Some(get), .. } => {
                self.invoke(host, roots, &Value::object(get), Arguments::EMPTY, receiver)
            }
            _ => self.throw_type_error(roots, "private member has no getter"),
        }
    }

    pub(crate) fn set_private(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        brand: ObjectId,
        index: u32,
        value: Value,
    ) -> Result<Invocation, VmError> {
        let kind = self
            .private_class(brand, index)?
            .source
            .private
            .get(&index)
            .map(|member| member.kind);
        let ValueRef::Object(id) = receiver.view() else {
            return self.throw_type_error(roots, "private member requires an object");
        };
        let Some(element) = self
            .heap
            .private
            .get_mut(&id)
            .and_then(|elements| elements.get_mut(&(brand, index)))
        else {
            return self.throw_type_error(roots, "private member is not installed");
        };
        if let PrivateElement::Field(slot) = element {
            *slot = value;
            return Ok(Invocation::Value(Value::UNDEFINED));
        }
        match kind {
            Some(PrivateMemberKind::Accessor { set: Some(set), .. }) => self.invoke(
                host,
                roots,
                &Value::object(set),
                Arguments::Values(&[value]),
                receiver,
            ),
            _ => self.throw_type_error(roots, "private member is not writable"),
        }
    }
}
