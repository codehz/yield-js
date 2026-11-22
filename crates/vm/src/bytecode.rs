//! Versioned bytecode contract shared by the compiler and the VM.
//!
//! The VM owns this module: the compiler depends on it, never the other way
//! around. All operands are explicit slots. No implicit operand stack is
//! persisted across an `Await` or a `Yield`.

use std::collections::{BTreeMap, BTreeSet, btree_map::Entry};
use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bincode::Options;
use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

use crate::digest::Digest;
use crate::value::{BindingId, JsBigInt, JsString};

mod instr;
mod validate;

pub use instr::Instr;

pub const BYTECODE_VERSION: u32 = 55;
pub const BUILD_FORMAT_VERSION: u32 = 1;

const BUILD_MAGIC: [u8; 8] = *b"YIELDBLD";
const BUILD_HEADER_LEN: usize = 12;

pub use crate::builtin::BUILTINS;

/// Compiler-only builtins. A separate range leaves the public global indices
/// stable when new names are appended to `BUILTINS`.
#[doc(hidden)]
#[derive(Clone, Copy)]
#[repr(u32)]
pub enum DisposeBuiltin {
    NewScope = 0x10000,
    AddResource,
    Step,
    RecordThrow,
    HasThrow,
    GetThrow,
}

impl DisposeBuiltin {
    pub(crate) const COUNT: usize = Self::GetThrow as usize - Self::NewScope as usize + 1;

    pub(crate) fn offset(index: u32) -> Option<usize> {
        let offset = index.checked_sub(Self::NewScope as u32)? as usize;
        (offset < Self::COUNT).then_some(offset)
    }
}

const _: () = assert!(BUILTINS.len() <= DisposeBuiltin::NewScope as usize);

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
    /// Shared so fetching an instruction never copies the caller's slots.
    Slots(Arc<[Slot]>),
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
    #[serde(rename = "bigint")]
    BigInt(JsBigInt),
}

/// One tagged-template call site. The VM allocates the frozen cooked and raw
/// arrays once per VM and keeps the object identity across snapshots.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemplateSite {
    /// Cooked strings; `None` is a tagged template's invalid escape.
    pub cooked: Vec<Option<JsString>>,
    /// Source text chunks with escapes intact.
    pub raw: Vec<JsString>,
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
    /// ToNumeric: Number for most primitives, BigInt for BigInt.
    ToNumeric,
    Increment,
    Decrement,
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

/// Resume points of one synchronous `yield`.
///
/// A generator suspension is resumed from outside the VM with one of three
/// completions. The entry supplies the destination each injected value lands
/// in, so no result slot has to survive in the saved set. `return_pc` declares
/// no destination: its first instruction consumes the pending payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneratorEntry {
    /// Always the instruction after the `Yield`.
    pub next_pc: u32,
    pub next_dst: Slot,
    pub throw_pc: u32,
    pub throw_dst: Slot,
    pub return_pc: u32,
    /// Strictly ascending persistent-slot indices. Values are stored in this
    /// order in the generator object and snapshot.
    pub saved_slots: Vec<u16>,
}

/// Where a generator's body starts once the call-time parameters are initialized.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneratorStart {
    /// First body instruction. Entered only by the first `next`, never by
    /// falling out of [`Instr::GeneratorStart`].
    pub body_pc: u32,
    /// Strictly ascending persistent-slot indices live at `body_pc`.
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

/// A change of source position at an instruction boundary. Zero line and
/// column mark the start of an unmapped range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackSite {
    pub pc: u32,
    pub line: u32,
    pub column: u32,
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
    /// Generator, synchronous unless `is_async` is also set. An async
    /// generator then carries both `resumes` and `generator_resumes`.
    pub is_generator: bool,
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
    /// Present exactly for a generator, synchronous or asynchronous.
    pub generator_start: Option<GeneratorStart>,
    /// One entry per `Yield`, indexed by `Instr::Yield::generator`.
    pub generator_resumes: Vec<GeneratorEntry>,
    pub handlers: Vec<ExceptionHandler>,
    /// Defining source module, absent on source-less bytecode.
    pub module: Option<u32>,
    /// Sparse PC-to-source map, ordered by PC. Empty hides the frame.
    pub stack_sites: Vec<StackSite>,
}

impl Function {
    pub fn is_constructable(&self) -> bool {
        !self.is_async && !self.is_arrow && !self.is_method && !self.is_generator
    }

    pub(crate) fn is_accessor_method(&self) -> bool {
        self.is_method && !self.is_async && !self.is_arrow && !self.is_generator
    }

    fn param_targets(&self) -> impl Iterator<Item = &ParamTarget> {
        self.params
            .iter()
            .chain(&self.rest_param)
            .chain(&self.this_param)
            .chain(&self.new_target_param)
            .chain(&self.home_param)
            .chain(&self.arguments_param)
    }

