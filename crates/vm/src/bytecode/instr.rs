//! The instruction set of the bytecode contract.
//!
//! [`define_instructions!`] is the single declaration of that set: it emits
//! `Instr` and its exhaustive slot validation from one table, so a new
//! instruction cannot be added to one and forgotten in the other. Operand
//! types, the build container and the checks that are more than a slot range
//! stay in the parent module.
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

/// Declares every instruction once and derives both `Instr` and
/// `Instr::validate_slots_only` from it.
///
/// Each entry ends in one of two rules:
///
/// - `custom`: the instruction needs more than slot ranges, so
///   `validate_slots_only` reports `Ok(false)` and the caller runs the
///   handwritten arm in `Build::validate_function`.
/// - `slots[...]`: the instruction is validated completely here, in the listed
///   order. `one(field)` dereferences a slot, `optional(field)` checks a
///   present one, `many(field)` walks a slot collection in order and
///   `args(field)` walks `CallArgs::slots()`; each takes an optional label that
///   replaces the default `concat!(variant, ".", field)`. Empty collections and
///   absent options never call the check.
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

        impl Instr {
            /// Checks the slot operands of an instruction that needs nothing
            /// else, reporting the same messages in the same order the
            /// handwritten arms used to.
            ///
            /// `Ok(true)` means the instruction is fully validated; `Ok(false)`
            /// leaves it to the caller.
            pub(super) fn validate_slots_only(
                &self,
                mut check: impl FnMut(Slot, &'static str) -> Result<(), String>,
            ) -> Result<bool, String> {
                match self {
                    $(
                        Self::$variant { .. } => define_arm_body!(
                            self,
                            check,
                            $variant => $rule $([$($checks)*])?
                        ),
                    )*
                }
            }
        }
    };
}

/// Emits one match arm body of `validate_slots_only`.
///
/// A `slots[...]` entry binds each operand where its check needs it, so the
/// arm binds exactly the fields the list names.
macro_rules! define_arm_body {
    ($slf:ident, $check:ident, $variant:ident => custom) => {
        Ok(false)
    };
    ($slf:ident, $check:ident, $variant:ident => slots [$($checks:tt)*]) => {{
        check_slots!($slf, $check, $variant; $($checks)*);
        Ok(true)
    }};
}

/// Borrows one operand of the instruction the enclosing arm already matched.
macro_rules! operand {
    ($slf:ident, $variant:ident, $field:ident) => {{
        let Self::$variant { $field, .. } = $slf else {
            unreachable!("slot-only instruction already matched")
        };
        $field
    }};
}

/// Expands one `slots[...]` list into the checks it names, in order.
macro_rules! check_slots {
    ($slf:ident, $check:ident, $variant:ident;) => {};
    ($slf:ident, $check:ident, $variant:ident; one($field:ident $(, $label:literal)?) $(, $($rest:tt)*)?) => {
        $check(
            *operand!($slf, $variant, $field),
            slot_label!($variant, $field $(, $label)?),
        )?;
        check_slots!($slf, $check, $variant; $($($rest)*)?);
    };
    ($slf:ident, $check:ident, $variant:ident; optional($field:ident $(, $label:literal)?) $(, $($rest:tt)*)?) => {
        if let Some(slot) = operand!($slf, $variant, $field) {
            $check(*slot, slot_label!($variant, $field $(, $label)?))?;
        }
        check_slots!($slf, $check, $variant; $($($rest)*)?);
    };
    ($slf:ident, $check:ident, $variant:ident; many($field:ident $(, $label:literal)?) $(, $($rest:tt)*)?) => {
        for slot in operand!($slf, $variant, $field) {
            $check(*slot, slot_label!($variant, $field $(, $label)?))?;
        }
        check_slots!($slf, $check, $variant; $($($rest)*)?);
    };
    ($slf:ident, $check:ident, $variant:ident; args($field:ident $(, $label:literal)?) $(, $($rest:tt)*)?) => {
        for slot in operand!($slf, $variant, $field).slots() {
            $check(*slot, slot_label!($variant, $field $(, $label)?))?;
        }
        check_slots!($slf, $check, $variant; $($($rest)*)?);
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

define_instructions! {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A `slots[...]` instruction reports its checks in the listed order, so the
    /// first reported label is the one the handwritten arm reported.
    #[test]
    fn slot_checks_report_listed_order() {
        let instruction = Instr::ToPropertyKey {
            dst: Slot::transient(1),
            src: Slot::transient(2),
            object: Some(Slot::transient(3)),
        };
        let error = instruction
            .validate_slots_only(|_, label| Err(label.to_owned()))
            .expect_err("the check always fails");
        assert_eq!(error, "ToPropertyKey.object");
    }

    /// A `custom` instruction is not validated here, so the closure must not run.
    #[test]
    fn custom_instruction_defers_to_the_caller() {
        let instruction = Instr::Const {
            dst: Slot::transient(1),
            constant: 0,
        };
        let validated = instruction
            .validate_slots_only(|_, label| Err(label.to_owned()))
            .expect("no slot check runs");
        assert!(!validated);
    }

    /// Out-of-range operands and empty collections.
    #[test]
    fn slot_checks_visit_every_operand_in_the_list() {
        let instruction = Instr::IteratorCall {
            dst: Slot::transient(1),
            iterator: Slot::transient(2),
            method: Slot::transient(3),
            argument: None,
        };
        let mut labels = Vec::new();
        let validated = instruction
            .validate_slots_only(|_, label| {
                labels.push(label);
                Ok(())
            })
            .expect("valid");
        assert!(validated);
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
