use serde::{Deserialize, Serialize};
use url::Url;

use super::body::{self, BodyMode, BodyRecord, get, usv};
use super::headers::{HeadersGuard, HeadersRecord, valid_name};
use crate::builtin::{
    FUNCTION_PARENT, OBJECT_PARENT, define_builtin_data,
    event::install_accessor,
    globals::Owner,
    link_constructor,
    roots::{RootKind, RootSpec},
};
use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::heap::{AbortSignalState, Attributes, Object, PlainObject, TO_STRING_TAG_SYMBOL};
use crate::host::{Host, PlatformServices};
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::runtime::Invocation;
use crate::value::{ObjectId, PropertyKey, Value, ValueRef};
use crate::vm::VmCore;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RequestRecord {
    pub(crate) url: Url,
    pub(crate) method: String,
    pub(crate) headers: ObjectId,
    pub(crate) signal: ObjectId,
    pub(crate) body: Option<BodyRecord>,
    pub(crate) destination: String,
    pub(crate) referrer: String,
    pub(crate) referrer_policy: String,
    pub(crate) mode: String,
    pub(crate) credentials: String,
    pub(crate) cache: String,
    pub(crate) redirect: String,
    pub(crate) integrity: String,
    pub(crate) keepalive: bool,
}

const NAME: &str = "Request";
const ROOT: &str = "request";
const PROTOTYPE: &str = "request.prototype";
const CONSTRUCTOR: IntrinsicKey = IntrinsicKey::new(NAME, Slot::Global, "");
const ROOTS: &[RootSpec] = &[
    RootSpec::new(ROOT, RootKind::Native(CONSTRUCTOR), FUNCTION_PARENT).fixed(PROTOTYPE),
    RootSpec::new(PROTOTYPE, RootKind::Plain, OBJECT_PARENT),
];

macro_rules! complete {
    ($value:expr) => {
        match $value? {
            Ok(value) => value,
            Err(error) => return Ok(Invocation::Thrown(error)),
        }
    };
}

macro_rules! definitions {
    ($($prop:literal => $getter:expr),+; $($method:literal => $mode:ident),+) => {
        const GETTERS: &[&str] = &[$($prop),+];
        const METHODS: &[&str] = &["clone", $($method),+];
        impl<P: crate::host::PlatformServices> BuiltinTable<P> {
            const INTRINSICS: &[IntrinsicDef<P>] = &[
                IntrinsicDef::constructor(CONSTRUCTOR, 1, |vm, _, roots, _, _| vm.throw_type_error(roots, "Request requires new"),
                    |vm, host, roots, args, prototype| construct(vm, host, roots, &args, prototype)),
                IntrinsicDef::call(IntrinsicKey::new(NAME, Slot::Prototype, "clone"), 0,
                    |vm, host, roots, _, this| clone_request(vm, host, roots, this)),
                $(IntrinsicDef::call(IntrinsicKey::new(NAME, Slot::PrototypeGetter, $prop), 0,
                    |vm, _, roots, _, this| read(vm, roots, this, $getter))),+
                ,$(IntrinsicDef::call(IntrinsicKey::new(NAME, Slot::Prototype, $method), 0,
                    |vm, host, roots, _, this| consume(vm, host, roots, this, BodyMode::$mode))),+
            ];
        }
    };
}

definitions! {
    "method" => |d, _| string(&d.method), "url" => |d, _| string(d.url.as_str()),
    "headers" => |d, _| Value::object(d.headers), "signal" => |d, _| Value::object(d.signal),
    "destination" => |d, _| string(&d.destination), "referrer" => |d, _| string(&d.referrer),
    "referrerPolicy" => |d, _| string(&d.referrer_policy), "mode" => |d, _| string(&d.mode),
    "credentials" => |d, _| string(&d.credentials), "cache" => |d, _| string(&d.cache),
    "redirect" => |d, _| string(&d.redirect), "integrity" => |d, _| string(&d.integrity),
    "keepalive" => |d, _| Value::boolean(d.keepalive), "isHistoryNavigation" => |_, _| Value::boolean(false),
    "isReloadNavigation" => |_, _| Value::boolean(false), "duplex" => |_, _| string("half"),
    "body" => |d, _| d.body.as_ref().map_or(Value::NULL, |b| Value::object(b.stream)),
    "bodyUsed" => |d, vm| Value::boolean(d.body.as_ref().is_some_and(|b| b.used(vm)));
    "arrayBuffer" => ArrayBuffer, "blob" => Blob, "bytes" => Bytes, "text" => Text, "json" => Json, "formData" => FormData
}

