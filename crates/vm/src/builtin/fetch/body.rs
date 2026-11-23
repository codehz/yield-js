use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::blob::{BLOB_PROTOTYPE, BlobData, FileData, alloc_file};
use super::form_data::{FormDataRecord, FormDataValue};
use crate::builtin::binary::{
    ARRAY_BUFFER_PROTOTYPE, MAX_BYTE_LENGTH,
    text::{Source, source_bytes},
};
use crate::builtin::url_api::usv_string;
use crate::error::VmError;
use crate::gc::ActiveRoots;
use crate::heap::{
    ArrayBufferObject, ElementKind, Object, PlainObject, ReactionJob, TypedArrayObject,
};
use crate::host::Host;
use crate::intrinsic::{IntrinsicKey, Slot};
use crate::runtime::{Arguments, Invocation, Settlement};
use crate::value::{ObjectId, PropertyKeyRef, Value, ValueRef};
use crate::vm::Vm;

pub(crate) type Completion<T> = Result<Result<T, Value>, VmError>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct BodyRecord {
    pub(crate) stream: ObjectId,
    pub(crate) source: Option<Arc<Vec<u8>>>,
    pub(crate) length: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BodyRead {
    pub(crate) reader: ObjectId,
    pub(crate) promise: ObjectId,
    /// Read promise this step is waiting on; the output promise keeps it alive.
    pub(crate) active: ObjectId,
    pub(crate) mode: BodyMode,
    pub(crate) chunks: Vec<Vec<u8>>,
    pub(crate) total: u64,
    pub(crate) mime: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) enum BodyMode {
    ArrayBuffer,
    Blob,
    Bytes,
    Text,
    Json,
    FormData,
}

impl BodyRecord {
    pub(crate) fn unusable(&self, vm: &Vm) -> Result<bool, VmError> {
        Ok(
            matches!(vm.heap.get(self.stream)?, Object::ReadableStream(stream) if stream.disturbed || stream.reader.is_some()),
        )
    }

    pub(super) fn used(&self, vm: &Vm) -> bool {
        matches!(vm.heap.get(self.stream), Ok(Object::ReadableStream(stream)) if stream.disturbed)
    }
}

pub(super) fn get(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    obj: &Value,
    name: &str,
) -> Completion<Value> {
    Ok(
        match vm.read_property(host, roots, obj, PropertyKeyRef::String(name))? {
            Invocation::Value(value) => Ok(value),
            Invocation::Thrown(error) => Err(error),
        },
    )
}

pub(super) fn usv(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    value: &Value,
) -> Completion<String> {
    Ok(vm
        .coerce_string(host, roots, value)?
        .map(|value| usv_string(&value)))
}

pub(super) fn json_call(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    name: &'static str,
    value: Value,
) -> Result<Invocation, VmError> {
    let function = Value::object(vm.alloc_intrinsic(IntrinsicKey::new("JSON", Slot::Static, name)));
    vm.invoke(
        host,
        roots,
        &function,
        Arguments::Values(&[value]),
        &Value::UNDEFINED,
    )
}

impl Vm {
    pub(crate) fn body_from_bytes(&mut self, bytes: Vec<u8>) -> Result<BodyRecord, VmError> {
        let stream = self.readable_from_bytes(bytes.clone())?;
        Ok(BodyRecord {
            stream,
            length: Some(bytes.len() as u64),
            source: Some(Arc::new(bytes)),
        })
    }

    pub(crate) fn extract_body(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        value: &Value,
    ) -> Completion<(BodyRecord, Option<String>)> {
        if let ValueRef::Object(id) = value.view() {
            match self.heap.get(id)? {
                Object::ReadableStream(_) => {
                    let body = BodyRecord {
                        stream: id,
                        length: None,
                        source: None,
                    };
                    return if body.unusable(self)? {
                        Ok(Err(
                            self.type_error(roots, "Body stream is disturbed or locked")?
                        ))
                    } else {
                        Ok(Ok((body, None)))
                    };
                }
                Object::Blob { data, .. }
                | Object::File {
                    data: FileData { blob: data, .. },
                    ..
                } => {
                    let bytes = data.as_bytes().to_vec();
                    let mime = (!data.mime_type.is_empty()).then(|| data.mime_type.clone());
                    return Ok(Ok((self.body_from_bytes(bytes)?, mime)));
                }
                Object::UrlSearchParams { pairs, .. } => {
                    let bytes = url::form_urlencoded::Serializer::new(String::new())
                        .extend_pairs(pairs)
                        .finish()
                        .into_bytes();
                    return Ok(Ok((
                        self.body_from_bytes(bytes)?,
                        Some("application/x-www-form-urlencoded;charset=UTF-8".into()),
                    )));
                }
                Object::FormData { data, .. } => {
                    let (bytes, boundary) = encode_form(self, data)?;
                    return Ok(Ok((
                        self.body_from_bytes(bytes)?,
                        Some(format!("multipart/form-data; boundary={boundary}")),
                    )));
                }
                _ => {}
            }
            let source = if self.heap.buffer(id)?.is_some() {
                Some(Source::Buffer(id))
            } else if self.heap.typed_array(id)?.is_some() || self.heap.data_view(id)?.is_some() {
                Some(Source::View(id))
            } else {
                None
            };
            if let Some(source) = source {
                let bytes = source_bytes(self, source)?.to_vec();
                return Ok(Ok((self.body_from_bytes(bytes)?, None)));
            }
        }
        match usv(self, host, roots, value)? {
            Ok(text) => Ok(Ok((
                self.body_from_bytes(text.into_bytes())?,
                Some("text/plain;charset=UTF-8".into()),
            ))),
            Err(error) => Ok(Err(error)),
        }
    }

    pub(super) fn tee_fetch_body(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        body: &BodyRecord,
    ) -> Completion<(BodyRecord, BodyRecord)> {
        if body.unusable(self)? {
            return Ok(Err(self.type_error(roots, "Body is disturbed or locked")?));
        }
        let array = match crate::builtin::streams::tee::tee(
            self,
            host,
            roots,
            Vec::new(),
            &Value::object(body.stream),
        )? {
            Invocation::Value(value) => value,
            Invocation::Thrown(error) => return Ok(Err(error)),
        };
        let roots = &roots.with(&array);
        let mut branches = [body.clone(), body.clone()];
        for (index, branch) in ["0", "1"].into_iter().zip(&mut branches) {
            let Ok(value) = get(self, host, roots, &array, index)? else {
                unreachable!()
            };
            let ValueRef::Object(id) = value.view() else {
                unreachable!()
            };
            branch.stream = id;
        }
        let [first, second] = branches;
        Ok(Ok((first, second)))
    }

    pub(super) fn transfer_fetch_body(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        body: &BodyRecord,
    ) -> Completion<BodyRecord> {
        let (discard, branch) = match self.tee_fetch_body(host, roots, body)? {
            Ok(pair) => pair,
            Err(error) => return Ok(Err(error)),
        };
        let live = (discard.stream, branch.stream, body.stream);
        let roots = &roots.with(&live);
        let Object::ReadableStream(source) = self.heap.get_mut(body.stream)? else {
            unreachable!()
        };
        source.disturbed = true;
        let cancel = Value::object(self.alloc_intrinsic(IntrinsicKey::new(
            "ReadableStream",
            Slot::Prototype,
            "cancel",
        )));
        match self.invoke(
            host,
            roots,
            &cancel,
            Arguments::Values(&[]),
            &Value::object(discard.stream),
        )? {
            Invocation::Value(_) => Ok(Ok(branch)),
            Invocation::Thrown(error) => Ok(Err(error)),
        }
    }

    pub(crate) fn begin_fetch_body(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        body: &BodyRecord,
        mode: BodyMode,
        promise: ObjectId,
        mime: String,
    ) -> Result<(), VmError> {
        if body.unusable(self)? {
            let error = self.type_error(roots, "Body is disturbed or locked")?;
            return self.settle(promise, Settlement::Rejected(error));
        }
        let prototype = self.builtins.get("readableStreamDefaultReader.prototype");
        let reader = match self.acquire_readable_reader(roots, body.stream, prototype)? {
            Invocation::Value(value) => match value.view() {
                ValueRef::Object(id) => id,
                _ => unreachable!(),
            },
            Invocation::Thrown(error) => return self.settle(promise, Settlement::Rejected(error)),
        };
        let active = self.heap.alloc_promise();
        self.fetch_body_read_next(
            host,
            roots,
            BodyRead {
                reader,
                promise,
                active,
                mode,
                chunks: Vec::new(),
                total: 0,
                mime,
            },
        )
    }

    fn fetch_body_read_next(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        mut job: BodyRead,
    ) -> Result<(), VmError> {
        let live = (job.reader, job.promise, job.active);
        let roots = &roots.with(&live);
        let read = match self.read_readable(host, roots, job.reader)? {
            Invocation::Value(value) => match value.view() {
                ValueRef::Object(id) => id,
                _ => unreachable!(),
            },
            Invocation::Thrown(error) => {
                return self.resume_fetch_body(host, roots, job, Settlement::Rejected(error));
            }
        };
        job.active = read;
        self.heap.promise_mut(job.promise)?.body_read = Some(read);
        self.react(read, ReactionJob::FetchBody(job))
    }

    pub(crate) fn resume_fetch_body(
        &mut self,
        host: &mut dyn Host,
        roots: &ActiveRoots<'_>,
        mut job: BodyRead,
        settlement: Settlement,
    ) -> Result<(), VmError> {
        let live = (job.reader, job.promise);
        let roots = &roots.with(&live);
        let outcome = match settlement {
            Settlement::Rejected(error) => Settlement::Rejected(error),
            Settlement::Fulfilled(result) => {
                let roots = &roots.with(&result);
                let Ok(done) = get(self, host, roots, &result, "done")? else {
                    unreachable!()
                };
                if matches!(done.view(), ValueRef::Bool(true)) {
                    let bytes = if job.chunks.len() == 1 {
                        job.chunks.pop().unwrap()
                    } else {
                        let mut bytes = Vec::with_capacity(job.total as usize);
                        for chunk in job.chunks {
                            bytes.extend(chunk);
                        }
                        bytes
                    };
                    package(self, host, roots, job.mode, bytes, &job.mime)?
                } else {
                    let Ok(value) = get(self, host, roots, &result, "value")? else {
                        unreachable!()
                    };
                    if let ValueRef::Object(id) = value.view()
                        && self
                            .heap
                            .typed_array(id)?
                            .is_some_and(|view| view.kind == ElementKind::Uint8)
                    {
                        let bytes = source_bytes(self, Source::View(id))?;
                        let total = job.total + bytes.len() as u64;
                        if total <= MAX_BYTE_LENGTH as u64 {
                            if !bytes.is_empty() {
                                job.chunks.push(bytes.to_vec());
                            }
                            job.total = total;
                            return self.fetch_body_read_next(host, roots, job);
                        }
                        Settlement::Rejected(
                            self.make_range_error(roots, "Body exceeds byte length limit")?,
                        )
                    } else {
                        Settlement::Rejected(
                            self.type_error(roots, "Body stream must produce Uint8Array chunks")?,
                        )
                    }
                }
            }
        };
        self.release_readable_reader(roots, job.reader)?;
        self.heap.promise_mut(job.promise)?.body_read = None;
        self.settle(job.promise, outcome)
    }
}

pub(super) fn consume(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    body: Option<BodyRecord>,
    mode: BodyMode,
    mime: String,
) -> Result<Invocation, VmError> {
    let promise = vm.heap.alloc_promise();
    let roots = &roots.with(&promise);
    if let Some(body) = body {
        vm.begin_fetch_body(host, roots, &body, mode, promise, mime)?;
    } else {
        let output = package(vm, host, roots, mode, Vec::new(), &mime)?;
        vm.settle(promise, output)?;
    }
    Ok(Invocation::Value(Value::object(promise)))
}

pub(super) fn rejected(
    vm: &mut Vm,
    roots: &ActiveRoots<'_>,
    message: &str,
) -> Result<Invocation, VmError> {
    let error = vm.type_error(roots, message)?;
    let promise = vm.heap.alloc_promise();
    vm.settle(promise, Settlement::Rejected(error))?;
    Ok(Invocation::Value(Value::object(promise)))
}

fn package(
    vm: &mut Vm,
    host: &mut dyn Host,
    roots: &ActiveRoots<'_>,
    mode: BodyMode,
    bytes: Vec<u8>,
    mime: &str,
) -> Result<Settlement, VmError> {
    let value = match mode {
        BodyMode::ArrayBuffer | BodyMode::Bytes => {
            let len = bytes.len();
            let buffer = vm.heap.alloc(Object::ArrayBuffer(ArrayBufferObject {
                object: PlainObject::new(Some(vm.builtins.get(ARRAY_BUFFER_PROTOTYPE))),
                bytes: Some(bytes),
                max_byte_length: None,
            }));
            Value::object(if mode == BodyMode::Bytes {
                vm.heap.alloc(Object::TypedArray(TypedArrayObject {
                    object: PlainObject::new(Some(vm.builtins.get("uint8Array.prototype"))),
                    buffer,
                    byte_offset: 0,
                    byte_length: Some(len),
                    kind: ElementKind::Uint8,
                }))
            } else {
                buffer
            })
        }
        BodyMode::Blob => {
            let mime_type = mime.to_ascii_lowercase();
            Value::object(vm.heap.alloc(Object::Blob {
                object: PlainObject::new(Some(vm.builtins.get(BLOB_PROTOTYPE))),
                data: BlobData {
                    start: 0,
                    end: bytes.len(),
                    bytes: Arc::new(bytes),
                    mime_type,
                },
            }))
        }
        BodyMode::Text | BodyMode::Json => {
            let text =
                String::from_utf8_lossy(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes));
            let value = Value::string(text.as_ref().into());
            if mode == BodyMode::Json {
                return Ok(match json_call(vm, host, roots, "parse", value)? {
                    Invocation::Value(value) => Settlement::Fulfilled(value),
                    Invocation::Thrown(error) => Settlement::Rejected(error),
                });
            }
            value
        }
        BodyMode::FormData => {
            let Some(entries) = parse_form(vm, mime, &bytes) else {
                return Ok(Settlement::Rejected(
                    vm.type_error(roots, "Invalid form body or Content-Type")?,
                ));
            };
            Value::object(vm.alloc_form_data(None, FormDataRecord { entries }))
        }
    };
    Ok(Settlement::Fulfilled(value))
}

