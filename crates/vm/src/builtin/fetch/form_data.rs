//! FormData's ordered entry list and live Web IDL pair iterators.

use serde::{Deserialize, Serialize};

use super::blob::{FileData, alloc_file, blob_now};
use crate::builtin::{
    FUNCTION_PARENT, OBJECT_PARENT, define_builtin_data,
    globals::Owner,
    iterator::SYNC_ROOT,
    link_constructor,
    roots::{RootKind, RootSpec},
};
use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::heap::{Attributes, ITERATOR_SYMBOL, Object, PlainObject, TO_STRING_TAG_SYMBOL};
use crate::host::Host;
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::runtime::{Arguments, Invocation};
use crate::value::{ObjectId, PropertyKey, Value, ValueRef};
use crate::vm::Vm;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FormDataRecord {
    pub(crate) entries: Vec<(String, FormDataValue)>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) enum FormDataValue {
    String(String),
    File(ObjectId),
}

impl FormDataValue {
    fn to_value(&self) -> Value {
        match self {
            Self::String(string) => Value::string(string.as_str().into()),
            Self::File(id) => Value::object(*id),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct FormDataIteratorRecord {
    pub(crate) target: ObjectId,
    pub(crate) index: usize,
    pub(crate) kind: FormDataIteratorKind,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub(crate) enum FormDataIteratorKind {
    Entries,
    Keys,
    Values,
}

const NAME: &str = "FormData";
const ROOT: &str = "formData";
const PROTOTYPE: &str = "formData.prototype";
const ITERATOR: &str = "formDataIterator.prototype";
const CONSTRUCTOR: IntrinsicKey = IntrinsicKey::new(NAME, Slot::Global, "");
const NEXT: IntrinsicKey = IntrinsicKey::new("FormDataIterator", Slot::Prototype, "next");
const ROOTS: &[RootSpec] = &[
    RootSpec::new(ROOT, RootKind::Native(CONSTRUCTOR), FUNCTION_PARENT).fixed(PROTOTYPE),
    RootSpec::new(PROTOTYPE, RootKind::Plain, OBJECT_PARENT),
    RootSpec::new(ITERATOR, RootKind::Plain, Some(SYNC_ROOT)),
];

#[derive(Clone, Copy)]
enum Method {
    Append,
    Delete,
    Get,
    GetAll,
    Has,
    Set,
    Iterate(FormDataIteratorKind),
    ForEach,
}

macro_rules! methods {
    ($($method:expr => $name:literal / $length:literal),+ $(,)?) => {
        const METHODS: &[(&str, IntrinsicKey)] = &[
            $(($name, IntrinsicKey::new(NAME, Slot::Prototype, $name))),+
        ];
        const INTRINSICS: &[IntrinsicDef] = &[
            IntrinsicDef::constructor(CONSTRUCTOR, 0,
                |vm, _, roots, _, _| vm.throw_type_error(roots, "FormData requires new"),
                |vm, _, roots, args, prototype| vm.construct_form_data(roots, &args, prototype),
            ),
            IntrinsicDef::call(NEXT, 0, |vm, _, roots, _, receiver| vm.form_data_next(roots, receiver)),
            $(IntrinsicDef::call(IntrinsicKey::new(NAME, Slot::Prototype, $name), $length,
                |vm, host, roots, args, receiver| {
                    vm.form_data_method(host, roots, receiver, &args, $method, $length)
                },
            )),+
        ];
    };
}

methods! {
    Method::Append => "append" / 2,
    Method::Delete => "delete" / 1,
    Method::Get => "get" / 1,
    Method::GetAll => "getAll" / 1,
    Method::Has => "has" / 1,
    Method::Set => "set" / 2,
    Method::Iterate(FormDataIteratorKind::Entries) => "entries" / 0,
    Method::Iterate(FormDataIteratorKind::Keys) => "keys" / 0,
    Method::Iterate(FormDataIteratorKind::Values) => "values" / 0,
    Method::ForEach => "forEach" / 1,
}

pub(crate) const FORM_DATA_OWNER: Owner = Owner::new(ROOT, ROOTS, &[INTRINSICS], Some(install));

fn install(vm: &mut Vm, _: &Value) {
    let prototype = vm.builtins.get(PROTOTYPE);
    link_constructor(vm, vm.builtins.get(ROOT), prototype);
    for &(name, key) in METHODS {
        let method = Value::object(vm.alloc_intrinsic(key));
        if name == "entries" {
            define_builtin_data(
                vm,
                prototype,
                PropertyKey::Symbol(ITERATOR_SYMBOL),
                method.clone(),
                Attributes::METHOD,
            );
        }
        define_builtin_data(vm, prototype, name, method, Attributes::ORDINARY);
    }
    let iterator = vm.builtins.get(ITERATOR);
    let next = Value::object(vm.alloc_intrinsic(NEXT));
    define_builtin_data(vm, iterator, "next", next, Attributes::ORDINARY);
    for (target, tag) in [(prototype, NAME), (iterator, "FormData Iterator")] {
        define_builtin_data(
            vm,
            target,
            PropertyKey::Symbol(TO_STRING_TAG_SYMBOL),
            Value::string(tag.into()),
            Attributes::FUNCTION_METADATA,
        );
    }
}

impl Vm {
    fn construct_form_data(
        &mut self,
        roots: &ActiveRoots<'_>,
        args: &[Value],
        prototype: Option<ObjectId>,
    ) -> Result<Invocation, VmError> {
        if args
            .first()
            .is_some_and(|arg| !matches!(arg.view(), ValueRef::Undefined))
            || args
                .get(1)
                .is_some_and(|arg| !matches!(arg.view(), ValueRef::Undefined | ValueRef::Null))
        {
            return self.throw_type_error(roots, "FormData does not accept HTML form arguments");
        }
        Ok(Invocation::Value(Value::object(
            self.alloc_form_data(prototype, FormDataRecord::default()),
        )))
    }

    pub(crate) fn alloc_form_data(
        &mut self,
        prototype: Option<ObjectId>,
        data: FormDataRecord,
    ) -> ObjectId {
        let prototype = prototype.unwrap_or_else(|| self.builtins.get(PROTOTYPE));
        self.heap.alloc(Object::FormData {
            object: PlainObject::new(Some(prototype)),
            data,
        })
    }

    fn form_data_usv(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        value: &Value,
    ) -> Result<Result<String, Value>, VmError> {
        Ok(self.coerce_string(host, roots, value)?.map(|string| {
            char::decode_utf16(string.units())
                .map(|unit| unit.unwrap_or(char::REPLACEMENT_CHARACTER))
                .collect()
        }))
    }

    fn form_data_entry_value(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        value: &Value,
        filename: Option<&Value>,
    ) -> Result<Result<FormDataValue, Value>, VmError> {
        let converted_filename =
            filename.filter(|name| !matches!(name.view(), ValueRef::Undefined));
        let id = match value.view() {
            ValueRef::Object(id) => Some(id),
            _ => None,
        };
        let (blob, last_modified) = match id.map(|id| self.heap.get(id)).transpose()? {
            Some(Object::File { .. }) if converted_filename.is_none() => {
                return Ok(Ok(FormDataValue::File(id.expect("File object ID"))));
            }
            Some(Object::File { data, .. }) => (data.blob.clone(), Some(data.last_modified)),
            Some(Object::Blob { data, .. }) => (data.clone(), None),
            _ if filename.is_some() => {
                return Ok(Err(
                    self.type_error(roots, "FormData filename requires a Blob value")?
                ));
            }
            _ => {
                return Ok(self
                    .form_data_usv(host, roots, value)?
                    .map(FormDataValue::String));
            }
        };
        let name = match converted_filename {
            Some(value) => match self.form_data_usv(host, roots, value)? {
                Ok(name) => name,
                Err(error) => return Ok(Err(error)),
            },
            None => "blob".to_owned(),
        };
        let data = FileData {
            blob,
            name,
            last_modified: last_modified.unwrap_or_else(|| blob_now(host)),
        };
        Ok(Ok(FormDataValue::File(alloc_file(self, data))))
    }

    fn form_data_method(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        args: &[Value],
        method: Method,
        required: usize,
    ) -> Result<Invocation, VmError> {
        let invalid_receiver = "FormData method requires a FormData receiver";
        let ValueRef::Object(id) = receiver.view() else {
            return self.throw_type_error(roots, invalid_receiver);
        };
        if !matches!(self.heap.get(id)?, Object::FormData { .. }) {
            return self.throw_type_error(roots, invalid_receiver);
        }
        if args.len() < required {
            return self.throw_type_error(roots, "not enough FormData arguments");
        }
        let roots = &roots.with(&id);
        match method {
            Method::Iterate(kind) => {
                let iterator = self.heap.alloc(Object::FormDataIterator {
                    object: PlainObject::new(Some(self.builtins.get(ITERATOR))),
                    data: FormDataIteratorRecord {
                        target: id,
                        index: 0,
                        kind,
                    },
                });
                return Ok(Invocation::Value(Value::object(iterator)));
            }
            Method::ForEach => return self.form_data_for_each(host, roots, receiver, id, args),
            _ => {}
        }
        let name = match self.form_data_usv(host, roots, &args[0])? {
            Ok(name) => name,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let value = if matches!(method, Method::Append | Method::Set) {
            match self.form_data_entry_value(host, roots, &args[1], args.get(2))? {
                Ok(value) => Some(value),
                Err(error) => return Ok(Invocation::Thrown(error)),
            }
        } else {
            None
        };
        let Object::FormData { data, .. } = self.heap.get_mut(id)? else {
            unreachable!()
        };
        let entries = &mut data.entries;
        let result = match method {
            Method::Append => {
                entries.push((name, value.expect("converted value")));
                Value::UNDEFINED
            }
            Method::Delete => {
                entries.retain(|(key, _)| *key != name);
                Value::UNDEFINED
            }
            Method::Get => entries
                .iter()
                .find(|(key, _)| *key == name)
                .map_or(Value::NULL, |(_, value)| value.to_value()),
            Method::GetAll => {
                let values = entries
                    .iter()
                    .filter(|(key, _)| *key == name)
                    .map(|(_, value)| value.to_value())
                    .collect();
                Value::object(self.heap.alloc_array(values))
            }
            Method::Has => Value::boolean(entries.iter().any(|(key, _)| *key == name)),
            Method::Set => {
                let mut replacement = value;
                entries.retain_mut(|(key, old)| {
                    if *key != name {
                        true
                    } else if let Some(value) = replacement.take() {
                        *old = value;
                        true
                    } else {
                        false
                    }
                });
                if let Some(value) = replacement {
                    entries.push((name, value));
                }
                Value::UNDEFINED
            }
            _ => unreachable!(),
        };
        Ok(Invocation::Value(result))
    }

    fn form_data_for_each(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        id: ObjectId,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let callback = &args[0];
        if !self.is_callable(callback) {
            return self.throw_type_error(roots, "FormData.forEach callback must be callable");
        }
        let undefined = Value::UNDEFINED;
        let this_arg = args.get(1).unwrap_or(&undefined);
        for index in 0.. {
            let Object::FormData { data, .. } = self.heap.get(id)? else {
                unreachable!()
            };
            let Some((name, value)) = data.entries.get(index) else {
                break;
            };
            let call_args = [
                value.to_value(),
                Value::string(name.as_str().into()),
                receiver.clone(),
            ];
            if let thrown @ Invocation::Thrown(_) = self.invoke(
                host,
                roots,
                callback,
                Arguments::Values(&call_args),
                this_arg,
            )? {
                return Ok(thrown);
            }
        }
        Ok(Invocation::Value(Value::UNDEFINED))
    }

    fn form_data_next(
        &mut self,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        let invalid_receiver = "FormData iterator next requires a FormData iterator receiver";
        let ValueRef::Object(id) = receiver.view() else {
            return self.throw_type_error(roots, invalid_receiver);
        };
        let Object::FormDataIterator { data: iterator, .. } = self.heap.get(id)? else {
            return self.throw_type_error(roots, invalid_receiver);
        };
        let Object::FormData { data, .. } = self.heap.get(iterator.target)? else {
            unreachable!()
        };
        let Some((name, value)) = data.entries.get(iterator.index) else {
            // Web IDL pair iterators do not latch exhaustion; appends can make them live again.
            return self.iterator_result(Value::UNDEFINED, true);
        };
        let value = match iterator.kind {
            FormDataIteratorKind::Entries => {
                let pair = vec![Value::string(name.as_str().into()), value.to_value()];
                Value::object(self.heap.alloc_array(pair))
            }
            FormDataIteratorKind::Keys => Value::string(name.as_str().into()),
            FormDataIteratorKind::Values => value.to_value(),
        };
        let Object::FormDataIterator { data, .. } = self.heap.get_mut(id)? else {
            unreachable!()
        };
        data.index += 1;
        self.iterator_result(value, false)
    }
}
