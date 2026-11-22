//! The instruction set of the bytecode contract.
//!
//! `instruction_table!` is the single declaration of the instruction set.
//! Its consumers generate `Instr` and an exhaustive validation dispatch.
//! Operand types and the build container stay in the parent module.
//!
//! Three properties of the table are wire format, not style:
//!
//! - Variant order is the bincode variant index and field order is the field
//!   order inside a variant, so both reach `Build` artifacts and
//!   `Build::compute_id`. Entries stay in the original order; appending one is
//!   compatible, reordering or removing one is a `BYTECODE_VERSION` change.
//! - A `slots[...]` list is the check order of that instruction and its labels
//!   are the reported error text, which is why `optional(object)` precedes
//!   `one(dst)` where it used to.
//! - `custom` keeps the whole check in `Build::validate_function`. Moving a
//!   check out of there reorders the first error, so it is a separate change.

use serde::{Deserialize, Serialize};

use crate::value::{BindingId, JsString};

use super::{
    AccessorPrefix, BinaryOp, CallArgs, ImportDecl, PrivateDefinition, PropertyDefinition, Slot,
    UnaryOp,
};

/// Emits the enum, preserving variant and field attributes and order.
macro_rules! define_instructions {
    ($(
        $(#[$attr:meta])*
        $variant:ident $({ $($(#[$field_attr:meta])* $field:ident: $ty:ty),* })?
        => $rule:ident $([$($checks:tt)*])?;
    )*) => {
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum Instr {
            $(
                $(#[$attr])*
                $variant $({ $($(#[$field_attr])* $field: $ty),* })?
            ),*
        }
    };
}

/// Emits one match over the entire instruction set. A `custom` entry must
/// match a rule of the caller's macro, with bindings in declaration order.
macro_rules! validate_instruction {
    ($instr:ident, $check:ident, $custom:ident; $(
        $(#[$attr:meta])*
        $variant:ident $({ $($(#[$field_attr:meta])* $field:ident: $ty:ty),* })?
        => $rule:ident $([$($checks:tt)*])?;
    )*) => {
        match $instr {
            $(
                #[allow(unused_variables)]
                $crate::bytecode::instr::Instr::$variant $({ $($field),* })? =>
                    $crate::bytecode::instr::dispatch!(
                        $custom, $check, $variant $({ $($field),* })? => $rule $([$($checks)*])?
                    ),
            )*
        }
    };
}

macro_rules! dispatch {
    ($custom:ident, $check:ident, $variant:ident $({ $($field:ident),* })? => custom) => {
        $custom!($variant $({ $($field),* })?)
    };
    ($custom:ident, $check:ident, $variant:ident $({ $($field:ident),* })? => slots [$($checks:tt)*]) => {{
        $crate::bytecode::instr::check_slots!($check, $variant; $($checks)*);
    }};
}

/// Expands one `slots[...]` list into the checks it names, in order.
macro_rules! check_slots {
    ($check:ident, $variant:ident;) => {};
    ($check:ident, $variant:ident; one($field:ident $(, $label:literal)?) $(, $($rest:tt)*)?) => {
        $check(*$field, $crate::bytecode::instr::slot_label!($variant, $field $(, $label)?))?;
        $crate::bytecode::instr::check_slots!($check, $variant; $($($rest)*)?);
    };
    ($check:ident, $variant:ident; optional($field:ident $(, $label:literal)?) $(, $($rest:tt)*)?) => {
        if let Some(slot) = $field {
            $check(*slot, $crate::bytecode::instr::slot_label!($variant, $field $(, $label)?))?;
        }
        $crate::bytecode::instr::check_slots!($check, $variant; $($($rest)*)?);
    };
    ($check:ident, $variant:ident; many($field:ident $(, $label:literal)?) $(, $($rest:tt)*)?) => {
        for slot in $field {
            $check(*slot, $crate::bytecode::instr::slot_label!($variant, $field $(, $label)?))?;
        }
        $crate::bytecode::instr::check_slots!($check, $variant; $($($rest)*)?);
    };
    ($check:ident, $variant:ident; args($field:ident $(, $label:literal)?) $(, $($rest:tt)*)?) => {
        for slot in $field.slots() {
            $check(*slot, $crate::bytecode::instr::slot_label!($variant, $field $(, $label)?))?;
        }
        $crate::bytecode::instr::check_slots!($check, $variant; $($($rest)*)?);
    };
}

/// Default operand label, unless the table names one.
macro_rules! slot_label {
    ($variant:ident, $field:ident) => {
        concat!(stringify!($variant), ".", stringify!($field))
    };
    ($variant:ident, $field:ident, $label:literal) => {
        $label
    };
}

/// Each entry ends in one of two validation rules:
///
/// - `custom`: checks are supplied by the consumer.
/// - `slots[...]`: run `one(field)` on a slot, `optional(field)` on a present
///   slot, `many(field)` on each element, and `args(field)` on `CallArgs::slots()`.
///   Each accepts a label overriding the default `Variant.field`.
macro_rules! instruction_table {
    ($consumer:ident; $($prefix:tt)*) => {
        $consumer! {
            $($prefix)*
            Const { dst: Slot, constant: u32 } => custom;
            Undefined { dst: Slot } => slots[one(dst)];
            Move { dst: Slot, src: Slot } => slots[one(dst), one(src)];
            Binary { dst: Slot, op: BinaryOp, lhs: Slot, rhs: Slot } => slots[
                one(dst), one(lhs), one(rhs)
            ];
            /// Binary operation whose right operand is a build constant.
            BinaryConst { dst: Slot, op: BinaryOp, lhs: Slot, constant: u32 } => custom;
            InstanceOf { dst: Slot, value: Slot, constructor: Slot } => slots[
                one(dst), one(value), one(constructor)
            ];
            Unary { dst: Slot, op: UnaryOp, src: Slot } => slots[one(dst), one(src)];
            /// Applies JavaScript ToString, including string-hint ToPrimitive.
            ToString { dst: Slot, src: Slot } => slots[one(dst), one(src)];
            /// Converts a computed key before its reference or definition is reused.
            ToPropertyKey {
                dst: Slot,
                src: Slot,
                /// For member read-modify-write, validate the base before converting.
                object: Option<Slot>
            } => slots[optional(object), one(dst), one(src)];
            GetImport { dst: Slot, import: ImportDecl } => custom;
            GetModuleBinding { dst: Slot, module: u32, binding: BindingId } => custom;
            GetModuleNamespace { dst: Slot, module: u32 } => custom;
            CallValue {
                dst: Slot,
                callee: Slot,
                args: CallArgs,
                /// `None` is a bare call (`this` is `undefined`). `Some` is the member
                /// base, evaluated once and passed as the receiver.
                receiver: Option<Slot>
            } => slots[one(dst), one(callee), args(args, "CallValue.arg"), optional(receiver)];
            /// Constructs `callee` with `args`. The new instance, not a member base, is `this`.
            Construct { dst: Slot, callee: Slot, args: CallArgs } => slots[
                one(dst), one(callee), args(args, "Construct.arg")
            ];
            SetHome { function: Slot, home: Slot } => slots[one(function), one(home)];
            SetFunctionName { function: Slot, key: Slot, prefix: Option<AccessorPrefix> } => slots[
                one(function), one(key)
            ];
            GetSuperBase { dst: Slot, home: Slot } => slots[one(dst), one(home)];
            GetSuper { dst: Slot, base: Slot, key: Slot, receiver: Slot } => slots[
                one(dst, "super value"),
                one(base, "super base"),
                one(key, "super key"),
                one(receiver, "super receiver")
            ];
            SetSuper { base: Slot, key: Slot, receiver: Slot, src: Slot } => slots[
                one(src, "super value"),
                one(base, "super base"),
                one(key, "super key"),
                one(receiver, "super receiver")
            ];
            GetDerivedThis { dst: Slot, context: Slot } => slots[
                one(dst, "derived constructor result"),
                one(context, "derived constructor context")
            ];
            GetSuperConstructor { dst: Slot, context: Slot } => slots[
                one(dst, "derived constructor result"),
                one(context, "derived constructor context")
            ];
            SuperConstruct { dst: Slot, context: Slot, callee: Slot, args: CallArgs } => custom;
            /// Creates a closure that captures the frame's current environment.
            Closure { dst: Slot, function: u32 } => custom;
            NewClassScope { dst: Slot, parent: Option<Slot> } => slots[
                one(dst, "class scope result"),
                optional(parent, "class scope")
            ];
            LoadClassBinding { dst: Slot, scope: Option<Slot>, binding: BindingId } => custom;
            InitClassBinding { scope: Slot, binding: BindingId, src: Slot } => custom;
            CreateClass {
                dst: Slot,
                scope: Option<Slot>,
                heritage: Option<Slot>,
                name_key: Option<Slot>,
                constructor: u32,
                default_constructor: bool,
                private_count: u32,
                private_binding: Option<BindingId>
            } => custom;
            ScopedClosure { dst: Slot, scope: Slot, function: u32 } => custom;
            DefineClassMethod {
                class: Slot,
                key: Slot,
                src: Slot,
                is_static: bool,
                kind: PropertyDefinition
            } => slots[one(class), one(key), one(src)];
            AddClassPrivate {
                class: Slot,
                index: u32,
                lexical: BindingId,
                src: Option<Slot>,
                is_static: bool,
                kind: PrivateDefinition
            } => custom;
            GetPrivate { dst: Slot, object: Slot, brand: Slot, index: u32, lexical: BindingId } => custom;
            SetPrivate { object: Slot, brand: Slot, src: Slot, index: u32, lexical: BindingId } => custom;
            HasPrivate { dst: Slot, object: Slot, brand: Slot, index: u32, lexical: BindingId } => custom;
            AddClassField { class: Slot, key: Slot, initializer: Option<Slot>, is_static: bool } => slots[
                one(class), one(key), optional(initializer)
            ];
            AddClassBlock { class: Slot, body: Slot } => slots[one(class), one(body)];
            RunClassStatic { class: Slot } => slots[one(class)];
            NewObject { dst: Slot } => slots[one(dst)];
            /// Allocates a fresh literal using the intrinsic RegExp prototype.
            NewRegExp { dst: Slot, source: JsString, flags: String } => custom;
            /// Copies enumerable own properties from source onto target.
            CopyDataProperties { target: Slot, source: Slot } => slots[one(target), one(source)];
            NewArray { dst: Slot, elements: Vec<Slot> } => slots[
                one(dst), many(elements, "NewArray.element")
            ];
            /// Appends one value to an array created by [`Instr::NewArray`].
            ArrayAppend { array: Slot, src: Slot } => slots[one(array), one(src)];
            /// Grows an array's `length` by one without creating an element property,
            /// forming a sparse hole.
            ArrayHole { array: Slot } => slots[one(array)];
            GetMember { dst: Slot, object: Slot, key: Slot } => slots[one(dst), one(object), one(key)];
            /// Reads a property whose key is a string in the constant pool.
            GetMemberConstKey { dst: Slot, object: Slot, key: u32 } => custom;
            /// Tests property presence without reading the property value.
            HasProperty { dst: Slot, object: Slot, key: Slot } => slots[
                one(dst), one(object), one(key)
            ];
            /// Deletes an own property through a strict-mode member reference.
            DeleteMember { dst: Slot, object: Slot, key: Slot } => slots[
                one(dst), one(object), one(key)
            ];
            SetMember { object: Slot, key: Slot, src: Slot } => slots[one(object), one(key), one(src)];
            DefineProperty {
                object: Slot,
                key: Slot,
                src: Slot,
                kind: PropertyDefinition
            } => slots[one(object), one(key), one(src)];
            GetBinding { dst: Slot, binding: BindingId } => custom;
            SetBinding { binding: BindingId, src: Slot } => custom;
            DeclareBinding { binding: BindingId } => custom;
            /// Reads a binding through a statically known number of environment
            /// parents. `depth` is relative to the frame's current environment.
            GetBindingDirect { dst: Slot, depth: u16, binding: BindingId } => custom;
            /// Writes a binding through a statically known environment depth.
            SetBindingDirect { depth: u16, binding: BindingId, src: Slot } => custom;
            /// Reads a source module's own binding without walking the closure
            /// environment chain. Unlike the linked `GetModuleBinding`, this keeps the
            /// ordinary binding read rule that a present but uninitialized slot yields
            /// `undefined`.
            GetModuleBindingDirect { dst: Slot, module: u32, binding: BindingId } => custom;
            /// Writes a source module's own binding directly. It removes the module
            /// TDZ marker only when the writing frame is the module environment itself,
            /// matching `SetBinding` for a nested function.
            SetModuleBindingDirect { module: u32, binding: BindingId, src: Slot } => custom;
            /// Writes the frame's current environment to `dst`. Used by loops that
            /// need a fresh per-iteration environment and must restore the outer one.
            SaveEnv { dst: Slot } => slots[one(dst)];
            /// Allocates a fresh environment whose parent is `parent` and copies the
            /// listed bindings from the frame's current environment, then installs the
            /// new environment for the loop body. Generator and async-generator bodies
            /// keep the same environment object across a yield only when it is the one
            /// selected by this instruction, so closures created in an iteration keep
            /// that iteration's values.
            NewIterationEnv { parent: Slot, bindings: Vec<BindingId> } => custom;
            /// Restores the frame's environment from `src` after a loop.
            RestoreEnv { src: Slot } => slots[one(src)];
            /// Suspends the current async frame. The VM writes the awaited value to
            /// either resume destination and branches to the matching resume PC.
            Await { dst: Slot, input: Slot, resume: u32 } => custom;
            /// Suspends a generator, handing `input` to its consumer.
            ///
            /// The VM stores `generator_resumes[generator].saved_slots` and returns the
            /// yielded value. Resuming enters `next_pc` (the fall-through here) or
            /// `throw_pc` with the injected value already written, or `return_pc` with
            /// a pending completion payload for `LoadGeneratorReturn`.
            ///
            /// An async generator never yields a delegate result object: the compiler
            /// awaits the operand and the request queue wraps the value.
            Yield {
                input: Slot,
                generator: u32,
                /// `input` is already the delegate's own result object, handed back
                /// unwrapped instead of being wrapped in a fresh iteration result.
                delegate: bool
            } => custom;
            /// Ends the invocation after a generator's parameters were initialized.
            ///
            /// The VM captures the environment and `generator_start.saved_slots` into a
            /// new generator object and returns it. Control never falls through here.
            GeneratorStart => custom;
            /// Reads the return payload of the current generator resume. Valid only as
            /// the first instruction of a `GeneratorEntry::return_pc`, so the pending
            /// completion value never lives in VM-global state.
            LoadGeneratorReturn { dst: Slot } => slots[one(dst)];
            Jump { target: u32 } => custom;
            BranchFalse { cond: Slot, target: u32 } => custom;
            /// Branches when the condition is truthy.
            BranchTrue { cond: Slot, target: u32 } => custom;
            /// Evaluates a comparison and branches when its boolean result is false.
            /// The result never occupies a frame slot.
            BranchCompareFalse { lhs: Slot, op: BinaryOp, rhs: Slot, target: u32 } => custom;
            /// Comparison-with-constant variant of `BranchCompareFalse`.
            BranchCompareConstFalse { lhs: Slot, op: BinaryOp, constant: u32, target: u32 } => custom;
            /// Evaluates a comparison and branches when its boolean result is true.
            BranchCompareTrue { lhs: Slot, op: BinaryOp, rhs: Slot, target: u32 } => custom;
            /// Comparison-with-constant variant of `BranchCompareTrue`.
            BranchCompareConstTrue { lhs: Slot, op: BinaryOp, constant: u32, target: u32 } => custom;
            /// Evaluates an arbitrary binary result and branches when it is falsy.
            BranchBinary { op: BinaryOp, lhs: Slot, rhs: Slot, target: u32 } => custom;
            /// Binary-with-constant variant of `BranchBinary`.
            BranchBinaryConst { op: BinaryOp, lhs: Slot, constant: u32, target: u32 } => custom;
            /// Evaluates an arbitrary unary result and branches when it is falsy.
            BranchUnary { op: UnaryOp, src: Slot, target: u32 } => custom;
            Return { src: Slot } => slots[one(src)];
            Throw { src: Slot } => slots[one(src)];
            /// Throws a TypeError for an assignment to a constant binding.
            ThrowAssignmentToConst => slots[];
            ThrowDeleteSuper => slots[];
            /// Loads a cached builtin value (object builtins, or the number globals
            /// `NaN` and `Infinity`).
            GetBuiltin { dst: Slot, builtin: u32 } => custom;
            /// Loads the cached frozen template object for one tagged-template call site.
            GetTemplate { dst: Slot, template: u32 } => custom;
            /// Calls the intrinsic `Symbol.iterator` method and caches the result's `next` method.
            GetIterator { iterator: Slot, next: Slot, iterable: Slot } => custom;
            /// Calls one iterator method with the iterator as receiver and requires an
            /// object result, preserving that result object's own identity.
            ///
            /// `argument` is `None` for a method called with no arguments.
            IteratorCall {
                dst: Slot,
                iterator: Slot,
                method: Slot,
                argument: Option<Slot>
            } => slots[one(dst), one(iterator), one(method), optional(argument)];
            /// Calls cached `next` and reads `done`, then `value` only when not done.
            IteratorNext { done: Slot, value: Slot, iterator: Slot, next: Slot } => custom;
            /// Calls the optional `return` method. A pending throw takes precedence over script errors.
            IteratorClose { iterator: Slot, preserve_throw: bool } => slots[one(iterator)];
            /// Creates an internal property enumeration cursor, independent of Symbol.iterator.
            GetForInIterator { dst: Slot, source: Slot } => slots[one(dst), one(source)];
            /// Advances the property enumeration cursor and yields a string key or undefined.
            ForInNext { done: Slot, value: Slot, iterator: Slot } => custom;
            /// Throws a TypeError for null or undefined without otherwise converting the value.
            AssertObjectCoercible { src: Slot } => slots[one(src)];
            /// Throws a TypeError unless `src` is an object, without converting it.
            ///
            /// An asynchronous iterator method resolves to an object that must be
            /// rechecked after the implicit `await`.
            RequireObject { src: Slot } => slots[one(src)];
            /// Reads a property, projecting primitive sources as needed. The key is already converted.
            GetDestructuredMember { dst: Slot, object: Slot, key: Slot } => slots[
                one(dst), one(object), one(key)
            ];
            /// Copies own enumerable properties into a new object, excluding converted keys.
            ObjectRest { dst: Slot, src: Slot, excluded: Vec<Slot> } => slots[
                one(dst), one(src), many(excluded, "ObjectRest.excluded")
            ];
            /// Calls cached `next` and reads only `done`, without accessing `value`.
            IteratorSkip { done: Slot, iterator: Slot, next: Slot } => slots[
                one(done), one(iterator), one(next)
            ];
        }
    };
}

pub(super) use {check_slots, dispatch, instruction_table, slot_label, validate_instruction};

instruction_table!(define_instructions;);

#[cfg(test)]
mod tests {
    use super::{Instr, Slot};

    macro_rules! unexpected_custom {
        ($variant:ident $({ $($field:ident),* })?) => {
            return Err(concat!("unexpected custom instruction: ", stringify!($variant)).to_owned())
        };
    }

    /// A `slots[...]` instruction reports its checks in the listed order, so the
    /// first reported label is the one the handwritten arm reported.
    #[test]
    fn slot_checks_report_listed_order() {
        let instruction = &Instr::ToPropertyKey {
            dst: Slot::transient(1),
            src: Slot::transient(2),
            object: Some(Slot::transient(3)),
        };
        let check_slot = |_, label: &str| Err(label.to_owned());
        let error = (|| -> Result<(), String> {
            super::instruction_table!(validate_instruction; instruction, check_slot, unexpected_custom;);
            Ok(())
        })()
        .expect_err("the check always fails");
        assert_eq!(error, "ToPropertyKey.object");
    }

    /// An absent optional operand adds no check to the ordered list.
    #[test]
    fn slot_checks_visit_every_operand_in_the_list() {
        let instruction = &Instr::IteratorCall {
            dst: Slot::transient(1),
            iterator: Slot::transient(2),
            method: Slot::transient(3),
            argument: None,
        };
        let mut labels = Vec::new();
        let mut check_slot = |_, label| -> Result<(), String> {
            labels.push(label);
            Ok(())
        };
        (|| -> Result<(), String> {
            super::instruction_table!(validate_instruction; instruction, check_slot, unexpected_custom;);
            Ok(())
        })()
        .expect("all listed slots are valid");
        assert_eq!(
            labels,
            [
                "IteratorCall.dst",
                "IteratorCall.iterator",
                "IteratorCall.method"
            ]
        );
    }
}