impl<P: crate::host::PlatformServices> BuiltinTable<P> {
    pub(crate) const REQUEST_OWNER: Owner<P> =
        Owner::new(ROOT, ROOTS, &[Self::INTRINSICS], Some(install));
}

fn install(vm: &mut VmCore, _: &Value) {
    install_interface(vm, NAME, ROOT, PROTOTYPE, GETTERS, METHODS);
}

pub(super) fn install_interface(
    vm: &mut VmCore,
    name: &'static str,
    root: &'static str,
    proto: &'static str,
    getters: &[&'static str],
    methods: &[&'static str],
) {
    let prototype = vm.builtins.get(proto);
    link_constructor(vm, vm.builtins.get(root), prototype);
    define_builtin_data(
        vm,
        prototype,
        PropertyKey::Symbol(TO_STRING_TAG_SYMBOL),
        string(name),
        Attributes::FUNCTION_METADATA,
    );
    let name_owner = name;
    for &name in getters {
        let getter = vm.alloc_intrinsic(IntrinsicKey::new(name_owner, Slot::PrototypeGetter, name));
        install_accessor(vm, prototype, name, getter, None, true)
            .expect("Fetch interface accessor");
    }
    for &name in methods {
        let method =
            Value::object(vm.alloc_intrinsic(IntrinsicKey::new(name_owner, Slot::Prototype, name)));
        define_builtin_data(vm, prototype, name, method, Attributes::ORDINARY);
    }
}

pub(super) fn string(text: &str) -> Value {
    Value::string(text.into())
}
pub(super) fn present(value: &Value) -> bool {
    !matches!(value.view(), ValueRef::Undefined)
}
pub(super) fn nullish(value: &Value) -> bool {
    matches!(value.view(), ValueRef::Undefined | ValueRef::Null)
}

pub(super) fn headers(vm: &VmCore, id: ObjectId) -> Result<&HeadersRecord, VmError> {
    let Object::Headers { data, .. } = vm.heap.get(id)? else {
        unreachable!()
    };
    Ok(data)
}

pub(super) fn mime(vm: &VmCore, headers_id: ObjectId) -> Result<String, VmError> {
    let value = headers(vm, headers_id)?
        .get(b"content-type")
        .unwrap_or_default();
    let text = String::from_utf8_lossy(&value);
    let essence = text.split(';').next().unwrap_or_default().trim();
    if let Some((ty, subtype)) = essence.split_once('/')
        && valid_name(ty.as_bytes())
        && valid_name(subtype.as_bytes())
        && value.iter().all(|byte| (32..=126).contains(byte))
    {
        return Ok(text.into_owned());
    }
    Ok(String::new())
}

pub(super) fn apply_content_type(headers: &mut HeadersRecord, ty: Option<String>) {
    if let Some(ty) = ty
        && headers.get(b"content-type").is_none()
    {
        headers
            .list
            .push((b"content-type".to_vec(), ty.into_bytes()));
    }
}

fn record<'a>(
    vm: &'a VmCore,
    value: &Value,
) -> Result<Option<(ObjectId, &'a RequestRecord)>, VmError> {
    let ValueRef::Object(id) = value.view() else {
        return Ok(None);
    };
    Ok(match vm.heap.get(id)? {
        Object::Request { data, .. } => Some((id, data)),
        _ => None,
    })
}

fn read(
    vm: &mut VmCore,
    roots: &ActiveRoots<'_>,
    this: &Value,
    getter: impl FnOnce(&RequestRecord, &VmCore) -> Value,
) -> Result<Invocation, VmError> {
    let Some((_, data)) = record(vm, this)? else {
        return vm.throw_type_error(roots, "Expected a Request receiver");
    };
    Ok(Invocation::Value(getter(data, vm)))
}

fn consume<P: PlatformServices>(
    vm: &mut VmCore,
    host: &Host<P>,
    roots: &ActiveRoots<'_>,
    this: &Value,
    mode: BodyMode,
) -> Result<Invocation, VmError> {
    let Some((_, data)) = record(vm, this)? else {
        return body::rejected(vm, roots, "Expected a Request receiver");
    };
    let mime = mime(vm, data.headers)?;
    body::consume(vm, host, roots, data.body.clone(), mode, mime)
}

fn dependent_signal(vm: &mut VmCore, source: Option<ObjectId>) -> Result<ObjectId, VmError> {
    let mut state = AbortSignalState {
        dependent: true,
        ..Default::default()
    };
    if let Some(source) = source {
        let original = vm.abort_signal_state(source)?.expect("checked signal");
        if let Some(reason) = &original.reason {
            state.reason = Some(reason.clone());
        } else if original.dependent {
            state.sources.clone_from(&original.sources);
        } else {
            state.sources.push(source);
        }
    }
    let dependent = vm.alloc_abort_signal(state);
    let len = vm.abort_signal_state(dependent)?.unwrap().sources.len();
    for index in 0..len {
        let source = vm.abort_signal_state(dependent)?.unwrap().sources[index];
        vm.abort_signal_state_mut(source)?
            .unwrap()
            .dependents
            .push(dependent);
    }
    Ok(dependent)
}

fn parse_url(vm: &VmCore, text: &str) -> Option<Url> {
    Url::options()
        .base_url(vm.base_url.as_ref())
        .parse(text)
        .ok()
}

fn same_origin(first: &Url, second: &Url) -> bool {
    let first = first.origin();
    !matches!(first, url::Origin::Opaque(_)) && first == second.origin()
}

const INIT: &[&str] = &[
    "body",
    "cache",
    "credentials",
    "duplex",
    "headers",
    "integrity",
    "keepalive",
    "method",
    "mode",
    "redirect",
    "referrer",
    "referrerPolicy",
    "signal",
    "window",
];

fn enum_string<P: PlatformServices>(
    vm: &mut VmCore,
    host: &Host<P>,
    roots: &ActiveRoots<'_>,
    value: &Value,
    names: &[&str],
) -> body::Completion<String> {
    let text = match vm.coerce_string(host, roots, value)? {
        Ok(text) => text,
        Err(error) => return Ok(Err(error)),
    };
    let value = crate::builtin::url_api::usv_string(&text);
    if names.contains(&value.as_str()) {
        Ok(Ok(value))
    } else {
        Ok(Err(vm.type_error(roots, "Invalid RequestInit enum value")?))
    }
}

impl VmCore {
    pub(crate) fn request_from_fetch<P: PlatformServices>(
        &mut self,
        host: &Host<P>,
        roots: &ActiveRoots<'_>,
        input: &Value,
        init: &Value,
    ) -> body::Completion<ObjectId> {
        let args = [input.clone(), init.clone()];
        let roots = &roots.with(&args);
        Ok(match construct(self, host, roots, &args, None)? {
            Invocation::Value(value) => match value.view() {
                ValueRef::Object(id) => Ok(id),
                _ => unreachable!(),
            },
            Invocation::Thrown(error) => Err(error),
        })
    }
}

fn construct<P: PlatformServices>(
    vm: &mut VmCore,
    host: &Host<P>,
    roots: &ActiveRoots<'_>,
    args: &[Value],
    prototype: Option<ObjectId>,
) -> Result<Invocation, VmError> {
    let roots = &roots.with(&prototype);
    let Some(input) = args.first() else {
        return vm.throw_type_error(roots, "Request requires an input");
    };
    let existing = record(vm, input)?.map(|(_, data)| data.clone());
    let url = if let Some(data) = &existing {
        data.url.clone()
    } else {
        let text = complete!(usv(vm, host, roots, input));
        let Some(url) = parse_url(vm, &text) else {
            return vm.throw_type_error(roots, "Invalid Request URL");
        };
        if !url.username().is_empty() || url.password().is_some() {
            return vm.throw_type_error(roots, "Request URL includes credentials");
        }
        url
    };
    let undefined = Value::UNDEFINED;
    let init = args.get(1).unwrap_or(&undefined);
    let mut values = Vec::with_capacity(INIT.len());
    let mut strings = Vec::with_capacity(INIT.len());
    if !nullish(init) && !matches!(init.view(), ValueRef::Object(_)) {
        return vm.throw_type_error(roots, "Request init must be a dictionary");
    }
    for &name in INIT {
        let roots = &roots.with(&values);
        let value = if nullish(init) {
            Value::UNDEFINED
        } else {
            complete!(get(vm, host, roots, init, name))
        };
        let roots = &roots.with(&value);
        let text = if present(&value) {
            match name {
                "cache" => Some(complete!(enum_string(
                    vm,
                    host,
                    roots,
                    &value,
                    &[
                        "default",
                        "no-store",
                        "reload",
                        "no-cache",
                        "force-cache",
                        "only-if-cached"
                    ]
                ))),
                "credentials" => Some(complete!(enum_string(
                    vm,
                    host,
                    roots,
                    &value,
                    &["omit", "same-origin", "include"]
                ))),
                "duplex" => Some(complete!(enum_string(vm, host, roots, &value, &["half"]))),
                "mode" => Some(complete!(enum_string(
                    vm,
                    host,
                    roots,
                    &value,
                    &["navigate", "same-origin", "no-cors", "cors"]
                ))),
                "redirect" => Some(complete!(enum_string(
                    vm,
                    host,
                    roots,
                    &value,
                    &["follow", "error", "manual"]
                ))),
                "referrerPolicy" => Some(complete!(enum_string(
                    vm,
                    host,
                    roots,
                    &value,
                    &[
                        "",
                        "no-referrer",
                        "no-referrer-when-downgrade",
                        "same-origin",
                        "origin",
                        "strict-origin",
                        "origin-when-cross-origin",
                        "strict-origin-when-cross-origin",
                        "unsafe-url"
                    ]
                ))),
                "integrity" | "method" | "referrer" => {
                    Some(complete!(usv(vm, host, roots, &value)))
                }
                "signal" if !nullish(&value) => {
                    if !matches!(value.view(), ValueRef::Object(id) if vm.abort_signal_state(id)?.is_some())
                    {
                        return vm.throw_type_error(roots, "Request signal must be an AbortSignal");
                    }
                    None
                }
                _ => None,
            }
        } else {
            None
        };
        strings.push(text);
        values.push(value);
    }
    let roots = &roots.with(&values);
    let index = |name| INIT.iter().position(|key| *key == name).unwrap();
    let value = |name| &values[index(name)];
    let text = |name| strings[index(name)].clone();
    let mut data = existing.clone().unwrap_or_else(|| RequestRecord {
        url,
        method: "GET".into(),
        headers: 0,
        signal: 0,
        body: None,
        destination: String::new(),
        referrer: "about:client".into(),
        referrer_policy: String::new(),
        mode: "cors".into(),
        credentials: "same-origin".into(),
        cache: "default".into(),
        redirect: "follow".into(),
        integrity: String::new(),
        keepalive: false,
    });
    if present(value("window")) && !nullish(value("window")) {
        return vm.throw_type_error(roots, "Request window must be null");
    }
    if values.iter().any(present) {
        if data.mode == "navigate" {
            data.mode = "same-origin".into();
        }
        data.referrer = "about:client".into();
        data.referrer_policy.clear();
    }
    if let Some(referrer) = text("referrer") {
        data.referrer = if referrer.is_empty() {
            referrer
        } else {
            let Some(url) = parse_url(vm, &referrer) else {
                return vm.throw_type_error(roots, "Invalid Request referrer");
            };
            if url.as_str() == "about:client"
                || !vm
                    .base_url
                    .as_ref()
                    .is_some_and(|base| same_origin(base, &url))
            {
                "about:client".into()
            } else {
                url.into()
            }
        };
    }
    if let Some(policy) = text("referrerPolicy") {
        data.referrer_policy = policy;
    }
    if let Some(mode) = text("mode") {
        if mode == "navigate" {
            return vm.throw_type_error(roots, "Request mode cannot be navigate");
        }
        data.mode = mode;
    }
    if let Some(credentials) = text("credentials") {
        data.credentials = credentials;
    }
    if let Some(cache) = text("cache") {
        data.cache = cache;
    }
    if data.cache == "only-if-cached" && data.mode != "same-origin" {
        return vm.throw_type_error(roots, "only-if-cached requires same-origin mode");
    }
    if let Some(redirect) = text("redirect") {
        data.redirect = redirect;
    }
    if let Some(integrity) = text("integrity") {
        data.integrity = integrity;
    }
    if present(value("keepalive")) {
        data.keepalive = value("keepalive").truthy();
    }
    if let Some(method) = text("method") {
        if !valid_name(method.as_bytes()) {
            return vm.throw_type_error(roots, "Invalid Request method");
        }
        let upper = method.to_ascii_uppercase();
        if matches!(upper.as_str(), "CONNECT" | "TRACE" | "TRACK") {
            return vm.throw_type_error(roots, "Forbidden Request method");
        }
        data.method = if matches!(
            upper.as_str(),
            "DELETE" | "GET" | "HEAD" | "OPTIONS" | "POST" | "PUT"
        ) {
            upper
        } else {
            method
        };
    }
    let guard = if data.mode == "no-cors" {
        if !matches!(data.method.as_str(), "GET" | "HEAD" | "POST") {
            return vm.throw_type_error(roots, "Method not allowed in no-cors mode");
        }
        HeadersGuard::RequestNoCors
    } else {
        HeadersGuard::Request
    };
    let mut header_data = if present(value("headers")) {
        complete!(vm.headers_from_init(host, roots, value("headers"), guard))
    } else if let Some(original) = &existing {
        let mut copy = headers(vm, original.headers)?.clone();
        copy.guard = guard;
        copy
    } else {
        HeadersRecord::new(guard)
    };
    let init_body = value("body");
    if matches!(data.method.as_str(), "GET" | "HEAD")
        && (!nullish(init_body) || data.body.is_some())
    {
        return vm.throw_type_error(roots, "GET or HEAD request cannot have a body");
    }
    if !nullish(init_body) {
        let (body, mime) = complete!(vm.extract_body(host, roots, init_body));
        if body.source.is_none()
            && (data.keepalive
                || !present(value("duplex"))
                || !matches!(data.mode.as_str(), "same-origin" | "cors"))
        {
            return vm.throw_type_error(roots, "Streaming Request body requires duplex half, without keepalive, in cors or same-origin mode");
        }
        apply_content_type(&mut header_data, mime);
        data.body = Some(body);
    } else if let Some((_, original)) = record(vm, input)?
        && let Some(source_body) = original.body.clone()
    {
        data.body = Some(complete!(vm.transfer_fetch_body(host, roots, &source_body)));
    }
    let signal = match value("signal").view() {
        ValueRef::Undefined => existing.as_ref().map(|data| data.signal),
        ValueRef::Object(id) => Some(id),
        _ => None,
    };
    data.signal = dependent_signal(vm, signal)?;
    data.headers = vm.alloc_headers(header_data);
    let id = vm.heap.alloc(Object::Request {
        object: PlainObject::new(Some(
            prototype.unwrap_or_else(|| vm.builtins.get(PROTOTYPE)),
        )),
        data,
    });
    Ok(Invocation::Value(Value::object(id)))
}

fn clone_request<P: PlatformServices>(
    vm: &mut VmCore,
    host: &Host<P>,
    roots: &ActiveRoots<'_>,
    this: &Value,
) -> Result<Invocation, VmError> {
    let Some((id, data)) = record(vm, this)? else {
        return vm.throw_type_error(roots, "Expected a Request receiver");
    };
    let mut copy = data.clone();
    if let Some(body) = &copy.body {
        let (first, second) = complete!(vm.tee_fetch_body(host, roots, body));
        let Object::Request { data, .. } = vm.heap.get_mut(id)? else {
            unreachable!()
        };
        data.body = Some(first);
        copy.body = Some(second);
    }
    copy.signal = dependent_signal(vm, Some(copy.signal))?;
    copy.headers = vm.alloc_headers(headers(vm, copy.headers)?.clone());
    Ok(Invocation::Value(Value::object(vm.heap.alloc(
        Object::Request {
            object: PlainObject::new(Some(vm.builtins.get(PROTOTYPE))),
            data: copy,
        },
    ))))
}

pub(crate) struct BuiltinTable<P>(std::marker::PhantomData<P>);
