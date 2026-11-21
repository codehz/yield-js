use serde::{Deserialize, Serialize};

use std::{
    borrow::Cow,
    cmp::Ordering,
    fmt,
    hash::{Hash, Hasher},
    mem::{self, ManuallyDrop},
    num::NonZeroU64,
    ops::Range,
    ptr,
    sync::Arc,
};

const UNDEFINED_TAG: u16 = 0xfff8;
const NULL_TAG: u16 = 0xfff9;
const BOOL_TAG: u16 = 0xfffa;
const SYMBOL_TAG: u16 = 0xfffb;
const OBJECT_TAG: u16 = 0xfffc;
const EMPTY_STRING_TAG: u16 = 0xfffd;
const LATIN1_TAG: u16 = 0xfffe;
const WIDE_TAG: u16 = 0xffff;
const PAYLOAD_MASK: u64 = 0x0000_ffff_ffff_ffff;

// Invert the logical bits so zero is available for Option's niche, not +0.
// Only the private encoders may create words; none produces all-one bits.
#[repr(transparent)]
#[derive(Clone, Copy)]
struct PackedWord(NonZeroU64);

impl PackedWord {
    const fn from_bits(bits: u64) -> Self {
        Self(NonZeroU64::new(!bits).expect("invalid NaN-box word"))
    }

    const fn tagged(tag: u16, payload: u64) -> Self {
        Self::from_bits((tag as u64) << 48 | payload)
    }

    fn number(number: f64) -> Self {
        Self::from_bits(if number.is_nan() {
            0x7ff8_0000_0000_0000
        } else {
            number.to_bits()
        })
    }

    const fn bits(self) -> u64 {
        !self.0.get()
    }

    const fn tag(self) -> u16 {
        (self.bits() >> 48) as u16
    }

    const fn payload(self) -> u64 {
        self.bits() & PAYLOAD_MASK
    }
}

const _: () = {
    assert!(size_of::<PackedWord>() == 8);
    assert!(size_of::<Option<PackedWord>>() == 8);
};

enum StringStorage {
    Empty,
    Latin1(Arc<Vec<u8>>),
    Wide(Arc<Vec<u16>>),
}

#[derive(Debug)]
enum StringStorageRef<'a> {
    Empty,
    Latin1(&'a Vec<u8>),
    Wide(&'a Vec<u16>),
}

impl PackedWord {
    fn from_arc<T>(tag: u16, data: Arc<Vec<T>>) -> Self {
        let address = Arc::as_ptr(&data).expose_provenance() as u64;
        assert!(
            address <= PAYLOAD_MASK,
            "string pointer exceeds 48-bit NaN-box payload"
        );
        // Vec alignment rules out the all-one payload for the wide tag.
        let word = Self::tagged(tag, address);
        let _ = Arc::into_raw(data);
        word
    }

    // The tag determines T. The payload is the checked, exposed address
    // of the Vec returned by Arc::into_raw, not its elements or header.
    fn string_ptr<T>(self) -> *const Vec<T> {
        ptr::with_exposed_provenance(self.payload() as usize)
    }

    fn clone_owner(self) -> Self {
        // SAFETY: The tag selects the original Vec type and exposed address;
        // the borrowing caller keeps a strong reference alive for the increment.
        unsafe {
            match self.tag() {
                LATIN1_TAG => Arc::increment_strong_count(self.string_ptr::<u8>()),
                WIDE_TAG => Arc::increment_strong_count(self.string_ptr::<u16>()),
                _ => (),
            }
        }
        self
    }

    fn drop_owner(self) {
        // SAFETY: The tag selects the original Vec type and exposed address.
        // The dropping wrapper owns one live strong reference, consumed here.
        unsafe {
            match self.tag() {
                LATIN1_TAG => Arc::decrement_strong_count(self.string_ptr::<u8>()),
                WIDE_TAG => Arc::decrement_strong_count(self.string_ptr::<u16>()),
                _ => (),
            }
        }
    }
}

/// A JavaScript string, including any unpaired UTF-16 surrogate code units.
#[repr(transparent)]
pub struct JsString(PackedWord);

const _: () = assert!(size_of::<JsString>() == 8);

impl Clone for JsString {
    fn clone(&self) -> Self {
        Self(self.0.clone_owner())
    }
}

impl Drop for JsString {
    fn drop(&mut self) {
        self.0.drop_owner();
    }
}

impl fmt::Debug for JsString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("JsString").field(&self.storage()).finish()
    }
}