fn normalize_newlines(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\r' || ch == '\n' {
            if ch == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
            }
            out.push_str("\r\n");
        } else {
            out.push(ch);
        }
    }
    out
}

fn quote(text: &str) -> String {
    text.replace('\r', "%0D")
        .replace('\n', "%0A")
        .replace('"', "%22")
}

fn encode_form(vm: &Vm, data: &FormDataRecord) -> Result<(Vec<u8>, String), VmError> {
    let mut sequence = vm.random_state;
    let boundary = loop {
        let boundary = format!("----yield-formdata-{sequence:016x}");
        let mut collision = false;
        for (name, value) in &data.entries {
            let bytes = match value {
                FormDataValue::String(value) => value.as_bytes(),
                FormDataValue::File(id) => match vm.heap.get(*id)? {
                    Object::File { data, .. } => data.blob.as_bytes(),
                    _ => unreachable!(),
                },
            };
            collision |= name.contains(&boundary) || find(bytes, boundary.as_bytes()).is_some();
        }
        if !collision {
            break boundary;
        }
        sequence = sequence.wrapping_add(1);
    };
    let mut bytes = Vec::new();
    for (name, value) in &data.entries {
        bytes.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{}\"",
                quote(&normalize_newlines(name))
            )
            .as_bytes(),
        );
        match value {
            FormDataValue::String(text) => {
                bytes.extend_from_slice(b"\r\n\r\n");
                bytes.extend_from_slice(normalize_newlines(text).as_bytes());
            }
            FormDataValue::File(id) => {
                let Object::File { data, .. } = vm.heap.get(*id)? else {
                    unreachable!()
                };
                let mime = if data.blob.mime_type.is_empty() {
                    "application/octet-stream"
                } else {
                    &data.blob.mime_type
                };
                bytes.extend_from_slice(
                    format!(
                        "; filename=\"{}\"\r\nContent-Type: {mime}\r\n\r\n",
                        quote(&data.name)
                    )
                    .as_bytes(),
                );
                bytes.extend_from_slice(data.blob.as_bytes());
            }
        }
        bytes.extend_from_slice(b"\r\n");
    }
    bytes.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    Ok((bytes, boundary))
}

