//! Versioned bytecode contract shared by the compiler and the VM.
//!
//! The VM owns this module: the compiler depends on it, never the other way
//! around. All operands are explicit slots. No implicit operand stack is
//! persisted across an `Await`.

use std::collections::{BTreeMap, btree_map::Entry};

use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};

use crate::value::BindingId;

pub const BYTECODE_VERSION: u32 = 24;

/// VM-provided global names, in builtin-index order.
/// `0` is `Promise` (constructor and static methods). `1` is `queueMicrotask`.
/// Entries `7` (`NaN`) and `8` (`Infinity`) are primitive number values, not
/// objects; they load via [`Instr::GetBuiltin`]. Error constructors are appended
/// after `String` at indices 11–16. The compiler and `Build::validate` both use
/// this table; do not reorder it.
pub const BUILTINS: [&str; 18] = [
    "Promise",
    "queueMicrotask",
    "Error",
    "TypeError",
    "Symbol",
    "Reflect",
    "Object",
    "NaN",
    "Infinity",
    "Array",
    "String",
    "RangeError",
    "ReferenceError",
    "SyntaxError",
    "EvalError",
    "URIError",
    "AggregateError",
    "Number",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotKind {
    /// Discarded when an async frame suspends.
    Transient,
    /// Candidate for retention at a resume point.
    Persistent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Slot {
    pub kind: SlotKind,
    pub index: u16,
}

impl Slot {
    pub const fn transient(index: u16) -> Self {
        Self {
            kind: SlotKind::Transient,
            index,
        }
    }

    pub const fn persistent(index: u16) -> Self {
        Self {
            kind: SlotKind::Persistent,
            index,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallArgs {
    Slots(Vec<Slot>),
    /// A private, dense argument list built with `NewArray` and `ArrayAppend`.
    Spread(Slot),
}

impl CallArgs {
    pub fn slots(&self) -> &[Slot] {
        match self {
            Self::Slots(slots) => slots,
            Self::Spread(slot) => std::slice::from_ref(slot),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Constant {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportDecl {
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Exp,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    StrictEq,
    StrictNe,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    UShr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnaryOp {
    Neg,
    Pos,
    Not,
    BitNot,
    TypeOf,
    Void,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PropertyDefinition {
    Data,
    Get,
    Set,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "instruction", rename_all = "snake_case")]
pub enum Instr {
    Const {
        dst: Slot,
        constant: u32,
    },
    Undefined {
        dst: Slot,
    },
    Move {
        dst: Slot,
        src: Slot,
    },
    Binary {
        dst: Slot,
        op: BinaryOp,
        lhs: Slot,
        rhs: Slot,
    },
    InstanceOf {
        dst: Slot,
        value: Slot,
        constructor: Slot,
    },
    Unary {
        dst: Slot,
        op: UnaryOp,
        src: Slot,
    },
    /// Applies JavaScript ToString, including string-hint ToPrimitive.
    ToString {
        dst: Slot,
        src: Slot,
    },
    /// Converts a computed key before its reference or definition is reused.
    ToPropertyKey {
        dst: Slot,
        src: Slot,
        /// For member read-modify-write, validate the base before converting.
        object: Option<Slot>,
    },
    CallImport {
        dst: Slot,
        import: u32,
        args: CallArgs,
    },
    CallValue {
        dst: Slot,
        callee: Slot,
        args: CallArgs,
        /// `None` is a bare call (`this` is `undefined`). `Some` is the member
        /// base, evaluated once and passed as the receiver.
        receiver: Option<Slot>,
    },
    /// Constructs `callee` with `args`. The new instance, not a member base, is `this`.
    Construct {
        dst: Slot,
        callee: Slot,
        args: CallArgs,
    },
    /// Creates a closure that captures the frame's current environment.
    Closure {
        dst: Slot,
        function: u32,
    },
    NewObject {
        dst: Slot,
    },
    /// Copies enumerable own properties from source onto target.
    CopyDataProperties {
        target: Slot,
        source: Slot,
    },
    NewArray {
        dst: Slot,
        elements: Vec<Slot>,
    },
    /// Appends one value to an array created by [`Instr::NewArray`].
    ArrayAppend {
        array: Slot,
        src: Slot,
    },
    GetMember {
        dst: Slot,
        object: Slot,
        key: Slot,
    },
    /// Tests property presence without reading the property value.
    HasProperty {
        dst: Slot,
        object: Slot,
        key: Slot,
    },
    /// Deletes an own property through a strict-mode member reference.
    DeleteMember {
        dst: Slot,
        object: Slot,
        key: Slot,
    },
    SetMember {
        object: Slot,
        key: Slot,
        src: Slot,
    },
    DefineProperty {
        object: Slot,
        key: Slot,
        src: Slot,
        kind: PropertyDefinition,
    },
    GetBinding {
        dst: Slot,
        binding: BindingId,
    },
    SetBinding {
        binding: BindingId,
        src: Slot,
    },
    DeclareBinding {
        binding: BindingId,
    },
    /// Suspends the current async frame. The VM writes the awaited value to
    /// either resume destination and branches to the matching resume PC.
    Await {
        dst: Slot,
        input: Slot,
        resume: u32,
    },
    Jump {
        target: u32,
    },
    BranchFalse {
        cond: Slot,
        target: u32,
    },
    Return {
        src: Slot,
    },
    Throw {
        src: Slot,
    },
    /// Throws a TypeError for an assignment to a constant binding.
    ThrowAssignmentToConst,
    /// Loads a cached builtin value (object builtins, or the number globals
    /// `NaN` and `Infinity`).
    GetBuiltin {
        dst: Slot,
        builtin: u32,
    },
    /// Calls the intrinsic `Symbol.iterator` method and caches the result's `next` method.
    GetIterator {
        iterator: Slot,
        next: Slot,
        iterable: Slot,
    },
    /// Calls cached `next` and reads `done`, then `value` only when not done.
    IteratorNext {
        done: Slot,
        value: Slot,
        iterator: Slot,
        next: Slot,
    },
    /// Calls the optional `return` method. A pending throw takes precedence over script errors.
    IteratorClose {
        iterator: Slot,
        preserve_throw: bool,
    },
    /// Creates an internal property enumeration cursor, independent of Symbol.iterator.
    GetForInIterator {
        dst: Slot,
        source: Slot,
    },
    /// Advances the property enumeration cursor and yields a string key or undefined.
    ForInNext {
        done: Slot,
        value: Slot,
        iterator: Slot,
    },
    /// Throws a TypeError for null or undefined without otherwise converting the value.
    AssertObjectCoercible {
        src: Slot,
    },
    /// Reads a property, projecting primitive sources as needed. The key is already converted.
    GetDestructuredMember {
        dst: Slot,
        object: Slot,
        key: Slot,
    },
    /// Copies own enumerable properties into a new object, excluding converted keys.
    ObjectRest {
        dst: Slot,
        src: Slot,
        excluded: Vec<Slot>,
    },
    /// Calls cached `next` and reads only `done`, without accessing `value`.
    IteratorSkip {
        done: Slot,
        iterator: Slot,
        next: Slot,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeEntry {
    pub fulfill_pc: u32,
    pub fulfill_dst: Slot,
    pub reject_pc: u32,
    pub reject_dst: Slot,
    /// Strictly ascending persistent-slot indices. Values are stored in this
    /// order in the continuation and snapshot.
    pub saved_slots: Vec<u16>,
}

/// Static exception region. The VM writes the thrown value to `dst` and jumps
/// to `target_pc`. Regions may nest; the innermost covering PC wins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExceptionHandler {
    pub start_pc: u32,
    pub end_pc: u32,
    pub target_pc: u32,
    pub dst: Slot,
}
/// Where one source parameter is written before the first instruction.
///
/// A shared binding stays in the invocation environment. A binding private to
/// this function is a frame slot. The two destinations cannot share one field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParamTarget {
    Binding(BindingId),
    Slot(Slot),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Function {
    pub func_id: u32,
    pub name: String,
    pub is_async: bool,
    pub is_arrow: bool,
    pub is_method: bool,
    /// Positional parameter destinations in source order.
    pub params: Vec<ParamTarget>,
    /// Destination for a fresh array of arguments beyond the positional parameters.
    pub rest_param: Option<ParamTarget>,
    /// Receiver binding when this function, or an arrow nested in it, uses `this`.
    /// Not a JavaScript argument.
    pub this_param: Option<ParamTarget>,
    /// Destination for the fresh, unmapped arguments object.
    pub arguments_param: Option<ParamTarget>,
    pub transient_slots: u16,
    pub persistent_slots: u16,
    pub code: Vec<Instr>,
    pub resumes: Vec<ResumeEntry>,
    pub handlers: Vec<ExceptionHandler>,
}

impl Function {
    pub fn is_constructable(&self) -> bool {
        !self.is_async && !self.is_arrow && !self.is_method
    }

    pub(crate) fn is_accessor_method(&self) -> bool {
        self.is_method && !self.is_async && !self.is_arrow
    }

    /// Whether this function allocates an invocation environment.
    ///
    /// Derived from validated code. A bytecode flag would be forgeable and is
    /// not part of the contract. Closure creation keeps the existing capture
    /// protocol even when this function's own bindings were promoted.
    pub fn needs_environment(&self) -> bool {
        if self
            .params
            .iter()
            .chain(&self.rest_param)
            .chain(&self.this_param)
            .chain(&self.arguments_param)
            .any(|param| matches!(param, ParamTarget::Binding(_)))
        {
            return true;
        }
        self.code.iter().any(|instruction| {
            matches!(
                instruction,
                Instr::GetBinding { .. }
                    | Instr::SetBinding { .. }
                    | Instr::DeclareBinding { .. }
                    | Instr::Closure { .. }
            )
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Build {
    pub bytecode_version: u32,
    pub build_id: String,
    pub constants: Vec<Constant>,
    pub imports: Vec<ImportDecl>,
    pub functions: Vec<Function>,
    /// Compiler script function. It runs once when the VM is created.
    pub script: u32,
    /// Public module name to a live binding declared in [`Self::script`].
    #[serde(deserialize_with = "deserialize_exports")]
    pub exports: BTreeMap<String, BindingId>,
}

fn deserialize_exports<'de, D>(deserializer: D) -> Result<BTreeMap<String, BindingId>, D::Error>
where
    D: Deserializer<'de>,
{
    struct Exports;

    impl<'de> Visitor<'de> for Exports {
        type Value = BTreeMap<String, BindingId>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a map of unique export names to binding IDs")
        }

        fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
            let mut exports = BTreeMap::new();
            while let Some(name) = map.next_key::<String>()? {
                match exports.entry(name) {
                    Entry::Vacant(entry) => {
                        entry.insert(map.next_value()?);
                    }
                    Entry::Occupied(entry) => {
                        return Err(de::Error::custom(format_args!(
                            "duplicate export name {:?}",
                            entry.key()
                        )));
                    }
                }
            }
            Ok(exports)
        }
    }

    deserializer.deserialize_map(Exports)
}

#[derive(Serialize)]
struct DigestInput<'a> {
    bytecode_version: u32,
    execution_revision: u32,
    constants: &'a [Constant],
    imports: &'a [ImportDecl],
    functions: &'a [Function],
    script: u32,
    exports: &'a BTreeMap<String, BindingId>,
}

impl Build {
    pub fn new(
        constants: Vec<Constant>,
        imports: Vec<ImportDecl>,
        functions: Vec<Function>,
        script: u32,
        exports: BTreeMap<String, BindingId>,
    ) -> Self {
        Self {
            bytecode_version: BYTECODE_VERSION,
            build_id: String::new(),
            constants,
            imports,
            functions,
            script,
            exports,
        }
    }

    /// Sets `build_id` to a digest of every execution-relevant field. Debug
    /// metadata is deliberately absent from the contract.
    pub fn finalize(&mut self) {
        self.build_id = self.compute_id();
    }
    pub fn compute_id(&self) -> String {
        let input = DigestInput {
            bytecode_version: self.bytecode_version,
            // Reject artifacts from older builtin semantics even when their
            // instruction stream and snapshot record layout are unchanged.
            execution_revision: 5,
            constants: &self.constants,
            imports: &self.imports,
            functions: &self.functions,
            script: self.script,
            exports: &self.exports,
        };
        let bytes = serde_json::to_vec(&input).expect("bytecode digest serialization cannot fail");
        let digest = Sha256::digest(bytes);
        let mut out = String::with_capacity(7 + digest.len() * 2);
        out.push_str("sha256:");
        for byte in digest {
            use std::fmt::Write;
            write!(out, "{byte:02x}").expect("writing to a String cannot fail");
        }
        out
    }

    pub fn function(&self, id: u32) -> Option<&Function> {
        self.functions
            .iter()
            .find(|function| function.func_id == id)
    }

    pub fn function_named(&self, name: &str) -> Option<&Function> {
        self.functions.iter().find(|function| function.name == name)
    }

    /// Structural validation performed before any script code runs.
    pub fn validate(&self) -> Result<(), String> {
        if self.bytecode_version != BYTECODE_VERSION {
            return Err(format!(
                "unsupported bytecode version {}, expected {}",
                self.bytecode_version, BYTECODE_VERSION
            ));
        }
        if self.build_id != self.compute_id() {
            return Err("build_id does not match the bytecode content".to_owned());
        }
        for (index, constant) in self.constants.iter().enumerate() {
            if let Constant::Number(value) = constant
                && !value.is_finite()
            {
                return Err(format!("constant {index} is not a finite number"));
            }
        }

        let mut import_names = std::collections::BTreeSet::new();
        for import in &self.imports {
            if import.name.is_empty() {
                return Err("empty host import name".to_owned());
            }
            if !import_names.insert(import.name.as_str()) {
                return Err(format!("duplicate host import {:?}", import.name));
            }
        }

        let mut func_ids = std::collections::BTreeSet::new();
        let mut func_names = std::collections::BTreeSet::new();
        for function in &self.functions {
            if function.is_method && function.is_arrow {
                return Err(format!(
                    "function {} cannot be both a method and an arrow",
                    function.func_id
                ));
            }
            if !func_ids.insert(function.func_id) {
                return Err(format!("duplicate func_id {}", function.func_id));
            }
            if function.name.is_empty() {
                return Err(format!("function {} has an empty name", function.func_id));
            }
            if !func_names.insert(function.name.as_str()) {
                return Err(format!("duplicate function name {:?}", function.name));
            }
        }

        for function in &self.functions {
            self.validate_function(function)?;
        }
        self.validate_script()?;
        Ok(())
    }

    fn validate_script(&self) -> Result<(), String> {
        let Some(script) = self.function(self.script) else {
            return Err(format!("script function {} does not exist", self.script));
        };
        if script.is_async || script.is_arrow || script.is_method {
            return Err(format!(
                "script function {} must be a synchronous ordinary function",
                self.script
            ));
        }
        if !script.params.is_empty()
            || script.rest_param.is_some()
            || script.this_param.is_some()
            || script.arguments_param.is_some()
        {
            return Err(format!(
                "script function {} must not take arguments or a receiver",
                self.script
            ));
        }
        let mut declared = std::collections::BTreeSet::new();
        for function in &self.functions {
            for instruction in &function.code {
                if let Instr::Closure {
                    function: callee, ..
                } = instruction
                    && *callee == self.script
                {
                    return Err(format!(
                        "function {} ({}) closes over the script function",
                        function.func_id, function.name
                    ));
                }
                if function.func_id == self.script
                    && let Instr::DeclareBinding { binding } = instruction
                {
                    declared.insert(*binding);
                }
            }
        }
        for (name, binding) in &self.exports {
            if !declared.contains(binding) {
                return Err(format!(
                    "export {name:?} binding {binding} is not declared in the script"
                ));
            }
        }
        Ok(())
    }
    fn check_builtin(
        &self,
        builtin: u32,
        location: &str,
        pc: usize,
        what: &str,
    ) -> Result<(), String> {
        if builtin as usize >= BUILTINS.len() {
            return Err(format!(
                "{location}: {what} at pc {pc} references builtin {builtin}"
            ));
        }
        Ok(())
    }

    fn validate_function(&self, function: &Function) -> Result<(), String> {
        let location = format!("function {} ({})", function.func_id, function.name);
        if function.code.is_empty() {
            return Err(format!("{location}: empty code"));
        }
        if function.resumes.len() > u32::MAX as usize {
            return Err(format!("{location}: too many resume entries"));
        }

        let check_slot = |slot: Slot, what: &str| -> Result<(), String> {
            let limit = match slot.kind {
                SlotKind::Transient => function.transient_slots,
                SlotKind::Persistent => function.persistent_slots,
            };
            if slot.index >= limit {
                return Err(format!(
                    "{location}: {what} references out-of-range {:?} slot {} (size {limit})",
                    slot.kind, slot.index
                ));
            }
            Ok(())
        };

        if function.params.len() > u16::MAX as usize {
            return Err(format!("{location}: too many parameters"));
        }
        if function.is_arrow && function.arguments_param.is_some() {
            return Err(format!("{location}: arrow cannot have an arguments target"));
        }
        let mut seen_bindings = std::collections::BTreeSet::new();
        let mut seen_slots = Vec::new();
        for param in function
            .params
            .iter()
            .chain(&function.rest_param)
            .chain(&function.this_param)
            .chain(&function.arguments_param)
        {
            match param {
                ParamTarget::Binding(binding) => {
                    if !seen_bindings.insert(*binding) {
                        return Err(format!("{location}: duplicate parameter binding {binding}"));
                    }
                }
                ParamTarget::Slot(slot) => {
                    check_slot(*slot, "parameter")?;
                    if seen_slots.contains(slot) {
                        return Err(format!("{location}: duplicate parameter slot {slot:?}"));
                    }
                    seen_slots.push(*slot);
                }
            }
        }

        for (pc, instr) in function.code.iter().enumerate() {
            let check_target = |target: u32, what: &str| -> Result<(), String> {
                if target as usize >= function.code.len() {
                    return Err(format!(
                        "{location}: {what} at pc {pc} targets pc {target}, code len {}",
                        function.code.len()
                    ));
                }
                Ok(())
            };
            match instr {
                Instr::Const { dst, constant } => {
                    check_slot(*dst, "Const.dst")?;
                    if *constant as usize >= self.constants.len() {
                        return Err(format!(
                            "{location}: Const at pc {pc} references constant {constant}"
                        ));
                    }
                }
                Instr::Undefined { dst } => check_slot(*dst, "Undefined.dst")?,
                Instr::Move { dst, src } => {
                    check_slot(*dst, "Move.dst")?;
                    check_slot(*src, "Move.src")?;
                }
                Instr::Binary { dst, lhs, rhs, .. } => {
                    check_slot(*dst, "Binary.dst")?;
                    check_slot(*lhs, "Binary.lhs")?;
                    check_slot(*rhs, "Binary.rhs")?;
                }
                Instr::InstanceOf {
                    dst,
                    value,
                    constructor,
                } => {
                    check_slot(*dst, "InstanceOf.dst")?;
                    check_slot(*value, "InstanceOf.value")?;
                    check_slot(*constructor, "InstanceOf.constructor")?;
                }
                Instr::Unary { dst, src, .. } => {
                    check_slot(*dst, "Unary.dst")?;
                    check_slot(*src, "Unary.src")?;
                }
                Instr::ToString { dst, src } => {
                    check_slot(*dst, "ToString.dst")?;
                    check_slot(*src, "ToString.src")?;
                }
                Instr::ToPropertyKey { dst, src, object } => {
                    if let Some(object) = object {
                        check_slot(*object, "ToPropertyKey.object")?;
                    }
                    check_slot(*dst, "ToPropertyKey.dst")?;
                    check_slot(*src, "ToPropertyKey.src")?;
                }
                Instr::CallImport { dst, import, args } => {
                    check_slot(*dst, "CallImport.dst")?;
                    if *import as usize >= self.imports.len() {
                        return Err(format!(
                            "{location}: CallImport at pc {pc} references import {import}"
                        ));
                    }
                    for arg in args.slots() {
                        check_slot(*arg, "CallImport.arg")?;
                    }
                }
                Instr::CallValue {
                    dst,
                    callee,
                    args,
                    receiver,
                } => {
                    check_slot(*dst, "CallValue.dst")?;
                    check_slot(*callee, "CallValue.callee")?;
                    for arg in args.slots() {
                        check_slot(*arg, "CallValue.arg")?;
                    }
                    if let Some(slot) = receiver {
                        check_slot(*slot, "CallValue.receiver")?;
                    }
                }
                Instr::Construct { dst, callee, args } => {
                    check_slot(*dst, "Construct.dst")?;
                    check_slot(*callee, "Construct.callee")?;
                    for arg in args.slots() {
                        check_slot(*arg, "Construct.arg")?;
                    }
                }
                Instr::Closure {
                    dst,
                    function: callee,
                } => {
                    check_slot(*dst, "Closure.dst")?;
                    if self.function(*callee).is_none() {
                        return Err(format!(
                            "{location}: Closure at pc {pc} references function {callee}"
                        ));
                    }
                }
                Instr::NewObject { dst } => check_slot(*dst, "NewObject.dst")?,
                Instr::CopyDataProperties { target, source } => {
                    check_slot(*target, "CopyDataProperties.target")?;
                    check_slot(*source, "CopyDataProperties.source")?;
                }
                Instr::AssertObjectCoercible { src } => {
                    check_slot(*src, "AssertObjectCoercible.src")?;
                }
                Instr::ObjectRest { dst, src, excluded } => {
                    check_slot(*dst, "ObjectRest.dst")?;
                    check_slot(*src, "ObjectRest.src")?;
                    for key in excluded {
                        check_slot(*key, "ObjectRest.excluded")?;
                    }
                }
                Instr::NewArray { dst, elements } => {
                    check_slot(*dst, "NewArray.dst")?;
                    for element in elements {
                        check_slot(*element, "NewArray.element")?;
                    }
                }
                Instr::ArrayAppend { array, src } => {
                    check_slot(*array, "ArrayAppend.array")?;
                    check_slot(*src, "ArrayAppend.src")?;
                }
                Instr::GetMember { dst, object, key } => {
                    check_slot(*dst, "GetMember.dst")?;
                    check_slot(*object, "GetMember.object")?;
                    check_slot(*key, "GetMember.key")?;
                }
                Instr::GetDestructuredMember { dst, object, key } => {
                    check_slot(*dst, "GetDestructuredMember.dst")?;
                    check_slot(*object, "GetDestructuredMember.object")?;
                    check_slot(*key, "GetDestructuredMember.key")?;
                }
                Instr::HasProperty { dst, object, key } => {
                    check_slot(*dst, "HasProperty.dst")?;
                    check_slot(*object, "HasProperty.object")?;
                    check_slot(*key, "HasProperty.key")?;
                }
                Instr::DeleteMember { dst, object, key } => {
                    check_slot(*dst, "DeleteMember.dst")?;
                    check_slot(*object, "DeleteMember.object")?;
                    check_slot(*key, "DeleteMember.key")?;
                }
                Instr::GetIterator {
                    iterator,
                    next,
                    iterable,
                } => {
                    check_slot(*iterator, "GetIterator.iterator")?;
                    check_slot(*next, "GetIterator.next")?;
                    if iterator == next {
                        return Err(format!(
                            "{location}: GetIterator destinations overlap at pc {pc}"
                        ));
                    }
                    check_slot(*iterable, "GetIterator.iterable")?;
                }
                Instr::IteratorNext {
                    done,
                    value,
                    iterator,
                    next,
                } => {
                    check_slot(*done, "IteratorNext.done")?;
                    check_slot(*value, "IteratorNext.value")?;
                    if done == value {
                        return Err(format!(
                            "{location}: IteratorNext destinations overlap at pc {pc}"
                        ));
                    }
                    check_slot(*iterator, "IteratorNext.iterator")?;
                    check_slot(*next, "IteratorNext.next")?;
                }
                Instr::IteratorSkip {
                    done,
                    iterator,
                    next,
                } => {
                    check_slot(*done, "IteratorSkip.done")?;
                    check_slot(*iterator, "IteratorSkip.iterator")?;
                    check_slot(*next, "IteratorSkip.next")?;
                }
                Instr::IteratorClose { iterator, .. } => {
                    check_slot(*iterator, "IteratorClose.iterator")?;
                }
                Instr::GetForInIterator { dst, source } => {
                    check_slot(*dst, "GetForInIterator.dst")?;
                    check_slot(*source, "GetForInIterator.source")?;
                }
                Instr::ForInNext {
                    done,
                    value,
                    iterator,
                } => {
                    check_slot(*done, "ForInNext.done")?;
                    check_slot(*value, "ForInNext.value")?;
                    if done == value {
                        return Err(format!(
                            "{location}: ForInNext destinations overlap at pc {pc}"
                        ));
                    }
                    check_slot(*iterator, "ForInNext.iterator")?;
                }
                Instr::SetMember { object, key, src } => {
                    check_slot(*object, "SetMember.object")?;
                    check_slot(*key, "SetMember.key")?;
                    check_slot(*src, "SetMember.src")?;
                }
                Instr::DefineProperty {
                    object, key, src, ..
                } => {
                    check_slot(*object, "DefineProperty.object")?;
                    check_slot(*key, "DefineProperty.key")?;
                    check_slot(*src, "DefineProperty.src")?;
                }
                Instr::GetBinding { dst, .. } => check_slot(*dst, "GetBinding.dst")?,
                Instr::SetBinding { src, .. } => check_slot(*src, "SetBinding.src")?,
                Instr::DeclareBinding { .. } => {}
                Instr::Await { dst, input, resume } => {
                    if !function.is_async {
                        return Err(format!("{location}: Await in a synchronous function"));
                    }
                    check_slot(*dst, "Await.dst")?;
                    check_slot(*input, "Await.input")?;
                    if *resume as usize >= function.resumes.len() {
                        return Err(format!(
                            "{location}: Await at pc {pc} references resume {resume}"
                        ));
                    }
                }
                Instr::Jump { target } => check_target(*target, "Jump")?,
                Instr::BranchFalse { cond, target } => {
                    check_slot(*cond, "BranchFalse.cond")?;
                    check_target(*target, "BranchFalse")?;
                }
                Instr::Return { src } => check_slot(*src, "Return.src")?,
                Instr::Throw { src } => check_slot(*src, "Throw.src")?,
                Instr::ThrowAssignmentToConst => {}
                Instr::GetBuiltin { dst, builtin } => {
                    check_slot(*dst, "GetBuiltin.dst")?;
                    self.check_builtin(*builtin, &location, pc, "GetBuiltin")?;
                }
            }
        }

        for (resume_id, resume) in function.resumes.iter().enumerate() {
            let check_pc = |pc: u32, what: &str| -> Result<(), String> {
                if pc as usize >= function.code.len() {
                    return Err(format!(
                        "{location}: resume {resume_id} {what} pc {pc} is out of range"
                    ));
                }
                Ok(())
            };
            check_pc(resume.fulfill_pc, "fulfill")?;
            check_pc(resume.reject_pc, "reject")?;
            check_slot(resume.fulfill_dst, "fulfill_dst")?;
            check_slot(resume.reject_dst, "reject_dst")?;
            let mut previous = None;
            for slot in &resume.saved_slots {
                if *slot >= function.persistent_slots {
                    return Err(format!(
                        "{location}: resume {resume_id} saves persistent slot {slot}, size {}",
                        function.persistent_slots
                    ));
                }
                if previous.is_some_and(|prev| *slot <= prev) {
                    return Err(format!(
                        "{location}: resume {resume_id} saved_slots are not strictly ascending"
                    ));
                }
                previous = Some(*slot);
            }
        }

        for (handler_id, handler) in function.handlers.iter().enumerate() {
            if handler.start_pc >= handler.end_pc {
                return Err(format!(
                    "{location}: handler {handler_id} has empty or inverted range {}..{}",
                    handler.start_pc, handler.end_pc
                ));
            }
            if handler.end_pc as usize > function.code.len() {
                return Err(format!(
                    "{location}: handler {handler_id} ends at pc {}, code len {}",
                    handler.end_pc,
                    function.code.len()
                ));
            }
            if handler.target_pc as usize >= function.code.len() {
                return Err(format!(
                    "{location}: handler {handler_id} targets pc {}, code len {}",
                    handler.target_pc,
                    function.code.len()
                ));
            }
            if handler.target_pc >= handler.start_pc && handler.target_pc < handler.end_pc {
                return Err(format!(
                    "{location}: handler {handler_id} targets a pc inside its own protected range"
                ));
            }
            check_slot(handler.dst, &format!("handler {handler_id}.dst"))?;
        }

        Ok(())
    }
}