impl JsString {
    fn storage(&self) -> StringStorageRef<'_> {
        // SAFETY: The tag selects the original Vec type and exposed address;
        // self's strong reference keeps it alive throughout the borrow.
        unsafe {
            match self.0.tag() {
                EMPTY_STRING_TAG => StringStorageRef::Empty,
                LATIN1_TAG => StringStorageRef::Latin1(&*self.0.string_ptr::<u8>()),
                WIDE_TAG => StringStorageRef::Wide(&*self.0.string_ptr::<u16>()),
                _ => unreachable!(),
            }
        }
    }

    fn from_storage(storage: StringStorage) -> Self {
        match storage {
            StringStorage::Empty => Self::new(),
            StringStorage::Latin1(data) => Self(PackedWord::from_arc(LATIN1_TAG, data)),
            StringStorage::Wide(data) => Self(PackedWord::from_arc(WIDE_TAG, data)),
        }
    }

    fn into_storage(self) -> StringStorage {
        let word = ManuallyDrop::new(self).0;
        // SAFETY: The tag selects the original Vec type and exposed address
        // from Arc::into_raw. Transfer the live strong reference from self
        // (whose destructor is disabled) to the returned Arc.
        unsafe {
            match word.tag() {
                EMPTY_STRING_TAG => StringStorage::Empty,
                LATIN1_TAG => StringStorage::Latin1(Arc::from_raw(word.string_ptr::<u8>())),
                WIDE_TAG => StringStorage::Wide(Arc::from_raw(word.string_ptr::<u16>())),
                _ => unreachable!(),
            }
        }
    }
}

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
        Self(PackedWord::tagged(EMPTY_STRING_TAG, 0))
    }

    pub fn from_units(units: Vec<u16>) -> Self {
        if units.is_empty() {
            return Self::new();
        }
        if units.iter().all(|&unit| unit <= 0xff) {
            Self::from_latin1(units.into_iter().map(|u| u as u8).collect())
        } else {
            Self::from_wide(units)
        }
    }
    pub(crate) fn is_latin1(&self) -> bool {
        self.0.tag() != WIDE_TAG
    }

    pub(crate) fn latin1_bytes(&self) -> Option<&[u8]> {
        match self.storage() {
            StringStorageRef::Empty => Some(&[]),
            StringStorageRef::Latin1(bytes) => Some(bytes),
            StringStorageRef::Wide(_) => None,
        }
    }

    pub(crate) fn from_latin1(bytes: Vec<u8>) -> Self {
        if bytes.is_empty() {
            Self::new()
        } else {
            Self::from_storage(StringStorage::Latin1(Arc::new(bytes)))
        }
    }

    pub(crate) fn from_wide(units: Vec<u16>) -> Self {
        if units.is_empty() {
            Self::new()
        } else {
            Self::from_storage(StringStorage::Wide(Arc::new(units)))
        }
    }

    pub fn units(&self) -> JsStringUnits<'_> {
        JsStringUnits(match self.storage() {
            StringStorageRef::Empty => UnitsInner::Empty,
            StringStorageRef::Latin1(bytes) => UnitsInner::Latin1(bytes.iter().copied()),
            StringStorageRef::Wide(units) => UnitsInner::Wide(units.iter().copied()),
        })
    }

    pub fn unit_at(&self, index: usize) -> Option<u16> {
        match self.storage() {
            StringStorageRef::Empty => None,
            StringStorageRef::Latin1(bytes) => bytes.get(index).copied().map(u16::from),
            StringStorageRef::Wide(units) => units.get(index).copied(),
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
        match self.storage() {
            StringStorageRef::Empty => Self::new(),
            StringStorageRef::Latin1(bytes) => Self::from_latin1(bytes[range].to_vec()),
            StringStorageRef::Wide(units) => Self::from_units(units[range].to_vec()),
        }
    }

    pub fn to_utf16(&self) -> Cow<'_, [u16]> {
        match self.storage() {
            StringStorageRef::Empty => Cow::Borrowed(&[]),
            StringStorageRef::Wide(units) => Cow::Borrowed(units),
            StringStorageRef::Latin1(bytes) => {
                Cow::Owned(bytes.iter().map(|&b| u16::from(b)).collect())
            }
        }
    }

    pub fn to_utf8(&self) -> Result<String, std::string::FromUtf16Error> {
        match self.storage() {
            StringStorageRef::Empty => Ok(String::new()),
            StringStorageRef::Latin1(bytes) => {
                Ok(bytes.iter().map(|&byte| char::from(byte)).collect())
            }
            StringStorageRef::Wide(units) => String::from_utf16(units),
        }
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
        self.0.tag() == EMPTY_STRING_TAG
    }

    /// Search for a substring by UTF-16 code units, including lone surrogates.
    pub fn contains(&self, needle: &Self) -> bool {
        crate::builtin::string::find_from(self, needle, 0).is_some()
    }

    pub fn push_str(&mut self, text: &Self) {
        if text.is_empty() {
            return;
        }
        if self.is_empty() {
            *self = text.clone();
            return;
        }
        let mut storage = mem::take(self).into_storage();
        match (&mut storage, text.storage()) {
            (StringStorage::Latin1(left), StringStorageRef::Latin1(right)) => {
                Arc::make_mut(left).extend_from_slice(right)
            }
            (StringStorage::Wide(left), StringStorageRef::Latin1(right)) => {
                Arc::make_mut(left).extend(right.iter().map(|&b| u16::from(b)))
            }
            (StringStorage::Wide(left), StringStorageRef::Wide(right)) => {
                Arc::make_mut(left).extend_from_slice(right)
            }
            (StringStorage::Latin1(left), StringStorageRef::Wide(right)) => {
                let mut units = Vec::with_capacity(left.len() + right.len());
                units.extend(left.iter().map(|&b| u16::from(b)));
                units.extend_from_slice(right);
                storage = StringStorage::Wide(Arc::new(units));
            }
            (StringStorage::Empty, _) => unreachable!(),
            (_, StringStorageRef::Empty) => unreachable!(),
        }
        *self = Self::from_storage(storage);
    }

    pub fn push(&mut self, ch: char) {
        if ch as u32 <= 0xff {
            let byte = ch as u8;
            let mut storage = mem::take(self).into_storage();
            match &mut storage {
                StringStorage::Empty => storage = StringStorage::Latin1(Arc::new(vec![byte])),
                StringStorage::Latin1(bytes) => Arc::make_mut(bytes).push(byte),
                StringStorage::Wide(units) => Arc::make_mut(units).push(u16::from(byte)),
            }
            *self = Self::from_storage(storage);
        } else {
            let mut encoded = [0; 2];
            self.push_units(ch.encode_utf16(&mut encoded));
        }
    }

    fn push_units(&mut self, units: &[u16]) {
        if self.is_empty() {
            *self = Self::from_units(units.to_vec());
            return;
        }
        let mut storage = mem::take(self).into_storage();
        match &mut storage {
            StringStorage::Empty => unreachable!(),
            StringStorage::Latin1(bytes) if units.iter().all(|&unit| unit <= 0xff) => {
                Arc::make_mut(bytes).extend(units.iter().map(|&u| u as u8))
            }
            StringStorage::Latin1(bytes) => {
                let mut wide = Vec::with_capacity(bytes.len() + units.len());
                wide.extend(bytes.iter().map(|&b| u16::from(b)));
                wide.extend_from_slice(units);
                storage = StringStorage::Wide(Arc::new(wide));
            }
            StringStorage::Wide(wide) => Arc::make_mut(wide).extend_from_slice(units),
        }
        *self = Self::from_storage(storage);
    }
}

