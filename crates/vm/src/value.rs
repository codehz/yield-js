use serde::{Deserialize, Serialize};

use std::{cmp::Ordering, fmt};

/// A JavaScript string, including any unpaired UTF-16 surrogate code units.
#[derive(Debug, Default, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct JsString(Vec<u16>);

impl JsString {
    pub const fn new() -> Self {
        Self(Vec::new())
    }

    pub fn from_units(units: Vec<u16>) -> Self {
        Self(units)
    }

    pub fn as_units(&self) -> &[u16] {
        &self.0
    }

    pub fn to_utf8(&self) -> Result<String, std::string::FromUtf16Error> {
        String::from_utf16(&self.0)
    }

    /// The length in bytes of the equivalent WTF-8 encoding.
    pub fn len(&self) -> usize {
        char::decode_utf16(self.0.iter().copied())
            .map(|ch| ch.map_or(3, char::len_utf8))
            .sum()
    }

    pub fn unit_len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn push_str(&mut self, text: &Self) {
        self.0.extend_from_slice(text.as_units());
    }

    pub fn push(&mut self, ch: char) {
        self.0.extend_from_slice(ch.encode_utf16(&mut [0; 2]));
    }
}

impl std::borrow::Borrow<[u16]> for JsString {
    fn borrow(&self) -> &[u16] {
        self.as_units()
    }
}

impl From<&str> for JsString {
    fn from(text: &str) -> Self {
        Self(text.encode_utf16().collect())
    }
}

impl From<String> for JsString {
    fn from(text: String) -> Self {
        Self::from(text.as_str())
    }
}

impl From<&[u16]> for JsString {
    fn from(units: &[u16]) -> Self {
        Self(units.to_vec())
    }
}

impl PartialEq<str> for JsString {
    fn eq(&self, other: &str) -> bool {
        self.0.iter().copied().eq(other.encode_utf16())
    }
}

impl PartialEq<&str> for JsString {
    fn eq(&self, other: &&str) -> bool {
        self == *other
    }
}

// Display is for diagnostics only; script strings must not pass through it.
impl fmt::Display for JsString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use fmt::Write;
        for ch in char::decode_utf16(self.0.iter().copied()) {
            match ch {
                Ok(ch) => f.write_char(ch)?,
                Err(ch) => write!(f, "\\u{:04x}", ch.unpaired_surrogate())?,
            }
        }
        Ok(())
    }
}

impl Serialize for JsString {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if let Ok(text) = self.to_utf8() {
            serializer.serialize_str(&text)
        } else {
            #[derive(Serialize)]
            struct Units<'a> {
                utf16: &'a [u16],
            }
            Units { utf16: &self.0 }.serialize(serializer)
        }
    }
}

impl<'de> Deserialize<'de> for JsString {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged, deny_unknown_fields)]
        enum Text {
            Utf8(String),
            Units { utf16: Vec<u16> },
        }
        Ok(match Text::deserialize(deserializer)? {
            Text::Utf8(text) => text.into(),
            Text::Units { utf16 } => Self(utf16),
        })
    }
}

/// Heap object identity. Stable across snapshot/restore.
pub type ObjectId = u32;
/// Compile-time identity of one lexical binding.
pub type BindingId = u32;
/// Continuation identity, local to one VM instance and persisted in snapshots.
pub type ContinuationId = u64;

/// Symbol identity. Local to one VM instance and persisted in snapshots.
/// The first three IDs are reserved for `Symbol.iterator`, `Symbol.toPrimitive`, and `Symbol.species`.
pub type SymbolId = u32;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Value {
    Undefined,
    Null,
    Bool(bool),
    Number(#[serde(with = "number_serde")] f64),
    String(JsString),
    Symbol(SymbolId),
    Object(ObjectId),
}

// JSON serializes non-finite floats as null, which cannot deserialize as f64.
// Keep finite numbers as numeric tokens and spell out the three special values.
pub(crate) mod number_serde {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub(crate) fn serialize<S: Serializer>(value: &f64, serializer: S) -> Result<S::Ok, S::Error> {
        if value.is_finite() {
            serializer.serialize_f64(*value)
        } else {
            serializer.serialize_str(if value.is_nan() {
                "NaN"
            } else if value.is_sign_negative() {
                "-Infinity"
            } else {
                "Infinity"
            })
        }
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<f64, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Number {
            Finite(f64),
            Special(String),
        }
        match Number::deserialize(deserializer)? {
            Number::Finite(value) => Ok(value),
            Number::Special(value) => match value.as_str() {
                "NaN" => Ok(f64::NAN),
                "Infinity" => Ok(f64::INFINITY),
                "-Infinity" => Ok(f64::NEG_INFINITY),
                _ => Err(D::Error::custom("invalid special number")),
            },
        }
    }
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
    String(JsString),
    Symbol(SymbolId),
}

#[derive(Clone, Copy)]
pub(crate) enum PropertyKeyRef<'a> {
    String(&'a str),
    JsString(&'a [u16]),
    Symbol(SymbolId),
}

impl PropertyKeyRef<'_> {
    pub(crate) fn to_owned(self) -> PropertyKey {
        match self {
            Self::String(text) => text.into(),
            Self::JsString(units) => PropertyKey::String(units.into()),
            Self::Symbol(id) => PropertyKey::Symbol(id),
        }
    }

    pub(crate) fn is_str(self, text: &str) -> bool {
        self == PropertyKeyRef::String(text)
    }
}

impl Ord for PropertyKeyRef<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Symbol(left), Self::Symbol(right)) => left.cmp(right),
            (Self::Symbol(_), _) => Ordering::Greater,
            (_, Self::Symbol(_)) => Ordering::Less,
            (Self::String(left), Self::String(right)) => {
                left.encode_utf16().cmp(right.encode_utf16())
            }
            (Self::JsString(left), Self::JsString(right)) => left.cmp(right),
            (Self::String(left), Self::JsString(right)) => {
                left.encode_utf16().cmp(right.iter().copied())
            }
            (Self::JsString(left), Self::String(right)) => {
                left.iter().copied().cmp(right.encode_utf16())
            }
        }
    }
}

impl PartialOrd for PropertyKeyRef<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for PropertyKeyRef<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for PropertyKeyRef<'_> {}

impl PropertyKey {
    pub(crate) fn borrowed(&self) -> PropertyKeyRef<'_> {
        match self {
            Self::String(key) => PropertyKeyRef::JsString(key.as_units()),
            Self::Symbol(id) => PropertyKeyRef::Symbol(*id),
        }
    }

    /// The string form, for paths that only apply to string keys.
    pub fn as_string(&self) -> Option<&JsString> {
        match self {
            Self::String(value) => Some(value),
            Self::Symbol(_) => None,
        }
    }
}

impl From<&str> for PropertyKey {
    fn from(key: &str) -> Self {
        Self::String(key.into())
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
    String(JsString),
    Native(NativeKey),
}
