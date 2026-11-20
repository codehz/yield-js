//! Static host bindings: modules, classes, values, and recoverable operations.
//!
//! Declarations are immutable and can be shared between VMs. A VM owns every
//! prototype, instance, module state, and operation record derived from them.
//! Host code never receives a heap id it can use outside the current call: a
//! [`JsValue`] carries the calling VM generation and call id, and slot writes
//! are the only way to retain a value across calls.

use std::any::Any;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::heap::{Attributes, Object, Property};
use crate::value::{JsString, ObjectId, PropertyKey, SymbolId, Value};
use crate::vm::Vm;

pub type ClassId = usize;

/// A stable export identity inside one [`Bindings`] table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ExportId {
    pub module: u32,
    pub export: u32,
}

/// A stable prototype-method identity inside one [`Bindings`] table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MethodId {
    pub class: u32,
    pub method: u32,
}

/// What a host function object calls. The target is resolved at link time;
/// runtime dispatch never uses a string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeTarget {
    Export(ExportId),
    Method(MethodId),
    Accessor {
        class: u32,
        accessor: u32,
        set: bool,
    },
}

#[derive(Debug, Clone)]
pub struct CodecError(pub String);

impl CodecError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for CodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone)]
pub struct HostFault(pub String);

impl HostFault {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for HostFault {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A script-visible or host-contract error.
#[derive(Debug)]
pub enum JsError {
    /// Catchable by script `try`/`catch`.
    Throw(JsValue),
    /// Invalid host use; terminates the VM instead of the script.
    Fault(HostFault),
}

pub type JsResult<T> = Result<T, JsError>;

impl JsError {
    pub fn fault(message: impl Into<String>) -> Self {
        Self::Fault(HostFault::new(message))
    }
}

pub type HostCall = fn(&mut CallCx<'_>) -> JsResult<JsValue>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConstKind {
    Undefined,
    Null,
    Bool,
    Number,
    String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConstValue {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(JsString),
}

impl ConstValue {
    pub fn kind(&self) -> ConstKind {
        match self {
            Self::Undefined => ConstKind::Undefined,
            Self::Null => ConstKind::Null,
            Self::Bool(_) => ConstKind::Bool,
            Self::Number(_) => ConstKind::Number,
            Self::String(_) => ConstKind::String,
        }
    }

    pub(crate) fn value(&self) -> Value {
        match self {
            Self::Undefined => Value::Undefined,
            Self::Null => Value::Null,
            Self::Bool(value) => Value::Bool(*value),
            Self::Number(value) => Value::Number(*value),
            Self::String(value) => Value::String(value.clone()),
        }
    }
}

impl From<&str> for ConstValue {
    fn from(value: &str) -> Self {
        Self::String(value.into())
    }
}

impl From<String> for ConstValue {
    fn from(value: String) -> Self {
        Self::String(value.into())
    }
}

impl From<f64> for ConstValue {
    fn from(value: f64) -> Self {
        Self::Number(value)
    }
}

impl From<bool> for ConstValue {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

#[derive(Clone)]
pub struct FunctionDecl {
    pub name: String,
    pub arity: usize,
    pub constructable: bool,
    pub class: Option<&'static str>,
    pub call: HostCall,
}

#[derive(Clone)]
pub struct ConstantDecl {
    pub name: String,
    pub value: ConstValue,
}

#[derive(Clone)]
pub enum ExportDecl {
    Function(FunctionDecl),
    Constant(ConstantDecl),
}

impl ExportDecl {
    pub fn name(&self) -> &str {
        match self {
            Self::Function(function) => &function.name,
            Self::Constant(constant) => &constant.name,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotKind {
    Js,
    JsList,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotDecl {
    pub name: &'static str,
    pub kind: SlotKind,
}

/// Well-known symbols that a host declaration may use as a prototype member
/// key. Declarations name the stable enum variant, never a runtime symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WellKnownSymbol {
    Iterator,
    ToPrimitive,
    Species,
    Match,
    MatchAll,
    Replace,
    Search,
    Split,
}

impl WellKnownSymbol {
    pub fn label(self) -> &'static str {
        match self {
            Self::Iterator => "Symbol.iterator",
            Self::ToPrimitive => "Symbol.toPrimitive",
            Self::Species => "Symbol.species",
            Self::Match => "Symbol.match",
            Self::MatchAll => "Symbol.matchAll",
            Self::Replace => "Symbol.replace",
            Self::Search => "Symbol.search",
            Self::Split => "Symbol.split",
        }
    }

    pub(crate) fn id(self) -> SymbolId {
        match self {
            Self::Iterator => crate::heap::ITERATOR_SYMBOL,
            Self::ToPrimitive => crate::heap::TO_PRIMITIVE_SYMBOL,
            Self::Species => crate::heap::SPECIES_SYMBOL,
            Self::Match => crate::heap::MATCH_SYMBOL,
            Self::MatchAll => crate::heap::MATCH_ALL_SYMBOL,
            Self::Replace => crate::heap::REPLACE_SYMBOL,
            Self::Search => crate::heap::SEARCH_SYMBOL,
            Self::Split => crate::heap::SPLIT_SYMBOL,
        }
    }
}

/// Prototype member key: a string name or an explicitly declared well-known
/// symbol. Arbitrary runtime symbols are not accepted as declarations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MemberName {
    Name(&'static str),
    Symbol(WellKnownSymbol),
}

impl MemberName {
    pub fn label(self) -> &'static str {
        match self {
            Self::Name(name) => name,
            Self::Symbol(symbol) => symbol.label(),
        }
    }

    pub(crate) fn key(self) -> PropertyKey {
        match self {
            Self::Name(name) => PropertyKey::from(name),
            Self::Symbol(symbol) => PropertyKey::Symbol(symbol.id()),
        }
    }
}

impl From<&'static str> for MemberName {
    fn from(name: &'static str) -> Self {
        Self::Name(name)
    }
}

impl From<WellKnownSymbol> for MemberName {
    fn from(symbol: WellKnownSymbol) -> Self {
        Self::Symbol(symbol)
    }
}

impl fmt::Display for MemberName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

#[derive(Clone)]
pub struct MethodDecl {
    pub name: MemberName,
    pub arity: usize,
    pub call: HostCall,
}

impl MethodDecl {
    pub const fn new(name: &'static str, arity: usize, call: HostCall) -> Self {
        Self {
            name: MemberName::Name(name),
            arity,
            call,
        }
    }

    pub const fn symbol(symbol: WellKnownSymbol, arity: usize, call: HostCall) -> Self {
        Self {
            name: MemberName::Symbol(symbol),
            arity,
            call,
        }
    }
}

#[derive(Clone)]
pub struct AccessorDecl {
    pub name: MemberName,
    pub get: Option<HostCall>,
    pub set: Option<HostCall>,
}

impl AccessorDecl {
    pub const fn new(name: &'static str, get: Option<HostCall>, set: Option<HostCall>) -> Self {
        Self {
            name: MemberName::Name(name),
            get,
            set,
        }
    }

    pub const fn symbol(
        symbol: WellKnownSymbol,
        get: Option<HostCall>,
        set: Option<HostCall>,
    ) -> Self {
        Self {
            name: MemberName::Symbol(symbol),
            get,
            set,
        }
    }
}

/// One host class. Instances are ordinary objects with a VM-owned prototype,
/// hidden typed slots, and an optional non-JS payload.
pub trait HostClass: Sized + 'static {
    const TAG: &'static str;
    /// Tag of the class whose prototype and slot layout this class extends.
    /// A derived instance is accepted by base-class methods, and base slots
    /// occupy the leading part of its slot layout.
    const PARENT: Option<&'static str> = None;
    const SLOTS: &'static [SlotDecl] = &[];
    const METHODS: &'static [MethodDecl] = &[];
    const ACCESSORS: &'static [AccessorDecl] = &[];
    /// `None` when the class has no payload. Payload bytes never contain VM
    /// object ids; JavaScript references belong in slots.
    const PAYLOAD_SCHEMA: Option<u32> = None;

    fn encode_payload(&self, _out: &mut Vec<u8>) -> Result<(), CodecError> {
        Ok(())
    }

    fn decode_payload(_input: &[u8], _cx: &mut DecodeCx<'_>) -> Result<Self, CodecError> {
        Err(CodecError::new("class declaration has no payload decoder"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Recovery {
    /// Fulfil the original Promise with `undefined`; do not re-register work.
    Pause,
    /// Re-register the work; a later delivery settles the original Promise.
    Resume,
    /// Re-issue idempotent work; a later delivery settles the original Promise.
    Reissue,
    /// Reject the original Promise with a declared message.
    Reject,
}

/// Per-VM module state. JavaScript references live in slots; the payload is
/// ordinary Rust data with an independent schema version.
pub trait HostModuleState: Sized + 'static {
    const MODULE: &'static str;
    const SLOTS: &'static [SlotDecl] = &[];
    const PAYLOAD_SCHEMA: u32;

    fn new() -> Self;

    fn encode_payload(&self, out: &mut Vec<u8>) -> Result<(), CodecError>;
    fn decode_payload(input: &[u8], cx: &mut DecodeCx<'_>) -> Result<Self, CodecError>;
}

/// One recoverable asynchronous operation type.
pub trait Operation: Sized + 'static {
    const NAME: &'static str;
    const SCHEMA: u32;
    const RECOVERY: Recovery;
    const REJECTION: &'static str = "operation rejected during restore";

    fn encode(&self, out: &mut Vec<u8>) -> Result<(), CodecError>;
    fn decode(input: &[u8], bindings: &Bindings) -> Result<Self, CodecError>;
}

/// Capability handed to the host when an operation starts. A restored VM has a
/// new generation, so tokens minted before the snapshot never settle it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct OperationToken {
    pub(crate) epoch: u32,
    pub(crate) id: u64,
}

impl OperationToken {
    pub fn id(self) -> u64 {
        self.id
    }
}

#[derive(Debug, Clone, Copy)]
pub struct OperationStart<'a> {
    pub token: OperationToken,
    pub name: &'a str,
    pub schema: u32,
    pub recovery: Recovery,
    pub state: &'a [u8],
}

/// Read-side context for payload decoding. Services are process resources and
/// are never captured by the snapshot.
pub struct DecodeCx<'a> {
    bindings: &'a Bindings,
    services: &'a Services,
}

impl<'a> DecodeCx<'a> {
    pub fn service<T: Any + Send + Sync>(&self, id: &str) -> Option<&T> {
        self.services.get(id)
    }

    pub fn bindings(&self) -> &'a Bindings {
        self.bindings
    }
}

#[derive(Default)]
pub struct Services {
    values: BTreeMap<String, Arc<dyn Any + Send + Sync>>,
}

impl Services {
    pub fn get<T: Any + Send + Sync>(&self, id: &str) -> Option<&T> {
        self.values.get(id)?.downcast_ref()
    }
}

type DecodeFn = fn(&[u8], &mut DecodeCx<'_>) -> Result<Box<dyn Any>, CodecError>;
type EncodeFn = fn(&dyn Any, &mut Vec<u8>) -> Result<(), CodecError>;
type EmptyFn = fn(&dyn Any) -> bool;

struct ClassDecl {
    tag: String,
    parent_tag: Option<&'static str>,
    parent: Option<ClassId>,
    /// Leading slots inherited from the parent chain, followed by this
    /// class's own declarations. `slot_base` is the first own slot.
    slots: Vec<SlotDecl>,
    slot_base: usize,
    methods: Vec<MethodDecl>,
    accessors: Vec<AccessorDecl>,
    payload_schema: Option<u32>,
    decode: DecodeFn,
    encode: EncodeFn,
    empty: EmptyFn,
}

struct StateDecl {
    module: String,
    slots: Vec<SlotDecl>,
    payload_schema: u32,
    new: fn() -> Box<dyn Any>,
    decode: DecodeFn,
    encode: EncodeFn,
}

struct OperationDecl {
    name: String,
    schema: u32,
    recovery: Recovery,
    rejection: &'static str,
    decode: fn(&[u8], &Bindings) -> Result<(), CodecError>,
}

pub struct ModuleDecl {
    name: String,
    exports: Vec<ExportDecl>,
}

/// Immutable declaration table shared by every VM using the same contract.
#[derive(Clone)]
pub struct Bindings {
    inner: Arc<BindingsInner>,
}

struct BindingsInner {
    modules: Vec<ModuleDecl>,
    classes: Vec<ClassDecl>,
    operations: Vec<OperationDecl>,
    states: BTreeMap<usize, StateDecl>,
    services: Services,
    fingerprint: String,
}

impl std::fmt::Debug for Bindings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Bindings")
            .field("fingerprint", &self.inner.fingerprint)
            .field("modules", &self.inner.modules.len())
            .field("classes", &self.inner.classes.len())
            .field("operations", &self.inner.operations.len())
            .finish()
    }
}

impl Bindings {
    pub fn builder() -> BindingsBuilder {
        BindingsBuilder {
            modules: Vec::new(),
            classes: Vec::new(),
            operations: Vec::new(),
            states: Vec::new(),
            services: Services::default(),
            service_ids: BTreeSet::new(),
        }
    }

