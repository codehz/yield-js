use std::sync::Arc;

use sha2::{Digest, Sha256, Sha384, Sha512};
use url::Url;

use super::body::{self, BodyRecord};
use super::headers::{HeadersGuard, HeadersRecord};
use super::request::RequestRecord;
use super::response::{ResponseRecord, ResponseType};
use super::transaction::{FetchStage, FetchTransaction, Upload};
use crate::binding::OperationToken;
use crate::builtin::binary::text::{Source, source_bytes};
use crate::builtin::{
    FUNCTION_PARENT,
    globals::Owner,
    roots::{RootKind, RootSpec},
};
use crate::error::VmError;
use crate::fetch::{
    CredentialsMode, FETCH_SERVICE, FetchEvent, FetchFailure, FetchRegistration, FetchService,
    HopRequest, Origin, ResponseHead, UploadBody,
};
use crate::gc::ActiveRoots;
use crate::heap::{AbortStep, ElementKind, Object, ReactionJob};
use crate::host::Host;
use crate::intrinsic::{IntrinsicDef, IntrinsicKey, Slot};
use crate::runtime::{Invocation, Settlement};
use crate::snapshot::OrdinaryTask;
use crate::value::{ObjectId, Value, ValueRef};
use crate::vm::Vm;

const KEY: IntrinsicKey = IntrinsicKey::new("fetch", Slot::Global, "");
const CREDIT: u64 = 64 * 1024;
const INTERRUPTION: &str = "Restored fetch was interrupted";
type Header = (Vec<u8>, Vec<u8>);

pub(crate) const OWNER: Owner = Owner::new(
    "fetch",
    &[RootSpec::new(
        "fetch",
        RootKind::Native(KEY),
        FUNCTION_PARENT,
    )],
    &[&[IntrinsicDef::call(KEY, 1, |vm, host, roots, args, _| {
        let promise = vm.heap.alloc_promise();
        let roots = &roots.with(&args);
        let roots = &roots.with(&promise);
        let undefined = Value::UNDEFINED;
        let input = args.first().unwrap_or(&undefined);
        let init = args.get(1).unwrap_or(&undefined);
        match vm.request_from_fetch(host, roots, input, init)? {
            Ok(request) => vm.begin_fetch(roots, promise, request)?,
            Err(reason) => vm.settle(promise, Settlement::Rejected(reason))?,
        }
        Ok(Invocation::Value(Value::object(promise)))
    })]],
    None,
);

fn base64(input: &[u8], url_safe: bool) -> Option<Vec<u8>> {
    let mut symbols = 0;
    let mut padding = 0;
    let mut bits = 0;
    let mut acc = 0u16;
    let mut out = Vec::new();
    for &b in input {
        let n = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'-' if url_safe => 62,
            b'_' if url_safe => 63,
            b'\t' | b'\n' | b'\x0c' | b'\r' | b' ' => continue,
            b'=' => {
                padding += 1;
                continue;
            }
            _ => return None,
        };
        if padding != 0 {
            return None;
        }
        symbols += 1;
        acc = (acc << 6) | u16::from(n);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    (symbols % 4 != 1 && (padding == 0 || (padding <= 2 && (symbols + padding) % 4 == 0)))
        .then_some(out)
}

fn percent_decode(mut bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    while let Some((&first, tail)) = bytes.split_first() {
        if first == b'%'
            && tail.len() >= 2
            && let (Some(a), Some(b)) = (
                (tail[0] as char).to_digit(16),
                (tail[1] as char).to_digit(16),
            )
        {
            out.push((a * 16 + b) as u8);
            bytes = &tail[2..];
        } else {
            out.push(first);
            bytes = tail;
        }
    }
    out
}

fn data_url(url: &Url) -> Option<(String, Vec<u8>)> {
    let serialized = url.as_str().split('#').next()?;
    let (mime, body) = serialized.strip_prefix("data:")?.split_once(',')?;
    let mut mime = mime.trim_ascii();
    let mut bytes = percent_decode(body.as_bytes());
    if let Some((prefix, marker)) = mime.rsplit_once(';')
        && marker.trim_ascii().eq_ignore_ascii_case("base64")
    {
        bytes = base64(&bytes, false)?;
        mime = prefix.trim_ascii_end();
    }
    let mime = if mime.is_empty() {
        "text/plain;charset=US-ASCII".into()
    } else if mime.starts_with(';') {
        format!("text/plain{mime}")
    } else if mime.split(';').next().is_some_and(|essence| {
        essence
            .split_once('/')
            .is_some_and(|(a, b)| token(a.as_bytes()) && token(b.trim_ascii_end().as_bytes()))
    }) {
        mime.into()
    } else {
        "text/plain;charset=US-ASCII".into()
    };
    Some((mime, bytes))
}

fn token(bytes: &[u8]) -> bool {
    !bytes.is_empty()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(b))
}

fn headers<'a>(list: &'a [Header], name: &'a [u8]) -> impl Iterator<Item = &'a [u8]> {
    list.iter()
        .filter_map(move |(n, v)| n.eq_ignore_ascii_case(name).then_some(v.as_slice()))
}

fn header<'a>(list: &'a [Header], name: &[u8]) -> Option<&'a [u8]> {
    list.iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_slice())
}

