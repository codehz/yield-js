//! Static host bindings: modules, classes, values, and recoverable operations.
//!
//! Declarations are immutable and can be shared between VMs. A VM owns every
//! prototype, instance, module state, and operation record derived from them.
//! Host code never receives a heap id it can use outside the current call: a
//! [`JsValue`] carries the calling VM generation and call id, and slot writes
//! are the only way to retain a value across calls.

use std::any::{Any, TypeId};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::call::{CallCx, JsValue};
use crate::value::{JsString, PropertyKey, SymbolId, Value};

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

pub type HostCall = fn(&mut CallCx<'_>) -> JsResult<HostStep>;

/// The result of one synchronous host binding step.
#[derive(Debug)]
pub enum HostStep {
    Return(JsValue),
    Suspend(SuspendRequest),
}

/// A host effect's completion, rewrapped for the resume context.
#[derive(Debug)]
pub enum GuestOutcome {
    Returned(JsValue),
    Thrown(JsValue),
}

pub(crate) type GuestResume = fn(&mut CallCx<'_>, GuestOutcome) -> JsResult<HostStep>;

/// One linked continuation entry and its portable saved-slot layout.
#[doc(hidden)]
#[derive(Debug, Clone, Copy)]
pub struct CpsStage {
    pub resume: GuestResume,
    pub kind: CpsBoundary,
    pub slots: &'static [u32],
    pub receiver: Option<&'static str>,
}

/// The effect whose completion enters a linked continuation.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpsBoundary {
    Call,
    Gc,
    External,
    Spawn,
}

#[derive(Debug)]
pub(crate) enum Action {
    Call {
        callee: Value,
        receiver: Value,
        args: Vec<Value>,
    },
    Gc,
    External {
        decl: usize,
        state: Vec<u8>,
    },
    /// Enter a new task immediately using its linked entry and saved slots.
    Spawn,
}

#[derive(Debug)]
pub(crate) enum Resume {
    Inline(GuestResume),
    Linked(u32),
}

/// An opaque request valid only as a result of the step that created it.
#[derive(Debug)]
pub struct SuspendRequest {
    epoch: u32,
    call: u64,
    action: Action,
    saved: Vec<Value>,
    resume: Resume,
}

impl SuspendRequest {
    pub(crate) fn new(
        epoch: u32,
        call: u64,
        action: Action,
        saved: Vec<Value>,
        resume: Resume,
    ) -> Self {
        Self {
            epoch,
            call,
            action,
            saved,
            resume,
        }
    }

    pub(crate) fn into_parts(
        self,
        cx: CallCx<'_>,
    ) -> Result<(Action, Vec<Value>, Resume), HostFault> {
        if self.epoch != cx.epoch || self.call != cx.call {
            return Err(HostFault::new(
                "SuspendRequest used outside the call that produced it",
            ));
        }
        Ok((self.action, self.saved, self.resume))
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ArgKind {
    Value,
    OptionalValue,
}

#[derive(Debug, Clone, Copy)]
pub enum ReturnKind {
    Value,
    Void,
    Instance(&'static str),
    Step,
}

#[derive(Debug, Clone, Copy)]
pub struct CallSignature {
    pub args: &'static [ArgKind],
    pub result: ReturnKind,
}

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
    signature: CallSignature,
    cps: Option<&'static [CpsStage]>,
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
    HasInstance,
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
            Self::HasInstance => "Symbol.hasInstance",
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
            Self::HasInstance => crate::heap::HAS_INSTANCE_SYMBOL,
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
    signature: CallSignature,
    cps: Option<&'static [CpsStage]>,
}

#[derive(Clone)]
pub struct AccessorDecl {
    pub name: MemberName,
    pub get: Option<AccessorCall>,
    pub set: Option<AccessorCall>,
}

#[derive(Clone, Copy)]
pub struct AccessorCall {
    pub call: HostCall,
    signature: CallSignature,
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

/// Owns the platform registration for an operation. Drivers must not re-enter
/// the VM; on failure they must roll back any platform side effects.
pub trait OperationDriver: Operation {
    type Live: Registration<Self>;

    fn start(&self, token: OperationToken, bindings: &Bindings) -> Result<Self::Live, HostFault>;

    fn resume(&self, token: OperationToken, bindings: &Bindings) -> Result<Self::Live, HostFault> {
        self.start(token, bindings)
    }
}

/// One live platform registration. Drop releases it. A registration must
/// not hold the VM or guest values.
pub trait Registration<O: Operation>: 'static {
    /// Read state at the capture cut without changing the live registration.
    fn checkpoint(&self) -> Result<Option<O>, HostFault> {
        Ok(None)
    }
}

pub(crate) trait LiveRegistration {
    fn checkpoint(&self, original: &[u8], out: &mut Vec<u8>) -> Result<(), HostFault>;
}

pub(crate) struct TypedRegistration<O: OperationDriver>(pub(crate) O::Live);

impl<O: OperationDriver> LiveRegistration for TypedRegistration<O> {
    fn checkpoint(&self, original: &[u8], out: &mut Vec<u8>) -> Result<(), HostFault> {
        if let Some(state) = self.0.checkpoint()? {
            state.encode(out).map_err(|error| HostFault(error.0))?;
        } else {
            out.extend_from_slice(original);
        }
        Ok(())
    }
}

type ArmOperation =
    fn(&[u8], OperationToken, &Bindings) -> Result<Box<dyn LiveRegistration>, HostFault>;

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
    rust_type: TypeId,
    start: ArmOperation,
    resume: ArmOperation,
}

#[doc(hidden)]
pub struct ModuleDecl {
    name: String,
    exports: Vec<ExportDecl>,
}

#[doc(hidden)]
pub mod __macro_support {
    use super::{AccessorCall, UnlinkedBindings};
    pub use super::{
        AccessorDecl, ArgKind, CallSignature, CpsBoundary, CpsStage, ExportDecl, MemberName,
        MethodDecl, ModuleDecl, ReturnKind,
    };
    use super::{
        Bindings, ClassDecl, HostClass, HostModuleState, OperationDecl, OperationDriver, StateDecl,
    };
    use super::{ConstValue, ConstantDecl, FunctionDecl, HostCall};
    use std::any::Any;
    use std::collections::btree_map::Entry;
    use std::sync::Arc;

    pub struct Class(ClassDecl);
    pub struct State(StateDecl);
    pub struct OperationType(OperationDecl);
    pub struct Service(&'static str, u32, Arc<dyn Any + Send + Sync>);

    pub fn class<T: HostClass>() -> Class {
        Class(ClassDecl::of::<T>())
    }

    pub fn state<T: HostModuleState>() -> State {
        State(StateDecl::of::<T>())
    }

    pub fn operation<T: OperationDriver>() -> OperationType {
        OperationType(OperationDecl::of::<T>())
    }

    pub fn service(id: &'static str, version: u32, value: impl Any + Send + Sync) -> Service {
        Service(id, version, Arc::new(value))
    }

    pub fn link(
        modules: impl IntoIterator<Item = ModuleDecl>,
        classes: impl IntoIterator<Item = Class>,
        states: impl IntoIterator<Item = State>,
        operations: impl IntoIterator<Item = OperationType>,
        services: impl IntoIterator<Item = Service>,
    ) -> Result<Bindings, String> {
        let mut table = UnlinkedBindings {
            modules: modules.into_iter().collect(),
            classes: classes.into_iter().map(|class| class.0).collect(),
            states: states.into_iter().map(|state| state.0).collect(),
            operations: operations
                .into_iter()
                .map(|operation| operation.0)
                .collect(),
            services: Default::default(),
            service_ids: Default::default(),
        };
        for Service(id, version, value) in services {
            match table.services.values.entry(id.to_owned()) {
                Entry::Vacant(entry) => {
                    entry.insert(value);
                }
                Entry::Occupied(_) => return Err(format!("duplicate host service {id:?}")),
            }
            table.service_ids.insert(format!("{id}:{version}"));
        }
        table.build()
    }

    pub fn module(name: &str, exports: Vec<ExportDecl>) -> ModuleDecl {
        ModuleDecl {
            name: name.to_owned(),
            exports,
        }
    }

    pub fn function(
        name: &str,
        constructable: bool,
        class: Option<&'static str>,
        call: HostCall,
        signature: CallSignature,
    ) -> ExportDecl {
        ExportDecl::Function(FunctionDecl {
            name: name.to_owned(),
            arity: signature.args.len(),
            constructable,
            class,
            call,
            signature,
            cps: None,
        })
    }

    pub fn function_cps(
        name: &str,
        constructable: bool,
        class: Option<&'static str>,
        call: HostCall,
        signature: CallSignature,
        stages: &'static [CpsStage],
    ) -> ExportDecl {
        ExportDecl::Function(FunctionDecl {
            name: name.to_owned(),
            arity: signature.args.len(),
            constructable,
            class,
            call,
            signature,
            cps: Some(stages),
        })
    }

    pub fn constant(name: &str, value: impl Into<ConstValue>) -> ExportDecl {
        ExportDecl::Constant(ConstantDecl {
            name: name.to_owned(),
            value: value.into(),
        })
    }

    pub const fn method(name: MemberName, call: HostCall, signature: CallSignature) -> MethodDecl {
        MethodDecl {
            name,
            arity: signature.args.len(),
            call,
            signature,
            cps: None,
        }
    }

    pub const fn method_cps(
        name: MemberName,
        call: HostCall,
        signature: CallSignature,
        stages: &'static [CpsStage],
    ) -> MethodDecl {
        MethodDecl {
            name,
            arity: signature.args.len(),
            call,
            signature,
            cps: Some(stages),
        }
    }

    pub const fn accessor(
        name: MemberName,
        get: Option<(HostCall, CallSignature)>,
        set: Option<(HostCall, CallSignature)>,
    ) -> AccessorDecl {
        const fn entry(value: Option<(HostCall, CallSignature)>) -> Option<AccessorCall> {
            match value {
                Some((call, signature)) => Some(AccessorCall { call, signature }),
                None => None,
            }
        }
        AccessorDecl {
            name,
            get: entry(get),
            set: entry(set),
        }
    }
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

    pub(crate) fn cps(&self, owner: NativeTarget) -> Option<&'static [CpsStage]> {
        match owner {
            NativeTarget::Export(id) => match self.export_decl(id)? {
                ExportDecl::Function(function) if !function.constructable => function.cps,
                _ => None,
            },
            NativeTarget::Method(id) => self.method(id)?.cps,
            NativeTarget::Accessor { .. } => None,
        }
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

    pub(crate) fn operation_index<O: OperationDriver>(&self) -> Result<usize, HostFault> {
        let (index, _, _) = self.operation(O::NAME, O::SCHEMA).ok_or_else(|| {
            HostFault::new(format!(
                "operation {} schema {} is not declared",
                O::NAME,
                O::SCHEMA
            ))
        })?;
        if self.inner.operations[index].rust_type != TypeId::of::<O>() {
            return Err(HostFault::new(format!(
                "operation {} has the wrong Rust type",
                O::NAME
            )));
        }
        Ok(index)
    }

    pub(crate) fn operation_metadata(&self, decl: usize) -> (&str, u32, Recovery, &'static str) {
        let operation = &self.inner.operations[decl];
        (
            &operation.name,
            operation.schema,
            operation.recovery,
            operation.rejection,
        )
    }

    pub(crate) fn start_encoded_operation(
        &self,
        decl: usize,
        bytes: &[u8],
        token: OperationToken,
    ) -> Result<Box<dyn LiveRegistration>, HostFault> {
        (self.inner.operations[decl].start)(bytes, token, self)
    }

    pub(crate) fn resume_operation(
        &self,
        index: usize,
        state: &[u8],
        token: OperationToken,
    ) -> Result<Box<dyn LiveRegistration>, HostFault> {
        (self.inner.operations[index].resume)(state, token, self)
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

    pub fn service<T: Any + Send + Sync>(&self, id: &str) -> Option<&T> {
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

struct UnlinkedBindings {
    modules: Vec<ModuleDecl>,
    classes: Vec<ClassDecl>,
    operations: Vec<OperationDecl>,
    states: Vec<StateDecl>,
    services: Services,
    service_ids: BTreeSet<String>,
}

impl ClassDecl {
    fn of<T: HostClass>() -> Self {
        Self {
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
        }
    }
}

impl StateDecl {
    fn of<S: HostModuleState>() -> Self {
        Self {
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
        }
    }
}

impl OperationDecl {
    fn of<O: OperationDriver>() -> Self {
        Self {
            name: O::NAME.to_owned(),
            schema: O::SCHEMA,
            recovery: O::RECOVERY,
            rejection: O::REJECTION,
            decode: |input, bindings| O::decode(input, bindings).map(|_| ()),
            rust_type: TypeId::of::<O>(),
            start: |bytes, token, bindings| {
                let state = O::decode(bytes, bindings).map_err(|error| HostFault(error.0))?;
                let live = state.start(token, bindings)?;
                Ok(Box::new(TypedRegistration::<O>(live)))
            },
            resume: |bytes, token, bindings| {
                let state = O::decode(bytes, bindings).map_err(|error| HostFault(error.0))?;
                let live = state.resume(token, bindings)?;
                Ok(Box::new(TypedRegistration::<O>(live)))
            },
        }
    }
}

impl UnlinkedBindings {
    fn build(mut self) -> Result<Bindings, String> {
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

fn hash_line(hasher: &mut Sha256, text: String) {
    hasher.update(text.as_bytes());
    hasher.update(b"\n");
}

fn hash_signature(hasher: &mut Sha256, direction: u8, signature: CallSignature) {
    hasher.update([direction]);
    hasher.update((signature.args.len() as u64).to_le_bytes());
    for arg in signature.args {
        hasher.update([match arg {
            ArgKind::Value => 0,
            ArgKind::OptionalValue => 1,
        }]);
    }
    match signature.result {
        ReturnKind::Value => hasher.update([0]),
        ReturnKind::Void => hasher.update([1]),
        ReturnKind::Instance(tag) => {
            hasher.update([2]);
            hasher.update((tag.len() as u64).to_le_bytes());
            hasher.update(tag.as_bytes());
        }
        ReturnKind::Step => hasher.update([3]),
    }
}

fn hash_cps(hasher: &mut Sha256, stages: Option<&[CpsStage]>) {
    let Some(stages) = stages else { return };
    hash_line(hasher, format!("cps stages={}", stages.len()));
    for stage in stages {
        let kind = match stage.kind {
            CpsBoundary::Call => "call",
            CpsBoundary::Gc => "gc",
            CpsBoundary::External => "external",
            CpsBoundary::Spawn => "spawn",
        };
        hash_line(
            hasher,
            format!(
                "cps-stage {kind} slots={:?} receiver={:?}",
                stage.slots, stage.receiver
            ),
        );
    }
}

fn fingerprint(
    modules: &[ModuleDecl],
    classes: &[ClassDecl],
    operations: &[OperationDecl],
    states: &BTreeMap<usize, StateDecl>,
    services: &BTreeSet<String>,
) -> String {
    let mut hasher = Sha256::new();
    for module in modules {
        hash_line(&mut hasher, format!("module {}", module.name));
        for export in &module.exports {
            match export {
                ExportDecl::Function(function) => {
                    hash_line(
                        &mut hasher,
                        format!(
                            "export {} function arity={} constructable={} class={:?}",
                            function.name, function.arity, function.constructable, function.class
                        ),
                    );
                    hash_signature(&mut hasher, 0, function.signature);
                    hash_cps(&mut hasher, function.cps);
                }
                ExportDecl::Constant(constant) => hash_line(
                    &mut hasher,
                    format!(
                        "export {} constant {:?}",
                        constant.name,
                        constant.value.kind()
                    ),
                ),
            }
        }
    }
    for class in classes {
        hash_line(
            &mut hasher,
            format!("class {} parent={:?}", class.tag, class.parent_tag),
        );
        for slot in &class.slots {
            hash_line(&mut hasher, format!("slot {} {:?}", slot.name, slot.kind));
        }
        if let Some(schema) = class.payload_schema {
            hash_line(&mut hasher, format!("payload {schema}"));
        }
        for method in &class.methods {
            hash_line(
                &mut hasher,
                format!("method {} arity={}", method.name, method.arity),
            );
            hash_signature(&mut hasher, 1, method.signature);
            hash_cps(&mut hasher, method.cps);
        }
        for accessor in &class.accessors {
            hash_line(
                &mut hasher,
                format!(
                    "accessor {} get={} set={}",
                    accessor.name,
                    accessor.get.is_some(),
                    accessor.set.is_some()
                ),
            );
            if let Some(get) = accessor.get {
                hash_signature(&mut hasher, 2, get.signature);
            }
            if let Some(set) = accessor.set {
                hash_signature(&mut hasher, 3, set.signature);
            }
        }
    }
    for operation in operations {
        hash_line(
            &mut hasher,
            format!(
                "operation {} schema={} recovery={:?}",
                operation.name, operation.schema, operation.recovery
            ),
        );
    }
    for (module, state) in states {
        hash_line(
            &mut hasher,
            format!(
                "module-state {} schema={}",
                modules[*module].name, state.payload_schema
            ),
        );
        for slot in &state.slots {
            hash_line(
                &mut hasher,
                format!("state-slot {} {:?}", slot.name, slot.kind),
            );
        }
    }
    for service in services {
        hash_line(&mut hasher, format!("service {service}"));
    }
    format!("{:x}", hasher.finalize())
}
