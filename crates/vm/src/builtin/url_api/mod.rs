//! URL records and live query lists. The heap owns both sides of the link;
//! neither parsing nor mutation performs host work.

pub(crate) mod params;
pub(crate) mod pattern;
pub(crate) mod record;

use url::{Url, form_urlencoded, quirks};

use super::event::install_accessor;
use super::globals::Owner;
use super::{
    FUNCTION_PARENT, OBJECT_PARENT, RootKind, RootSpec, define_builtin_data, link_constructor,
};
use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::heap::{Attributes, Object, PlainObject, TO_STRING_TAG_SYMBOL};
use crate::host::Host;
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::runtime::Invocation;
use crate::value::{JsString, ObjectId, PropertyKey, Value, ValueRef};
use crate::vm::VmCore;

const ROOT: &str = "url";
const PROTOTYPE: &str = "url.prototype";

const fn key(slot: Slot, member: &'static str) -> IntrinsicKey {
    IntrinsicKey::new("URL", slot, member)
}

macro_rules! url_members {
    ($($member:literal => $get:ident $(, $set:ident)?;)*) => {
        const MEMBERS: &[IntrinsicDef] = &[
            $(IntrinsicDef::call(key(Slot::PrototypeGetter, $member), 0, |vm, _, roots, _, this| {
                let Some((_, url)) = url_receiver(vm, this)? else { return bad_receiver(vm, roots) };
                Ok(string(quirks::$get(url)))
            }),
            $(IntrinsicDef::call(key(Slot::PrototypeSetter, $member), 1, |vm, host, roots, args, this| {
                let Some((id, _)) = url_receiver(vm, this)? else { return bad_receiver(vm, roots) };
                let text = match vm.coerce_string(host, roots, args.first().unwrap_or(&Value::UNDEFINED))? {
                    Ok(text) => usv_string(&text),
                    Err(error) => return Ok(Invocation::Thrown(error)),
                };
                let Object::Url { url, .. } = vm.heap.get_mut(id)? else { unreachable!() };
                url_members!(@set vm, roots, url, &text, $set);
                if matches!($member, "href" | "search") { vm.refresh_search_params(id)?; }
                Ok(Invocation::Value(Value::UNDEFINED))
            }),)?)*
        ];
    };
    (@set $vm:ident, $roots:ident, $url:ident, $text:expr, set_href) => {
        if quirks::set_href($url, $text).is_err() {
            return $vm.throw_type_error($roots, "invalid URL");
        }
    };
    (@set $vm:ident, $roots:ident, $url:ident, $text:expr, $set:ident) => {
        let _ = quirks::$set($url, $text);
    };
}

url_members! {
    "href" => href, set_href;
    "origin" => origin;
    "protocol" => protocol, set_protocol;
    "username" => username, set_username;
    "password" => password, set_password;
    "host" => host, set_host;
    "hostname" => hostname, set_hostname;
    "port" => port, set_port;
    "pathname" => pathname, set_pathname;
    "search" => search, set_search;
    "hash" => hash, set_hash;
}

