use serde::{Deserialize, Serialize};
use url::Url;

use super::body::{self, BodyMode, BodyRecord, get};
use super::headers::{HeadersGuard, HeadersRecord, valid_normalized_value};
use super::request::{
    apply_content_type, headers, install_interface, mime, nullish, present, string,
};
use crate::builtin::{
    FUNCTION_PARENT, OBJECT_PARENT, define_builtin_data,
    globals::Owner,
    roots::{RootKind, RootSpec},
};
use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::heap::{Attributes, Object, PlainObject};
use crate::host::Host;
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::runtime::Invocation;
use crate::value::{ObjectId, Value, ValueRef};
use crate::vm::VmCore;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) enum ResponseType {
    Default,
    Error,
    Basic,
    Cors,
    Opaque,
    OpaqueRedirect,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResponseRecord {
    pub(crate) kind: ResponseType,
    pub(crate) urls: Vec<Url>,
    pub(crate) status: u16,
    pub(crate) status_text: String,
    pub(crate) headers: ObjectId,
    pub(crate) body: Option<BodyRecord>,
}

const NAME: &str = "Response";
const ROOT: &str = "response";
const PROTOTYPE: &str = "response.prototype";
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
        const INTRINSICS: &[IntrinsicDef] = &[
            IntrinsicDef::constructor(CONSTRUCTOR, 0, |vm, _, roots, _, _| vm.throw_type_error(roots, "Response requires new"),
                |vm, host, roots, args, prototype| construct(vm, host, roots, &args, prototype)),
            IntrinsicDef::call(IntrinsicKey::new(NAME, Slot::Static, "error"), 0, |vm, _, _, _, _| error(vm)),
            IntrinsicDef::call(IntrinsicKey::new(NAME, Slot::Static, "redirect"), 1, |vm, host, roots, args, _| redirect(vm, host, roots, &args)),
            IntrinsicDef::call(IntrinsicKey::new(NAME, Slot::Static, "json"), 1, |vm, host, roots, args, _| json(vm, host, roots, &args)),
            IntrinsicDef::call(IntrinsicKey::new(NAME, Slot::Prototype, "clone"), 0,
                |vm, host, roots, _, this| clone_response(vm, host, roots, this)),
            $(IntrinsicDef::call(IntrinsicKey::new(NAME, Slot::PrototypeGetter, $prop), 0,
                |vm, _, roots, _, this| read(vm, roots, this, $getter))),+
            ,$(IntrinsicDef::call(IntrinsicKey::new(NAME, Slot::Prototype, $method), 0,
                |vm, host, roots, _, this| consume(vm, host, roots, this, BodyMode::$mode))),+
        ];
    };
}

definitions! {
    "type" => |d, _| string(match d.kind { ResponseType::Default => "default", ResponseType::Error => "error", ResponseType::Basic => "basic", ResponseType::Cors => "cors", ResponseType::Opaque => "opaque", ResponseType::OpaqueRedirect => "opaqueredirect" }),
    "url" => |d, _| d.urls.last().map_or_else(|| string(""), |url| string(url.as_str().split('#').next().unwrap_or_default())),
    "redirected" => |d, _| Value::boolean(d.urls.len() > 1),
    "status" => |d, _| Value::number(d.status.into()),
    "ok" => |d, _| Value::boolean((200..=299).contains(&d.status)),
    "statusText" => |d, _| string(&d.status_text), "headers" => |d, _| Value::object(d.headers),
    "body" => |d, _| d.body.as_ref().map_or(Value::NULL, |b| Value::object(b.stream)),
    "bodyUsed" => |d, vm| Value::boolean(d.body.as_ref().is_some_and(|b| b.used(vm)));
    "arrayBuffer" => ArrayBuffer, "blob" => Blob, "bytes" => Bytes, "text" => Text, "json" => Json, "formData" => FormData
}

pub(crate) const RESPONSE_OWNER: Owner = Owner::new(ROOT, ROOTS, &[INTRINSICS], Some(install));

fn install(vm: &mut VmCore, _: &Value) {
    install_interface(vm, NAME, ROOT, PROTOTYPE, GETTERS, METHODS);
    let constructor = vm.builtins.get(ROOT);
    for name in ["error", "redirect", "json"] {
        let function =
            Value::object(vm.alloc_intrinsic(IntrinsicKey::new(NAME, Slot::Static, name)));
        define_builtin_data(vm, constructor, name, function, Attributes::ORDINARY);
    }
}

fn record<'a>(
    vm: &'a VmCore,
    value: &Value,
) -> Result<Option<(ObjectId, &'a ResponseRecord)>, VmError> {
    let ValueRef::Object(id) = value.view() else {
        return Ok(None);
    };
    Ok(match vm.heap.get(id)? {
        Object::Response { data, .. } => Some((id, data)),
        _ => None,
    })
}

