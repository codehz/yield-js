//! Versioned bytecode contract shared by the compiler and the VM.
//!
//! The VM owns this module: the compiler depends on it, never the other way
//! around. All operands are explicit slots. No implicit operand stack is
//! persisted across an `Await`.

use std::collections::{BTreeMap, BTreeSet, btree_map::Entry};

use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};

use crate::value::{BindingId, JsString};

pub const BYTECODE_VERSION: u32 = 40;

pub use crate::builtin::BUILTINS;

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
    Number(#[serde(with = "crate::value::number_serde")] f64),
    String(JsString),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ImportDecl {
    /// Position in the build's sorted module manifest.
    pub module: u32,
    /// Position in the module's sorted export set, or `None` for its namespace.
    pub export: Option<u32>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessorPrefix {
    Get,
    Set,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivateDefinition {
    Field,
    Method,
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
    GetImport {
        dst: Slot,
        import: ImportDecl,
    },
    GetModuleBinding {
        dst: Slot,
        module: u32,
        binding: BindingId,
    },
    GetModuleNamespace {
        dst: Slot,
        module: u32,
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
    SetHome {
        function: Slot,
        home: Slot,
    },
    SetFunctionName {
        function: Slot,
        key: Slot,
        prefix: Option<AccessorPrefix>,
    },
    GetSuperBase {
        dst: Slot,
        home: Slot,
    },
    GetSuper {
        dst: Slot,
        base: Slot,
        key: Slot,
        receiver: Slot,
    },
    SetSuper {
        base: Slot,
        key: Slot,
        receiver: Slot,
        src: Slot,
    },
    GetDerivedThis {
        dst: Slot,
        context: Slot,
    },
    GetSuperConstructor {
        dst: Slot,
        context: Slot,
    },
    SuperConstruct {
        dst: Slot,
        context: Slot,
        callee: Slot,
        args: CallArgs,
    },
    /// Creates a closure that captures the frame's current environment.
    Closure {
        dst: Slot,
        function: u32,
    },
    NewClassScope {
        dst: Slot,
        parent: Option<Slot>,
    },
    LoadClassBinding {
        dst: Slot,
        scope: Option<Slot>,
        binding: BindingId,
    },
    InitClassBinding {
        scope: Slot,
        binding: BindingId,
        src: Slot,
    },
    CreateClass {
        dst: Slot,
        scope: Option<Slot>,
        heritage: Option<Slot>,
        name_key: Option<Slot>,
        constructor: u32,
        default_constructor: bool,
        private_count: u32,
        private_binding: Option<BindingId>,
    },
    ScopedClosure {
        dst: Slot,
        scope: Slot,
        function: u32,
    },
    DefineClassMethod {
        class: Slot,
        key: Slot,
        src: Slot,
        is_static: bool,
        kind: PropertyDefinition,
    },
    AddClassPrivate {
        class: Slot,
        index: u32,
        lexical: BindingId,
        src: Option<Slot>,
        is_static: bool,
        kind: PrivateDefinition,
    },
    GetPrivate {
        dst: Slot,
        object: Slot,
        brand: Slot,
        index: u32,
        lexical: BindingId,
    },
    SetPrivate {
        object: Slot,
        brand: Slot,
        src: Slot,
        index: u32,
        lexical: BindingId,
    },
    HasPrivate {
        dst: Slot,
        object: Slot,
        brand: Slot,
        index: u32,
        lexical: BindingId,
    },
    AddClassField {
        class: Slot,
        key: Slot,
        initializer: Option<Slot>,
        is_static: bool,
    },
    AddClassBlock {
        class: Slot,
        body: Slot,
    },
    RunClassStatic {
        class: Slot,
    },
    NewObject {
        dst: Slot,
    },
    /// Allocates a fresh literal using the intrinsic RegExp prototype.
    NewRegExp {
        dst: Slot,
        source: JsString,
        flags: String,
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
    /// Grows an array's `length` by one without creating an element property,
    /// forming a sparse hole.
    ArrayHole {
        array: Slot,
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
    ThrowDeleteSuper,
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
    /// Initial JavaScript name, independent of the function-table identifier.
    pub initial_name: String,
    pub is_async: bool,
    pub is_arrow: bool,
    pub is_method: bool,
    /// Number of positional parameters before the first default, excluding rest.
    pub length: u16,
    /// Positional parameter destinations in source order.
    pub params: Vec<ParamTarget>,
    /// Destination for a fresh array of arguments beyond the positional parameters.
    pub rest_param: Option<ParamTarget>,
    /// Receiver binding when this function, or an arrow nested in it, uses `this`.
    /// Not a JavaScript argument.
    pub this_param: Option<ParamTarget>,
    /// Destination for the lexical `new.target`, not a JavaScript argument.
    pub new_target_param: Option<ParamTarget>,
    /// Destination for the method's lexical home object.
    pub home_param: Option<ParamTarget>,
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
            .chain(&self.new_target_param)
            .chain(&self.home_param)
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
                    | Instr::NewClassScope { .. }
                    | Instr::LoadClassBinding { .. }
                    | Instr::CreateClass { .. }
            )
        })
    }
}

/// Final identity of a linked export, independent of the path used to resolve it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleTarget {
    Binding { module: u32, binding: BindingId },
    Namespace { module: u32 },
    Host(ImportDecl),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleExport {
    pub target: ModuleTarget,
    pub getter: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceModule {
    pub id: String,
    pub declaration: u32,
    pub evaluation: u32,
    /// Direct source dependencies in first occurrence order.
    pub dependencies: Vec<u32>,
    #[serde(deserialize_with = "deserialize_unique_map")]
    pub exports: BTreeMap<String, ModuleExport>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Build {
    pub bytecode_version: u32,
    pub build_id: String,
    pub constants: Vec<Constant>,
    pub imports: BTreeMap<String, BTreeSet<String>>,
    pub functions: Vec<Function>,
    /// Linked source modules, indexed by module operands.
    pub modules: Vec<SourceModule>,
    /// Source module whose linked exports are exposed to the host.
    pub entry: u32,
    /// Optional source module evaluated before the entry graph.
    pub preload: Option<u32>,
}

fn deserialize_unique_map<'de, D, V>(deserializer: D) -> Result<BTreeMap<String, V>, D::Error>
where
    D: Deserializer<'de>,
    V: Deserialize<'de>,
{
    struct UniqueMap<V>(std::marker::PhantomData<V>);

    impl<'de, V: Deserialize<'de>> Visitor<'de> for UniqueMap<V> {
        type Value = BTreeMap<String, V>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a map of unique export names")
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

    deserializer.deserialize_map(UniqueMap(std::marker::PhantomData))
}

#[derive(Serialize)]
struct DigestInput<'a> {
    bytecode_version: u32,
    execution_revision: u32,
    constants: &'a [Constant],
    imports: &'a BTreeMap<String, BTreeSet<String>>,
    functions: &'a [Function],
    modules: &'a [SourceModule],
    entry: u32,
    preload: Option<u32>,
}

impl Build {
    pub fn new(
        constants: Vec<Constant>,
        imports: BTreeMap<String, BTreeSet<String>>,
        functions: Vec<Function>,
        modules: Vec<SourceModule>,
        entry: u32,
        preload: Option<u32>,
    ) -> Self {
        Self {
            bytecode_version: BYTECODE_VERSION,
            build_id: String::new(),
            constants,
            imports,
            functions,
            modules,
            entry,
            preload,
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
            execution_revision: 11,
            constants: &self.constants,
            imports: &self.imports,
            functions: &self.functions,
            modules: &self.modules,
            entry: self.entry,
            preload: self.preload,
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
        let mut private_limits = BTreeMap::new();
        for instr in self.functions.iter().flat_map(|f| &f.code) {
            if let Instr::CreateClass {
                private_count,
                private_binding,
                ..
            } = instr
            {
                match private_binding {
                    None if *private_count == 0 => {}
                    Some(binding)
                        if *private_count != 0 && *binding < crate::builtin::call::DERIVED_THIS =>
                    {
                        if private_limits.insert(*binding, *private_count).is_some() {
                            return Err(format!("duplicate private class binding {binding}"));
                        }
                    }
                    _ => return Err("invalid private class binding".into()),
                }
            }
        }

        let (binding_owners, getters) = self.validate_modules()?;
        for function in &self.functions {
            self.validate_function(function, &private_limits, &binding_owners, &getters)?;
        }
        Ok(())
    }

    pub(crate) fn is_initializer(&self, func_id: u32) -> bool {
        self.modules
            .iter()
            .any(|module| func_id == module.declaration || func_id == module.evaluation)
    }

    /// Module-owned bindings are declared during the declaration phase.
    pub fn module_bindings(&self, module: u32) -> impl Iterator<Item = BindingId> + '_ {
        self.modules
            .get(module as usize)
            .and_then(|module| self.function(module.declaration))
            .into_iter()
            .flat_map(|function| &function.code)
            .filter_map(|instruction| match instruction {
                Instr::DeclareBinding { binding } => Some(*binding),
                _ => None,
            })
    }

    /// Both initialization phases share one module environment when needed.
    pub fn module_needs_environment(&self, module: u32) -> bool {
        let module = &self.modules[module as usize];
        [module.declaration, module.evaluation]
            .into_iter()
            .filter_map(|id| self.function(id))
            .any(Function::needs_environment)
    }

    fn validate_modules(&self) -> Result<(BTreeMap<BindingId, u32>, BTreeSet<u32>), String> {
        let mut pending = vec![self.entry];
        pending.extend(self.preload);
        for &root in &pending {
            self.check_module(root)?;
        }

        let mut module_ids = BTreeSet::new();
        let mut initializers = BTreeSet::new();
        let mut binding_owners = BTreeMap::new();
        for (index, module) in self.modules.iter().enumerate() {
            let index = u32::try_from(index).map_err(|_| "too many source modules")?;
            if module.id.is_empty() || !module_ids.insert(module.id.as_str()) {
                return Err(format!(
                    "empty or duplicate source module ID {:?}",
                    module.id
                ));
            }
            for initializer in [module.declaration, module.evaluation] {
                if !initializers.insert(initializer) {
                    return Err(format!("module initializer {initializer} is not distinct"));
                }
                let function = self.function(initializer).ok_or_else(|| {
                    format!(
                        "module {:?} references missing initializer {initializer}",
                        module.id
                    )
                })?;
                if !function.is_constructable()
                    || !Self::has_no_invocation_inputs(function)
                    || !function.resumes.is_empty()
                {
                    return Err(format!("invalid module initializer {initializer}"));
                }
                if initializer == module.declaration
                    && (!function.handlers.is_empty()
                        || function.code.iter().any(|instruction| {
                            !matches!(
                                instruction,
                                Instr::Undefined { .. }
                                    | Instr::Move { .. }
                                    | Instr::DeclareBinding { .. }
                                    | Instr::SetBinding { .. }
                                    | Instr::Closure { .. }
                                    | Instr::Return { .. }
                            )
                        }))
                {
                    return Err(format!(
                        "invalid module declaration initializer {initializer}"
                    ));
                }
            }
            let mut dependencies = BTreeSet::new();
            for &dependency in &module.dependencies {
                self.check_module(dependency)?;
                if !dependencies.insert(dependency) {
                    return Err(format!(
                        "duplicate dependency {dependency} in module {:?}",
                        module.id
                    ));
                }
            }
            for binding in self.module_bindings(index) {
                if binding_owners.insert(binding, index).is_some() {
                    return Err(format!("duplicate declaration of module binding {binding}"));
                }
            }
        }

        let mut reachable = vec![false; self.modules.len()];
        while let Some(index) = pending.pop() {
            if !std::mem::replace(&mut reachable[index as usize], true) {
                pending.extend(&self.modules[index as usize].dependencies);
            }
        }
        if let Some(index) = reachable.iter().position(|seen| !seen) {
            return Err(format!("unreachable module {:?}", self.modules[index].id));
        }

        let mut by_target = BTreeMap::new();
        let mut getters = BTreeSet::new();
        for module in &self.modules {
            for (name, export) in &module.exports {
                let ModuleExport { target, getter } = *export;
                match by_target.entry(target) {
                    Entry::Occupied(entry) if *entry.get() == getter => continue,
                    Entry::Occupied(_) => {
                        return Err(format!("exports of {target:?} must share a getter"));
                    }
                    Entry::Vacant(entry) => entry.insert(getter),
                };
                self.check_module_target(target, &binding_owners)?;
                let function = self.function(getter).ok_or_else(|| {
                    format!(
                        "missing getter {getter} for module {:?} export {name:?}",
                        module.id
                    )
                })?;
                if initializers.contains(&getter)
                    || !getters.insert(getter)
                    || !function.is_accessor_method()
                    || !Self::has_no_invocation_inputs(function)
                    || !function.resumes.is_empty()
                    || !function.handlers.is_empty()
                    || Self::getter_target(&function.code) != Some(target)
                {
                    return Err(format!(
                        "invalid getter {getter} for module {:?} export {name:?}",
                        module.id
                    ));
                }
            }
        }
        Ok((binding_owners, getters))
    }

    fn has_no_invocation_inputs(function: &Function) -> bool {
        function.params.is_empty()
            && function.rest_param.is_none()
            && function.this_param.is_none()
            && function.new_target_param.is_none()
            && function.home_param.is_none()
            && function.arguments_param.is_none()
    }

    fn getter_target(code: &[Instr]) -> Option<ModuleTarget> {
        let [read, Instr::Return { src }] = code else {
            return None;
        };
        match *read {
            Instr::GetModuleBinding {
                dst,
                module,
                binding,
            } if dst == *src => Some(ModuleTarget::Binding { module, binding }),
            Instr::GetModuleNamespace { dst, module } if dst == *src => {
                Some(ModuleTarget::Namespace { module })
            }
            Instr::GetImport { dst, import } if dst == *src => Some(ModuleTarget::Host(import)),
            _ => None,
        }
    }

    fn check_module(&self, module: u32) -> Result<(), String> {
        if module as usize >= self.modules.len() {
            return Err(format!("invalid source module index {module}"));
        }
        Ok(())
    }

    fn check_module_target(
        &self,
        target: ModuleTarget,
        binding_owners: &BTreeMap<BindingId, u32>,
    ) -> Result<(), String> {
        match target {
            ModuleTarget::Binding { module, binding } => {
                self.check_module(module)?;
                if binding_owners.get(&binding) != Some(&module) {
                    return Err(format!(
                        "binding {binding} is not declared in module {module}"
                    ));
                }
            }
            ModuleTarget::Namespace { module } => self.check_module(module)?,
            ModuleTarget::Host(import) => {
                let exports = self
                    .imports
                    .values()
                    .nth(import.module as usize)
                    .ok_or_else(|| format!("invalid host module index {}", import.module))?;
                if let Some(export) = import.export
                    && export as usize >= exports.len()
                {
                    return Err(format!(
                        "invalid host export index {export} for module {}",
                        import.module
                    ));
                }
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

    fn validate_function(
        &self,
        function: &Function,
        private_limits: &BTreeMap<BindingId, u32>,
        binding_owners: &BTreeMap<BindingId, u32>,
        getters: &BTreeSet<u32>,
    ) -> Result<(), String> {
        let check_private = |lexical, index| {
            if private_limits
                .get(&lexical)
                .is_some_and(|limit| index < *limit)
            {
                Ok(())
            } else {
                Err(format!(
                    "invalid class private index {index} for binding {lexical}"
                ))
            }
        };
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
        let check_binding = |binding: BindingId| {
            if binding >= crate::builtin::call::DERIVED_THIS {
                Err(format!("{location}: reserved binding {binding}"))
            } else {
                Ok(())
            }
        };

        if function.params.len() > u16::MAX as usize {
            return Err(format!("{location}: too many parameters"));
        }
        if usize::from(function.length) > function.params.len() {
            return Err(format!(
                "{location}: length exceeds positional parameter count"
            ));
        }
        if function.is_arrow && function.arguments_param.is_some() {
            return Err(format!("{location}: arrow cannot have an arguments target"));
        }
        if function.is_arrow && function.new_target_param.is_some() {
            return Err(format!(
                "{location}: arrow cannot have a new.target destination"
            ));
        }
        if function.is_arrow && function.home_param.is_some() {
            return Err(format!("{location}: arrow cannot have a home destination"));
        }
        let mut seen_bindings = std::collections::BTreeSet::new();
        let mut seen_slots = Vec::new();
        for param in function
            .params
            .iter()
            .chain(&function.rest_param)
            .chain(&function.this_param)
            .chain(&function.new_target_param)
            .chain(&function.home_param)
            .chain(&function.arguments_param)
        {
            match param {
                ParamTarget::Binding(binding) => {
                    check_binding(*binding)?;
                    if binding_owners.contains_key(binding) {
                        return Err(format!(
                            "{location}: parameter shadows module binding {binding}"
                        ));
                    }
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
                Instr::GetImport { dst, import } => {
                    check_slot(*dst, "GetImport.dst")?;
                    self.check_module_target(ModuleTarget::Host(*import), binding_owners)
                        .map_err(|err| format!("{location}: GetImport at pc {pc}: {err}"))?;
                }
                Instr::GetModuleBinding {
                    dst,
                    module,
                    binding,
                } => {
                    check_slot(*dst, "GetModuleBinding.dst")?;
                    self.check_module_target(
                        ModuleTarget::Binding {
                            module: *module,
                            binding: *binding,
                        },
                        binding_owners,
                    )
                    .map_err(|err| format!("{location}: GetModuleBinding at pc {pc}: {err}"))?;
                }
                Instr::GetModuleNamespace { dst, module } => {
                    check_slot(*dst, "GetModuleNamespace.dst")?;
                    self.check_module(*module).map_err(|err| {
                        format!("{location}: GetModuleNamespace at pc {pc}: {err}")
                    })?;
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
                Instr::SetHome { function, home } => {
                    check_slot(*function, "SetHome.function")?;
                    check_slot(*home, "SetHome.home")?;
                }
                Instr::SetFunctionName { function, key, .. } => {
                    check_slot(*function, "SetFunctionName.function")?;
                    check_slot(*key, "SetFunctionName.key")?;
                }
                Instr::GetSuperBase { dst, home } => {
                    check_slot(*dst, "GetSuperBase.dst")?;
                    check_slot(*home, "GetSuperBase.home")?;
                }
                Instr::GetSuper {
                    dst: result,
                    base,
                    key,
                    receiver,
                }
                | Instr::SetSuper {
                    src: result,
                    base,
                    key,
                    receiver,
                } => {
                    check_slot(*result, "super value")?;
                    check_slot(*base, "super base")?;
                    check_slot(*key, "super key")?;
                    check_slot(*receiver, "super receiver")?;
                }
                Instr::GetDerivedThis { dst, context }
                | Instr::GetSuperConstructor { dst, context }
                | Instr::SuperConstruct { dst, context, .. } => {
                    check_slot(*dst, "derived constructor result")?;
                    check_slot(*context, "derived constructor context")?;
                    if let Instr::SuperConstruct { args, callee, .. } = instr {
                        check_slot(*callee, "SuperConstruct.callee")?;
                        for arg in args.slots() {
                            check_slot(*arg, "SuperConstruct.arg")?;
                        }
                    }
                }
                Instr::Closure {
                    dst,
                    function: callee,
                } => {
                    check_slot(*dst, "Closure.dst")?;
                    if self.is_initializer(*callee) || getters.contains(callee) {
                        return Err(format!(
                            "{location}: Closure at pc {pc} references reserved function {callee}"
                        ));
                    }
                    if self.function(*callee).is_none() {
                        return Err(format!(
                            "{location}: Closure at pc {pc} references function {callee}"
                        ));
                    }
                }
                Instr::NewClassScope { dst, parent }
                | Instr::LoadClassBinding {
                    dst, scope: parent, ..
                } => {
                    if let Instr::LoadClassBinding { binding, .. } = instr {
                        check_binding(*binding)?;
                    }
                    check_slot(*dst, "class scope result")?;
                    if let Some(parent) = parent {
                        check_slot(*parent, "class scope")?;
                    }
                }
                Instr::InitClassBinding {
                    scope,
                    src,
                    binding,
                } => {
                    check_binding(*binding)?;
                    check_slot(*scope, "InitClassBinding.scope")?;
                    check_slot(*src, "InitClassBinding.src")?;
                }
                Instr::CreateClass {
                    dst,
                    scope,
                    heritage,
                    name_key,
                    constructor,
                    ..
                } => {
                    check_slot(*dst, "CreateClass.dst")?;
                    if let Some(scope) = scope {
                        check_slot(*scope, "CreateClass.scope")?;
                    }
                    if let Some(heritage) = heritage {
                        check_slot(*heritage, "CreateClass.heritage")?;
                    }
                    if let Some(key) = name_key {
                        check_slot(*key, "CreateClass.name_key")?;
                    }
                    if self.is_initializer(*constructor)
                        || !self
                            .function(*constructor)
                            .is_some_and(Function::is_constructable)
                    {
                        return Err(format!(
                            "{location}: invalid class constructor {constructor} at pc {pc}"
                        ));
                    }
                }
                Instr::ScopedClosure {
                    dst,
                    scope,
                    function,
                } => {
                    check_slot(*dst, "ScopedClosure.dst")?;
                    check_slot(*scope, "ScopedClosure.scope")?;
                    if self.is_initializer(*function)
                        || getters.contains(function)
                        || self.function(*function).is_none()
                    {
                        return Err(format!(
                            "{location}: invalid scoped closure function {function} at pc {pc}"
                        ));
                    }
                }
                Instr::DefineClassMethod {
                    class, key, src, ..
                } => {
                    check_slot(*class, "DefineClassMethod.class")?;
                    check_slot(*key, "DefineClassMethod.key")?;
                    check_slot(*src, "DefineClassMethod.src")?;
                }
                Instr::AddClassPrivate {
                    class,
                    index,
                    lexical,
                    src,
                    kind,
                    ..
                } => {
                    check_slot(*class, "AddClassPrivate.class")?;
                    if let Some(src) = src {
                        check_slot(*src, "AddClassPrivate.src")?;
                    } else if *kind != PrivateDefinition::Field {
                        return Err(format!(
                            "{location}: private method definition is missing its closure at pc {pc}"
                        ));
                    }
                    check_private(*lexical, *index)?;
                }
                Instr::GetPrivate {
                    dst: value,
                    object,
                    brand,
                    index,
                    lexical,
                }
                | Instr::SetPrivate {
                    src: value,
                    object,
                    brand,
                    index,
                    lexical,
                }
                | Instr::HasPrivate {
                    dst: value,
                    object,
                    brand,
                    index,
                    lexical,
                } => {
                    check_slot(*value, "private element value")?;
                    check_slot(*object, "private element receiver")?;
                    check_slot(*brand, "private element brand")?;
                    check_private(*lexical, *index)?;
                }
                Instr::AddClassField {
                    class,
                    key,
                    initializer,
                    ..
                } => {
                    check_slot(*class, "AddClassField.class")?;
                    check_slot(*key, "AddClassField.key")?;
                    if let Some(slot) = initializer {
                        check_slot(*slot, "AddClassField.initializer")?;
                    }
                }
                Instr::AddClassBlock { class, body } => {
                    check_slot(*class, "AddClassBlock.class")?;
                    check_slot(*body, "AddClassBlock.body")?;
                }
                Instr::RunClassStatic { class } => check_slot(*class, "RunClassStatic.class")?,
                Instr::NewObject { dst } => check_slot(*dst, "NewObject.dst")?,
                Instr::NewRegExp { dst, source, flags } => {
                    check_slot(*dst, "NewRegExp.dst")?;
                    crate::regexp::validate_regexp_string(source, flags).map_err(|message| {
                        format!("{location}: invalid NewRegExp at pc {pc}: {message}")
                    })?;
                }
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
                Instr::ArrayHole { array } => {
                    check_slot(*array, "ArrayHole.array")?;
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
                Instr::GetBinding { dst, binding } => {
                    check_binding(*binding)?;
                    check_slot(*dst, "GetBinding.dst")?;
                }
                Instr::SetBinding { src, binding } => {
                    check_binding(*binding)?;
                    check_slot(*src, "SetBinding.src")?;
                }
                Instr::DeclareBinding { binding } => {
                    check_binding(*binding)?;
                    if let Some(&module) = binding_owners.get(binding)
                        && self.modules[module as usize].declaration != function.func_id
                    {
                        return Err(format!("{location}: redeclares module binding {binding}"));
                    }
                }
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
                Instr::ThrowAssignmentToConst | Instr::ThrowDeleteSuper => {}
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
