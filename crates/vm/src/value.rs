use serde::{Deserialize, Serialize};

use std::{
    borrow::Cow,
    cmp::Ordering,
    fmt,
    hash::{Hash, Hasher},
    ops::Range,
    sync::Arc,
};

#[derive(Debug, Clone)]
enum StringStorage {
    Empty,
    Latin1(Arc<Vec<u8>>),
    Wide(Arc<Vec<u16>>),
}

/// A JavaScript string, including any unpaired UTF-16 surrogate code units.
#[derive(Debug, Clone)]
pub struct JsString(StringStorage);

#[derive(Clone)]
pub struct JsStringUnits<'a>(UnitsInner<'a>);

#[derive(Clone)]
enum UnitsInner<'a> {
    Empty,
    Latin1(std::iter::Copied<std::slice::Iter<'a, u8>>),
    Wide(std::iter::Copied<std::slice::Iter<'a, u16>>),
}

impl Iterator for JsStringUnits<'_> {
    type Item = u16;
    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.0 {
            UnitsInner::Empty => None,
            UnitsInner::Latin1(iter) => iter.next().map(u16::from),
            UnitsInner::Wide(iter) => iter.next(),
        }
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = self.len();
        (len, Some(len))
    }
}
impl DoubleEndedIterator for JsStringUnits<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        match &mut self.0 {
            UnitsInner::Empty => None,
            UnitsInner::Latin1(iter) => iter.next_back().map(u16::from),
            UnitsInner::Wide(iter) => iter.next_back(),
        }
    }
}
impl ExactSizeIterator for JsStringUnits<'_> {
    fn len(&self) -> usize {
        match &self.0 {
            UnitsInner::Empty => 0,
            UnitsInner::Latin1(iter) => iter.len(),
            UnitsInner::Wide(iter) => iter.len(),
        }
    }
}

impl Default for JsString {
    fn default() -> Self {
        Self::new()
    }
}
impl JsString {
    pub const fn new() -> Self {
        Self(StringStorage::Empty)
    }

    pub fn from_units(units: Vec<u16>) -> Self {
        if units.is_empty() {
            return Self::new();
        }
        if units.iter().all(|&unit| unit <= 0xff) {
            Self(StringStorage::Latin1(Arc::new(
                units.into_iter().map(|u| u as u8).collect(),
            )))
        } else {
            Self(StringStorage::Wide(Arc::new(units)))
        }
    }
    pub(crate) fn is_latin1(&self) -> bool {
        matches!(self.0, StringStorage::Empty | StringStorage::Latin1(_))
    }

    pub(crate) fn from_latin1(bytes: Vec<u8>) -> Self {
        if bytes.is_empty() {
            Self::new()
        } else {
            Self(StringStorage::Latin1(Arc::new(bytes)))
        }
    }

    pub(crate) fn from_wide(units: Vec<u16>) -> Self {
        if units.is_empty() {
            Self::new()
        } else {
            Self(StringStorage::Wide(Arc::new(units)))
        }
    }

    pub fn units(&self) -> JsStringUnits<'_> {
        JsStringUnits(match &self.0 {
            StringStorage::Empty => UnitsInner::Empty,
            StringStorage::Latin1(bytes) => UnitsInner::Latin1(bytes.iter().copied()),
            StringStorage::Wide(units) => UnitsInner::Wide(units.iter().copied()),
        })
    }

    pub fn unit_at(&self, index: usize) -> Option<u16> {
        match &self.0 {
            StringStorage::Empty => None,
            StringStorage::Latin1(bytes) => bytes.get(index).copied().map(u16::from),
            StringStorage::Wide(units) => units.get(index).copied(),
        }
    }

    pub(crate) fn slice_units(&self, range: Range<usize>) -> Self {
        assert!(range.start <= range.end && range.end <= self.unit_len());
        if range.start == 0 && range.end == self.unit_len() {
            return self.clone();
        }
        if range.is_empty() {
            return Self::new();
        }
        match &self.0 {
            StringStorage::Empty => Self::new(),
            StringStorage::Latin1(bytes) => Self::from_latin1(bytes[range].to_vec()),
            StringStorage::Wide(units) => Self::from_units(units[range].to_vec()),
        }
    }

    pub fn to_utf16(&self) -> Cow<'_, [u16]> {
        match &self.0 {
            StringStorage::Empty => Cow::Borrowed(&[]),
            StringStorage::Wide(units) => Cow::Borrowed(units),
            StringStorage::Latin1(bytes) => {
                Cow::Owned(bytes.iter().map(|&b| u16::from(b)).collect())
            }
        }
    }

    pub fn to_utf8(&self) -> Result<String, std::string::FromUtf16Error> {
        String::from_utf16(&self.to_utf16())
    }

    /// The length in bytes of the equivalent WTF-8 encoding.
    pub fn len(&self) -> usize {
        char::decode_utf16(self.units())
            .map(|ch| ch.map_or(3, char::len_utf8))
            .sum()
    }
    pub fn unit_len(&self) -> usize {
        self.units().len()
    }
    pub fn is_empty(&self) -> bool {
        matches!(self.0, StringStorage::Empty)
    }

    pub fn push_str(&mut self, text: &Self) {
        if text.is_empty() {
            return;
        }
        if self.is_empty() {
            *self = text.clone();
            return;
        }
        match (&mut self.0, &text.0) {
            (StringStorage::Latin1(left), StringStorage::Latin1(right)) => {
                Arc::make_mut(left).extend_from_slice(right)
            }
            (StringStorage::Wide(left), StringStorage::Latin1(right)) => {
                Arc::make_mut(left).extend(right.iter().map(|&b| u16::from(b)))
            }
            (StringStorage::Wide(left), StringStorage::Wide(right)) => {
                Arc::make_mut(left).extend_from_slice(right)
            }
            (StringStorage::Latin1(left), StringStorage::Wide(right)) => {
                let mut units = Vec::with_capacity(left.len() + right.len());
                units.extend(left.iter().map(|&b| u16::from(b)));
                units.extend_from_slice(right);
                self.0 = StringStorage::Wide(Arc::new(units));
            }
            (StringStorage::Empty, _) => unreachable!(),
            (_, StringStorage::Empty) => unreachable!(),
        }
    }

    pub fn push(&mut self, ch: char) {
        if ch as u32 <= 0xff {
            let byte = ch as u8;
            match &mut self.0 {
                StringStorage::Empty => self.0 = StringStorage::Latin1(Arc::new(vec![byte])),
                StringStorage::Latin1(bytes) => Arc::make_mut(bytes).push(byte),
                StringStorage::Wide(units) => Arc::make_mut(units).push(u16::from(byte)),
            }
        } else {
            let mut encoded = [0; 2];
            self.push_units(ch.encode_utf16(&mut encoded));
        }
    }

    fn push_units(&mut self, units: &[u16]) {
        match &mut self.0 {
            StringStorage::Empty => self.0 = Self::from_units(units.to_vec()).0,
            StringStorage::Latin1(bytes) if units.iter().all(|&unit| unit <= 0xff) => {
                Arc::make_mut(bytes).extend(units.iter().map(|&u| u as u8))
            }
            StringStorage::Latin1(bytes) => {
                let mut wide = Vec::with_capacity(bytes.len() + units.len());
                wide.extend(bytes.iter().map(|&b| u16::from(b)));
                wide.extend_from_slice(units);
                self.0 = StringStorage::Wide(Arc::new(wide));
            }
            StringStorage::Wide(wide) => Arc::make_mut(wide).extend_from_slice(units),
        }
    }
}