fn read(
    vm: &mut VmCore,
    roots: &ActiveRoots<'_>,
    this: &Value,
    getter: impl FnOnce(&ResponseRecord, &VmCore) -> Value,
) -> Result<Invocation, VmError> {
    let Some((_, data)) = record(vm, this)? else {
        return vm.throw_type_error(roots, "Expected a Response receiver");
    };
    Ok(Invocation::Value(getter(data, vm)))
}

fn consume(
    vm: &mut VmCore,
    host: &Host,
    roots: &ActiveRoots<'_>,
    this: &Value,
    mode: BodyMode,
) -> Result<Invocation, VmError> {
    let Some((_, data)) = record(vm, this)? else {
        return body::rejected(vm, roots, "Expected a Response receiver");
    };
    let mime = mime(vm, data.headers)?;
    body::consume(vm, host, roots, data.body.clone(), mode, mime)
}

impl VmCore {
    pub(crate) fn alloc_response(
        &mut self,
        prototype: Option<ObjectId>,
        data: ResponseRecord,
    ) -> ObjectId {
        let prototype = prototype.unwrap_or_else(|| self.builtins.get(PROTOTYPE));
        self.heap.alloc(Object::Response {
            object: PlainObject::new(Some(prototype)),
            data,
        })
    }

    pub(crate) fn offline_response(
        &mut self,
        url: Url,
        status: u16,
        content_type: Option<String>,
        body: Option<BodyRecord>,
    ) -> ObjectId {
        let mut headers = HeadersRecord::new(HeadersGuard::Immutable);
        apply_content_type(&mut headers, content_type);
        let headers = self.alloc_headers(headers);
        self.alloc_response(
            None,
            ResponseRecord {
                kind: ResponseType::Basic,
                urls: vec![url],
                status,
                status_text: if status == 200 {
                    "OK".into()
                } else {
                    String::new()
                },
                headers,
                body,
            },
        )
    }
}

fn status(
    vm: &mut VmCore,
    host: &Host,
    roots: &ActiveRoots<'_>,
    value: &Value,
    default: u16,
) -> body::Completion<u16> {
    if !present(value) {
        return Ok(Ok(default));
    }
    Ok(vm.coerce_number(host, roots, value)?.map(|number| {
        if number.is_finite() {
            number.trunc().rem_euclid(65536.0) as u16
        } else {
            0
        }
    }))
}

fn init(
    vm: &mut VmCore,
    host: &Host,
    roots: &ActiveRoots<'_>,
    value: &Value,
) -> body::Completion<(u16, String, Value)> {
    if nullish(value) {
        return Ok(Ok((200, String::new(), Value::UNDEFINED)));
    }
    if !matches!(value.view(), ValueRef::Object(_)) {
        return Ok(Err(
            vm.type_error(roots, "Response init must be a dictionary")?
        ));
    }
    macro_rules! convert {
        ($expr:expr) => {
            match $expr? {
                Ok(value) => value,
                Err(error) => return Ok(Err(error)),
            }
        };
    }
    let headers = convert!(get(vm, host, roots, value, "headers"));
    let roots = &roots.with(&headers);
    let number = convert!(get(vm, host, roots, value, "status"));
    let status = convert!(status(vm, host, roots, &number, 200));
    let text = convert!(get(vm, host, roots, value, "statusText"));
    let text = if present(&text) {
        let string = convert!(vm.coerce_string(host, roots, &text));
        if string.units().any(|unit| unit > 255) {
            return Ok(Err(
                vm.type_error(roots, "Response status text must be a ByteString")?
            ));
        }
        crate::builtin::url_api::usv_string(&string)
    } else {
        String::new()
    };
    Ok(Ok((status, text, headers)))
}

fn initialize(
    vm: &mut VmCore,
    host: &Host,
    roots: &ActiveRoots<'_>,
    proto: Option<ObjectId>,
    options: (u16, String, Value),
    body: Option<(BodyRecord, Option<String>)>,
) -> Result<Invocation, VmError> {
    let (status, status_text, raw_headers) = options;
    let roots = &roots.with(&raw_headers);
    if !(200..=599).contains(&status) {
        return Ok(Invocation::Thrown(
            vm.make_range_error(roots, "Response status outside 200–599")?,
        ));
    }
    if status_text
        .chars()
        .any(|ch| !matches!(ch as u32, 9 | 32..=126 | 128..=255))
    {
        return vm.throw_type_error(roots, "Invalid Response status text");
    }
    let mut header_data =
        complete!(vm.headers_from_init(host, roots, &raw_headers, HeadersGuard::Response));
    let body = if let Some((body, mime)) = body {
        if matches!(status, 204 | 205 | 304) {
            return vm.throw_type_error(roots, "Null body status cannot have a body");
        }
        apply_content_type(&mut header_data, mime);
        Some(body)
    } else {
        None
    };
    let headers = vm.alloc_headers(header_data);
    let data = ResponseRecord {
        kind: ResponseType::Default,
        urls: Vec::new(),
        status,
        status_text,
        headers,
        body,
    };
    Ok(Invocation::Value(Value::object(
        vm.alloc_response(proto, data),
    )))
}