    pub fn fingerprint(&self) -> &str {
        &self.inner.fingerprint
    }

    pub fn module_names(&self) -> impl Iterator<Item = &str> {
        self.inner.modules.iter().map(|module| module.name.as_str())
    }

    pub fn export_names<'a>(&'a self, module: &'a str) -> impl Iterator<Item = &'a str> {
        self.module_index(module)
            .and_then(|index| self.inner.modules.get(index))
            .into_iter()
            .flat_map(|module| module.exports.iter().map(|export| export.name()))
    }

    pub fn module_count(&self) -> usize {
        self.inner.modules.len()
    }

    pub fn export_count(&self, module: u32) -> usize {
        self.inner.modules[module as usize].exports.len()
    }

    pub fn module_name(&self, index: usize) -> Option<&str> {
        self.inner
            .modules
            .get(index)
            .map(|module| module.name.as_str())
    }

    pub fn module_index(&self, name: &str) -> Option<usize> {
        self.inner
            .modules
            .iter()
            .position(|module| module.name == name)
    }

    pub fn export(&self, module: &str, name: &str) -> Option<(ExportId, &ExportDecl)> {
        let module_index = self.module_index(module)?;
        let (export_index, export) = self.inner.modules[module_index]
            .exports
            .iter()
            .enumerate()
            .find(|(_, export)| export.name() == name)?;
        Some((
            ExportId {
                module: module_index as u32,
                export: export_index as u32,
            },
            export,
        ))
    }

