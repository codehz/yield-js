//! Compiler's view of the VM instruction set: which slots are uses or
//! definitions, which instructions may throw, and how to rewrite slots.
//!
//! Adding an opcode updates this module and the lowerer. CFG construction and
//! frame layout do not match on instruction shape themselves.

use std::collections::BTreeSet;

use yield_vm::bytecode::{BinaryOp, CallArgs, Instr, Slot, UnaryOp};

use crate::emit::Reg;

/// Whether an instruction can transfer control to a JavaScript exception
/// handler at this PC. `VmError` and `Await`'s asynchronous rejection do
/// not count; the latter has its own resume entry. `Await` can still throw
/// here when reading a promise's constructor. Classification is exhaustive.
///
/// Coercion may invoke script (including for loose equality).
pub(crate) fn may_throw(instruction: &Instr) -> bool {
    match instruction {
        Instr::Throw { .. }
        | Instr::CallValue { .. }
        | Instr::Construct { .. }
        | Instr::Await { .. }
        | Instr::GetMember { .. }
        | Instr::GetDestructuredMember { .. }
        | Instr::HasProperty { .. }
        | Instr::InstanceOf { .. }
        | Instr::DeleteMember { .. }
        | Instr::GetIterator { .. }
        | Instr::IteratorNext { .. }
        | Instr::IteratorSkip { .. }
        | Instr::IteratorClose {
            preserve_throw: false,
            ..
        }
        | Instr::SetMember { .. } => true,
        Instr::Binary { op, .. } => !matches!(*op, BinaryOp::StrictEq | BinaryOp::StrictNe),
        Instr::Unary { op, .. } => {
            matches!(*op, UnaryOp::Neg | UnaryOp::Pos | UnaryOp::BitNot)
        }
        Instr::ToString { .. }
        | Instr::ToPropertyKey { .. }
        | Instr::DefineProperty { .. }
        | Instr::AssertObjectCoercible { .. }
        | Instr::ObjectRest { .. }
        | Instr::CopyDataProperties { .. } => true,
        Instr::Const { .. }
        | Instr::Undefined { .. }
        | Instr::Move { .. }
        | Instr::CallImport { .. }
        | Instr::Closure { .. }
        | Instr::NewObject { .. }
        | Instr::NewArray { .. }
        | Instr::ArrayAppend { .. }
        | Instr::GetForInIterator { .. }
        | Instr::ForInNext { .. }
        | Instr::IteratorClose {
            preserve_throw: true,
            ..
        }
        | Instr::GetBuiltin { .. }
        | Instr::GetBinding { .. }
        | Instr::SetBinding { .. }
        | Instr::DeclareBinding { .. }
        | Instr::Jump { .. }
        | Instr::BranchFalse { .. }
        | Instr::Return { .. } => false,
    }
}