fn construct(
    vm: &mut VmCore,
    host: &Host,
    roots: &ActiveRoots<'_>,
    args: &[Value],
    prototype: Option<ObjectId>,
) -> Result<Invocation, VmError> {
    let roots = &roots.with(&prototype);
    let undefined = Value::UNDEFINED;
    let value = args.first().unwrap_or(&undefined);
    let body = if nullish(value) {
        None
    } else {
        Some(complete!(vm.extract_body(host, roots, value)))
    };
    let stream = body.as_ref().map(|(body, _)| body.stream);
    let roots = &roots.with(&stream);
    let roots = &roots.with(&args);
    let options = complete!(init(vm, host, roots, args.get(1).unwrap_or(&undefined)));
    initialize(vm, host, roots, prototype, options, body)
}

fn error(vm: &mut VmCore) -> Result<Invocation, VmError> {
    Ok(Invocation::Value(Value::object(vm.response_error())))
}

impl VmCore {
    pub(crate) fn response_error(&mut self) -> ObjectId {
        let headers = self.alloc_headers(HeadersRecord::new(HeadersGuard::Immutable));
        let data = ResponseRecord {
            kind: ResponseType::Error,
            urls: Vec::new(),
            status: 0,
            status_text: String::new(),
            headers,
            body: None,
        };
        self.alloc_response(None, data)
    }
}

fn redirect(
    vm: &mut VmCore,
    host: &Host,
    roots: &ActiveRoots<'_>,
    args: &[Value],
) -> Result<Invocation, VmError> {
    let Some(url) = args.first() else {
        return vm.throw_type_error(roots, "Response.redirect requires a URL");
    };
    let url = complete!(body::usv(vm, host, roots, url));
    let status = complete!(status(
        vm,
        host,
        roots,
        args.get(1).unwrap_or(&Value::UNDEFINED),
        302
    ));
    let Ok(url) = Url::options().base_url(vm.base_url.as_ref()).parse(&url) else {
        return vm.throw_type_error(roots, "Invalid redirect URL");
    };
    if !matches!(status, 301 | 302 | 303 | 307 | 308) {
        return Ok(Invocation::Thrown(
            vm.make_range_error(roots, "Invalid redirect status")?,
        ));
    }
    let location: Vec<_> = url.as_str().as_bytes().to_vec();
    if !valid_normalized_value(&location) {
        return vm.throw_type_error(roots, "Invalid Location header");
    }
    let headers = vm.alloc_headers(HeadersRecord {
        list: vec![(b"location".to_vec(), location)],
        guard: HeadersGuard::Immutable,
    });
    let data = ResponseRecord {
        kind: ResponseType::Default,
        urls: Vec::new(),
        status,
        status_text: String::new(),
        headers,
        body: None,
    };
    Ok(Invocation::Value(Value::object(
        vm.alloc_response(None, data),
    )))
}

fn json(
    vm: &mut VmCore,
    host: &Host,
    roots: &ActiveRoots<'_>,
    args: &[Value],
) -> Result<Invocation, VmError> {
    let Some(value) = args.first() else {
        return vm.throw_type_error(roots, "Response.json requires a value");
    };
    let options = complete!(init(
        vm,
        host,
        roots,
        args.get(1).unwrap_or(&Value::UNDEFINED)
    ));
    let roots = &roots.with(&options.2);
    let json = match body::json_call(vm, host, roots, "stringify", value.clone())? {
        Invocation::Value(value) => value,
        thrown => return Ok(thrown),
    };
    let ValueRef::String(json) = json.view() else {
        return vm.throw_type_error(roots, "Value is not JSON-serializable");
    };
    let body = vm.body_from_bytes(crate::builtin::url_api::usv_string(json).into_bytes())?;
    let stream = body.stream;
    let roots = &roots.with(&stream);
    initialize(
        vm,
        host,
        roots,
        None,
        options.clone(),
        Some((body, Some("application/json".into()))),
    )
}

fn clone_response(
    vm: &mut VmCore,
    host: &Host,
    roots: &ActiveRoots<'_>,
    this: &Value,
) -> Result<Invocation, VmError> {
    let Some((id, data)) = record(vm, this)? else {
        return vm.throw_type_error(roots, "Expected a Response receiver");
    };
    let mut copy = data.clone();
    if let Some(body) = &copy.body {
        let (first, second) = complete!(vm.tee_fetch_body(host, roots, body));
        let Object::Response { data, .. } = vm.heap.get_mut(id)? else {
            unreachable!()
        };
        data.body = Some(first);
        copy.body = Some(second);
    }
    copy.headers = vm.alloc_headers(headers(vm, copy.headers)?.clone());
    Ok(Invocation::Value(Value::object(
        vm.alloc_response(None, copy),
    )))
}
