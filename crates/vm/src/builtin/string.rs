//! UTF-16 string operations.

use icu_normalizer::{ComposingNormalizerBorrowed, DecomposingNormalizerBorrowed};

static UNDEFINED: Value = Value::UNDEFINED;

use crate::JsString;
use crate::builtin::globals::Owner;
use crate::builtin::regexp_runtime::RegExpMethod;
use crate::builtin::{
    ROOT_FUNCTION_PROTOTYPE, ROOT_OBJECT_PROTOTYPE, ROOT_RANGEERROR_PROTOTYPE,
    ROOT_STRING_ITERATOR, ROOT_STRING_ITERATOR_PROTOTYPE, ROOT_STRING_PROTOTYPE, RootKind,
    RootSpec, link_constructor,
};
use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::heap::{
    Attributes, BoxedPrimitive, ITERATOR_SYMBOL, MATCH_ALL_SYMBOL, MATCH_SYMBOL, Object,
    PlainObject, REPLACE_SYMBOL, SEARCH_SYMBOL, SPLIT_SYMBOL,
};
use crate::host::{Host, PlatformServices};
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::runtime::{Arguments, Invocation};
use crate::value::{ObjectId, PropertyKey, PropertyKeyRef, SymbolId, Value, ValueRef};
use crate::vm::VmCore;

const MAX_LENGTH: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy)]
enum StringMethod {
    CharAt,
    CharCodeAt,
    CodePointAt,
    At,
    IndexOf,
    LastIndexOf,
    Includes,
    StartsWith,
    EndsWith,
    Slice,
    Substring,
    Substr,
    IsWellFormed,
    ToWellFormed,
    Normalize,
    ToUpperCase,
    ToLowerCase,
    Trim,
    TrimStart,
    TrimEnd,
    Split,
    Concat,
    Repeat,
    PadStart,
    PadEnd,
    Replace,
    ReplaceAll,
    MatchAll,
    Match,
    Search,
    ToString,
    ValueOf,
    Html(&'static str, Option<&'static str>),
}

impl StringMethod {
    const ANCHOR: Self = Self::Html("a", Some("name"));
    const BIG: Self = Self::Html("big", None);
    const BLINK: Self = Self::Html("blink", None);
    const BOLD: Self = Self::Html("b", None);
    const FIXED: Self = Self::Html("tt", None);
    const FONTCOLOR: Self = Self::Html("font", Some("color"));
    const FONTSIZE: Self = Self::Html("font", Some("size"));
    const ITALICS: Self = Self::Html("i", None);
    const LINK: Self = Self::Html("a", Some("href"));
    const SMALL: Self = Self::Html("small", None);
    const STRIKE: Self = Self::Html("strike", None);
    const SUB: Self = Self::Html("sub", None);
    const SUP: Self = Self::Html("sup", None);
}

#[derive(Clone, Copy)]
enum StringStatic {
    FromCharCode,
    FromCodePoint,
    Raw,
}

#[yield_binding_macros::builtin_global(name = "String", root = "string", kind = "constructor")]
pub(crate) mod members {
    use super::{
        ActiveRoots, Host, Invocation, PlatformServices, StringMethod, StringStatic, Value, VmCore,
        VmError,
    };

    #[methods(
        charAt: 1 = StringMethod::CharAt,
        charCodeAt: 1 = StringMethod::CharCodeAt,
        codePointAt: 1 = StringMethod::CodePointAt,
        at: 1 = StringMethod::At,
        indexOf: 1 = StringMethod::IndexOf,
        lastIndexOf: 1 = StringMethod::LastIndexOf,
        includes: 1 = StringMethod::Includes,
        startsWith: 1 = StringMethod::StartsWith,
        endsWith: 1 = StringMethod::EndsWith,
        slice: 2 = StringMethod::Slice,
        substring: 2 = StringMethod::Substring,
        isWellFormed: 0 = StringMethod::IsWellFormed,
        toWellFormed: 0 = StringMethod::ToWellFormed,
        normalize: 0 = StringMethod::Normalize,
        toUpperCase: 0 = StringMethod::ToUpperCase,
        toLowerCase: 0 = StringMethod::ToLowerCase,
        trim: 0 = StringMethod::Trim,
        trimStart: 0 = StringMethod::TrimStart,
        trimEnd: 0 = StringMethod::TrimEnd,
        split: 2 = StringMethod::Split,
        concat: 1 = StringMethod::Concat,
        repeat: 1 = StringMethod::Repeat,
        padStart: 1 = StringMethod::PadStart,
        padEnd: 1 = StringMethod::PadEnd,
        replace: 2 = StringMethod::Replace,
        replaceAll: 2 = StringMethod::ReplaceAll,
        matchAll: 1 = StringMethod::MatchAll,
        r#match: 1 = StringMethod::Match,
        search: 1 = StringMethod::Search,
        toString: 0 = StringMethod::ToString,
        valueOf: 0 = StringMethod::ValueOf,
        substr: 2 = StringMethod::Substr,
        anchor: 1 = StringMethod::ANCHOR,
        big: 0 = StringMethod::BIG,
        blink: 0 = StringMethod::BLINK,
        bold: 0 = StringMethod::BOLD,
        fixed: 0 = StringMethod::FIXED,
        fontcolor: 1 = StringMethod::FONTCOLOR,
        fontsize: 1 = StringMethod::FONTSIZE,
        italics: 0 = StringMethod::ITALICS,
        link: 1 = StringMethod::LINK,
        small: 0 = StringMethod::SMALL,
        strike: 0 = StringMethod::STRIKE,
        sub: 0 = StringMethod::SUB,
        sup: 0 = StringMethod::SUP,
    )]
    fn call_method<P: PlatformServices>(
        vm: &mut VmCore,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        key: StringMethod,
        receiver: &Value,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let root_view = (receiver, args);
        let roots = &roots.with(&root_view);
        vm.call_string_method(host, roots, key, receiver, args)
    }

    #[functions(
        fromCharCode: 1 = StringStatic::FromCharCode,
        fromCodePoint: 1 = StringStatic::FromCodePoint,
        raw: 1 = StringStatic::Raw,
    )]
    fn call_static<P: PlatformServices>(
        vm: &mut VmCore,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        key: StringStatic,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        vm.call_string_static(host, roots, key, args)
    }
}

const CONSTRUCTOR: IntrinsicKey = IntrinsicKey::new("String", Slot::Global, "");
const ITERATOR: IntrinsicKey = IntrinsicKey::new("String", Slot::PrototypeSymbol, "iterator");
const ITERATOR_NEXT: IntrinsicKey = IntrinsicKey::new("StringIterator", Slot::Prototype, "next");