const FUNCTIONS: &[IntrinsicDef] = &[
    IntrinsicDef::constructor(
        key(Slot::Global, ""),
        1,
        |vm, _, roots, _, _| vm.throw_type_error(roots, "URL constructor requires new"),
        |vm, host, roots, args, prototype| {
            parse(vm, host, roots, &args, ParseMode::Construct(prototype))
        },
    ),
    IntrinsicDef::call(
        key(Slot::Static, "canParse"),
        1,
        |vm, host, roots, args, _| parse(vm, host, roots, &args, ParseMode::CanParse),
    ),
    IntrinsicDef::call(key(Slot::Static, "parse"), 1, |vm, host, roots, args, _| {
        parse(vm, host, roots, &args, ParseMode::Parse)
    }),
    IntrinsicDef::call(
        key(Slot::Static, "createObjectURL"),
        1,
        super::fetch::object_url::create,
    ),
    IntrinsicDef::call(
        key(Slot::Static, "revokeObjectURL"),
        1,
        super::fetch::object_url::revoke,
    ),
    IntrinsicDef::call(key(Slot::Prototype, "toString"), 0, stringify),
    IntrinsicDef::call(key(Slot::Prototype, "toJSON"), 0, stringify),
    IntrinsicDef::call(
        key(Slot::PrototypeGetter, "searchParams"),
        0,
        |vm, _, roots, _, this| {
            let Some((id, url)) = url_receiver(vm, this)? else {
                return bad_receiver(vm, roots);
            };
            if let Object::Url {
                params: Some(params),
                ..
            } = vm.heap.get(id)?
            {
                return Ok(Invocation::Value(Value::object(*params)));
            }
            let pairs = parse_query(url.query().unwrap_or_default());
            let params = vm.heap.alloc(Object::UrlSearchParams {
                object: PlainObject::new(Some(vm.builtins.get(params::PROTOTYPE))),
                pairs,
                owner: Some(id),
            });
            let Object::Url { params: slot, .. } = vm.heap.get_mut(id)? else {
                unreachable!()
            };
            *slot = Some(params);
            Ok(Invocation::Value(Value::object(params)))
        },
    ),
];

pub(crate) const URL_OWNER: Owner = Owner::new(
    ROOT,
    &[
        RootSpec::new(
            ROOT,
            RootKind::Native(key(Slot::Global, "")),
            FUNCTION_PARENT,
        )
        .fixed(PROTOTYPE)
        .backlink(Some(Attributes::METHOD)),
        RootSpec::new(PROTOTYPE, RootKind::Plain, OBJECT_PARENT),
    ],
    &[FUNCTIONS, MEMBERS],
    Some(install),
);

fn install(vm: &mut VmCore, _: &Value) {
    let constructor = vm.builtins.get(ROOT);
    let prototype = vm.builtins.get(PROTOTYPE);
    link_constructor(vm, constructor, prototype);
    for def in MEMBERS.iter().chain(FUNCTIONS) {
        let k = def.key;
        match k.slot {
            Slot::PrototypeGetter => {
                let getter = vm.alloc_intrinsic(k);
                let setter_key = key(Slot::PrototypeSetter, k.member);
                let setter = MEMBERS
                    .iter()
                    .any(|def| def.key == setter_key)
                    .then(|| vm.alloc_intrinsic(setter_key));
                install_accessor(vm, prototype, k.member, getter, setter, true)
                    .expect("URL accessor");
            }
            Slot::Prototype | Slot::Static => {
                let method = vm.alloc_intrinsic(k);
                let target = if matches!(k.slot, Slot::Static) {
                    constructor
                } else {
                    prototype
                };
                define_builtin_data(
                    vm,
                    target,
                    k.member,
                    Value::object(method),
                    Attributes::ORDINARY,
                );
            }
            _ => {}
        }
    }
    define_builtin_data(
        vm,
        prototype,
        PropertyKey::Symbol(TO_STRING_TAG_SYMBOL),
        Value::string("URL".into()),
        Attributes::FUNCTION_METADATA,
    );
}

fn url_receiver<'a>(vm: &'a VmCore, value: &Value) -> Result<Option<(ObjectId, &'a Url)>, VmError> {
    let ValueRef::Object(id) = value.view() else {
        return Ok(None);
    };
    Ok(match vm.heap.get(id)? {
        Object::Url { url, .. } => Some((id, url)),
        _ => None,
    })
}

fn bad_receiver(vm: &mut VmCore, roots: &ActiveRoots<'_>) -> Result<Invocation, VmError> {
    vm.throw_type_error(roots, "URL member called on an incompatible receiver")
}

pub(crate) fn usv_string(text: &JsString) -> String {
    text.to_utf8()
        .unwrap_or_else(|_| String::from_utf16_lossy(&text.to_utf16()))
}

