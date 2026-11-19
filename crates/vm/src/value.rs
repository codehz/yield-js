use serde::{Deserialize, Serialize};

/// Heap object identity. Stable across snapshot/restore.
pub type ObjectId = u32;
/// Compile-time identity of one lexical binding.
pub type BindingId = u32;
/// Continuation identity, local to one VM instance and persisted in snapshots.
pub type ContinuationId = u64;

/// Symbol identity. Local to one VM instance and persisted in snapshots.
/// `0` is the reserved `Symbol.iterator`; ordinary symbols allocate from 1.
pub type SymbolId = u32;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Value {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Symbol(SymbolId),
    Object(ObjectId),
}

impl Value {
    pub fn truthy(&self) -> bool {
        match self {
            Self::Undefined | Self::Null => false,
            Self::Bool(value) => *value,
            Self::Number(value) => *value != 0.0 && !value.is_nan(),
            Self::String(value) => !value.is_empty(),
            Self::Symbol(_) => true,
            Self::Object(_) => true,
        }
    }

    pub fn type_of(&self) -> &'static str {
        match self {
            Self::Undefined => "undefined",
            Self::Null => "object",
            Self::Bool(_) => "boolean",
            Self::Number(_) => "number",
            Self::String(_) => "string",
            Self::Symbol(_) => "symbol",
            Self::Object(_) => "object",
        }
    }
}

/// A property key after value-to-key conversion. Strings keep the original
/// conversion; symbols are their own identity and never coerce to a string.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum PropertyKey {
    String(String),
    Symbol(SymbolId),
}

impl PropertyKey {
    /// The string form, for paths that only apply to string keys.
    pub fn as_string(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            Self::Symbol(_) => None,
        }
    }
}

/// Stable key for a host-owned object wrapper.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct NativeKey {
    #[serde(rename = "type")]
    pub type_name: String,
    pub id: String,
}

impl std::fmt::Display for NativeKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "({}, {})", self.type_name, self.id)
    }
}

/// Values that may cross the host boundary. Non-native VM objects are not
/// exposed: doing so would leak VM heap identities into host state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostValue {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Native(NativeKey),
}