    pub fn export_decl(&self, id: ExportId) -> Option<&ExportDecl> {
        self.inner
            .modules
            .get(id.module as usize)?
            .exports
            .get(id.export as usize)
    }

    pub fn export_class(&self, id: ExportId) -> Option<ClassId> {
        match self.export_decl(id)? {
            ExportDecl::Function(function) => function.class.and_then(|tag| self.class_by_tag(tag)),
            ExportDecl::Constant(_) => None,
        }
    }

    pub fn class_tag(&self, class: ClassId) -> Option<&str> {
        self.inner
            .classes
            .get(class)
            .map(|class| class.tag.as_str())
    }

    pub fn class_by_tag(&self, tag: &str) -> Option<ClassId> {
        self.inner.classes.iter().position(|class| class.tag == tag)
    }

    pub fn classes(&self) -> impl Iterator<Item = (ClassId, &str)> + '_ {
        self.inner
            .classes
            .iter()
            .enumerate()
            .map(|(index, class)| (index, class.tag.as_str()))
    }

    pub fn class_slots(&self, class: ClassId) -> &[SlotDecl] {
        &self.inner.classes[class].slots
    }

    /// Index of the class's first own slot in the combined layout.
    pub fn class_slot_base(&self, class: ClassId) -> usize {
        self.inner.classes[class].slot_base
    }

    pub fn class_parent(&self, class: ClassId) -> Option<ClassId> {
        self.inner.classes[class].parent
    }

    /// Whether `class` is `base` or inherits from it.
    pub fn class_is_a(&self, class: ClassId, base: ClassId) -> bool {
        let mut current = Some(class);
        while let Some(class) = current {
            if class == base {
                return true;
            }
            current = self.inner.classes[class].parent;
        }
        false
    }

    pub fn class_payload_schema(&self, class: ClassId) -> Option<u32> {
        self.inner.classes[class].payload_schema
    }

    pub(crate) fn class_payload_is_empty(&self, class: ClassId, payload: &dyn Any) -> bool {
        (self.inner.classes[class].empty)(payload)
    }

    pub fn method_count(&self, class: ClassId) -> usize {
        self.inner.classes[class].methods.len()
    }

    pub fn method(&self, id: MethodId) -> Option<&MethodDecl> {
        self.inner
            .classes
            .get(id.class as usize)?
            .methods
            .get(id.method as usize)
    }

    pub fn accessor(&self, class: ClassId, index: usize) -> Option<&AccessorDecl> {
        self.inner.classes.get(class)?.accessors.get(index)
    }

    pub fn accessor_count(&self, class: ClassId) -> usize {
        self.inner.classes[class].accessors.len()
    }

    pub fn operation_count(&self) -> usize {
        self.inner.operations.len()
    }

    pub fn operation(&self, name: &str, schema: u32) -> Option<(usize, Recovery, &'static str)> {
        self.inner
            .operations
            .iter()
            .enumerate()
            .find(|(_, operation)| operation.name == name && operation.schema == schema)
            .map(|(index, operation)| (index, operation.recovery, operation.rejection))
    }

    pub fn decode_operation(
        &self,
        name: &str,
        schema: u32,
        state: &[u8],
    ) -> Result<(), CodecError> {
        let operation = self
            .inner
            .operations
            .iter()
            .find(|operation| operation.name == name && operation.schema == schema)
            .ok_or_else(|| CodecError::new(format!("unknown operation {name} schema {schema}")))?;
        (operation.decode)(state, self)
    }

    pub(crate) fn state_module(&self, name: &str) -> Option<usize> {
        self.module_index(name)
            .filter(|module| self.inner.states.contains_key(module))
    }

    pub(crate) fn state_slots(&self, module: usize) -> Option<&[SlotDecl]> {
        self.inner
            .states
            .get(&module)
            .map(|state| state.slots.as_slice())
    }

    pub(crate) fn state_schema(&self, module: usize) -> Option<u32> {
        self.inner
            .states
            .get(&module)
            .map(|state| state.payload_schema)
    }

    pub(crate) fn state_new(&self, module: usize) -> Option<Box<dyn Any>> {
        self.inner.states.get(&module).map(|state| (state.new)())
    }

    pub(crate) fn state_decode(
        &self,
        module: usize,
        input: &[u8],
    ) -> Option<Result<Box<dyn Any>, CodecError>> {
        let state = self.inner.states.get(&module)?;
        let mut cx = self.decode_cx();
        Some((state.decode)(input, &mut cx))
    }

    pub(crate) fn state_encode(
        &self,
        module: usize,
        payload: &dyn Any,
        out: &mut Vec<u8>,
    ) -> Option<Result<(), CodecError>> {
        Some((self.inner.states.get(&module)?.encode)(payload, out))
    }

    pub(crate) fn state_modules(&self) -> Vec<usize> {
        self.inner.states.keys().copied().collect()
    }

    pub(crate) fn state_name(&self, module: usize) -> Option<&str> {
        self.inner
            .states
            .get(&module)
            .map(|state| state.module.as_str())
    }

    pub(crate) fn service<T: Any + Send + Sync>(&self, id: &str) -> Option<&T> {
        self.inner.services.get(id)
    }

    pub(crate) fn decode_cx(&self) -> DecodeCx<'_> {
        DecodeCx {
            bindings: self,
            services: &self.inner.services,
        }
    }

    pub(crate) fn class_decode(
        &self,
        class: ClassId,
        input: &[u8],
    ) -> Result<Box<dyn Any>, CodecError> {
        let declaration = &self.inner.classes[class];
        let mut cx = self.decode_cx();
        (declaration.decode)(input, &mut cx)
    }

    pub(crate) fn class_encode(
        &self,
        class: ClassId,
        payload: &dyn Any,
        out: &mut Vec<u8>,
    ) -> Result<(), CodecError> {
        (self.inner.classes[class].encode)(payload, out)
    }

    pub(crate) fn classes_len(&self) -> usize {
        self.inner.classes.len()
    }
}

pub struct BindingsBuilder {
    modules: Vec<ModuleDecl>,
    classes: Vec<ClassDecl>,
    operations: Vec<OperationDecl>,
    states: Vec<StateDecl>,
    services: Services,
    service_ids: BTreeSet<String>,
}