pub(crate) fn uses_defs(instruction: &Instr) -> (BTreeSet<Reg>, BTreeSet<Reg>) {
    let mut uses = BTreeSet::new();
    let mut defs = BTreeSet::new();
    match instruction {
        Instr::Const { dst, .. } | Instr::Undefined { dst } => {
            defs.insert(dst.index);
        }
        Instr::Move { dst, src } => {
            uses.insert(src.index);
            defs.insert(dst.index);
        }
        Instr::Binary { dst, lhs, rhs, .. } => {
            uses.insert(lhs.index);
            uses.insert(rhs.index);
            defs.insert(dst.index);
        }
        Instr::InstanceOf {
            dst,
            value,
            constructor,
        } => {
            uses.insert(value.index);
            uses.insert(constructor.index);
            defs.insert(dst.index);
        }
        Instr::Unary { dst, src, .. } => {
            uses.insert(src.index);
            defs.insert(dst.index);
        }
        Instr::ToString { dst, src } => {
            uses.insert(src.index);
            defs.insert(dst.index);
        }
        Instr::ToPropertyKey { dst, src, object } => {
            uses.insert(src.index);
            uses.extend(object.map(|slot| slot.index));
            defs.insert(dst.index);
        }
        Instr::CallImport { dst, args, .. } => {
            uses.extend(args.slots().iter().map(|arg| arg.index));
            defs.insert(dst.index);
        }
        Instr::CallValue {
            dst,
            callee,
            args,
            receiver,
        } => {
            uses.insert(callee.index);
            uses.extend(args.slots().iter().map(|arg| arg.index));
            if let Some(slot) = receiver {
                uses.insert(slot.index);
            }
            defs.insert(dst.index);
        }
        Instr::Construct { dst, callee, args } => {
            uses.insert(callee.index);
            uses.extend(args.slots().iter().map(|arg| arg.index));
            defs.insert(dst.index);
        }
        Instr::Closure { dst, .. }
        | Instr::NewObject { dst }
        | Instr::GetBinding { dst, .. }
        | Instr::GetBuiltin { dst, .. } => {
            defs.insert(dst.index);
        }
        Instr::NewArray { dst, elements } => {
            for element in elements {
                uses.insert(element.index);
            }
            defs.insert(dst.index);
        }
        Instr::ArrayAppend { array, src } => {
            uses.extend([array.index, src.index]);
        }
        Instr::CopyDataProperties { target, source } => {
            uses.extend([target.index, source.index]);
        }
        Instr::AssertObjectCoercible { src } => {
            uses.insert(src.index);
        }
        Instr::ObjectRest { dst, src, excluded } => {
            uses.insert(src.index);
            uses.extend(excluded.iter().map(|key| key.index));
            defs.insert(dst.index);
        }
        Instr::GetMember { dst, object, key }
        | Instr::GetDestructuredMember { dst, object, key }
        | Instr::HasProperty { dst, object, key }
        | Instr::DeleteMember { dst, object, key } => {
            uses.insert(object.index);
            uses.insert(key.index);
            defs.insert(dst.index);
        }
        Instr::GetIterator {
            iterator,
            next,
            iterable,
        } => {
            uses.insert(iterable.index);
            defs.extend([iterator.index, next.index]);
        }
        Instr::IteratorNext {
            done,
            value,
            iterator,
            next,
        } => {
            uses.extend([iterator.index, next.index]);
            defs.extend([done.index, value.index]);
        }
        Instr::IteratorSkip {
            done,
            iterator,
            next,
        } => {
            uses.extend([iterator.index, next.index]);
            defs.insert(done.index);
        }
        Instr::GetForInIterator { dst, source } => {
            uses.insert(source.index);
            defs.insert(dst.index);
        }
        Instr::ForInNext {
            done,
            value,
            iterator,
        } => {
            uses.insert(iterator.index);
            defs.extend([done.index, value.index]);
        }
        Instr::IteratorClose { iterator, .. } => {
            uses.insert(iterator.index);
        }
        Instr::SetMember { object, key, src }
        | Instr::DefineProperty {
            object, key, src, ..
        } => {
            uses.insert(object.index);
            uses.insert(key.index);
            uses.insert(src.index);
        }
        Instr::SetBinding { src, .. } => {
            uses.insert(src.index);
        }
        Instr::DeclareBinding { .. } => {}
        Instr::Await { dst, input, .. } => {
            uses.insert(input.index);
            defs.insert(dst.index);
        }
        Instr::Jump { .. } => {}
        Instr::BranchFalse { cond, .. } => {
            uses.insert(cond.index);
        }
        Instr::Return { src } | Instr::Throw { src } => {
            uses.insert(src.index);
        }
    }
    (uses, defs)
}