impl<P: crate::host::PlatformServices> BuiltinTable<P> {
    pub(crate) const STRING_INTRINSICS: &[IntrinsicDef<P>] = &[
        IntrinsicDef::constructor(
            CONSTRUCTOR,
            1,
            |vm, host, roots, args, _| vm.call_string(host, roots, &args),
            |vm, host, roots, args, prototype| {
                let result = match vm.call_string(host, roots, &args)? {
                    Invocation::Value(value) => vm.construct_object(&[value])?,
                    thrown => thrown,
                };
                vm.with_result_prototype(roots, result, prototype)
            },
        ),
        IntrinsicDef::call(ITERATOR, 0, |vm, host, roots, _, receiver| {
            vm.string_iterator(host, roots, receiver)
        }),
        IntrinsicDef::call(ITERATOR_NEXT, 0, |vm, _, roots, _, receiver| {
            vm.string_iterator_next(roots, receiver)
        }),
    ];
}

impl<P: crate::host::PlatformServices> BuiltinTable<P> {
    pub(crate) const OWNER: Owner<P> = Owner::new(
        members::ROOT,
        STRING_ROOTS,
        &[Self::STRING_INTRINSICS, members::Globals::<P>::INTRINSICS],
        Some(|vm, identity| {
            let ValueRef::Object(identity) = identity.view() else {
                unreachable!("iterator identity is a function")
            };
            install(vm, identity);
        }),
    );
}

pub(crate) const STRING_ROOTS: &[RootSpec] = &[
    RootSpec::new(
        members::ROOT,
        RootKind::Native(CONSTRUCTOR),
        Some(ROOT_FUNCTION_PROTOTYPE),
    )
    .fixed(ROOT_STRING_PROTOTYPE),
    RootSpec::new(
        ROOT_STRING_PROTOTYPE,
        RootKind::Plain,
        Some(ROOT_OBJECT_PROTOTYPE),
    ),
    RootSpec::new(
        ROOT_STRING_ITERATOR,
        RootKind::Native(ITERATOR),
        Some(ROOT_FUNCTION_PROTOTYPE),
    ),
    RootSpec::new(
        ROOT_STRING_ITERATOR_PROTOTYPE,
        RootKind::Plain,
        Some(super::iterator::SYNC_ROOT),
    ),
];

fn install(vm: &mut VmCore, identity: ObjectId) {
    let constructor = vm.builtins.get(members::ROOT);
    let prototype = vm.builtins.get(ROOT_STRING_PROTOTYPE);
    let iterator = vm.builtins.get(ROOT_STRING_ITERATOR);
    vm.init_string_constructor(constructor, prototype, iterator);
    vm.init_string_iterator_prototype(vm.builtins.get(ROOT_STRING_ITERATOR_PROTOTYPE), identity);
}

impl VmCore {
    fn init_string_constructor(
        &mut self,
        constructor: ObjectId,
        prototype: ObjectId,
        iterator: ObjectId,
    ) {
        link_constructor(self, constructor, prototype);
        self.heap
            .define_data(
                prototype,
                PropertyKey::Symbol(ITERATOR_SYMBOL),
                Value::object(iterator),
                Attributes::METHOD,
            )
            .expect("String.prototype stores Symbol.iterator");
        members::install_members(self, constructor, prototype);
        for (alias, canonical) in [("trimLeft", "trimStart"), ("trimRight", "trimEnd")] {
            let property = self
                .heap
                .get(prototype)
                .expect("String prototype")
                .own_property(PropertyKeyRef::String(canonical))
                .expect("String trim method");
            let value = property.data_value().expect("String method value").clone();
            self.heap
                .define_data(prototype, alias, value, Attributes::METHOD)
                .expect("String trim alias");
        }
    }

    fn init_string_iterator_prototype(&mut self, prototype: ObjectId, identity: ObjectId) {
        let next = Value::object(self.alloc_intrinsic(ITERATOR_NEXT));
        self.heap
            .define_data(prototype, "next", next, Attributes::METHOD)
            .expect("iterator prototype stores next");
        self.heap
            .define_data(
                prototype,
                PropertyKey::Symbol(ITERATOR_SYMBOL),
                Value::object(identity),
                Attributes::METHOD,
            )
            .expect("iterator prototype stores Symbol.iterator");
    }