    /// Whether this function allocates an invocation environment.
    ///
    /// Derived from validated code. A bytecode flag would be forgeable and is
    /// not part of the contract. Closure creation keeps the existing capture
    /// protocol even when this function's own bindings were promoted.
    pub fn needs_environment(&self) -> bool {
        if self
            .param_targets()
            .any(|param| matches!(param, ParamTarget::Binding(_)))
        {
            return true;
        }
        self.code.iter().any(|instruction| {
            matches!(
                instruction,
                Instr::GetBinding { .. }
                    | Instr::SetBinding { .. }
                    | Instr::GetBindingDirect { .. }
                    | Instr::SetBindingDirect { .. }
                    | Instr::DeclareBinding { .. }
                    | Instr::SaveEnv { .. }
                    | Instr::NewIterationEnv { .. }
                    | Instr::RestoreEnv { .. }
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
    /// Tagged-template call sites, indexed by `Instr::GetTemplate::template`.
    pub templates: Vec<TemplateSite>,
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

/// Shared binary configuration for digest inputs and build artifacts.
fn build_codec() -> impl bincode::Options {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_little_endian()
}

/// Digest inputs use bincode's fixed-int little-endian encoding. The crate
/// version and options are part of the build identity contract: changing either
/// changes every `build_id`.
#[derive(Serialize)]
struct DigestInput<'a> {
    bytecode_version: u32,
    execution_revision: u32,
    constants: &'a [Constant],
    templates: &'a [TemplateSite],
    imports: &'a BTreeMap<String, BTreeSet<String>>,
    functions: &'a [Function],
    modules: &'a [SourceModule],
    entry: u32,
    preload: Option<u32>,
}

impl Build {
    pub fn new(
        constants: Vec<Constant>,
        templates: Vec<TemplateSite>,
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
            templates,
            imports,
            functions,
            modules,
            entry,
            preload,
        }
    }

    /// Sets `build_id` to a digest of every execution-relevant field,
    /// including source positions observable through Error stacks.
    pub fn finalize(&mut self) {
        self.build_id = self.compute_id();
    }
    pub fn compute_id(&self) -> String {
        self.compute_id_profiled::<false>().0
    }

    fn compute_id_profiled<const PROFILE: bool>(&self) -> (String, Option<Duration>) {
        let input = DigestInput {
            bytecode_version: self.bytecode_version,
            // Reject artifacts from older builtin semantics even when their
            // instruction stream and snapshot record layout are unchanged.
            execution_revision: 16,
            constants: &self.constants,
            templates: &self.templates,
            imports: &self.imports,
            functions: &self.functions,
            modules: &self.modules,
            entry: self.entry,
            preload: self.preload,
        };
        let digest_started = PROFILE.then(Instant::now);
        let mut sink = std::io::BufWriter::with_capacity(64 * 1024, Digest::new());
        build_codec()
            .serialize_into(&mut sink, &input)
            .expect("bytecode digest serialization cannot fail");
        sink.flush().expect("bytecode digest flush cannot fail");
        let digest = sink
            .into_inner()
            .unwrap_or_else(|_| panic!("bytecode digest flush cannot fail"));
        let elapsed = digest_started.map(|started| started.elapsed());
        (digest.finish(), elapsed)
    }

    /// Encodes this build as a self-contained binary artifact.
    ///
    /// The header is the `YIELDBLD` magic, followed by a little-endian
    /// [`BUILD_FORMAT_VERSION`], followed by a bincode fixed-int little-endian
    /// payload. The result is not validated; pass it to `Vm::new` or call
    /// [`Build::validate`] after decoding.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&BUILD_MAGIC);
        out.extend_from_slice(&BUILD_FORMAT_VERSION.to_le_bytes());
        build_codec()
            .serialize_into(&mut out, self)
            .expect("build artifact serialization cannot fail");
        out
    }

    /// Decodes an artifact produced by [`Build::to_bytes`] without validating
    /// it.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() < BUILD_HEADER_LEN {
            return Err("build artifact is too short".to_owned());
        }
        if &bytes[..BUILD_MAGIC.len()] != BUILD_MAGIC.as_slice() {
            return Err("invalid build artifact magic".to_owned());
        }
        let version = u32::from_le_bytes(
            bytes[BUILD_MAGIC.len()..BUILD_HEADER_LEN]
                .try_into()
                .expect("build header length"),
        );
        if version != BUILD_FORMAT_VERSION {
            return Err(format!(
                "unsupported build artifact version {version}, expected {BUILD_FORMAT_VERSION}"
            ));
        }
        build_codec()
            .deserialize(&bytes[BUILD_HEADER_LEN..])
            .map_err(|error| format!("invalid build artifact: {error}"))
    }

    pub fn function(&self, id: u32) -> Option<&Function> {
        self.functions
            .iter()
            .find(|function| function.func_id == id)
    }

    pub fn function_named(&self, name: &str) -> Option<&Function> {
        self.functions.iter().find(|function| function.name == name)
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
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Variant order and field order are the artifact wire format: existing
    /// builds must keep decoding, so this pins one encoding instead of only
    /// round-tripping through the current serializer.
    #[test]
    fn instr_encoding_is_stable() {
        let instruction = Instr::Move {
            dst: Slot::transient(1),
            src: Slot::persistent(2),
        };
        let encoded = build_codec().serialize(&instruction).expect("serialize");
        assert_eq!(encoded, [2, 0, 0, 0, 0, 0, 0, 0, 1, 0, 1, 0, 0, 0, 2, 0]);
        let decoded: Instr = build_codec().deserialize(&encoded).expect("deserialize");
        assert_eq!(decoded, instruction);
    }
}