impl BindingsBuilder {
    pub fn service<T: Any + Send + Sync>(mut self, id: &str, version: u32, value: T) -> Self {
        self.service_ids.insert(format!("{id}:{version}"));
        self.services.values.insert(id.to_owned(), Arc::new(value));
        self
    }

    pub fn class<T: HostClass>(mut self) -> Self {
        self.classes.push(ClassDecl {
            tag: T::TAG.to_owned(),
            parent_tag: T::PARENT,
            parent: None,
            slot_base: 0,
            slots: T::SLOTS.to_vec(),
            methods: T::METHODS.to_vec(),
            accessors: T::ACCESSORS.to_vec(),
            payload_schema: T::PAYLOAD_SCHEMA,
            decode: |input, cx| {
                Ok(Box::new(T::decode_payload(input, cx).map_err(|error| {
                    CodecError::new(format!("{}: {error}", T::TAG))
                })?))
            },
            encode: |payload, out| {
                let payload = payload
                    .downcast_ref::<T>()
                    .ok_or_else(|| CodecError::new("payload type mismatch"))?;
                T::encode_payload(payload, out)
            },
            empty: |payload| {
                payload
                    .downcast_ref::<T>()
                    .is_some_and(|payload| std::mem::size_of_val(payload) == 0)
            },
        });
        self
    }

    pub fn state<S: HostModuleState>(mut self) -> Self {
        self.states.push(StateDecl {
            module: S::MODULE.to_owned(),
            slots: S::SLOTS.to_vec(),
            payload_schema: S::PAYLOAD_SCHEMA,
            new: || Box::new(S::new()),
            decode: |input, cx| {
                Ok(Box::new(S::decode_payload(input, cx).map_err(|error| {
                    CodecError::new(format!("{}: {error}", S::MODULE))
                })?))
            },
            encode: |payload, out| {
                let payload = payload
                    .downcast_ref::<S>()
                    .ok_or_else(|| CodecError::new("module state payload type mismatch"))?;
                S::encode_payload(payload, out)
            },
        });
        self
    }

    pub fn operation<O: Operation>(mut self) -> Self {
        self.operations.push(OperationDecl {
            name: O::NAME.to_owned(),
            schema: O::SCHEMA,
            recovery: O::RECOVERY,
            rejection: O::REJECTION,
            decode: |input, bindings| O::decode(input, bindings).map(|_| ()),
        });
        self
    }

    pub fn module(mut self, module: ModuleDecl) -> Self {
        self.modules.push(module);
        self
    }

    pub fn build(mut self) -> Result<Bindings, String> {
        self.modules.sort_by(|a, b| a.name.cmp(&b.name));
        for module in &mut self.modules {
            module.exports.sort_by(|a, b| a.name().cmp(b.name()));
        }
        for pair in self.modules.windows(2) {
            if pair[0].name == pair[1].name {
                return Err(format!("duplicate host module {:?}", pair[0].name));
            }
        }
        self.classes.sort_by(|a, b| a.tag.cmp(&b.tag));
        for pair in self.classes.windows(2) {
            if pair[0].tag == pair[1].tag {
                return Err(format!("duplicate host class {:?}", pair[0].tag));
            }
        }
        resolve_class_layouts(&mut self.classes)?;
        for class in &mut self.classes {
            class.methods.sort_by_key(|method| method.name);
            class.accessors.sort_by_key(|accessor| accessor.name);
            let mut names = BTreeSet::new();
            for method in &class.methods {
                if !names.insert(method.name) {
                    return Err(format!(
                        "duplicate host method {:?}.{}",
                        class.tag, method.name
                    ));
                }
            }
            for accessor in &class.accessors {
                if !names.insert(accessor.name) || accessor.get.is_none() && accessor.set.is_none()
                {
                    return Err(format!(
                        "invalid host accessor {:?}.{}",
                        class.tag, accessor.name
                    ));
                }
            }
        }
        self.operations.sort_by(|a, b| a.name.cmp(&b.name));
        let mut operation_names = BTreeSet::new();
        for operation in &self.operations {
            if !operation_names.insert(operation.name.as_str()) {
                return Err(format!("duplicate host operation {:?}", operation.name));
            }
        }
        self.states.sort_by(|a, b| a.module.cmp(&b.module));
        let mut states = BTreeMap::new();
        for state in self.states {
            let module = self
                .modules
                .iter()
                .position(|module| module.name == state.module)
                .ok_or_else(|| format!("module state for unknown module {:?}", state.module))?;
            if states.insert(module, state).is_some() {
                return Err(format!(
                    "duplicate module state for {:?}",
                    self.modules[module].name
                ));
            }
        }
        let fingerprint = fingerprint(
            &self.modules,
            &self.classes,
            &self.operations,
            &states,
            &self.service_ids,
        );
        Ok(Bindings {
            inner: Arc::new(BindingsInner {
                modules: self.modules,
                classes: self.classes,
                operations: self.operations,
                states,
                services: self.services,
                fingerprint,
            }),
        })
    }
}

impl ModuleDecl {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            exports: Vec::new(),
        }
    }

    pub fn function(mut self, name: &str, arity: usize, call: HostCall) -> Self {
        self.exports.push(ExportDecl::Function(FunctionDecl {
            name: name.to_owned(),
            arity,
            constructable: false,
            class: None,
            call,
        }));
        self
    }

    pub fn constructor(mut self, name: &str, arity: usize, call: HostCall) -> Self {
        self.exports.push(ExportDecl::Function(FunctionDecl {
            name: name.to_owned(),
            arity,
            constructable: true,
            class: None,
            call,
        }));
        self
    }

    pub fn constructor_for<T: HostClass>(
        mut self,
        name: &str,
        arity: usize,
        call: HostCall,
    ) -> Self {
        self.exports.push(ExportDecl::Function(FunctionDecl {
            name: name.to_owned(),
            arity,
            constructable: true,
            class: Some(T::TAG),
            call,
        }));
        self
    }

    pub fn constant(mut self, name: &str, value: impl Into<ConstValue>) -> Self {
        self.exports.push(ExportDecl::Constant(ConstantDecl {
            name: name.to_owned(),
            value: value.into(),
        }));
        self
    }
}

