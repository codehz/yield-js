//! Argument and iterator record helpers shared by builtin calls.

use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::heap::{Attributes, Heap, ITERATOR_SYMBOL, Object, PlainObject};
use crate::host::Host;
use crate::runtime::Invocation;
use crate::value::{ObjectId, PropertyKey, Value, ValueRef};
use crate::vm::VmCore;
impl VmCore {
    pub(crate) fn create_arguments_object(
        heap: &mut Heap,
        iterator: ObjectId,
        args: &[Value],
    ) -> Result<Value, VmError> {
        let id = heap.alloc_plain();
        for (index, value) in args.iter().enumerate() {
            heap.define_data(
                id,
                PropertyKey::String(index.to_string().into()),
                value.clone(),
                Attributes::ORDINARY,
            )?;
        }
        heap.define_data(
            id,
            "length",
            Value::number(args.len() as f64),
            Attributes::METHOD,
        )?;
        heap.define_data(
            id,
            PropertyKey::Symbol(ITERATOR_SYMBOL),
            Value::object(iterator),
            Attributes::METHOD,
        )?;
        Ok(Value::object(id))
    }

    pub(crate) fn array_iterator(
        &mut self,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
        kind: crate::heap::ArrayIterationKind,
    ) -> Result<Invocation, VmError> {
        let id = match self.array_receiver(roots, receiver)? {
            Ok(id) => id,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let iterator = self.heap.alloc(Object::ArrayIterator {
            object: PlainObject::new(Some(self.builtins.get("arrayIterator.prototype"))),
            array: Some(id),
            kind,
            next_index: 0,
        });
        Ok(Invocation::Value(Value::object(iterator)))
    }

    pub(crate) fn array_iterator_next(
        &mut self,
        host: &Host,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        use crate::heap::ArrayIterationKind;

        let invalid_receiver = "ArrayIterator.next requires an array iterator";
        let ValueRef::Object(id) = receiver.view() else {
            return self.throw_type_error(roots, invalid_receiver);
        };
        let Object::ArrayIterator {
            array,
            next_index,
            kind,
            ..
        } = self.heap.get(id)?
        else {
            return self.throw_type_error(roots, invalid_receiver);
        };
        let Some(source) = *array else {
            return self.iterator_result(Value::UNDEFINED, true);
        };
        let (index, kind) = (*next_index, *kind);
        let view = (id, source);
        let roots = &roots.with(&view);
        let length = match self.length_of_array_like(host, roots, source)? {
            Ok(length) => length,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let Object::ArrayIterator {
            array, next_index, ..
        } = self.heap.get_mut(id)?
        else {
            unreachable!()
        };
        if index >= length {
            *array = None;
            return self.iterator_result(Value::UNDEFINED, true);
        }
        *next_index = index + 1;
        let key = Value::number(index as f64);
        let value = if kind == ArrayIterationKind::Keys {
            key
        } else {
            let value = match self.read_array_like_index(host, roots, source, index)? {
                Invocation::Value(value) => value,
                thrown => return Ok(thrown),
            };
            if kind == ArrayIterationKind::Entries {
                Value::object(self.heap.alloc_array(vec![key, value]))
            } else {
                value
            }
        };
        self.iterator_result(value, false)
    }

    pub(crate) fn iterator_result(
        &mut self,
        value: Value,
        done: bool,
    ) -> Result<Invocation, VmError> {
        let result = self.heap.alloc_plain();
        self.heap.set_own_string(result, "value", value)?;
        self.heap
            .set_own_string(result, "done", Value::boolean(done))?;
        Ok(Invocation::Value(Value::object(result)))
    }

    /// Same result object, for callers that cannot report a script throw.
    pub(crate) fn iterator_result_value(
        &mut self,
        value: Value,
        done: bool,
    ) -> Result<Value, VmError> {
        match self.iterator_result(value, done)? {
            Invocation::Value(result) => Ok(result),
            Invocation::Thrown(_) => unreachable!("iteration results are never thrown"),
        }
    }
}