fn header_tokens<'a>(list: &'a [Header], name: &'a [u8]) -> impl Iterator<Item = &'a [u8]> {
    headers(list, name).flat_map(|v| v.split(|&b| b == b',').map(<[u8]>::trim_ascii))
}

fn unsafe_headers(list: &[Header]) -> Vec<Vec<u8>> {
    let mut names = Vec::new();
    let mut safe = Vec::new();
    let mut safe_size = 0usize;
    for (name, value) in list {
        let unsafe_byte =
            |&b: &u8| b < 0x20 && b != b'\t' || b == 0x7f || b"\"():<>?@[\\]{}".contains(&b);
        let safelisted = value.len() <= 128
            && match name.as_slice() {
                b"accept" => !value.iter().any(unsafe_byte),
                b"accept-language" | b"content-language" => value
                    .iter()
                    .all(|b| b.is_ascii_alphanumeric() || b" *,-.;=".contains(b)),
                b"content-type" => {
                    !value.iter().any(unsafe_byte)
                        && value.split(|&b| b == b';').next().is_some_and(|v| {
                            [
                                b"application/x-www-form-urlencoded".as_slice(),
                                b"multipart/form-data",
                                b"text/plain",
                            ]
                            .iter()
                            .any(|allowed| v.trim_ascii().eq_ignore_ascii_case(allowed))
                        })
                }
                _ => false,
            };
        if safelisted {
            safe_size += value.len();
            safe.push(name.clone());
        } else {
            names.push(name.clone());
        }
    }
    if safe_size > 1024 {
        names.extend(safe);
    }
    names.sort_unstable();
    names.dedup();
    names
}

fn simple_method(method: &str) -> bool {
    matches!(method, "GET" | "HEAD" | "POST")
}

fn cors_allows(hop: &HopRequest, head: &ResponseHead) -> bool {
    let include = hop.credentials == CredentialsMode::Include;
    let mut origins = headers(&head.headers, b"access-control-allow-origin");
    let Some(origin) = origins.next().map(<[u8]>::trim_ascii) else {
        return false;
    };
    origins.next().is_none()
        && (origin == b"*" && !include
            || hop
                .origin
                .as_deref()
                .is_some_and(|ours| ours.as_bytes() == origin))
        && (!include || {
            let mut values = headers(&head.headers, b"access-control-allow-credentials");
            values.next().is_some_and(|v| v.trim_ascii() == b"true") && values.next().is_none()
        })
}

fn preflight_allows(hop: &HopRequest, head: &ResponseHead) -> bool {
    if !(200..300).contains(&head.status) || !cors_allows(hop, head) {
        return false;
    }
    let wildcard = hop.credentials != CredentialsMode::Include;
    if !simple_method(&hop.method)
        && !header_tokens(&head.headers, b"access-control-allow-methods")
            .any(|method| method == hop.method.as_bytes() || wildcard && method == b"*")
    {
        return false;
    }
    unsafe_headers(&hop.headers).iter().all(|requested| {
        header_tokens(&head.headers, b"access-control-allow-headers").any(|allowed| {
            allowed.eq_ignore_ascii_case(requested)
                || wildcard && allowed == b"*" && requested != b"authorization"
        })
    })
}

fn integrity_matches(metadata: &str, bytes: &[u8]) -> bool {
    if metadata.trim().is_empty() {
        return true;
    }
    let hashes = || {
        metadata.split_ascii_whitespace().filter_map(|entry| {
            let (algorithm, value) = entry.split_once('-')?;
            let strength = match algorithm {
                "sha256" => 1,
                "sha384" => 2,
                "sha512" => 3,
                _ => return None,
            };
            Some((strength, value.split('?').next().unwrap_or_default()))
        })
    };
    let Some(strength) = hashes().map(|(strength, _)| strength).max() else {
        return false;
    };
    let digest = match strength {
        1 => Sha256::digest(bytes).to_vec(),
        2 => Sha384::digest(bytes).to_vec(),
        _ => Sha512::digest(bytes).to_vec(),
    };
    hashes().any(|(s, expected)| {
        s == strength && base64(expected.as_bytes(), true).is_some_and(|v| v == digest)
    })
}

fn decode(head: &ResponseHead, mut bytes: Vec<u8>) -> Result<Vec<u8>, ()> {
    let encodings: Vec<_> = header_tokens(&head.headers, b"content-encoding").collect();
    for encoding in encodings.into_iter().rev() {
        if !encoding.is_empty() && !encoding.eq_ignore_ascii_case(b"identity") {
            bytes = crate::builtin::streams::compression::decode_http_encoding(encoding, &bytes)?;
        }
    }
    Ok(bytes)
}

fn no_body(hop: &HopRequest, head: &ResponseHead) -> bool {
    hop.method == "HEAD" || matches!(head.status, 101 | 103 | 204 | 205 | 304)
}

impl Vm {
    fn fetch_request_record(&self, request: ObjectId) -> Result<&RequestRecord, VmError> {
        let Object::Request { data, .. } = self.heap.get(request)? else {
            unreachable!()
        };
        Ok(data)
    }