fn resolve_class_layouts(classes: &mut [ClassDecl]) -> Result<(), String> {
    let tags: BTreeMap<String, usize> = classes
        .iter()
        .enumerate()
        .map(|(index, class)| (class.tag.clone(), index))
        .collect();
    let parents = classes
        .iter()
        .map(|class| {
            class
                .parent_tag
                .map(|tag| {
                    tags.get(tag)
                        .copied()
                        .ok_or_else(|| format!("class {:?} has unknown parent {tag:?}", class.tag))
                })
                .transpose()
        })
        .collect::<Result<Vec<_>, String>>()?;

    fn visit(
        index: usize,
        parents: &[Option<usize>],
        tags: &[String],
        state: &mut [u8],
        order: &mut Vec<usize>,
    ) -> Result<(), String> {
        match state[index] {
            2 => return Ok(()),
            1 => {
                return Err(format!("class inheritance cycle at {:?}", tags[index]));
            }
            _ => {}
        }
        state[index] = 1;
        if let Some(parent) = parents[index] {
            visit(parent, parents, tags, state, order)?;
        }
        state[index] = 2;
        order.push(index);
        Ok(())
    }

    let tags: Vec<String> = classes.iter().map(|class| class.tag.clone()).collect();
    let mut state = vec![0u8; classes.len()];
    let mut order = Vec::with_capacity(classes.len());
    for index in 0..classes.len() {
        visit(index, &parents, &tags, &mut state, &mut order)?;
    }
    for index in order {
        let parent = parents[index];
        let inherited = parent
            .map(|parent| classes[parent].slots.clone())
            .unwrap_or_default();
        let slot_base = inherited.len();
        let own = std::mem::take(&mut classes[index].slots);
        let mut slots = inherited;
        slots.extend(own);
        let class = &mut classes[index];
        class.parent = parent;
        class.slot_base = slot_base;
        class.slots = slots;
    }
    for class in classes {
        let mut names = BTreeSet::new();
        for slot in &class.slots {
            if !names.insert(slot.name) {
                return Err(format!("duplicate host slot {:?}.{}", class.tag, slot.name));
            }
        }
    }
    Ok(())
}

fn fingerprint(
    modules: &[ModuleDecl],
    classes: &[ClassDecl],
    operations: &[OperationDecl],
    states: &BTreeMap<usize, StateDecl>,
    services: &BTreeSet<String>,
) -> String {
    let mut hasher = Sha256::new();
    let mut line = |text: String| {
        hasher.update(text.as_bytes());
        hasher.update(b"\n");
    };
    for module in modules {
        line(format!("module {}", module.name));
        for export in &module.exports {
            match export {
                ExportDecl::Function(function) => line(format!(
                    "export {} function arity={} constructable={} class={:?}",
                    function.name, function.arity, function.constructable, function.class
                )),
                ExportDecl::Constant(constant) => line(format!(
                    "export {} constant {:?}",
                    constant.name,
                    constant.value.kind()
                )),
            }
        }
    }
    for class in classes {
        line(format!("class {} parent={:?}", class.tag, class.parent_tag));
        for slot in &class.slots {
            line(format!("slot {} {:?}", slot.name, slot.kind));
        }
        if let Some(schema) = class.payload_schema {
            line(format!("payload {schema}"));
        }
        for method in &class.methods {
            line(format!("method {} arity={}", method.name, method.arity));
        }
        for accessor in &class.accessors {
            line(format!(
                "accessor {} get={} set={}",
                accessor.name,
                accessor.get.is_some(),
                accessor.set.is_some()
            ));
        }
    }
    for operation in operations {
        line(format!(
            "operation {} schema={} recovery={:?}",
            operation.name, operation.schema, operation.recovery
        ));
    }
    for (module, state) in states {
        line(format!(
            "module-state {} schema={}",
            modules[*module].name, state.payload_schema
        ));
        for slot in &state.slots {
            line(format!("state-slot {} {:?}", slot.name, slot.kind));
        }
    }
    for service in services {
        line(format!("service {service}"));
    }
    format!("{:x}", hasher.finalize())
}

// ---------------------------------------------------------------------------
// Values

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Origin {
    Constant,
    Handle { epoch: u32, call: u64 },
}

/// A value crossing the host boundary. Object and symbol identity only make
/// sense within the VM generation and call that produced the handle.
#[derive(Debug, Clone)]
pub struct JsValue {
    pub(crate) value: Value,
    pub(crate) origin: Origin,
}

impl JsValue {
    pub fn undefined() -> Self {
        Self::constant(Value::Undefined)
    }

    pub fn null() -> Self {
        Self::constant(Value::Null)
    }

    pub fn boolean(value: bool) -> Self {
        Self::constant(Value::Bool(value))
    }

    pub fn number(value: f64) -> Self {
        Self::constant(Value::Number(value))
    }

    pub fn string(value: impl Into<JsString>) -> Self {
        Self::constant(Value::String(value.into()))
    }

    fn constant(value: Value) -> Self {
        Self {
            value,
            origin: Origin::Constant,
        }
    }

    pub(crate) fn handle(value: Value, epoch: u32, call: u64) -> Self {
        Self {
            value,
            origin: Origin::Handle { epoch, call },
        }
    }

    pub fn is_undefined(&self) -> bool {
        matches!(self.value, Value::Undefined)
    }

    pub fn is_null(&self) -> bool {
        matches!(self.value, Value::Null)
    }

    pub fn is_object(&self) -> bool {
        matches!(self.value, Value::Object(_))
    }

