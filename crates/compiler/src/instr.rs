//! Compiler's view of the VM instruction set: which slots are uses or
//! definitions, which instructions may throw, and how to rewrite slots.
//!
//! Adding an opcode updates this module and the lowerer. CFG construction and
//! frame layout do not match on instruction shape themselves.

use std::collections::BTreeSet;

use yield_vm::bytecode::{BinaryOp, Instr, Slot, UnaryOp};

use crate::emit::Reg;

/// Whether an instruction can transfer control to a JavaScript exception
/// handler at this PC. `VmError` is not that control flow, and `Await`'s
/// rejection path has its own resume entry, so neither counts. Exhaustive so a
/// new opcode forces an explicit classification.
///
/// Arithmetic and relational operators coerce operands, and a Symbol operand
/// throws a catchable `TypeError` there. Equality and `typeof`/`!`/`void`
/// never coerce, so those instructions stay non-throwing.
pub(crate) fn may_throw(instruction: &Instr) -> bool {
    match instruction {
        Instr::Throw { .. }
        | Instr::CallFunction { .. }
        | Instr::CallValue { .. }
        | Instr::Construct { .. }
        | Instr::GetMember { .. }
        | Instr::SetMember { .. } => true,
        Instr::Binary { op, .. } => !matches!(
            *op,
            BinaryOp::Eq | BinaryOp::Ne | BinaryOp::StrictEq | BinaryOp::StrictNe
        ),
        Instr::Unary { op, .. } => {
            matches!(*op, UnaryOp::Neg | UnaryOp::Pos | UnaryOp::BitNot)
        }
        Instr::Const { .. }
        | Instr::Undefined { .. }
        | Instr::Move { .. }
        | Instr::CallImport { .. }
        | Instr::Closure { .. }
        | Instr::NewObject { .. }
        | Instr::NewArray { .. }
        | Instr::GetBuiltin { .. }
        | Instr::GetTopFunction { .. }
        | Instr::DefineProperty { .. }
        | Instr::GetBinding { .. }
        | Instr::SetBinding { .. }
        | Instr::DeclareBinding { .. }
        | Instr::Await { .. }
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
        Instr::Unary { dst, src, .. } => {
            uses.insert(src.index);
            defs.insert(dst.index);
        }
        Instr::CallImport { dst, args, .. } | Instr::CallFunction { dst, args, .. } => {
            for arg in args {
                uses.insert(arg.index);
            }
            defs.insert(dst.index);
        }
        Instr::CallValue {
            dst,
            callee,
            args,
            receiver,
        } => {
            uses.insert(callee.index);
            for arg in args {
                uses.insert(arg.index);
            }
            if let Some(slot) = receiver {
                uses.insert(slot.index);
            }
            defs.insert(dst.index);
        }
        Instr::Construct { dst, callee, args } => {
            uses.insert(callee.index);
            for arg in args {
                uses.insert(arg.index);
            }
            defs.insert(dst.index);
        }
        Instr::Closure { dst, .. }
        | Instr::NewObject { dst }
        | Instr::GetBinding { dst, .. }
        | Instr::GetBuiltin { dst, .. }
        | Instr::GetTopFunction { dst, .. } => {
            defs.insert(dst.index);
        }
        Instr::NewArray { dst, elements } => {
            for element in elements {
                uses.insert(element.index);
            }
            defs.insert(dst.index);
        }
        Instr::GetMember { dst, object, key } => {
            uses.insert(object.index);
            uses.insert(key.index);
            defs.insert(dst.index);
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
        Instr::Unary { dst, op, src } => Instr::Unary {
            dst: remap(dst),
            op,
            src: remap(src),
        },
        Instr::CallImport { dst, import, args } => Instr::CallImport {
            dst: remap(dst),
            import,
            args: args.into_iter().map(remap).collect(),
        },
        Instr::CallFunction {
            dst,
            function,
            args,
        } => Instr::CallFunction {
            dst: remap(dst),
            function,
            args: args.into_iter().map(remap).collect(),
        },
        Instr::CallValue {
            dst,
            callee,
            args,
            receiver,
        } => Instr::CallValue {
            dst: remap(dst),
            callee: remap(callee),
            args: args.into_iter().map(remap).collect(),
            receiver: receiver.map(remap),
        },
        Instr::Construct { dst, callee, args } => Instr::Construct {
            dst: remap(dst),
            callee: remap(callee),
            args: args.into_iter().map(remap).collect(),
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
        Instr::GetMember { dst, object, key } => Instr::GetMember {
            dst: remap(dst),
            object: remap(object),
            key: remap(key),
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
        Instr::GetTopFunction { dst, function } => Instr::GetTopFunction {
            dst: remap(dst),
            function,
        },
    }
}