fn find(bytes: &[u8], needle: &[u8]) -> Option<usize> {
    bytes
        .windows(needle.len())
        .position(|window| window == needle)
}

fn parameters(mut text: &str) -> impl Iterator<Item = (&str, String)> {
    std::iter::from_fn(move || {
        text = text.trim_start().strip_prefix(';')?.trim_start();
        let (key, rest) = text.split_once('=')?;
        let rest = rest.trim_start();
        let value;
        if let Some(quoted) = rest.strip_prefix('"') {
            let mut result = String::new();
            let mut escape = false;
            let mut end = None;
            for (index, ch) in quoted.char_indices() {
                if escape {
                    result.push(ch);
                    escape = false;
                } else if ch == '\\' {
                    escape = true;
                } else if ch == '"' {
                    end = Some(index);
                    break;
                } else {
                    result.push(ch);
                }
            }
            text = &quoted[end? + 1..];
            value = result;
        } else {
            let end = rest.find(';').unwrap_or(rest.len());
            value = rest[..end].trim().to_owned();
            text = &rest[end..];
        }
        Some((key.trim(), value))
    })
}

fn parse_form(vm: &mut Vm, mime: &str, mut bytes: &[u8]) -> Option<Vec<(String, FormDataValue)>> {
    let end = mime.find(';').unwrap_or(mime.len());
    let essence = mime[..end].trim();
    if essence.eq_ignore_ascii_case("application/x-www-form-urlencoded") {
        return Some(
            url::form_urlencoded::parse(bytes)
                .map(|(name, value)| (name.into_owned(), FormDataValue::String(value.into_owned())))
                .collect(),
        );
    }
    if !essence.eq_ignore_ascii_case("multipart/form-data") {
        return None;
    }
    let boundary = parameters(&mime[end..])
        .find(|(name, _)| name.eq_ignore_ascii_case("boundary"))?
        .1;
    if boundary.is_empty()
        || boundary.len() > 70
        || !boundary.bytes().all(|b| (b' '..=b'~').contains(&b))
    {
        return None;
    }
    let marker = format!("--{boundary}");
    let delim = format!("\r\n{marker}");
    bytes = bytes.strip_prefix(marker.as_bytes())?;
    let mut entries = Vec::new();
    loop {
        if bytes == b"--" || bytes.starts_with(b"--\r\n") {
            return Some(entries);
        }
        bytes = bytes.strip_prefix(b"\r\n")?;
        let head_end = find(bytes, b"\r\n\r\n")?;
        let headers = String::from_utf8_lossy(&bytes[..head_end]);
        let mut disposition = None;
        let mut mime_type = None;
        for line in headers.split("\r\n") {
            let (name, value) = line.split_once(':')?;
            if name.eq_ignore_ascii_case("content-disposition") {
                disposition = Some(value.trim());
            }
            if name.eq_ignore_ascii_case("content-type") {
                mime_type = Some(value.trim());
            }
        }
        let disposition = disposition?;
        let params = disposition.find(';')?;
        if !disposition[..params]
            .trim()
            .eq_ignore_ascii_case("form-data")
        {
            return None;
        }
        let mut name = None;
        let mut filename = None;
        for (key, value) in parameters(&disposition[params..]) {
            if key.eq_ignore_ascii_case("name") {
                name = Some(value);
            } else if key.eq_ignore_ascii_case("filename") {
                filename = Some(value);
            }
        }
        let name = name?;
        bytes = &bytes[head_end + 4..];
        let end = bytes
            .windows(delim.len())
            .enumerate()
            .find_map(|(index, window)| {
                let rest = &bytes[index + delim.len()..];
                (window == delim.as_bytes()
                    && (rest.starts_with(b"\r\n") || rest == b"--" || rest.starts_with(b"--\r\n")))
                .then_some(index)
            })?;
        let content = &bytes[..end];
        let value = if let Some(filename) = filename {
            let mime_type = mime_type
                .filter(|value| value.bytes().all(|b| (32..=126).contains(&b)))
                .unwrap_or("text/plain")
                .to_ascii_lowercase();
            FormDataValue::File(alloc_file(
                vm,
                FileData {
                    blob: BlobData {
                        bytes: Arc::new(content.to_vec()),
                        start: 0,
                        end: content.len(),
                        mime_type,
                    },
                    name: filename,
                    last_modified: vm.time().now_ms(),
                },
            ))
        } else {
            FormDataValue::String(String::from_utf8_lossy(content).into_owned())
        };
        entries.push((name, value));
        bytes = &bytes[end + delim.len()..];
    }
}
