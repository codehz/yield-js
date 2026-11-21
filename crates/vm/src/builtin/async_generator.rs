//! The shared async generator prototype.
//!
//! An async generator instance is an ordinary object that owns a suspended
//! frame and a FIFO of requests. Its internal prototype is the function's own
//! `.prototype` object when that is an object, and this shared prototype
//! otherwise. It deliberately does not inherit the synchronous generator
//! prototype: the two `next` methods settle different things.

use crate::builtin::roots::{RootKind, RootSpec};
use crate::exec::GeneratorCompletion;
use crate::heap::{ASYNC_ITERATOR_SYMBOL, Attributes};
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::runtime::Invocation;
use crate::value::{PropertyKey, Value};
use crate::vm::Vm;

pub(crate) const ROOT: &str = "asyncGenerator.prototype";

const NEXT: IntrinsicKey = IntrinsicKey::new("AsyncGenerator", Slot::Prototype, "next");
const RETURN: IntrinsicKey = IntrinsicKey::new("AsyncGenerator", Slot::Prototype, "return");
const THROW: IntrinsicKey = IntrinsicKey::new("AsyncGenerator", Slot::Prototype, "throw");
const ITERABLE: IntrinsicKey =
    IntrinsicKey::new("AsyncGenerator", Slot::PrototypeSymbol, "asyncIterator");

pub(crate) const ROOTS: &[RootSpec] = &[RootSpec::new(
    ROOT,
    RootKind::Plain,
    Some("object.prototype"),
)];

pub(crate) const INTRINSICS: &[IntrinsicDef] = &[
    IntrinsicDef::call(NEXT, 1, |vm, host, roots, args, receiver| {
        let argument = args.into_iter().next().unwrap_or(Value::UNDEFINED);
        vm.resume_async_generator(host, roots, receiver, GeneratorCompletion::Next, argument)
    }),
    IntrinsicDef::call(RETURN, 1, |vm, host, roots, args, receiver| {
        let argument = args.into_iter().next().unwrap_or(Value::UNDEFINED);
        vm.resume_async_generator(host, roots, receiver, GeneratorCompletion::Return, argument)
    }),
    IntrinsicDef::call(THROW, 1, |vm, host, roots, args, receiver| {
        let argument = args.into_iter().next().unwrap_or(Value::UNDEFINED);
        vm.resume_async_generator(host, roots, receiver, GeneratorCompletion::Throw, argument)
    }),
    IntrinsicDef::call(ITERABLE, 0, |_, _, _, _, receiver| {
        Ok(Invocation::Value(receiver.clone()))
    }),
];

impl Vm {
    pub(crate) fn install_async_generator_methods(&mut self) {
        let prototype = self.builtins.get(ROOT);
        for key in [NEXT, RETURN, THROW] {
            let method = self.alloc_intrinsic(key);
            super::define_builtin_data(
                self,
                prototype,
                key.member,
                Value::object(method),
                Attributes::METHOD,
            );
        }
        let iterable = self.alloc_intrinsic(ITERABLE);
        super::define_builtin_data(
            self,
            prototype,
            PropertyKey::Symbol(ASYNC_ITERATOR_SYMBOL),
            Value::object(iterable),
            Attributes::METHOD,
        );
    }
}