pub(crate) fn map_slots(instruction: &Instr, remap: &impl Fn(Slot) -> Slot) -> Instr {
    match instruction.clone() {
        Instr::Const { dst, constant } => Instr::Const {
            dst: remap(dst),
            constant,
        },
        Instr::Undefined { dst } => Instr::Undefined { dst: remap(dst) },
        Instr::Move { dst, src } => Instr::Move {
            dst: remap(dst),
            src: remap(src),
        },
        Instr::Binary { dst, op, lhs, rhs } => Instr::Binary {
            dst: remap(dst),
            op,
            lhs: remap(lhs),
            rhs: remap(rhs),
        },
        Instr::InstanceOf {
            dst,
            value,
            constructor,
        } => Instr::InstanceOf {
            dst: remap(dst),
            value: remap(value),
            constructor: remap(constructor),
        },
        Instr::Unary { dst, op, src } => Instr::Unary {
            dst: remap(dst),
            op,
            src: remap(src),
        },
        Instr::ToString { dst, src } => Instr::ToString {
            dst: remap(dst),
            src: remap(src),
        },
        Instr::ToPropertyKey { dst, src, object } => Instr::ToPropertyKey {
            dst: remap(dst),
            src: remap(src),
            object: object.map(remap),
        },
        Instr::CallImport { dst, import, args } => Instr::CallImport {
            dst: remap(dst),
            import,
            args: remap_args(args, remap),
        },
        Instr::CallValue {
            dst,
            callee,
            args,
            receiver,
        } => Instr::CallValue {
            dst: remap(dst),
            callee: remap(callee),
            args: remap_args(args, remap),
            receiver: receiver.map(remap),
        },
        Instr::Construct { dst, callee, args } => Instr::Construct {
            dst: remap(dst),
            callee: remap(callee),
            args: remap_args(args, remap),
        },
        Instr::Closure { dst, function } => Instr::Closure {
            dst: remap(dst),
            function,
        },
        Instr::NewObject { dst } => Instr::NewObject { dst: remap(dst) },
        Instr::NewArray { dst, elements } => Instr::NewArray {
            dst: remap(dst),
            elements: elements.into_iter().map(remap).collect(),
        },
        Instr::ArrayAppend { array, src } => Instr::ArrayAppend {
            array: remap(array),
            src: remap(src),
        },
        Instr::GetMember { dst, object, key } => Instr::GetMember {
            dst: remap(dst),
            object: remap(object),
            key: remap(key),
        },
        Instr::AssertObjectCoercible { src } => Instr::AssertObjectCoercible { src: remap(src) },
        Instr::GetDestructuredMember { dst, object, key } => Instr::GetDestructuredMember {
            dst: remap(dst),
            object: remap(object),
            key: remap(key),
        },
        Instr::ObjectRest { dst, src, excluded } => Instr::ObjectRest {
            dst: remap(dst),
            src: remap(src),
            excluded: excluded.into_iter().map(remap).collect(),
        },
        Instr::HasProperty { dst, object, key } => Instr::HasProperty {
            dst: remap(dst),
            object: remap(object),
            key: remap(key),
        },
        Instr::DeleteMember { dst, object, key } => Instr::DeleteMember {
            dst: remap(dst),
            object: remap(object),
            key: remap(key),
        },
        Instr::GetIterator {
            iterator,
            next,
            iterable,
        } => Instr::GetIterator {
            iterator: remap(iterator),
            next: remap(next),
            iterable: remap(iterable),
        },
        Instr::IteratorNext {
            done,
            value,
            iterator,
            next,
        } => Instr::IteratorNext {
            done: remap(done),
            value: remap(value),
            iterator: remap(iterator),
            next: remap(next),
        },
        Instr::IteratorSkip {
            done,
            iterator,
            next,
        } => Instr::IteratorSkip {
            done: remap(done),
            iterator: remap(iterator),
            next: remap(next),
        },
        Instr::GetForInIterator { dst, source } => Instr::GetForInIterator {
            dst: remap(dst),
            source: remap(source),
        },
        Instr::ForInNext {
            done,
            value,
            iterator,
        } => Instr::ForInNext {
            done: remap(done),
            value: remap(value),
            iterator: remap(iterator),
        },
        Instr::IteratorClose {
            iterator,
            preserve_throw,
        } => Instr::IteratorClose {
            iterator: remap(iterator),
            preserve_throw,
        },
        Instr::SetMember { object, key, src } => Instr::SetMember {
            object: remap(object),
            key: remap(key),
            src: remap(src),
        },
        Instr::DefineProperty {
            object,
            key,
            src,
            kind,
        } => Instr::DefineProperty {
            object: remap(object),
            key: remap(key),
            src: remap(src),
            kind,
        },
        Instr::CopyDataProperties { target, source } => Instr::CopyDataProperties {
            target: remap(target),
            source: remap(source),
        },
        Instr::GetBinding { dst, binding } => Instr::GetBinding {
            dst: remap(dst),
            binding,
        },
        Instr::SetBinding { binding, src } => Instr::SetBinding {
            binding,
            src: remap(src),
        },
        Instr::DeclareBinding { binding } => Instr::DeclareBinding { binding },
        Instr::Await { dst, input, resume } => Instr::Await {
            dst: remap(dst),
            input: remap(input),
            resume,
        },
        Instr::Jump { target } => Instr::Jump { target },
        Instr::BranchFalse { cond, target } => Instr::BranchFalse {
            cond: remap(cond),
            target,
        },
        Instr::Return { src } => Instr::Return { src: remap(src) },
        Instr::Throw { src } => Instr::Throw { src: remap(src) },
        Instr::GetBuiltin { dst, builtin } => Instr::GetBuiltin {
            dst: remap(dst),
            builtin,
        },
    }
}

fn remap_args(args: CallArgs, remap: &impl Fn(Slot) -> Slot) -> CallArgs {
    match args {
        CallArgs::Slots(slots) => CallArgs::Slots(slots.into_iter().map(remap).collect()),
        CallArgs::Spread(slot) => CallArgs::Spread(remap(slot)),
    }
}