    pub fn is_symbol(&self) -> bool {
        matches!(self.value, Value::Symbol(_))
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self.value {
            Value::Bool(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_number(&self) -> Option<f64> {
        match self.value {
            Value::Number(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_string(&self) -> Option<&JsString> {
        match &self.value {
            Value::String(value) => Some(value),
            _ => None,
        }
    }

    pub fn type_of(&self) -> &'static str {
        self.value.type_of()
    }

    pub fn truthy(&self) -> bool {
        self.value.truthy()
    }

    pub(crate) fn into_constant(self) -> Result<Value, HostFault> {
        match self.origin {
            Origin::Constant => Ok(self.value),
            Origin::Handle { .. } => Err(HostFault::new(
                "a JsValue handle cannot be used outside its call".to_owned(),
            )),
        }
    }
}

impl PartialEq for JsValue {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}

impl fmt::Display for JsValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.value {
            Value::Undefined => formatter.write_str("undefined"),
            Value::Null => formatter.write_str("null"),
            Value::Bool(value) => write!(formatter, "{value}"),
            Value::Number(value) => write!(formatter, "{value}"),
            Value::String(value) => write!(formatter, "{value:?}"),
            Value::Symbol(id) => write!(formatter, "symbol({id})"),
            Value::Object(id) => write!(formatter, "object({id})"),
        }
    }
}

/// A typed instance allocated by the VM. The marker only exists at compile
/// time; the class tag is checked when reading slots or payload.
pub struct Instance<T: HostClass> {
    id: ObjectId,
    marker: std::marker::PhantomData<T>,
}

impl<T: HostClass> Clone for Instance<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: HostClass> Copy for Instance<T> {}

impl<T: HostClass> Instance<T> {
    pub(crate) fn new(id: ObjectId) -> Self {
        Self {
            id,
            marker: std::marker::PhantomData,
        }
    }

    pub fn value(&self, cx: &CallCx<'_>) -> JsValue {
        cx.js_value(Value::Object(self.id))
    }

    pub fn identity(&self) -> u64 {
        self.id as u64
    }

    pub fn slot(&self, cx: &CallCx<'_>, index: usize) -> JsResult<JsValue> {
        cx.slot_value(self.id, cx.instance_slot::<T>(index)?)
    }

    pub fn set_slot(&self, cx: &mut CallCx<'_>, index: usize, value: JsValue) -> JsResult<()> {
        let index = cx.instance_slot::<T>(index)?;
        cx.set_slot_value(self.id, index, value)
    }

    pub fn list_len(&self, cx: &CallCx<'_>, index: usize) -> JsResult<usize> {
        cx.list_len_value(self.id, cx.instance_slot::<T>(index)?)
    }

    pub fn list_get(&self, cx: &CallCx<'_>, index: usize, position: usize) -> JsResult<JsValue> {
        let index = cx.instance_slot::<T>(index)?;
        cx.list_get_value(self.id, index, position)
    }

    pub fn list_push(&self, cx: &mut CallCx<'_>, index: usize, value: JsValue) -> JsResult<()> {
        let index = cx.instance_slot::<T>(index)?;
        cx.list_push_value(self.id, index, value)
    }

    pub fn list_clear(&self, cx: &mut CallCx<'_>, index: usize) -> JsResult<()> {
        let index = cx.instance_slot::<T>(index)?;
        cx.list_clear_value(self.id, index)
    }

    pub fn payload<'a>(&self, cx: &'a CallCx<'_>) -> JsResult<&'a T> {
        cx.payload_value::<T>(self.id)
    }
}

pub struct This<T: HostClass> {
    instance: Instance<T>,
}

impl<T: HostClass> This<T> {
    pub fn instance(&self) -> Instance<T> {
        self.instance
    }
}

// ---------------------------------------------------------------------------
// Calls

pub struct CallCx<'a> {
    pub(crate) vm: &'a mut Vm,
    pub(crate) host: &'a mut dyn crate::host::Host,
    pub(crate) bindings: Arc<Bindings>,
    pub(crate) epoch: u32,
    pub(crate) call: u64,
    pub(crate) args: Vec<JsValue>,
    pub(crate) this: JsValue,
    pub(crate) construct: bool,
}

impl<'a> CallCx<'a> {
    pub fn args(&self) -> &[JsValue] {
        &self.args
    }

    pub fn arg(&mut self, index: usize) -> JsResult<JsValue> {
        let value = self.args.get(index).cloned();
        value.ok_or_else(|| self.type_error("missing argument"))
    }

    pub fn this_value(&self) -> JsValue {
        self.this.clone()
    }

    pub fn is_construct(&self) -> bool {
        self.construct
    }

    pub fn bindings(&self) -> &Bindings {
        &self.bindings
    }

    pub fn throw(&self, value: JsValue) -> JsError {
        JsError::Throw(value)
    }

    pub fn throw_string(&self, message: impl Into<String>) -> JsError {
        JsError::Throw(self.js_value(Value::String(message.into().into())))
    }

    pub fn type_error(&mut self, message: impl Into<String>) -> JsError {
        let message = format!("TypeError: {}", message.into());
        match self.vm.type_error(&message) {
            Ok(value) => JsError::Throw(self.js_value(value)),
            Err(error) => JsError::Fault(HostFault::new(error.to_string())),
        }
    }

    pub fn fault(&self, message: impl Into<String>) -> HostFault {
        HostFault::new(message)
    }

    pub fn string<'v>(&mut self, value: &'v JsValue) -> JsResult<&'v JsString> {
        value
            .as_string()
            .ok_or_else(|| self.type_error("expected a string"))
    }

    pub fn number(&mut self, value: &JsValue) -> JsResult<f64> {
        value
            .as_number()
            .ok_or_else(|| self.type_error("expected a number"))
    }

    pub fn same_value(&self, left: &JsValue, right: &JsValue) -> bool {
        crate::ops::same_value(&left.value, &right.value)
    }

    pub fn service<T: Any + Send + Sync>(&self, id: &str) -> Option<&T> {
        self.bindings.service(id)
    }

    fn state_module<S: crate::binding::HostModuleState>(&self) -> JsResult<usize> {
        self.bindings.state_module(S::MODULE).ok_or_else(|| {
            JsError::Fault(HostFault::new(format!(
                "module state for {:?} is not declared",
                S::MODULE
            )))
        })
    }

    pub fn state<S: crate::binding::HostModuleState>(&mut self) -> JsResult<&mut S> {
        let module = self.state_module::<S>()?;
        let state = self
            .vm
            .module_states
            .get_mut(module)
            .and_then(Option::as_mut)
            .ok_or_else(|| JsError::Fault(HostFault::new("module state is missing")))?;
        state
            .payload
            .downcast_mut::<S>()
            .ok_or_else(|| JsError::Fault(HostFault::new("module state payload type mismatch")))
    }

    pub fn state_slot<S: crate::binding::HostModuleState>(
        &self,
        index: usize,
    ) -> JsResult<JsValue> {
        let module = self.state_module::<S>()?;
        match self
            .vm
            .module_states
            .get(module)
            .and_then(Option::as_ref)
            .and_then(|state| state.slots.get(index))
        {
            Some(crate::runtime::HostSlot::Js(value)) => Ok(self.js_value(value.clone())),
            Some(crate::runtime::HostSlot::JsList(_)) => Err(JsError::Fault(HostFault::new(
                "module state slot is a JavaScript list",
            ))),
            None => Err(JsError::Fault(HostFault::new(
                "module state slot index out of range",
            ))),
        }
    }

    pub fn set_state_slot<S: crate::binding::HostModuleState>(
        &mut self,
        index: usize,
        value: JsValue,
    ) -> JsResult<()> {
        self.check_value(&value)?;
        let module = self.state_module::<S>()?;
        let value = value.value;
        match self
            .vm
            .module_states
            .get_mut(module)
            .and_then(Option::as_mut)
            .and_then(|state| state.slots.get_mut(index))
        {
            Some(crate::runtime::HostSlot::Js(slot)) => {
                *slot = value;
                Ok(())
            }
            Some(crate::runtime::HostSlot::JsList(_)) => Err(JsError::Fault(HostFault::new(
                "module state slot is a JavaScript list",
            ))),
            None => Err(JsError::Fault(HostFault::new(
                "module state slot index out of range",
            ))),
        }
    }

    pub fn state_list_len<S: crate::binding::HostModuleState>(
        &self,
        index: usize,
    ) -> JsResult<usize> {
        let module = self.state_module::<S>()?;
        match self
            .vm
            .module_states
            .get(module)
            .and_then(Option::as_ref)
            .and_then(|state| state.slots.get(index))
        {
            Some(crate::runtime::HostSlot::JsList(values)) => Ok(values.len()),
            Some(crate::runtime::HostSlot::Js(_)) => Err(JsError::Fault(HostFault::new(
                "module state slot is a JavaScript value",
            ))),
            None => Err(JsError::Fault(HostFault::new(
                "module state slot index out of range",
            ))),
        }
    }