impl From<&str> for JsString {
    fn from(text: &str) -> Self {
        if text.is_empty() {
            return Self::new();
        }
        if text.is_ascii() {
            return Self(StringStorage::Latin1(Arc::new(text.as_bytes().to_vec())));
        }
        let bytes = text.as_bytes();
        let mut latin1 = Vec::with_capacity(bytes.len().div_ceil(2));
        let mut index = 0;
        while index < bytes.len() {
            let first = bytes[index];
            if first < 0x80 {
                latin1.push(first);
                index += 1;
            } else if first < 0xc4 {
                let second = bytes[index + 1];
                latin1.push(((first & 0x1f) << 6) | (second & 0x3f));
                index += 2;
            } else {
                return Self(StringStorage::Wide(Arc::new(text.encode_utf16().collect())));
            }
        }
        Self(StringStorage::Latin1(Arc::new(latin1)))
    }
}
impl From<String> for JsString {
    fn from(text: String) -> Self {
        if text.is_empty() {
            return Self::new();
        }
        if text.is_ascii() {
            return Self(StringStorage::Latin1(Arc::new(text.into_bytes())));
        }
        Self::from(text.as_str())
    }
}
impl From<&[u16]> for JsString {
    fn from(units: &[u16]) -> Self {
        Self::from_units(units.to_vec())
    }
}

impl PartialEq for JsString {
    fn eq(&self, other: &Self) -> bool {
        self.units().eq(other.units())
    }
}
impl Eq for JsString {}
impl PartialOrd for JsString {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for JsString {
    fn cmp(&self, other: &Self) -> Ordering {
        self.units().cmp(other.units())
    }
}
impl Hash for JsString {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.unit_len().hash(state);
        for unit in self.units() {
            unit.hash(state);
        }
    }
}
impl PartialEq<str> for JsString {
    fn eq(&self, other: &str) -> bool {
        self.units().eq(other.encode_utf16())
    }
}
impl PartialEq<&str> for JsString {
    fn eq(&self, other: &&str) -> bool {
        self == *other
    }
}

impl fmt::Display for JsString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use fmt::Write;
        for ch in char::decode_utf16(self.units()) {
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
            return serializer.serialize_str(&text);
        }
        #[derive(Serialize)]
        struct Units<'a> {
            utf16: &'a [u16],
        }
        let units = self.to_utf16();
        Units { utf16: &units }.serialize(serializer)
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
            Text::Units { utf16 } => Self::from_units(utf16),
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
    JsString(&'a JsString),
    Symbol(SymbolId),
}

impl PropertyKeyRef<'_> {
    pub(crate) fn to_owned(self) -> PropertyKey {
        match self {
            Self::String(text) => text.into(),
            Self::JsString(value) => PropertyKey::String(value.clone()),
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
            (Self::String(left), Self::JsString(right)) => left.encode_utf16().cmp(right.units()),
            (Self::JsString(left), Self::String(right)) => left.units().cmp(right.encode_utf16()),
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
            Self::String(key) => PropertyKeyRef::JsString(key),
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