    fn string_iterator<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&receiver);
        if matches!(receiver.view(), ValueRef::Null | ValueRef::Undefined) {
            return self.throw_type_error(
                roots,
                "String[Symbol.iterator] requires a non-null receiver",
            );
        }
        let string = match self.coerce_string(host, roots, receiver)? {
            Ok(string) => string,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let iterator = self.heap.alloc(Object::StringIterator {
            object: PlainObject::new(Some(self.builtins.get(ROOT_STRING_ITERATOR_PROTOTYPE))),
            string,
            position: 0,
        });
        Ok(Invocation::Value(Value::object(iterator)))
    }

    fn string_iterator_next(
        &mut self,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        let invalid_receiver = "StringIterator.next requires a string iterator";
        let ValueRef::Object(id) = receiver.view() else {
            return self.throw_type_error(roots, invalid_receiver);
        };
        let Object::StringIterator {
            string, position, ..
        } = self.heap.get_mut(id)?
        else {
            return self.throw_type_error(roots, invalid_receiver);
        };
        let Some(first) = utf16_unit(string, *position) else {
            return self.iterator_result(Value::UNDEFINED, true);
        };
        let width = if (0xd800..=0xdbff).contains(&first)
            && matches!(utf16_unit(string, *position + 1), Some(0xdc00..=0xdfff))
        {
            2
        } else {
            1
        };
        let end = *position + width;
        let value = string.slice_units(*position..end);
        *position = end;
        if end == utf16_len(string) {
            *string = JsString::default();
            *position = 0;
        }
        self.iterator_result(Value::string(value), false)
    }

    fn call_string<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let Some(value) = args.first() else {
            return Ok(Invocation::Value(Value::string(JsString::default())));
        };
        self.string_value(host, roots, value)
    }

    fn call_string_method<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        method: StringMethod,
        receiver: &Value,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let root_view = (receiver, args);
        let roots = &roots.with(&root_view);
        if matches!(method, StringMethod::ToString | StringMethod::ValueOf) {
            return self.string_primitive(roots, receiver);
        }
        if matches!(receiver.view(), ValueRef::Undefined | ValueRef::Null) {
            return self.throw_type_error(roots, "String method requires a non-null receiver");
        }
        if matches!(
            method,
            StringMethod::Match
                | StringMethod::MatchAll
                | StringMethod::Search
                | StringMethod::Replace
                | StringMethod::ReplaceAll
                | StringMethod::Split
        ) && let Some(result) =
            self.string_pattern_method(host, roots, method, receiver, args)?
        {
            return Ok(result);
        }
        let text = match self.coerce_string(host, roots, receiver)? {
            Ok(text) => text,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        match method {
            StringMethod::CharAt => self.char_at(&text, host, roots, arg(args, 0)),
            StringMethod::CharCodeAt => self.char_code_at(&text, host, roots, arg(args, 0)),
            StringMethod::CodePointAt => self.code_point_at(&text, host, roots, arg(args, 0)),
            StringMethod::At => self.string_at(&text, host, roots, arg(args, 0)),
            StringMethod::IndexOf => self.index_of(&text, host, roots, args, false),
            StringMethod::LastIndexOf => self.index_of(&text, host, roots, args, true),
            StringMethod::Includes => self.string_includes(&text, host, roots, args),
            StringMethod::StartsWith => self.string_affix(&text, host, roots, args, true),
            StringMethod::EndsWith => self.string_affix(&text, host, roots, args, false),
            StringMethod::Slice => self.string_slice(&text, host, roots, args),
            StringMethod::Substring => self.string_substring(&text, host, roots, args),
            StringMethod::Substr => self.string_substr(&text, host, roots, args),
            StringMethod::IsWellFormed => Ok(Invocation::Value(Value::boolean(
                text.is_latin1() || char::decode_utf16(text.units()).all(|item| item.is_ok()),
            ))),
            StringMethod::ToWellFormed => {
                Ok(Invocation::Value(Value::string(to_well_formed(text))))
            }
            StringMethod::Normalize => self.string_normalize(text, host, roots, arg(args, 0)),
            StringMethod::Html(tag, attribute) => {
                self.string_html(&text, host, roots, arg(args, 0), tag, attribute)
            }
            StringMethod::ToUpperCase => string_case(&text, true),
            StringMethod::ToLowerCase => string_case(&text, false),
            StringMethod::Trim => string_trim(&text, true, true),
            StringMethod::TrimStart => string_trim(&text, true, false),
            StringMethod::TrimEnd => string_trim(&text, false, true),
            StringMethod::Split => self.string_split(&text, host, roots, args),
            StringMethod::Concat => self.string_concat(&text, host, roots, args),
            StringMethod::Repeat => self.string_repeat(&text, host, roots, arg(args, 0)),
            StringMethod::PadStart => self.string_pad(&text, host, roots, args, true),
            StringMethod::PadEnd => self.string_pad(&text, host, roots, args, false),
            StringMethod::Replace => self.string_replace(&text, host, roots, args),
            StringMethod::ReplaceAll => self.string_replace_all(&text, host, roots, args),
            StringMethod::Match
            | StringMethod::MatchAll
            | StringMethod::Search
            | StringMethod::ToString
            | StringMethod::ValueOf => unreachable!("handled before ToString"),
        }
    }

    fn call_string_static<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        method: StringStatic,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        match method {
            StringStatic::FromCharCode => self.string_from_char_code(host, roots, args),
            StringStatic::FromCodePoint => self.string_from_code_point(host, roots, args),
            StringStatic::Raw => self.string_raw(host, roots, args),
        }
    }

    fn string_primitive(
        &mut self,
        roots: &ActiveRoots<'_>,
        receiver: &Value,
    ) -> Result<Invocation, VmError> {
        let text = match receiver.view() {
            ValueRef::String(text) => Some(text),
            ValueRef::Object(id) => match self.heap.get(id)? {
                Object::Boxed {
                    primitive: BoxedPrimitive::String(text),
                    ..
                } => Some(text),
                _ => None,
            },
            _ => None,
        };
        match text {
            Some(text) => Ok(Invocation::Value(Value::string(text.clone()))),
            None => self.throw_type_error(roots, "String method called on incompatible receiver"),
        }
    }

    fn char_at<P: PlatformServices>(
        &mut self,
        text: &JsString,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        index: &Value,
    ) -> Result<Invocation, VmError> {
        let unit = match self.relative_index(host, roots, index, utf16_len(text), false)? {
            Ok(index) => index.and_then(|index| utf16_unit(text, index)),
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        Ok(Invocation::Value(Value::string(
            unit.map(utf16_unit_string).unwrap_or_default(),
        )))
    }

    fn char_code_at<P: PlatformServices>(
        &mut self,
        text: &JsString,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        index: &Value,
    ) -> Result<Invocation, VmError> {
        let index = match self.relative_index(host, roots, index, utf16_len(text), false)? {
            Ok(index) => index,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let code = index.and_then(|index| utf16_unit(text, index));
        Ok(Invocation::Value(Value::number(
            code.map_or(f64::NAN, f64::from),
        )))
    }

    fn code_point_at<P: PlatformServices>(
        &mut self,
        text: &JsString,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        index: &Value,
    ) -> Result<Invocation, VmError> {
        let index = match self.relative_index(host, roots, index, utf16_len(text), false)? {
            Ok(index) => index,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        Ok(Invocation::Value(
            match index.and_then(|index| code_point_at(text, index)) {
                Some(point) => Value::number(f64::from(point)),
                None => Value::UNDEFINED,
            },
        ))
    }

    fn string_at<P: PlatformServices>(
        &mut self,
        text: &JsString,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        index: &Value,
    ) -> Result<Invocation, VmError> {
        let index = match self.relative_index(host, roots, index, utf16_len(text), true)? {
            Ok(index) => index,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        Ok(Invocation::Value(
            match index.and_then(|index| utf16_unit(text, index)) {
                Some(unit) => Value::string(utf16_unit_string(unit)),
                None => Value::UNDEFINED,
            },
        ))
    }

    fn index_of<P: PlatformServices>(
        &mut self,
        text: &JsString,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        args: &[Value],
        last: bool,
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let needle = match self.coerce_string(host, roots, arg(args, 0))? {
            Ok(needle) => needle,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let length = utf16_len(text);
        let start = match args.get(1) {
            Some(value) if !matches!(value.view(), ValueRef::Undefined) => {
                match self.integer_index(host, roots, value, last)? {
                    Ok(value) => value,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                }
            }
            _ if last => f64::INFINITY,
            _ => 0.0,
        };
        let position = if last {
            find_last(text, &needle, start)
        } else {
            find_first(text, &needle, start, length)
        };
        Ok(Invocation::Value(Value::number(position)))
    }

    fn string_includes<P: PlatformServices>(
        &mut self,
        text: &JsString,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let needle = match self.coerce_string(host, roots, arg(args, 0))? {
            Ok(needle) => needle,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let start = match self.clamp_index(host, roots, arg(args, 1), utf16_len(text))? {
            Ok(start) => start,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        Ok(Invocation::Value(Value::boolean(
            find_first(text, &needle, start as f64, utf16_len(text)) >= 0.0,
        )))
    }

    fn string_affix<P: PlatformServices>(
        &mut self,
        text: &JsString,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        args: &[Value],
        start: bool,
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let search = match self.coerce_string(host, roots, arg(args, 0))? {
            Ok(search) => search,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let length = utf16_len(text);
        let end = if start {
            length
        } else {
            match args.get(1) {
                Some(value) if !matches!(value.view(), ValueRef::Undefined) => {
                    match self.clamp_index(host, roots, value, length)? {
                        Ok(end) => end,
                        Err(error) => return Ok(Invocation::Thrown(error)),
                    }
                }
                _ => length,
            }
        };
        let matched = if start {
            let offset = match self.clamp_index(host, roots, arg(args, 1), length)? {
                Ok(offset) => offset,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
            search.unit_len() <= length - offset && matches_at(text, &search, offset)
        } else {
            search.unit_len() <= end && matches_at(text, &search, end - search.unit_len())
        };
        Ok(Invocation::Value(Value::boolean(matched)))
    }

    fn string_slice<P: PlatformServices>(
        &mut self,
        text: &JsString,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let length = utf16_len(text) as f64;
        let clamp = |index: f64| {
            (if index < 0.0 {
                (length + index).max(0.0)
            } else {
                index.min(length)
            }) as usize
        };
        let start = match self.integer_index(host, roots, arg(args, 0), false)? {
            Ok(start) => clamp(start),
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let end = match args.get(1) {
            Some(value) if !matches!(value.view(), ValueRef::Undefined) => {
                match self.integer_index(host, roots, value, false)? {
                    Ok(end) => clamp(end),
                    Err(error) => return Ok(Invocation::Thrown(error)),
                }
            }
            _ => length as usize,
        };
        Ok(Invocation::Value(Value::string(slice_units(
            text,
            start,
            end.max(start),
        ))))
    }

    fn string_substring<P: PlatformServices>(
        &mut self,
        text: &JsString,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let length = utf16_len(text) as f64;
        let start = match self.integer_index(host, roots, arg(args, 0), false)? {
            Ok(start) => start.clamp(0.0, length) as usize,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let end = match args.get(1) {
            Some(value) if !matches!(value.view(), ValueRef::Undefined) => {
                match self.integer_index(host, roots, value, false)? {
                    Ok(end) => end.clamp(0.0, length) as usize,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                }
            }
            _ => length as usize,
        };
        let (start, end) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        Ok(Invocation::Value(Value::string(slice_units(
            text, start, end,
        ))))
    }

    fn string_substr<P: PlatformServices>(
        &mut self,
        text: &JsString,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let size = text.unit_len() as f64;
        let start = match self.integer_index(host, roots, arg(args, 0), false)? {
            Ok(start) if start < 0.0 => (size + start).max(0.0),
            Ok(start) => start.min(size),
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let length = arg(args, 1);
        let remaining = size - start;
        let length = if matches!(length.view(), ValueRef::Undefined) {
            remaining
        } else {
            match self.integer_index(host, roots, length, false)? {
                Ok(length) => length.clamp(0.0, remaining),
                Err(error) => return Ok(Invocation::Thrown(error)),
            }
        };
        Ok(Invocation::Value(Value::string(
            text.slice_units(start as usize..(start + length) as usize),
        )))
    }

    fn string_normalize<P: PlatformServices>(
        &mut self,
        text: JsString,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        form: &Value,
    ) -> Result<Invocation, VmError> {
        let normalization = if matches!(form.view(), ValueRef::Undefined) {
            Normalization::Compose(ComposingNormalizerBorrowed::new_nfc())
        } else {
            let form = match self.coerce_string(host, roots, form)? {
                Ok(form) => form,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
            if form == "NFC" {
                Normalization::Compose(ComposingNormalizerBorrowed::new_nfc())
            } else if form == "NFD" {
                Normalization::Decompose(DecomposingNormalizerBorrowed::new_nfd())
            } else if form == "NFKC" {
                Normalization::Compose(ComposingNormalizerBorrowed::new_nfkc())
            } else if form == "NFKD" {
                Normalization::Decompose(DecomposingNormalizerBorrowed::new_nfkd())
            } else {
                return self.range_error(roots, "Invalid normalization form");
            }
        };
        if text.is_latin1() && text.units().all(|unit| unit <= 0x7f) {
            return Ok(Invocation::Value(Value::string(text)));
        }
        match normalize_text(&text, normalization) {
            Some(result) => Ok(Invocation::Value(Value::string(result))),
            None => self.range_error(roots, "Invalid string length"),
        }
    }

    fn string_html<P: PlatformServices>(
        &mut self,
        text: &JsString,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        value: &Value,
        tag: &str,
        attribute: Option<&str>,
    ) -> Result<Invocation, VmError> {
        let attribute = if let Some(name) = attribute {
            let value = match self.coerce_string(host, roots, value)? {
                Ok(value) => value,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
            Some((name, value))
        } else {
            None
        };
        let attr_size = attribute.as_ref().map_or(0, |(name, value)| {
            4 + name.len() + value.unit_len() + 5 * value.units().filter(|&u| u == 0x22).count()
        });
        let size = 5 + 2 * tag.len() + attr_size + text.unit_len();
        if size > MAX_LENGTH {
            return self.range_error(roots, "Invalid string length");
        }
        let mut out = Vec::with_capacity(size);
        out.push(b'<' as u16);
        out.extend(tag.encode_utf16());
        if let Some((name, value)) = attribute {
            out.push(b' ' as u16);
            out.extend(name.encode_utf16());
            out.extend("=\"".encode_utf16());
            for unit in value.units() {
                if unit == 0x22 {
                    out.extend("&quot;".encode_utf16());
                } else {
                    out.push(unit);
                }
            }
            out.push(b'"' as u16);
        }
        out.push(b'>' as u16);
        out.extend(text.units());
        out.extend("</".encode_utf16());
        out.extend(tag.encode_utf16());
        out.push(b'>' as u16);
        Ok(Invocation::Value(Value::string(JsString::from_units(out))))
    }

    fn string_split<P: PlatformServices>(
        &mut self,
        text: &JsString,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let limit = match args.get(1) {
            Some(value) if !matches!(value.view(), ValueRef::Undefined) => {
                match self.coerce_number(host, roots, value)? {
                    Ok(number) => crate::ops::number_to_uint32(number) as usize,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                }
            }
            _ => usize::MAX,
        };
        let mut parts = Vec::new();
        if limit == 0 {
            return self.array_result(parts);
        }
        match args
            .first()
            .filter(|value| !matches!(value.view(), ValueRef::Undefined))
        {
            None => parts.push(Value::string(text.to_owned())),
            Some(separator) => {
                let separator = match self.coerce_string(host, roots, separator)? {
                    Ok(separator) => separator,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                };
                if separator.is_empty() {
                    parts.extend(
                        text.units()
                            .take(limit)
                            .map(|unit| Value::string(utf16_unit_string(unit))),
                    );
                } else {
                    let mut cursor = 0;
                    while parts.len() < limit {
                        if let Some(found) = find_from(text, &separator, cursor) {
                            parts.push(Value::string(slice_units(text, cursor, found)));
                            cursor = found + utf16_len(&separator);
                        } else {
                            parts.push(Value::string(slice_units(text, cursor, utf16_len(text))));
                            break;
                        }
                    }
                }
            }
        }
        self.array_result(parts)
    }

    fn string_concat<P: PlatformServices>(
        &mut self,
        text: &JsString,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let mut parts = Vec::with_capacity(args.len() + 1);
        parts.push(text.to_owned());
        let mut length = text.len();
        let mut last_unit = text.units().next_back();
        for arg in args {
            match self.coerce_string(host, roots, arg)? {
                Ok(part) => {
                    let saves_pair = matches!(last_unit, Some(0xd800..=0xdbff))
                        && matches!(part.units().next(), Some(0xdc00..=0xdfff));
                    length = length.saturating_add(part.len()) - if saves_pair { 2 } else { 0 };
                    if length > MAX_LENGTH {
                        return self.range_error(roots, "Invalid string length");
                    }
                    if let Some(unit) = part.units().next_back() {
                        last_unit = Some(unit);
                    }
                    parts.push(part);
                }
                Err(error) => return Ok(Invocation::Thrown(error)),
            }
        }
        let unit_len = parts.iter().map(JsString::unit_len).sum();
        if parts.iter().all(JsString::is_latin1) {
            let mut out = Vec::with_capacity(unit_len);
            for part in &parts {
                out.extend(part.units().map(|unit| unit as u8));
            }
            Ok(Invocation::Value(Value::string(JsString::from_latin1(out))))
        } else {
            let mut out = Vec::with_capacity(unit_len);
            for part in &parts {
                out.extend(part.units());
            }
            Ok(Invocation::Value(Value::string(JsString::from_wide(out))))
        }
    }

    fn string_repeat<P: PlatformServices>(
        &mut self,
        text: &JsString,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        count: &Value,
    ) -> Result<Invocation, VmError> {
        let count = match self.integer_index(host, roots, count, false)? {
            Ok(count) => count,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        if count < 0.0 || !count.is_finite() {
            return self.range_error(roots, "Invalid count value");
        }
        let count = count as usize;
        if count == 0 || text.is_empty() {
            return Ok(Invocation::Value(Value::string(JsString::default())));
        }
        let first = text.len();
        let extra = first - pair_savings(text, text);
        if first.saturating_add(extra.saturating_mul(count - 1)) > MAX_LENGTH {
            return self.range_error(roots, "Invalid string length");
        }
        if text.is_latin1() {
            let mut out = Vec::with_capacity(text.unit_len() * count);
            for _ in 0..count {
                out.extend(text.units().map(|unit| unit as u8));
            }
            Ok(Invocation::Value(Value::string(JsString::from_latin1(out))))
        } else {
            let mut out = Vec::with_capacity(text.unit_len() * count);
            for _ in 0..count {
                out.extend(text.units());
            }
            Ok(Invocation::Value(Value::string(JsString::from_wide(out))))
        }
    }

    fn string_pad<P: PlatformServices>(
        &mut self,
        text: &JsString,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        args: &[Value],
        start: bool,
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let target = match self.integer_index(host, roots, arg(args, 0), false)? {
            Ok(target) => target.max(0.0) as usize,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let fill = match args.get(1) {
            Some(value) if !matches!(value.view(), ValueRef::Undefined) => {
                match self.coerce_string(host, roots, value)? {
                    Ok(fill) => fill,
                    Err(error) => return Ok(Invocation::Thrown(error)),
                }
            }
            _ => " ".into(),
        };
        let length = utf16_len(text);
        if target <= length || fill.is_empty() {
            return Ok(Invocation::Value(Value::string(text.to_owned())));
        }
        let pad_len = target - length;
        if target > MAX_LENGTH {
            return self.range_error(roots, "Invalid string length");
        }
        let (copies, tail) = (pad_len / fill.unit_len(), pad_len % fill.unit_len());
        let wide = !text.is_latin1() || fill.units().take(pad_len).any(|unit| unit > 0xff);
        if wide {
            let mut out = Vec::with_capacity(target);
            if !start {
                out.extend(text.units());
            }
            for _ in 0..copies {
                out.extend(fill.units());
            }
            out.extend(fill.units().take(tail));
            if start {
                out.extend(text.units());
            }
            let out = JsString::from_wide(out);
            if out.len() > MAX_LENGTH {
                return self.range_error(roots, "Invalid string length");
            }
            Ok(Invocation::Value(Value::string(out)))
        } else {
            let mut out = Vec::with_capacity(target);
            if !start {
                out.extend(text.units().map(|unit| unit as u8));
            }
            for _ in 0..copies {
                out.extend(fill.units().map(|unit| unit as u8));
            }
            out.extend(fill.units().take(tail).map(|unit| unit as u8));
            if start {
                out.extend(text.units().map(|unit| unit as u8));
            }
            let out = JsString::from_latin1(out);
            if out.len() > MAX_LENGTH {
                return self.range_error(roots, "Invalid string length");
            }
            Ok(Invocation::Value(Value::string(out)))
        }
    }

    fn string_replace<P: PlatformServices>(
        &mut self,
        text: &JsString,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let search = match self.coerce_string(host, roots, arg(args, 0))? {
            Ok(search) => search,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let Some(position) = find_from(text, &search, 0) else {
            return Ok(Invocation::Value(Value::string(text.to_owned())));
        };
        let matched = search;
        let end = position + utf16_len(&matched);
        let replacement = match args.get(1) {
            Some(value) if self.is_callable(value) => {
                match self.invoke(
                    host,
                    roots,
                    value,
                    Arguments::Values(&[
                        Value::string(matched),
                        Value::number(position as f64),
                        Value::string(text.to_owned()),
                    ]),
                    &Value::UNDEFINED,
                )? {
                    Invocation::Value(value) => match self.coerce_string(host, roots, &value)? {
                        Ok(value) => value,
                        Err(error) => return Ok(Invocation::Thrown(error)),
                    },
                    thrown @ Invocation::Thrown(_) => return Ok(thrown),
                }
            }
            Some(value) => match self.coerce_string(host, roots, value)? {
                Ok(pattern) => substitute(&pattern, text, &matched, position),
                Err(error) => return Ok(Invocation::Thrown(error)),
            },
            None => "undefined".into(),
        };
        let mut out = slice_units(text, 0, position);
        out.push_str(&replacement);
        out.push_str(&slice_units(text, end, utf16_len(text)));
        Ok(Invocation::Value(Value::string(out)))
    }

    /// Runs the `String.prototype` protocol lookup. `None` means the caller
    /// must fall back to the plain string algorithm.
    fn string_pattern_method<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        method: StringMethod,
        receiver: &Value,
        args: &[Value],
    ) -> Result<Option<Invocation>, VmError> {
        let root_view = (receiver, args);
        let roots = &roots.with(&root_view);
        let search = args.first().cloned().unwrap_or(Value::UNDEFINED);
        let (symbol, requires_global, builtin) = match method {
            StringMethod::Match => (MATCH_SYMBOL, false, Some(RegExpMethod::Match)),
            StringMethod::MatchAll => (MATCH_ALL_SYMBOL, true, Some(RegExpMethod::MatchAll)),
            StringMethod::Search => (SEARCH_SYMBOL, false, Some(RegExpMethod::Search)),
            StringMethod::Replace => (REPLACE_SYMBOL, false, None),
            StringMethod::ReplaceAll => (REPLACE_SYMBOL, true, None),
            StringMethod::Split => (SPLIT_SYMBOL, false, None),
            _ => return Ok(None),
        };
        if requires_global {
            match self.is_regexp(host, roots, &search)? {
                Ok(true) => {
                    let flags = match self.read_property(
                        host,
                        roots,
                        &search,
                        PropertyKeyRef::String("flags"),
                    )? {
                        Invocation::Value(value) => value,
                        Invocation::Thrown(error) => {
                            return Ok(Some(Invocation::Thrown(error)));
                        }
                    };
                    let flags = match self.coerce_string(host, roots, &flags)? {
                        Ok(flags) => flags,
                        Err(error) => return Ok(Some(Invocation::Thrown(error))),
                    };
                    if !flags.units().any(|unit| unit == u16::from(b'g')) {
                        return Ok(Some(self.throw_type_error(
                            roots,
                            "String.prototype method requires a global RegExp argument",
                        )?));
                    }
                }
                Ok(false) => {}
                Err(error) => return Ok(Some(Invocation::Thrown(error))),
            }
        }
        let protocol = if matches!(search.view(), ValueRef::Undefined | ValueRef::Null) {
            Value::UNDEFINED
        } else {
            match self.get_method(host, roots, &search, symbol)? {
                Ok(method) => method,
                Err(error) => return Ok(Some(Invocation::Thrown(error))),
            }
        };
        if !matches!(protocol.view(), ValueRef::Undefined) {
            let this = receiver.clone();
            let call_args = match method {
                StringMethod::Replace | StringMethod::ReplaceAll => {
                    vec![this, args.get(1).cloned().unwrap_or(Value::UNDEFINED)]
                }
                StringMethod::Split => vec![this, args.get(1).cloned().unwrap_or(Value::UNDEFINED)],
                _ => vec![this],
            };
            return Ok(Some(self.invoke(
                host,
                roots,
                &protocol,
                Arguments::Values(&call_args),
                &search,
            )?));
        }
        let Some(builtin) = builtin else {
            return Ok(None);
        };
        let flags = if matches!(method, StringMethod::MatchAll) {
            Value::string("g".into())
        } else {
            Value::UNDEFINED
        };
        let created = self.construct_regexp(host, roots, &[search.clone(), flags], None, false)?;
        let Invocation::Value(value) = &created else {
            return Ok(Some(created));
        };
        let ValueRef::Object(id) = value.view() else {
            return Ok(Some(created));
        };
        Ok(Some(self.call_regexp_method(
            host,
            roots,
            builtin,
            &Value::object(id),
            std::slice::from_ref(receiver),
        )?))
    }

    fn get_method<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        value: &Value,
        symbol: SymbolId,
    ) -> Result<Result<Value, Value>, VmError> {
        let roots = &roots.with(&value);
        match self.read_property(host, roots, value, PropertyKeyRef::Symbol(symbol))? {
            Invocation::Value(method)
                if matches!(method.view(), ValueRef::Undefined | ValueRef::Null) =>
            {
                Ok(Ok(Value::UNDEFINED))
            }
            Invocation::Value(method) => Ok(Ok(method)),
            Invocation::Thrown(error) => Ok(Err(error)),
        }
    }

    fn is_regexp<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        value: &Value,
    ) -> Result<Result<bool, Value>, VmError> {
        let roots = &roots.with(&value);
        let ValueRef::Object(id) = value.view() else {
            return Ok(Ok(false));
        };
        let matcher =
            match self.read_property(host, roots, value, PropertyKeyRef::Symbol(MATCH_SYMBOL))? {
                Invocation::Value(matcher) => matcher,
                Invocation::Thrown(error) => return Ok(Err(error)),
            };
        if !matches!(matcher.view(), ValueRef::Undefined) {
            return Ok(Ok(true));
        }
        Ok(Ok(matches!(self.heap.get(id)?, Object::RegExp { .. })))
    }

    fn string_replace_all<P: PlatformServices>(
        &mut self,
        text: &JsString,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let search = match self.coerce_string(host, roots, arg(args, 0))? {
            Ok(search) => search,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let replacement_arg = args.get(1).cloned().unwrap_or(Value::UNDEFINED);
        let functional = self.is_callable(&replacement_arg);
        let pattern = if functional {
            JsString::default()
        } else {
            match self.coerce_string(host, roots, &replacement_arg)? {
                Ok(pattern) => pattern,
                Err(error) => return Ok(Invocation::Thrown(error)),
            }
        };
        let search_length = utf16_len(&search);
        let Some(mut position) = find_from(text, &search, 0) else {
            return Ok(Invocation::Value(Value::string(text.clone())));
        };
        let has_substitution = !functional && pattern.units().any(|unit| unit == u16::from(b'$'));
        let mut out = Vec::new();
        let mut source = text.units();
        let mut last = 0usize;
        loop {
            out.extend(source.by_ref().take(position - last));
            let replacement = if functional {
                match self.invoke(
                    host,
                    roots,
                    &replacement_arg,
                    Arguments::Values(&[
                        Value::string(search.clone()),
                        Value::number(position as f64),
                        Value::string(text.clone()),
                    ]),
                    &Value::UNDEFINED,
                )? {
                    Invocation::Value(value) => match self.coerce_string(host, roots, &value)? {
                        Ok(value) => value,
                        Err(error) => return Ok(Invocation::Thrown(error)),
                    },
                    thrown @ Invocation::Thrown(_) => return Ok(thrown),
                }
            } else if has_substitution {
                substitute(&pattern, text, &search, position)
            } else {
                pattern.clone()
            };
            out.extend(replacement.units());
            if search_length != 0 {
                source.nth(search_length - 1);
            }
            last = position + search_length;
            let cursor = if search_length == 0 {
                position.saturating_add(1)
            } else {
                last
            };
            if cursor > text.unit_len() {
                break;
            }
            let Some(next) = find_from(text, &search, cursor) else {
                break;
            };
            position = next;
        }
        out.extend(source);
        Ok(Invocation::Value(Value::string(JsString::from_units(out))))
    }

    fn string_from_char_code<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let mut units = Vec::with_capacity(args.len());
        for arg in args {
            let number = match self.coerce_number(host, roots, arg)? {
                Ok(number) => number,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
            units.push(crate::ops::number_to_uint32(number) as u16);
        }
        Ok(Invocation::Value(Value::string(JsString::from_units(
            units,
        ))))
    }

    fn string_from_code_point<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let mut out = Vec::with_capacity(args.len());
        for arg in args {
            let number = match self.coerce_number(host, roots, arg)? {
                Ok(number) => number,
                Err(error) => return Ok(Invocation::Thrown(error)),
            };
            if !(0.0..=0x10_ffff as f64).contains(&number) || number.fract() != 0.0 {
                return self.range_error(roots, "Invalid code point");
            }
            let point = number as u32;
            if point <= 0xffff {
                out.push(point as u16);
            } else {
                let offset = point - 0x10000;
                out.extend([
                    0xd800 + (offset >> 10) as u16,
                    0xdc00 + (offset & 0x3ff) as u16,
                ]);
            }
        }
        Ok(Invocation::Value(Value::string(JsString::from_units(out))))
    }

    fn string_raw<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        args: &[Value],
    ) -> Result<Invocation, VmError> {
        let roots = &roots.with(&args);
        let template = arg(args, 0);
        let raw = match self.read_member(host, roots, template, &Value::string("raw".into()))? {
            Invocation::Value(value) => value,
            thrown @ Invocation::Thrown(_) => return Ok(thrown),
        };
        let ValueRef::Object(raw_id) = raw.view() else {
            return self.throw_type_error(roots, "String.raw requires a raw array");
        };
        let raw_object = Value::object(raw_id);
        let roots = &roots.with(&raw_object);
        let length =
            match self.read_member(host, roots, &raw_object, &Value::string("length".into()))? {
                Invocation::Value(value) => value,
                thrown @ Invocation::Thrown(_) => return Ok(thrown),
            };
        let length = match self.coerce_number(host, roots, &length)? {
            Ok(length) => crate::ops::number_to_uint32(length) as usize,
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        let mut out = JsString::default();
        let mut segment_index = 0;
        while segment_index < length {
            let key = Value::string(segment_index.to_string().into());
            let segment = match self.read_member(host, roots, &raw_object, &key)? {
                Invocation::Value(value) => value,
                thrown @ Invocation::Thrown(_) => return Ok(thrown),
            };
            match self.coerce_string(host, roots, &segment)? {
                Ok(part) => out.push_str(&part),
                Err(error) => return Ok(Invocation::Thrown(error)),
            }
            segment_index += 1;
            if segment_index < length
                && let Some(substitution) = args.get(segment_index)
            {
                match self.coerce_string(host, roots, substitution)? {
                    Ok(part) => out.push_str(&part),
                    Err(error) => return Ok(Invocation::Thrown(error)),
                }
            }
        }
        Ok(Invocation::Value(Value::string(out)))
    }

    fn relative_index<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        value: &Value,
        length: usize,
        relative: bool,
    ) -> Result<Result<Option<usize>, Value>, VmError> {
        let roots = &roots.with(&value);
        let number = match self.integer_index(host, roots, value, false)? {
            Ok(number) => number,
            Err(thrown) => return Ok(Err(thrown)),
        };
        if !number.is_finite() {
            return Ok(Ok(None));
        }
        let index = if relative && number < 0.0 {
            number + length as f64
        } else {
            number
        };
        Ok(Ok(if index < 0.0 || index >= length as f64 {
            None
        } else {
            Some(index as usize)
        }))
    }

    fn integer_index<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        value: &Value,
        infinity: bool,
    ) -> Result<Result<f64, Value>, VmError> {
        let roots = &roots.with(&value);
        let number = match self.coerce_number(host, roots, value)? {
            Ok(number) => number,
            Err(error) => return Ok(Err(error)),
        };
        Ok(Ok(if number.is_nan() {
            if infinity { f64::INFINITY } else { 0.0 }
        } else {
            number.trunc()
        }))
    }

    fn clamp_index<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        value: &Value,
        length: usize,
    ) -> Result<Result<usize, Value>, VmError> {
        let roots = &roots.with(&value);
        match self.integer_index(host, roots, value, false)? {
            Ok(index) => Ok(Ok(index.clamp(0.0, length as f64) as usize)),
            Err(error) => Ok(Err(error)),
        }
    }

    pub(crate) fn array_result(&mut self, elements: Vec<Value>) -> Result<Invocation, VmError> {
        Ok(Invocation::Value(Value::object(
            self.heap.alloc_array(elements),
        )))
    }

    fn range_error(
        &mut self,
        roots: &ActiveRoots<'_>,
        message: &str,
    ) -> Result<Invocation, VmError> {
        self.alloc_range_error(roots, message)
            .map(Invocation::Thrown)
    }

    fn alloc_range_error(
        &mut self,
        roots: &ActiveRoots<'_>,
        message: &str,
    ) -> Result<Value, VmError> {
        self.alloc_error(
            roots,
            self.builtins.get(ROOT_RANGEERROR_PROTOTYPE),
            "RangeError",
            Some(message.into()),
        )
    }
}

fn arg(args: &[Value], index: usize) -> &Value {
    args.get(index).unwrap_or(&UNDEFINED)
}

pub(crate) fn utf16_len(text: &JsString) -> usize {
    text.unit_len()
}

pub(crate) fn utf16_unit(text: &JsString, index: usize) -> Option<u16> {
    text.unit_at(index)
}

pub(crate) fn utf16_unit_string(unit: u16) -> JsString {
    JsString::from_units(vec![unit])
}

fn pair_savings(left: &JsString, right: &JsString) -> usize {
    if matches!(left.units().next_back(), Some(0xd800..=0xdbff))
        && matches!(right.units().next(), Some(0xdc00..=0xdfff))
    {
        2 // Three WTF-8 bytes per lone surrogate, four for the joined pair.
    } else {
        0
    }
}

fn code_point_at(text: &JsString, index: usize) -> Option<u32> {
    let first = u32::from(utf16_unit(text, index)?);
    if (0xD800..=0xDBFF).contains(&first)
        && let Some(second) = utf16_unit(text, index + 1).map(u32::from)
        && (0xDC00..=0xDFFF).contains(&second)
    {
        return Some(0x10000 + ((first - 0xD800) << 10) + (second - 0xDC00));
    }
    Some(first)
}

fn slice_units(text: &JsString, start: usize, end: usize) -> JsString {
    text.slice_units(start..end)
}

fn matches_at(text: &JsString, needle: &JsString, start: usize) -> bool {
    needle
        .units()
        .enumerate()
        .all(|(offset, unit)| text.unit_at(start + offset) == Some(unit))
}

pub(crate) fn find_from(text: &JsString, needle: &JsString, start: usize) -> Option<usize> {
    let start = start.min(text.unit_len());
    if needle.is_empty() {
        return Some(start);
    }
    if let (Some(text), Some(needle)) = (text.latin1_bytes(), needle.latin1_bytes()) {
        return text
            .get(start..)?
            .windows(needle.len())
            .position(|window| window == needle)
            .map(|position| start + position);
    }
    let last = text.unit_len().checked_sub(needle.unit_len())?;
    (start..=last).find(|&position| matches_at(text, needle, position))
}

fn find_first(text: &JsString, needle: &JsString, start: f64, length: usize) -> f64 {
    find_from(text, needle, start.clamp(0.0, length as f64) as usize)
        .map(|position| position as f64)
        .unwrap_or(-1.0)
}

fn find_last(text: &JsString, needle: &JsString, start: f64) -> f64 {
    let length = text.unit_len();
    let needle_length = needle.unit_len();
    let end = (start as usize).saturating_add(needle_length).min(length);
    if needle.is_empty() {
        return end as f64;
    }
    let Some(last) = end.checked_sub(needle_length) else {
        return -1.0;
    };
    if let (Some(text), Some(needle)) = (text.latin1_bytes(), needle.latin1_bytes()) {
        return text
            .get(..last + needle.len())
            .and_then(|text| {
                text.windows(needle.len())
                    .rposition(|window| window == needle)
            })
            .map(|position| position as f64)
            .unwrap_or(-1.0);
    }
    (0..=last)
        .rev()
        .find(|&position| matches_at(text, needle, position))
        .map(|position| position as f64)
        .unwrap_or(-1.0)
}

fn to_well_formed(text: JsString) -> JsString {
    if text.is_latin1() {
        return text;
    }
    let mut items = char::decode_utf16(text.units());
    let mut prefix = 0;
    for item in items.by_ref() {
        if let Ok(ch) = item {
            prefix += ch.len_utf16();
        } else {
            let mut out = Vec::with_capacity(text.unit_len());
            out.extend(text.units().take(prefix));
            out.push(0xfffd);
            for item in items {
                out.extend_from_slice(item.unwrap_or('\u{fffd}').encode_utf16(&mut [0; 2]));
            }
            return JsString::from_units(out);
        }
    }
    text
}

enum Normalization {
    Compose(ComposingNormalizerBorrowed<'static>),
    Decompose(DecomposingNormalizerBorrowed<'static>),
}

fn normalize_text(text: &JsString, normalization: Normalization) -> Option<JsString> {
    let mut items = char::decode_utf16(text.units());
    // Preserve the existing storage unless the normalized code units differ.
    let mut original = text.units();
    let mut length = 0;
    let mut changed: Option<Vec<u16>> = None;
    let mut append_units = |units: &[u16]| {
        let previous_length = length;
        length += units.len();
        if length > MAX_LENGTH {
            return None;
        }
        if let Some(out) = &mut changed {
            out.extend_from_slice(units);
        } else if !original
            .by_ref()
            .take(units.len())
            .eq(units.iter().copied())
        {
            let mut out = Vec::with_capacity(length.max(text.unit_len().min(MAX_LENGTH)));
            out.extend(text.units().take(previous_length));
            out.extend_from_slice(units);
            changed = Some(out);
        }
        Some(())
    };
    loop {
        // Lone surrogates are unchanged and cannot compose with their neighbors.
        let mut separator = None;
        let run = std::iter::from_fn(|| match items.next()? {
            Ok(ch) => Some(ch),
            Err(error) => {
                separator = Some(error.unpaired_surrogate());
                None
            }
        })
        .fuse();
        let append = |ch: char| append_units(ch.encode_utf16(&mut [0; 2]));
        match &normalization {
            Normalization::Compose(normalizer) => {
                normalizer.normalize_iter(run).try_for_each(append)?;
            }
            Normalization::Decompose(normalizer) => {
                normalizer.normalize_iter(run).try_for_each(append)?;
            }
        }
        let Some(separator) = separator else {
            return Some(match changed {
                Some(out) => JsString::from_units(out),
                None if length == text.unit_len() => text.clone(),
                None => text.slice_units(0..length),
            });
        };
        append_units(&[separator])?;
    }
}

fn string_case(text: &JsString, upper: bool) -> Result<Invocation, VmError> {
    // Case whole scalar runs so context-sensitive mappings (e.g. final sigma)
    // still see their neighbors. Lone surrogates delimit runs and survive unchanged.
    let mut run = String::new();
    let mut out = Vec::with_capacity(utf16_len(text));
    let convert = |run: &str| {
        if upper {
            run.to_uppercase()
        } else {
            run.to_lowercase()
        }
    };
    for item in char::decode_utf16(text.units()) {
        match item {
            Ok(ch) => run.push(ch),
            Err(surrogate) => {
                if !run.is_empty() {
                    out.extend(convert(&run).encode_utf16());
                    run.clear();
                }
                out.push(surrogate.unpaired_surrogate());
            }
        }
    }
    if !run.is_empty() {
        out.extend(convert(&run).encode_utf16());
    }
    Ok(Invocation::Value(Value::string(JsString::from_units(out))))
}

fn string_trim(text: &JsString, start: bool, end: bool) -> Result<Invocation, VmError> {
    let length = text.unit_len();
    let whitespace = |unit: &u16| char::from_u32(u32::from(*unit)).is_some_and(is_trim_whitespace);
    let first = if start {
        text.units().take_while(whitespace).count()
    } else {
        0
    };
    let last = if end {
        length
            - text
                .units()
                .rev()
                .take(length - first)
                .take_while(whitespace)
                .count()
    } else {
        length
    };
    Ok(Invocation::Value(Value::string(slice_units(
        text, first, last,
    ))))
}

fn is_trim_whitespace(ch: char) -> bool {
    matches!(
        ch,
        '\u{0009}'
            | '\u{000A}'
            | '\u{000B}'
            | '\u{000C}'
            | '\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{FEFF}'
            | '\u{2028}'
            | '\u{2029}'
    ) || ch.is_whitespace()
}

fn substitute(
    pattern: &JsString,
    text: &JsString,
    matched: &JsString,
    position: usize,
) -> JsString {
    let mut units = pattern.units().peekable();
    let mut out = Vec::new();
    while let Some(unit) = units.next() {
        if unit != u16::from(b'$') {
            out.push(unit);
            continue;
        }
        match units.peek().copied() {
            Some(0x24) => out.push(unit),
            Some(0x26) => out.extend(matched.units()),
            Some(0x60) => out.extend(text.units().take(position)),
            Some(0x27) => out.extend(text.units().skip(position + utf16_len(matched))),
            _ => {
                out.push(unit);
                continue;
            }
        }
        units.next();
    }
    JsString::from_units(out)
}

pub(crate) struct BuiltinTable<P>(std::marker::PhantomData<P>);