    pub fn state_list_push<S: crate::binding::HostModuleState>(
        &mut self,
        index: usize,
        value: JsValue,
    ) -> JsResult<()> {
        self.check_value(&value)?;
        let module = self.state_module::<S>()?;
        let value = value.value;
        match self
            .vm
            .module_states
            .get_mut(module)
            .and_then(Option::as_mut)
            .and_then(|state| state.slots.get_mut(index))
        {
            Some(crate::runtime::HostSlot::JsList(values)) => {
                values.push(value);
                Ok(())
            }
            Some(crate::runtime::HostSlot::Js(_)) => Err(JsError::Fault(HostFault::new(
                "module state slot is a JavaScript value",
            ))),
            None => Err(JsError::Fault(HostFault::new(
                "module state slot index out of range",
            ))),
        }
    }

    pub fn alloc<T: HostClass>(&mut self, payload: T) -> JsResult<Instance<T>> {
        let class = self
            .bindings
            .class_by_tag(T::TAG)
            .ok_or_else(|| JsError::Fault(self.fault(format!("unregistered class {}", T::TAG))))?;
        let prototype = *self.vm.class_prototypes.get(class).ok_or_else(|| {
            JsError::Fault(self.fault(format!("class {} has no prototype", T::TAG)))
        })?;
        let payload = Box::new(payload);
        if self.bindings.class_payload_schema(class).is_none()
            && !self
                .bindings
                .class_payload_is_empty(class, payload.as_ref())
        {
            return Err(JsError::Fault(HostFault::new(format!(
                "class {} has data but no payload schema",
                T::TAG
            ))));
        }
        let object = self.vm.heap.alloc_plain_with(Some(prototype));
        let slots = self
            .bindings
            .class_slots(class)
            .iter()
            .map(|slot| match slot.kind {
                SlotKind::Js => crate::runtime::HostSlot::Js(Value::Undefined),
                SlotKind::JsList => crate::runtime::HostSlot::JsList(Vec::new()),
            });
        let slots = slots.collect();
        self.vm.host_instances.insert(
            object,
            crate::runtime::HostInstance {
                class,
                slots,
                payload: Some(payload),
            },
        );
        Ok(Instance::new(object))
    }

    pub fn this<T: HostClass>(&mut self) -> JsResult<This<T>> {
        let Value::Object(id) = self.this.value else {
            return Err(self.type_error(format!("{} called on incompatible receiver", T::TAG)));
        };
        let Some(class) = self.bindings.class_by_tag(T::TAG) else {
            return Err(JsError::Fault(
                self.fault(format!("unregistered class {}", T::TAG)),
            ));
        };
        let Some(instance) = self.vm.host_instances.get(&id) else {
            return Err(self.type_error(format!("{} called on incompatible receiver", T::TAG)));
        };
        if !self.bindings.class_is_a(instance.class, class) {
            return Err(self.type_error(format!("{} called on incompatible receiver", T::TAG)));
        }
        Ok(This {
            instance: Instance::new(id),
        })
    }

    pub fn inspect(&self, value: JsValue) -> JsResult<Inspect<'_>> {
        self.check_value(&value)?;
        Ok(Inspect {
            vm: self.vm,
            bindings: &self.bindings,
            value,
            epoch: self.epoch,
            call: self.call,
        })
    }

    pub fn start_operation<O: Operation>(&mut self, state: O) -> JsResult<JsValue> {
        let mut bytes = Vec::new();
        state
            .encode(&mut bytes)
            .map_err(|error| JsError::Fault(HostFault(error.0)))?;
        let promise = self.vm.heap.alloc_promise();
        let token = self.vm.alloc_operation::<O>(bytes.clone(), promise);
        let start = OperationStart {
            token,
            name: O::NAME,
            schema: O::SCHEMA,
            recovery: O::RECOVERY,
            state: &bytes,
        };
        if let Err(error) = self.host.start_operation(start) {
            self.vm.operations.remove(&token.id);
            return Err(JsError::Fault(error));
        }
        Ok(self.js_value(Value::Object(promise)))
    }

    pub(crate) fn js_value(&self, value: Value) -> JsValue {
        JsValue::handle(value, self.epoch, self.call)
    }

    /// Translate a declaration-local slot index into the combined layout.
    pub(crate) fn instance_slot<T: HostClass>(&self, index: usize) -> JsResult<usize> {
        let class = self.bindings.class_by_tag(T::TAG).ok_or_else(|| {
            JsError::Fault(HostFault::new(format!("unregistered class {}", T::TAG)))
        })?;
        self.bindings
            .class_slot_base(class)
            .checked_add(index)
            .ok_or_else(|| JsError::Fault(HostFault::new("slot index out of range")))
    }

    pub(crate) fn check_value(&self, value: &JsValue) -> Result<(), JsError> {
        match value.origin {
            Origin::Constant => Ok(()),
            Origin::Handle { epoch, call } if epoch == self.epoch && call == self.call => Ok(()),
            Origin::Handle { .. } => Err(JsError::Fault(HostFault::new(
                "JsValue handle used outside the call that produced it",
            ))),
        }
    }

    pub(crate) fn into_value(self, value: JsValue) -> Result<Value, HostFault> {
        match value.origin {
            Origin::Constant => Ok(value.value),
            Origin::Handle { epoch, call } if epoch == self.epoch && call == self.call => {
                Ok(value.value)
            }
            Origin::Handle { .. } => Err(HostFault::new(
                "JsValue handle used outside the call that produced it",
            )),
        }
    }

    fn instance(&self, id: ObjectId) -> JsResult<&crate::runtime::HostInstance> {
        self.vm
            .host_instances
            .get(&id)
            .ok_or_else(|| JsError::Fault(HostFault::new("value is not a host instance")))
    }

    pub(crate) fn slot_value(&self, id: ObjectId, index: usize) -> JsResult<JsValue> {
        let instance = self.instance(id)?;
        match instance.slots.get(index) {
            Some(crate::runtime::HostSlot::Js(value)) => Ok(self.js_value(value.clone())),
            Some(crate::runtime::HostSlot::JsList(_)) => {
                Err(JsError::Fault(HostFault::new("slot is a JavaScript list")))
            }
            None => Err(JsError::Fault(HostFault::new("slot index out of range"))),
        }
    }

    pub(crate) fn set_slot_value(
        &mut self,
        id: ObjectId,
        index: usize,
        value: JsValue,
    ) -> JsResult<()> {
        self.check_value(&value)?;
        let value = value.value;
        let instance = self
            .vm
            .host_instances
            .get_mut(&id)
            .ok_or_else(|| JsError::Fault(HostFault::new("value is not a host instance")))?;
        match instance.slots.get_mut(index) {
            Some(crate::runtime::HostSlot::Js(slot)) => {
                *slot = value;
                Ok(())
            }
            Some(crate::runtime::HostSlot::JsList(_)) => {
                Err(JsError::Fault(HostFault::new("slot is a JavaScript list")))
            }
            None => Err(JsError::Fault(HostFault::new("slot index out of range"))),
        }
    }

    pub(crate) fn list_len_value(&self, id: ObjectId, index: usize) -> JsResult<usize> {
        let instance = self.instance(id)?;
        match instance.slots.get(index) {
            Some(crate::runtime::HostSlot::JsList(values)) => Ok(values.len()),
            Some(crate::runtime::HostSlot::Js(_)) => Err(JsError::Fault(HostFault::new(
                "slot is a single JavaScript value",
            ))),
            None => Err(JsError::Fault(HostFault::new("slot index out of range"))),
        }
    }

    pub(crate) fn list_get_value(
        &self,
        id: ObjectId,
        index: usize,
        position: usize,
    ) -> JsResult<JsValue> {
        let instance = self.instance(id)?;
        match instance.slots.get(index) {
            Some(crate::runtime::HostSlot::JsList(values)) => values
                .get(position)
                .cloned()
                .map(|value| self.js_value(value))
                .ok_or_else(|| JsError::Fault(HostFault::new("list index out of range"))),
            Some(crate::runtime::HostSlot::Js(_)) => Err(JsError::Fault(HostFault::new(
                "slot is a single JavaScript value",
            ))),
            None => Err(JsError::Fault(HostFault::new("slot index out of range"))),
        }
    }

    pub(crate) fn list_push_value(
        &mut self,
        id: ObjectId,
        index: usize,
        value: JsValue,
    ) -> JsResult<()> {
        self.check_value(&value)?;
        let value = value.value;
        let instance = self
            .vm
            .host_instances
            .get_mut(&id)
            .ok_or_else(|| JsError::Fault(HostFault::new("value is not a host instance")))?;
        match instance.slots.get_mut(index) {
            Some(crate::runtime::HostSlot::JsList(values)) => {
                values.push(value);
                Ok(())
            }
            Some(crate::runtime::HostSlot::Js(_)) => Err(JsError::Fault(HostFault::new(
                "slot is a single JavaScript value",
            ))),
            None => Err(JsError::Fault(HostFault::new("slot index out of range"))),
        }
    }

    pub(crate) fn list_clear_value(&mut self, id: ObjectId, index: usize) -> JsResult<()> {
        let instance = self
            .vm
            .host_instances
            .get_mut(&id)
            .ok_or_else(|| JsError::Fault(HostFault::new("value is not a host instance")))?;
        match instance.slots.get_mut(index) {
            Some(crate::runtime::HostSlot::JsList(values)) => {
                values.clear();
                Ok(())
            }
            Some(crate::runtime::HostSlot::Js(_)) => Err(JsError::Fault(HostFault::new(
                "slot is a single JavaScript value",
            ))),
            None => Err(JsError::Fault(HostFault::new("slot index out of range"))),
        }
    }

    pub(crate) fn payload_value<T: HostClass>(&self, id: ObjectId) -> JsResult<&T> {
        let class = self.bindings.class_by_tag(T::TAG).ok_or_else(|| {
            JsError::Fault(HostFault::new(format!("unregistered class {}", T::TAG)))
        })?;
        let instance = self.instance(id)?;
        if instance.class != class {
            return Err(JsError::Fault(HostFault::new(format!(
                "instance is not a {}",
                T::TAG
            ))));
        }
        instance
            .payload
            .as_ref()
            .and_then(|payload| payload.downcast_ref::<T>())
            .ok_or_else(|| JsError::Fault(HostFault::new(format!("{} has no payload", T::TAG))))
    }
}