fn string(text: impl AsRef<str>) -> Invocation {
    Invocation::Value(Value::string(text.as_ref().into()))
}

fn stringify(
    vm: &mut VmCore,
    _: &Host,
    roots: &ActiveRoots<'_>,
    _: Vec<Value>,
    this: &Value,
) -> Result<Invocation, VmError> {
    match url_receiver(vm, this)? {
        Some((_, url)) => Ok(string(url.as_str())),
        None => bad_receiver(vm, roots),
    }
}

enum ParseMode {
    Construct(Option<ObjectId>),
    CanParse,
    Parse,
}

fn parse(
    vm: &mut VmCore,
    host: &Host,
    roots: &ActiveRoots<'_>,
    args: &[Value],
    mode: ParseMode,
) -> Result<Invocation, VmError> {
    let roots = &roots.with(&args);
    let Some(input) = args.first() else {
        return vm.throw_type_error(roots, "URL input is required");
    };
    let input = match vm.coerce_string(host, roots, input)? {
        Ok(text) => usv_string(&text),
        Err(error) => return Ok(Invocation::Thrown(error)),
    };
    let explicit_base = if let Some(base) = args
        .get(1)
        .filter(|base| !matches!(base.view(), ValueRef::Undefined))
    {
        let base = match vm.coerce_string(host, roots, base)? {
            Ok(text) => usv_string(&text),
            Err(error) => return Ok(Invocation::Thrown(error)),
        };
        Some(Url::parse(&base))
    } else {
        None
    };
    let parsed = match explicit_base.as_ref() {
        Some(Ok(base)) => Url::options().base_url(Some(base)).parse(&input),
        Some(Err(error)) => Err(*error),
        None => {
            let base = if matches!(mode, ParseMode::Construct(_)) {
                vm.base_url.as_ref()
            } else {
                None
            };
            Url::options().base_url(base).parse(&input)
        }
    };
    if matches!(mode, ParseMode::CanParse) {
        return Ok(Invocation::Value(Value::boolean(parsed.is_ok())));
    }
    let url = match parsed {
        Ok(url) => url,
        Err(_) if matches!(mode, ParseMode::Parse) => return Ok(Invocation::Value(Value::NULL)),
        Err(_) => return vm.throw_type_error(roots, "invalid URL"),
    };
    let prototype = match mode {
        ParseMode::Construct(prototype) => prototype,
        _ => None,
    }
    .unwrap_or_else(|| vm.builtins.get(PROTOTYPE));
    let id = vm.heap.alloc(Object::Url {
        object: PlainObject::new(Some(prototype)),
        url,
        params: None,
    });
    Ok(Invocation::Value(Value::object(id)))
}

pub(crate) fn parse_query(query: &str) -> Vec<(String, String)> {
    form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect()
}

fn serialize_query(pairs: &[(String, String)]) -> String {
    form_urlencoded::Serializer::new(String::new())
        .extend_pairs(
            pairs
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str())),
        )
        .finish()
}

impl VmCore {
    fn refresh_search_params(&mut self, id: ObjectId) -> Result<(), VmError> {
        let Object::Url {
            url,
            params: Some(params),
            ..
        } = self.heap.get(id)?
        else {
            return Ok(());
        };
        let params = *params;
        let updated = parse_query(url.query().unwrap_or_default());
        let Object::UrlSearchParams { pairs, .. } = self.heap.get_mut(params)? else {
            unreachable!()
        };
        *pairs = updated;
        Ok(())
    }

    fn sync_url_query(&mut self, id: ObjectId) -> Result<(), VmError> {
        let Object::UrlSearchParams {
            pairs,
            owner: Some(owner),
            ..
        } = self.heap.get(id)?
        else {
            return Ok(());
        };
        let owner = *owner;
        let query = (!pairs.is_empty()).then(|| serialize_query(pairs));
        let Object::Url { url, .. } = self.heap.get_mut(owner)? else {
            unreachable!()
        };
        url.set_query(query.as_deref());
        Ok(())
    }
}