impl From<&str> for JsString {
    fn from(text: &str) -> Self {
        if text.is_empty() {
            return Self::new();
        }
        if text.is_ascii() {
            return Self::from_latin1(text.as_bytes().to_vec());
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
                return Self::from_wide(text.encode_utf16().collect());
            }
        }
        Self::from_latin1(latin1)
    }
}
impl From<String> for JsString {
    fn from(text: String) -> Self {
        if text.is_empty() {
            return Self::new();
        }
        if text.is_ascii() {
            return Self::from_latin1(text.into_bytes());
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

/// An owned JavaScript value. Inspect its logical type with [`Value::view`].
#[repr(transparent)]
pub struct Value(PackedWord);

const _: () = {
    assert!(std::mem::size_of::<Value>() == 8);
    assert!(std::mem::size_of::<Option<Value>>() == 8);
};

/// The logical type and payload of a borrowed JavaScript value.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename = "Value", rename_all = "snake_case")]
pub enum ValueRef<'a> {
    Undefined,
    Null,
    Bool(bool),
    Number(#[serde(with = "number_serde")] f64),
    String(&'a JsString),
    Symbol(SymbolId),
    Object(ObjectId),
}

impl Clone for Value {
    fn clone(&self) -> Self {
        Self(self.0.clone_owner())
    }
}

impl Drop for Value {
    fn drop(&mut self) {
        self.0.drop_owner();
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.view().fmt(f)
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        self.view() == other.view()
    }
}

impl Serialize for Value {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.view().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Value {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename = "Value", rename_all = "snake_case")]
        enum WireValue {
            Undefined,
            Null,
            Bool(bool),
            Number(#[serde(with = "number_serde")] f64),
            String(JsString),
            Symbol(SymbolId),
            Object(ObjectId),
        }

        Ok(match WireValue::deserialize(deserializer)? {
            WireValue::Undefined => Self::UNDEFINED,
            WireValue::Null => Self::NULL,
            WireValue::Bool(value) => Self::boolean(value),
            WireValue::Number(value) => Self::number(value),
            WireValue::String(value) => Self::string(value),
            WireValue::Symbol(value) => Self::symbol(value),
            WireValue::Object(value) => Self::object(value),
        })
    }
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
    pub const UNDEFINED: Self = Self(PackedWord::tagged(UNDEFINED_TAG, 0));
    pub const NULL: Self = Self(PackedWord::tagged(NULL_TAG, 0));

    pub const fn boolean(value: bool) -> Self {
        Self(PackedWord::tagged(BOOL_TAG, value as u64))
    }

    pub fn number(value: f64) -> Self {
        Self(PackedWord::number(value))
    }

    pub fn string(value: JsString) -> Self {
        Self(ManuallyDrop::new(value).0)
    }

    pub const fn symbol(value: SymbolId) -> Self {
        Self(PackedWord::tagged(SYMBOL_TAG, value as u64))
    }

    pub const fn object(value: ObjectId) -> Self {
        Self(PackedWord::tagged(OBJECT_TAG, value as u64))
    }

    /// Inspect the logical value without taking ownership or cloning a string.
    pub fn view(&self) -> ValueRef<'_> {
        match self.0.tag() {
            UNDEFINED_TAG => ValueRef::Undefined,
            NULL_TAG => ValueRef::Null,
            BOOL_TAG => ValueRef::Bool(self.0.payload() != 0),
            SYMBOL_TAG => ValueRef::Symbol(self.0.payload() as SymbolId),
            OBJECT_TAG => ValueRef::Object(self.0.payload() as ObjectId),
            EMPTY_STRING_TAG | LATIN1_TAG | WIDE_TAG => {
                // SAFETY: Both wrappers are transparent over PackedWord. The tag
                // guarantees a valid JsString word. This borrow has self's lifetime,
                // during which self retains ownership of the string's strong reference.
                ValueRef::String(unsafe { &*ptr::from_ref(self).cast::<JsString>() })
            }
            _ => ValueRef::Number(f64::from_bits(self.0.bits())),
        }
    }

    /// Transfer the string payload if this value is a string.
    pub fn into_string(self) -> Option<JsString> {
        match self.0.tag() {
            EMPTY_STRING_TAG | LATIN1_TAG | WIDE_TAG => Some(JsString(ManuallyDrop::new(self).0)),
            _ => None,
        }
    }

    pub fn truthy(&self) -> bool {
        match self.view() {
            ValueRef::Undefined | ValueRef::Null => false,
            ValueRef::Bool(value) => value,
            ValueRef::Number(value) => value != 0.0 && !value.is_nan(),
            ValueRef::String(value) => !value.is_empty(),
            ValueRef::Symbol(_) | ValueRef::Object(_) => true,
        }
    }

    pub fn type_of(&self) -> &'static str {
        match self.view() {
            ValueRef::Undefined => "undefined",
            ValueRef::Null | ValueRef::Object(_) => "object",
            ValueRef::Bool(_) => "boolean",
            ValueRef::Number(_) => "number",
            ValueRef::String(_) => "string",
            ValueRef::Symbol(_) => "symbol",
        }
    }
}

/// A property key after value-to-key conversion. Strings keep the original
/// conversion; symbols are their own identity and never coerce to a string.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
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