// ---------------------------------------------------------------------------
// Read-only inspection

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InspectKind {
    Undefined,
    Null,
    Bool,
    Number,
    String,
    Symbol,
    Object,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum InspectKey {
    String(JsString),
    Symbol(SymbolId),
}

#[derive(Debug, Clone)]
pub enum InspectValue {
    Data(JsValue),
    Accessor,
}

/// A borrowed, read-only view over one script value. It never invokes getters
/// or proxy traps and cannot write script-visible state.
pub struct Inspect<'a> {
    vm: &'a Vm,
    bindings: &'a Bindings,
    value: JsValue,
    epoch: u32,
    call: u64,
}

impl<'a> Inspect<'a> {
    pub fn kind(&self) -> InspectKind {
        match self.value.value {
            Value::Undefined => InspectKind::Undefined,
            Value::Null => InspectKind::Null,
            Value::Bool(_) => InspectKind::Bool,
            Value::Number(_) => InspectKind::Number,
            Value::String(_) => InspectKind::String,
            Value::Symbol(_) => InspectKind::Symbol,
            Value::Object(_) => InspectKind::Object,
        }
    }

    pub fn value(&self) -> JsValue {
        self.value.clone()
    }

    pub fn identity(&self) -> Option<u64> {
        match self.value.value {
            Value::Object(id) | Value::Symbol(id) => Some(u64::from(id)),
            _ => None,
        }
    }

    pub fn array_length(&self) -> Option<usize> {
        let Value::Object(id) = self.value.value else {
            return None;
        };
        match self.vm.heap.get(id).ok()? {
            Object::Array(array) => Some(array.length),
            _ => None,
        }
    }

    pub fn own_keys(&self) -> Vec<InspectKey> {
        let Value::Object(id) = self.value.value else {
            return Vec::new();
        };
        let Ok(object) = self.vm.heap.get(id) else {
            return Vec::new();
        };
        let mut keys = Vec::new();
        for key in object.own_keys(false) {
            let Some(property) = object.own_property(key.borrowed()) else {
                continue;
            };
            if !property.attributes().enumerable {
                continue;
            }
            keys.push(match key {
                PropertyKey::String(key) => InspectKey::String(key),
                PropertyKey::Symbol(key) => InspectKey::Symbol(key),
            });
        }
        keys.sort();
        keys
    }

    pub fn get(&self, key: &InspectKey) -> Option<InspectValue> {
        let Value::Object(id) = self.value.value else {
            return None;
        };
        let key = match key {
            InspectKey::String(key) => PropertyKey::String(key.clone()),
            InspectKey::Symbol(key) => PropertyKey::Symbol(*key),
        };
        let object = self.vm.heap.get(id).ok()?;
        match object.own_property(key.borrowed()) {
            Some(property) => match property.into_owned() {
                Property::Data(value) => Some(InspectValue::Data(JsValue::handle(
                    value, self.epoch, self.call,
                ))),
                Property::Accessor { .. } => Some(InspectValue::Accessor),
            },
            None => None,
        }
    }

    pub fn bindings(&self) -> &'a Bindings {
        self.bindings
    }
}

impl InspectValue {
    pub fn data(&self) -> Option<JsValue> {
        match self {
            Self::Data(value) => Some(value.clone()),
            Self::Accessor => None,
        }
    }
}

/// Marker used by [`Bindings::classes`]; kept public so diagnostics can print
/// the declaration order deterministically.
pub const _CLASS_ORDER_HINT: () = ();

/// Attributes for prototype methods: writable and configurable, not enumerable.
pub(crate) const PROTOTYPE_METHOD_ATTRIBUTES: Attributes = Attributes::METHOD;