    fn fetch_same_origin(&self, url: &Url) -> bool {
        self.fetch_environment
            .origin
            .as_ref()
            .zip(Origin::from_url(url))
            .is_some_and(|(a, b)| a.same_as(&b))
    }

    fn configure_hop(
        &self,
        request: &RequestRecord,
        hop: &mut HopRequest,
    ) -> Result<(), &'static str> {
        let same = self.fetch_same_origin(&hop.url);
        if request.mode == "same-origin" && !same {
            return Err("Cross-origin same-origin fetch");
        }
        hop.credentials = match request.credentials.as_str() {
            "include" => CredentialsMode::Include,
            "same-origin" if same => CredentialsMode::SameOrigin,
            _ => CredentialsMode::Omit,
        };
        hop.origin = (!same && request.mode == "cors").then(|| {
            self.fetch_environment.origin.as_ref().map_or_else(
                || "null".into(),
                |origin| {
                    if matches!(
                        (origin.scheme.as_str(), origin.port),
                        ("http", 80) | ("https", 443)
                    ) {
                        format!("{}://{}", origin.scheme, origin.host)
                    } else {
                        format!("{}://{}:{}", origin.scheme, origin.host, origin.port)
                    }
                },
            )
        });
        Ok(())
    }

    fn fetch_referrer(&self, request: &RequestRecord, target: &Url) -> Option<Vec<u8>> {
        let source = match request.referrer.as_str() {
            "" => return None,
            "about:client" => self.fetch_environment.referrer.as_deref()?,
            source => source,
        };
        let mut url = Url::parse(source).ok()?;
        if !matches!(url.scheme(), "http" | "https") {
            return None;
        }
        url.set_fragment(None);
        url.set_username("").ok()?;
        url.set_password(None).ok()?;
        let same = url.origin() == target.origin();
        let downgrade = url.scheme() == "https" && target.scheme() != "https";
        let origin_only = match request.referrer_policy.as_str() {
            "no-referrer" => return None,
            "same-origin" if !same => return None,
            "strict-origin"
            | "strict-origin-when-cross-origin"
            | ""
            | "no-referrer-when-downgrade"
                if downgrade =>
            {
                return None;
            }
            "origin" | "strict-origin" => true,
            "origin-when-cross-origin" | "strict-origin-when-cross-origin" | "" => !same,
            _ => false,
        };
        if origin_only || url.as_str().len() > 4096 {
            url.set_path("/");
            url.set_query(None);
        }
        Some(url.as_str().as_bytes().to_vec())
    }

    fn begin_fetch(
        &mut self,
        roots: &ActiveRoots<'_>,
        promise: ObjectId,
        request: ObjectId,
    ) -> Result<(), VmError> {
        let roots = &roots.with(&request);
        let data = self.fetch_request_record(request)?.clone();
        let reject = |vm: &mut Self, message| {
            let error = vm.type_error(roots, message)?;
            vm.settle(promise, Settlement::Rejected(error))
        };
        if let Some(reason) = self
            .abort_signal_state(data.signal)?
            .and_then(|s| s.reason.clone())
        {
            return self.settle(promise, Settlement::Rejected(reason));
        }
        if data.cache == "only-if-cached" {
            return reject(
                self,
                if data.mode != "same-origin" {
                    "only-if-cached requires same-origin mode"
                } else {
                    "Fetch HTTP cache miss"
                },
            );
        }
        let upload = match &data.body {
            None => UploadBody::None,
            Some(body) => match &body.source {
                Some(bytes) => UploadBody::Known(bytes.len() as u64),
                None => UploadBody::Unknown,
            },
        };
        if data.keepalive
            && (matches!(upload, UploadBody::Known(n) if n > CREDIT)
                || upload == UploadBody::Unknown)
        {
            return reject(self, "Fetch keepalive body exceeds the upload budget");
        }
        let offline = match data.url.scheme() {
            "about" if data.url.path() == "blank" && data.url.query().is_none() => {
                Some((Some("text/html;charset=UTF-8".into()), Vec::new()))
            }
            "data" => data_url(&data.url).map(|(mime, bytes)| (Some(mime), bytes)),
            "blob" => match self.object_urls.resolve(&data.url) {
                Some(id) => {
                    let blob = match self.heap.get(id)? {
                        Object::Blob { data, .. } => data,
                        Object::File { data, .. } => &data.blob,
                        _ => unreachable!(),
                    };
                    Some((Some(blob.mime_type.clone()), blob.as_bytes().to_vec()))
                }
                None => None,
            },
            "http" | "https" => None,
            _ => return reject(self, "Unsupported fetch URL scheme"),
        };
        if !matches!(data.url.scheme(), "http" | "https") {
            let Some((mime, bytes)) = offline else {
                return reject(self, "Invalid or unavailable fetch URL");
            };
            if !integrity_matches(&data.integrity, &bytes) {
                return reject(self, "Fetch integrity mismatch");
            }
            let body = if data.method == "HEAD" {
                None
            } else {
                Some(self.body_from_bytes(bytes)?)
            };
            let response = self.offline_response(data.url, 200, mime, body);
            return self.settle(promise, Settlement::Fulfilled(Value::object(response)));
        }
        let Object::Headers { data: headers, .. } = self.heap.get(data.headers)? else {
            unreachable!()
        };
        let mut hop = HopRequest {
            url: data.url.clone(),
            method: data.method.clone(),
            headers: headers.list.clone(),
            upload,
            origin: None,
            credentials: CredentialsMode::Omit,
            partition: self.fetch_environment.partition.clone(),
            authorization: self.fetch_environment.authorization.clone(),
        };
        hop.url.set_fragment(None);
        if let Err(message) = self.configure_hop(&data, &mut hop) {
            return reject(self, message);
        }
        if self
            .bindings
            .service::<Arc<dyn FetchService>>(FETCH_SERVICE)
            .is_none()
        {
            return reject(self, "Fetch service unavailable");
        }
        let upload = match &data.body {
            None => Upload::None,
            Some(body) => match &body.source {
                Some(bytes) => Upload::Buffered {
                    bytes: bytes.clone(),
                    sent: 0,
                },
                None => {
                    let prototype = self.builtins.get("readableStreamDefaultReader.prototype");
                    let reader =
                        match self.acquire_readable_reader(roots, body.stream, prototype)? {
                            Invocation::Value(value) => match value.view() {
                                ValueRef::Object(id) => id,
                                _ => unreachable!(),
                            },
                            Invocation::Thrown(_) => {
                                return reject(self, "Cannot acquire fetch upload reader");
                            }
                        };
                    Upload::Streaming {
                        body: body.clone(),
                        reader,
                        finished: false,
                    }
                }
            },
        };
        let urls = vec![hop.url.clone()];
        self.start_fetch_hop(
            FetchTransaction {
                promise,
                request,
                signal: Some(data.signal),
                stage: FetchStage::AwaitingHead,
                hop,
                upload,
                upload_pending: Vec::new(),
                upload_credit: 0,
                download_credit: 0,
                redirects: 0,
                urls,
                pending_upload_credit: 0,
                preflight: false,
                buffer: None,
                upload_read: None,
                terminal_queued: false,
                interruption_queued: false,
                response: None,
                response_stream: None,
                settled: false,
                head: None,
                queued: 0,
            },
            true,
        )
    }

    fn start_fetch_hop(
        &mut self,
        mut tx: FetchTransaction,
        check_preflight: bool,
    ) -> Result<(), VmError> {
        let id = self.next_operation_id;
        self.next_operation_id = id
            .checked_add(1)
            .ok_or_else(|| VmError::InvalidState("Fetch operation id exhausted".into()))?;
        let mut outgoing = tx.hop.clone();
        let unsafe_names = if check_preflight && outgoing.origin.is_some() {
            unsafe_headers(&outgoing.headers)
        } else {
            Vec::new()
        };
        tx.preflight = check_preflight
            && outgoing.origin.is_some()
            && (!simple_method(&outgoing.method) || !unsafe_names.is_empty());
        if tx.preflight {
            outgoing.headers = vec![(
                b"access-control-request-method".to_vec(),
                outgoing.method.as_bytes().to_vec(),
            )];
            if !unsafe_names.is_empty() {
                outgoing.headers.push((
                    b"access-control-request-headers".to_vec(),
                    unsafe_names.join(b",".as_slice()),
                ));
            }
            outgoing.method = "OPTIONS".into();
            outgoing.upload = UploadBody::None;
            outgoing.credentials = CredentialsMode::Omit;
        }
        if let Some(value) =
            self.fetch_referrer(self.fetch_request_record(tx.request)?, &outgoing.url)
        {
            outgoing.headers.push((b"referer".to_vec(), value));
        }
        if let Some(signal) = tx.signal {
            self.abort_signal_state_mut(signal)?
                .unwrap()
                .steps
                .push(AbortStep::Operation(id));
        }
        self.fetch_state.transactions.insert(id, tx);
        let registration = self
            .bindings
            .service::<Arc<dyn FetchService>>(FETCH_SERVICE)
            .and_then(|service| {
                Arc::clone(service)
                    .start(
                        OperationToken {
                            epoch: self.epoch,
                            id,
                        },
                        outgoing,
                    )
                    .ok()
            });
        if let Some(registration) = registration {
            self.fetch_live.insert(id, registration);
            let tx = &self.fetch_state.transactions[&id];
            if !tx.preflight
                && matches!(tx.upload, Upload::Buffered { .. })
                && let Some(stream) = self
                    .fetch_request_record(tx.request)?
                    .body
                    .as_ref()
                    .map(|body| body.stream)
                && let Object::ReadableStream(record) = self.heap.get_mut(stream)?
            {
                record.disturbed = true;
            }
            if matches!(self.fetch_state.transactions[&id].upload, Upload::Buffered { ref bytes, .. } if bytes.is_empty())
                && !self.fetch_state.transactions[&id].preflight
            {
                self.finish_fetch_upload(id)?;
            }
        } else {
            self.fail_fetch(id, "Fetch hop could not be started")?;
        }
        Ok(())
    }

    fn remove_fetch_hop(
        &mut self,
        id: u64,
        cancel: bool,
    ) -> Result<Option<FetchTransaction>, VmError> {
        if let Some(mut registration) = self.fetch_live.remove(&id)
            && cancel
        {
            let _ = registration.cancel();
        }
        let tx = self.fetch_state.transactions.remove(&id);
        if tx.as_ref().is_some_and(|tx| tx.queued != 0) {
            self.ordinary_tasks.retain(
                |task| !matches!(task, OrdinaryTask::FetchEvent(operation, _) if *operation == id),
            );
        }
        if let Some(signal) = tx.as_ref().and_then(|tx| tx.signal)
            && let Some(state) = self.abort_signal_state_mut(signal)?
        {
            state.steps.retain(
                |step| !matches!(step, AbortStep::Operation(operation) if *operation == id),
            );
        }
        Ok(tx)
    }

    fn release_fetch_upload(&mut self, tx: &mut FetchTransaction) -> Result<(), VmError> {
        if let Upload::Streaming { reader, .. } = std::mem::replace(&mut tx.upload, Upload::None) {
            self.release_readable_reader(&ActiveRoots::EMPTY.with(&reader), reader)?;
        }
        tx.upload_pending.clear();
        tx.upload_read = None;
        Ok(())
    }

    pub(crate) fn abort_fetch(&mut self, id: u64, reason: Value) -> Result<bool, VmError> {
        let Some(mut tx) = self.remove_fetch_hop(id, true)? else {
            return Ok(false);
        };
        self.release_fetch_upload(&mut tx)?;
        if let Some(stream) = tx.response_stream {
            self.error_body_stream(stream, &reason)?;
        } else if tx.response.is_none() {
            self.settle(tx.promise, Settlement::Rejected(reason))?;
        }
        Ok(true)
    }

    fn fail_fetch(&mut self, id: u64, message: &str) -> Result<(), VmError> {
        let error = self.type_error(&ActiveRoots::EMPTY, message)?;
        self.abort_fetch(id, error)?;
        Ok(())
    }

    pub(crate) fn start_fetch_interruptions(
        &mut self,
        _host: &mut dyn Host,
    ) -> Result<(), VmError> {
        for (&id, tx) in &mut self.fetch_state.transactions {
            if tx.is_external() && !tx.interruption_queued && !self.fetch_live.contains_key(&id) {
                tx.interruption_queued = true;
                tx.queued += 1;
                self.ordinary_tasks.push_back(OrdinaryTask::FetchEvent(
                    id,
                    FetchEvent::Failed(FetchFailure::Internal(INTERRUPTION.into())),
                ));
            }
        }
        Ok(())
    }

    fn fetch_registration(
        &mut self,
        id: u64,
        f: impl FnOnce(&mut dyn FetchRegistration) -> Result<(), crate::binding::HostFault>,
    ) -> Result<(), VmError> {
        if let Some(registration) = self.fetch_live.get_mut(&id)
            && f(registration.as_mut()).is_err()
        {
            self.fail_fetch(id, "Fetch transport failed")?;
        }
        Ok(())
    }

    fn grant_fetch_download(&mut self, id: u64, amount: u64) -> Result<(), VmError> {
        let Some(tx) = self.fetch_state.transactions.get_mut(&id) else {
            return Ok(());
        };
        if amount == 0 || tx.terminal_queued {
            return Ok(());
        }
        tx.download_credit += amount;
        self.fetch_registration(id, |r| r.grant_download(amount))
    }

    pub(crate) fn fetch_bytes_consumed(
        &mut self,
        stream: ObjectId,
        count: u64,
    ) -> Result<(), VmError> {
        if count == 0 {
            return Ok(());
        }
        if let Some((&id, _)) = self
            .fetch_state
            .transactions
            .iter()
            .find(|(_, tx)| tx.response_stream == Some(stream))
        {
            // Defer host failure handling until the current read finishes mutating the stream.
            if self.fetch_live.contains_key(&id)
                && !self.fetch_state.transactions[&id].terminal_queued
            {
                let result = self.fetch_live.get_mut(&id).unwrap().grant_download(count);
                if result.is_ok() {
                    self.fetch_state
                        .transactions
                        .get_mut(&id)
                        .unwrap()
                        .download_credit += count;
                } else {
                    self.deliver_fetch_event(
                        OperationToken {
                            id,
                            epoch: self.epoch,
                        },
                        FetchEvent::Failed(FetchFailure::Internal("Fetch transport failed".into())),
                    )?;
                }
            }
        }
        Ok(())
    }

    fn finish_fetch_upload(&mut self, id: u64) -> Result<(), VmError> {
        if let Some(tx) = self.fetch_state.transactions.get_mut(&id)
            && let Upload::Streaming { finished, .. } = &mut tx.upload
        {
            *finished = true;
        }
        self.fetch_registration(id, |r| r.finish_upload())
    }

    fn pump_fetch_upload(&mut self, host: &mut dyn Host, id: u64) -> Result<(), VmError> {
        let Some(tx) = self.fetch_state.transactions.get_mut(&id) else {
            return Ok(());
        };
        if tx.preflight
            || tx.stage != FetchStage::AwaitingHead
            || tx.upload_credit == 0
            || !self.fetch_live.contains_key(&id)
        {
            return Ok(());
        }
        let (bytes, finished) = match &mut tx.upload {
            Upload::None => return Ok(()),
            Upload::Buffered { bytes, sent } => {
                let end = (*sent)
                    .saturating_add(tx.upload_credit)
                    .min(bytes.len() as u64);
                if end == *sent {
                    return Ok(());
                }
                let block = bytes[*sent as usize..end as usize].to_vec();
                *sent = end;
                (block, end == bytes.len() as u64)
            }
            Upload::Streaming {
                reader, finished, ..
            } => {
                if *finished || tx.upload_read.is_some() {
                    return Ok(());
                }
                if tx.upload_pending.is_empty() {
                    let reader = *reader;
                    let roots = &ActiveRoots::EMPTY.with(&reader);
                    let promise = match self.read_readable(host, roots, reader)? {
                        Invocation::Value(value) => match value.view() {
                            ValueRef::Object(id) => id,
                            _ => unreachable!(),
                        },
                        Invocation::Thrown(_) => {
                            return self.fail_fetch(id, "Fetch upload read failed");
                        }
                    };
                    let Some(tx) = self.fetch_state.transactions.get_mut(&id) else {
                        return Ok(());
                    };
                    tx.upload_read = Some(promise);
                    return self.react(promise, ReactionJob::FetchUpload(id));
                }
                let n = tx.upload_credit.min(tx.upload_pending.len() as u64) as usize;
                let tail = tx.upload_pending.split_off(n);
                (std::mem::replace(&mut tx.upload_pending, tail), false)
            }
        };
        tx.upload_credit -= bytes.len() as u64;
        self.fetch_registration(id, |r| r.send_upload(bytes))?;
        if finished {
            self.finish_fetch_upload(id)
        } else if matches!(
            self.fetch_state.transactions.get(&id).map(|tx| &tx.upload),
            Some(Upload::Streaming { .. })
        ) {
            self.pump_fetch_upload(host, id)
        } else {
            Ok(())
        }
    }

    pub(crate) fn resume_fetch_upload(
        &mut self,
        host: &mut dyn Host,
        id: u64,
        settlement: &Settlement,
    ) -> Result<(), VmError> {
        let Some(tx) = self.fetch_state.transactions.get_mut(&id) else {
            return Ok(());
        };
        tx.upload_read = None;
        if tx.stage != FetchStage::AwaitingHead || !self.fetch_live.contains_key(&id) {
            return Ok(());
        }
        let Settlement::Fulfilled(result) = settlement else {
            return self.fail_fetch(id, "Fetch upload read failed");
        };
        let roots = &ActiveRoots::EMPTY.with(result);
        if matches!(body::get(self, host, roots, result, "done")?, Ok(value) if matches!(value.view(), ValueRef::Bool(true)))
        {
            return self.finish_fetch_upload(id);
        }
        let Ok(value) = body::get(self, host, roots, result, "value")? else {
            return self.fail_fetch(id, "Invalid fetch upload chunk");
        };
        let ValueRef::Object(view) = value.view() else {
            return self.fail_fetch(id, "Fetch upload requires Uint8Array chunks");
        };
        if !self
            .heap
            .typed_array(view)?
            .is_some_and(|view| view.kind == ElementKind::Uint8)
        {
            return self.fail_fetch(id, "Fetch upload requires Uint8Array chunks");
        }
        let bytes = source_bytes(self, Source::View(view))?.to_vec();
        if let Some(tx) = self.fetch_state.transactions.get_mut(&id) {
            tx.upload_pending = bytes;
        }
        self.pump_fetch_upload(host, id)
    }

    pub(crate) fn process_fetch_event(
        &mut self,
        host: &mut dyn Host,
        id: u64,
        event: FetchEvent,
    ) -> Result<(), VmError> {
        let Some(tx) = self.fetch_state.transactions.get_mut(&id) else {
            return Ok(());
        };
        tx.queued = tx
            .queued
            .checked_sub(1)
            .ok_or_else(|| VmError::InvalidState("Fetch event task was not accepted".into()))?;
        match event {
            FetchEvent::Informational(_) => Ok(()),
            FetchEvent::UploadCredit(amount) => {
                tx.upload_credit = tx
                    .upload_credit
                    .checked_add(amount)
                    .ok_or_else(|| VmError::HostFault("Fetch upload credit overflow".into()))?;
                tx.pending_upload_credit = tx
                    .pending_upload_credit
                    .checked_sub(amount)
                    .ok_or_else(|| {
                        VmError::InvalidState("Fetch upload credit task was not accepted".into())
                    })?;
                self.pump_fetch_upload(host, id)
            }
            FetchEvent::Head(head) => self.fetch_head(id, head),
            FetchEvent::Data(bytes) => {
                let length = bytes.len() as u64;
                if let Some(buffer) = &mut tx.buffer {
                    buffer.extend(bytes);
                    self.grant_fetch_download(id, length)
                } else if let Some(stream) = tx.response_stream {
                    if matches!(self.heap.get(stream)?, Object::ReadableStream(stream) if matches!(stream.state, crate::heap::ReadableState::Readable))
                    {
                        self.enqueue_byte_vec(
                            host,
                            &ActiveRoots::EMPTY.with(&stream),
                            stream,
                            bytes,
                        )
                    } else {
                        let mut tx = self.remove_fetch_hop(id, true)?.unwrap();
                        self.release_fetch_upload(&mut tx)
                    }
                } else {
                    self.grant_fetch_download(id, length)
                }
            }
            FetchEvent::Eof => self.fetch_eof(host, id),
            FetchEvent::Failed(failure) => {
                if matches!(&failure, FetchFailure::Internal(message) if message == INTERRUPTION)
                    && !self.fetch_live.contains_key(&id)
                {
                    let reason = self.alloc_dom_exception("AbortError".into(), INTERRUPTION.into());
                    self.abort_fetch(id, reason)?;
                    Ok(())
                } else {
                    self.fail_fetch(id, "Fetch transport failed")
                }
            }
        }
    }

    fn fetch_head(&mut self, id: u64, head: ResponseHead) -> Result<(), VmError> {
        let tx = &self.fetch_state.transactions[&id];
        if tx.preflight {
            if !preflight_allows(&tx.hop, &head) {
                return self.fail_fetch(id, "Fetch CORS preflight failed");
            }
            return self.restart_fetch(id, None);
        }
        if tx.hop.origin.is_some() && !cors_allows(&tx.hop, &head) {
            return self.fail_fetch(id, "Fetch CORS check failed");
        }
        let request = self.fetch_request_record(tx.request)?;
        if matches!(head.status, 301 | 302 | 303 | 307 | 308)
            && let Some(location) = header(&head.headers, b"location")
        {
            if request.redirect == "error" {
                return self.fail_fetch(id, "Fetch redirects are disabled");
            }
            if request.redirect == "manual" {
                self.expose_fetch_response(id, &head, ResponseType::OpaqueRedirect)?;
                let mut tx = self.remove_fetch_hop(id, true)?.unwrap();
                return self.release_fetch_upload(&mut tx);
            }
            let Ok(mut url) = tx
                .hop
                .url
                .join(&String::from_utf8_lossy(location.trim_ascii()))
            else {
                return self.fail_fetch(id, "Invalid fetch redirect URL");
            };
            if !matches!(url.scheme(), "http" | "https")
                || tx.redirects >= 20
                || !url.username().is_empty()
                || url.password().is_some()
            {
                return self.fail_fetch(id, "Fetch redirect is not allowed");
            }
            url.set_fragment(None);
            let rewrite = (301..=303).contains(&head.status)
                && !matches!(tx.hop.method.as_str(), "GET" | "HEAD");
            if !rewrite && matches!(tx.upload, Upload::Streaming { .. }) {
                return self.fail_fetch(id, "Fetch redirect cannot replay a streaming body");
            }
            return self.restart_fetch(id, Some((url, rewrite)));
        }
        let mut upload = {
            let tx = self.fetch_state.transactions.get_mut(&id).unwrap();
            tx.upload_read = None;
            tx.upload_pending.clear();
            std::mem::replace(&mut tx.upload, Upload::None)
        };
        if let Upload::Streaming { reader, .. } = &mut upload {
            self.release_readable_reader(&ActiveRoots::EMPTY.with(reader), *reader)?;
        }
        let tx = &self.fetch_state.transactions[&id];
        let request = self.fetch_request_record(tx.request)?;
        let kind = self.fetch_response_type(tx)?;
        let opaque = kind == ResponseType::Opaque;
        let null_body = no_body(&tx.hop, &head);
        let integrity = !request.integrity.trim().is_empty();
        if opaque && integrity {
            return self.fail_fetch(id, "Cannot verify an opaque fetch response");
        }
        let encoded = !null_body
            && header_tokens(&head.headers, b"content-encoding")
                .any(|v| !v.is_empty() && !v.eq_ignore_ascii_case(b"identity"));
        let buffer = !opaque && (encoded || integrity);
        if buffer {
            self.fetch_state.transactions.get_mut(&id).unwrap().buffer = Some(Vec::new());
        } else {
            self.expose_fetch_response(id, &head, kind)?;
        }
        self.fetch_state.transactions.get_mut(&id).unwrap().head = Some(head);
        if !self.fetch_live.contains_key(&id) {
            return Ok(());
        }
        self.grant_fetch_download(id, CREDIT)
    }

    fn restart_fetch(&mut self, id: u64, redirect: Option<(Url, bool)>) -> Result<(), VmError> {
        // Restored fetches may consume an already-accepted head, but must never start another hop.
        if !self.fetch_live.contains_key(&id) {
            let reason = self.alloc_dom_exception("AbortError".into(), INTERRUPTION.into());
            self.abort_fetch(id, reason)?;
            return Ok(());
        }
        if let Some((url, _)) = &redirect {
            let tx = &self.fetch_state.transactions[&id];
            if self.fetch_request_record(tx.request)?.mode == "same-origin"
                && !self.fetch_same_origin(url)
            {
                return self.fail_fetch(id, "Cross-origin same-origin fetch redirect");
            }
        }
        let mut tx = self.remove_fetch_hop(id, true)?.unwrap();
        let check_preflight = redirect.is_some();
        if let Some((url, rewrite)) = redirect {
            if Origin::from_url(&tx.hop.url) != Origin::from_url(&url) {
                tx.hop.headers.retain(|(name, _)| {
                    !matches!(
                        name.as_slice(),
                        b"authorization" | b"cookie" | b"proxy-authorization" | b"host"
                    )
                });
            }
            if rewrite {
                self.release_fetch_upload(&mut tx)?;
                tx.hop.method = "GET".into();
                tx.hop.upload = UploadBody::None;
                tx.hop.headers.retain(|(name, _)| {
                    !matches!(
                        name.as_slice(),
                        b"content-type"
                            | b"content-encoding"
                            | b"content-language"
                            | b"content-location"
                            | b"content-length"
                    )
                });
            } else if let Upload::Buffered { sent, .. } = &mut tx.upload {
                *sent = 0;
            }
            tx.hop.url = url;
            tx.urls.push(tx.hop.url.clone());
            tx.redirects += 1;
            self.configure_hop(self.fetch_request_record(tx.request)?, &mut tx.hop)
                .expect("redirect mode checked");
        }
        tx.stage = FetchStage::AwaitingHead;
        tx.head = None;
        tx.queued = 0;
        tx.terminal_queued = false;
        tx.upload_credit = 0;
        tx.pending_upload_credit = 0;
        tx.download_credit = 0;
        self.start_fetch_hop(tx, check_preflight)
    }

    fn fetch_response_type(&self, tx: &FetchTransaction) -> Result<ResponseType, VmError> {
        let request = self.fetch_request_record(tx.request)?;
        Ok(if tx.hop.origin.is_some() {
            ResponseType::Cors
        } else if request.mode == "no-cors"
            && tx.urls.iter().any(|url| !self.fetch_same_origin(url))
        {
            ResponseType::Opaque
        } else {
            ResponseType::Basic
        })
    }

    fn expose_fetch_response(
        &mut self,
        id: u64,
        head: &ResponseHead,
        kind: ResponseType,
    ) -> Result<(), VmError> {
        let tx = &self.fetch_state.transactions[&id];
        let opaque = matches!(kind, ResponseType::Opaque | ResponseType::OpaqueRedirect);
        let body = !opaque && !no_body(&tx.hop, head);
        let urls = if kind == ResponseType::Opaque {
            Vec::new()
        } else {
            tx.urls.clone()
        };
        let mut list = Vec::new();
        if !opaque {
            let wildcard = tx.hop.credentials != CredentialsMode::Include
                && header_tokens(&head.headers, b"access-control-expose-headers")
                    .any(|name| name == b"*");
            for (name, value) in &head.headers {
                let name = name.to_ascii_lowercase();
                if matches!(name.as_slice(), b"set-cookie" | b"set-cookie2") {
                    continue;
                }
                if kind == ResponseType::Cors
                    && !wildcard
                    && !matches!(
                        name.as_slice(),
                        b"cache-control"
                            | b"content-language"
                            | b"content-length"
                            | b"content-type"
                            | b"expires"
                            | b"last-modified"
                            | b"pragma"
                    )
                    && !header_tokens(&head.headers, b"access-control-expose-headers")
                        .any(|exposed| exposed.eq_ignore_ascii_case(&name))
                {
                    continue;
                }
                list.push((name, value.clone()));
            }
        }
        let headers = self.alloc_headers(HeadersRecord {
            list,
            guard: HeadersGuard::Immutable,
        });
        let stream = if body {
            Some(self.create_byte_stream()?)
        } else {
            None
        };
        let response = self.alloc_response(
            None,
            ResponseRecord {
                kind,
                urls,
                status: if opaque { 0 } else { head.status },
                status_text: if opaque {
                    String::new()
                } else {
                    head.status_text.iter().map(|&b| char::from(b)).collect()
                },
                headers,
                body: stream.map(|stream| BodyRecord {
                    stream,
                    source: None,
                    length: None,
                }),
            },
        );
        let tx = self.fetch_state.transactions.get_mut(&id).unwrap();
        tx.response = Some(response);
        tx.response_stream = stream;
        tx.stage = FetchStage::Streaming;
        let promise = tx.promise;
        self.settle(promise, Settlement::Fulfilled(Value::object(response)))
    }

    fn fetch_eof(&mut self, host: &mut dyn Host, id: u64) -> Result<(), VmError> {
        let tx = self.fetch_state.transactions.get_mut(&id).unwrap();
        if let Some(bytes) = tx.buffer.take() {
            let head = tx.head.clone().expect("final head processed before EOF");
            let null_body = no_body(&tx.hop, &head);
            let bytes = if null_body {
                Vec::new()
            } else {
                match decode(&head, bytes) {
                    Ok(bytes) => bytes,
                    Err(_) => return self.fail_fetch(id, "Fetch content decoding failed"),
                }
            };
            let tx = &self.fetch_state.transactions[&id];
            if !integrity_matches(&self.fetch_request_record(tx.request)?.integrity, &bytes) {
                return self.fail_fetch(id, "Fetch integrity mismatch");
            }
            self.expose_fetch_response(id, &head, self.fetch_response_type(tx)?)?;
            if let Some(stream) = self.fetch_state.transactions[&id].response_stream {
                self.enqueue_byte_vec(host, &ActiveRoots::EMPTY.with(&stream), stream, bytes)?;
            }
        }
        let mut tx = self.remove_fetch_hop(id, false)?.unwrap();
        self.release_fetch_upload(&mut tx)?;
        if let Some(stream) = tx.response_stream {
            self.close_body_stream(host, &ActiveRoots::EMPTY.with(&stream), stream)?;
        }
        Ok(())
    }
}
