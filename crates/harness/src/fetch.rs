//! Opt-in, single-hop HTTP/1.1 fixtures for the Fetch host protocol.
//!
//! Socket workers never call the VM. Each registration owns a bounded FIFO;
//! `take` transfers its events and `drain` invokes a callback on the caller's
//! thread. Queue exhaustion closes the socket and reserves one terminal event.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpStream};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use yield_vm::binding::{HostFault, OperationToken};
use yield_vm::fetch::{
    FetchEvent, FetchFailure, FetchRegistration, FetchService, HopRequest, ResponseHead, UploadBody,
};

const WINDOW: usize = 16 * 1024;
const EVENTS: usize = 64;
const HEAD_LIMIT: usize = 64 * 1024;

/// Plain HTTP to explicitly allowed `127.0.0.1:port` endpoints only.
/// No DNS, TLS, retries, redirects, proxy, cookie jar or content decoding.
pub struct LoopbackFetchService {
    allowlist: BTreeSet<String>,
    live: Arc<Mutex<BTreeMap<OperationToken, Arc<Exchange>>>>,
}

impl LoopbackFetchService {
    pub fn new(allowlist: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            allowlist: allowlist.into_iter().map(Into::into).collect(),
            live: Arc::default(),
        }
    }

    /// Remove the events currently queued for this registration, in wire order.
    /// An unknown, dropped or canceled registration yields no events.
    pub fn take(&self, token: OperationToken) -> Vec<FetchEvent> {
        let exchange = self.live.lock().unwrap().get(&token).cloned();
        exchange.map_or_else(Vec::new, |exchange| {
            exchange.state.lock().unwrap().events.drain(..).collect()
        })
    }

    /// Tokens with a registration that has not been dropped yet.
    pub fn tokens(&self) -> Vec<OperationToken> {
        self.live.lock().unwrap().keys().copied().collect()
    }

    /// Deliver one FIFO batch without holding locks across callbacks.
    pub fn drain(&self, token: OperationToken, deliver: impl Fn(FetchEvent)) {
        self.take(token).into_iter().for_each(deliver);
    }

    fn endpoint(&self, request: &HopRequest) -> Option<SocketAddr> {
        let port = request.url.port_or_known_default()?;
        (request.url.scheme() == "http"
            && request.url.host_str() == Some("127.0.0.1")
            && request.url.username().is_empty()
            && request.url.password().is_none()
            && self.allowlist.contains(&format!("127.0.0.1:{port}")))
        .then(|| SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
    }
}

impl FetchService for LoopbackFetchService {
    fn start(
        &self,
        token: OperationToken,
        request: HopRequest,
    ) -> Result<Box<dyn FetchRegistration>, HostFault> {
        let exchange = Arc::new(Exchange::new(&request.upload));
        {
            use std::collections::btree_map::Entry;
            match self.live.lock().unwrap().entry(token) {
                Entry::Vacant(entry) => {
                    entry.insert(exchange.clone());
                }
                Entry::Occupied(_) => return Err(fault("duplicate fetch token")),
            }
        }
        if let Some(endpoint) = self.endpoint(&request) {
            let worker = exchange.clone();
            if let Err(error) = thread::Builder::new().spawn(move || run(worker, endpoint, request))
            {
                exchange.emit(FetchEvent::Failed(FetchFailure::Internal(
                    error.to_string(),
                )));
            }
        } else {
            exchange.emit(FetchEvent::Failed(FetchFailure::Policy(
                "HTTP endpoint is not allowed".into(),
            )));
        }
        Ok(Box::new(LoopbackRegistration {
            live: self.live.clone(),
            token,
            exchange,
        }))
    }
}

struct Exchange {
    state: Mutex<State>,
    ready: Condvar,
}

struct State {
    events: VecDeque<FetchEvent>,
    socket: Option<TcpStream>,
    stopped: bool,
    canceled: bool,
    download: u64,
    upload_credit: usize,
    upload_remaining: Option<u64>,
    upload_finished: bool,
    upload_queue: VecDeque<Vec<u8>>,
}

impl Exchange {
    fn new(upload: &UploadBody) -> Self {
        Self {
            state: Mutex::new(State {
                events: VecDeque::new(),
                socket: None,
                stopped: false,
                download: 0,
                canceled: false,
                upload_credit: 0,
                upload_remaining: match upload {
                    UploadBody::None => Some(0),
                    UploadBody::Known(len) => Some(*len),
                    UploadBody::Unknown => None,
                },
                upload_finished: *upload == UploadBody::None,
                upload_queue: VecDeque::new(),
            }),
            ready: Condvar::new(),
        }
    }

    fn emit(&self, event: FetchEvent) -> bool {
        self.emit_locked(&mut self.state.lock().unwrap(), event)
    }

    fn emit_locked(&self, state: &mut State, event: FetchEvent) -> bool {
        if state.stopped {
            return false;
        }
        let terminal = matches!(event, FetchEvent::Eof | FetchEvent::Failed(_));
        if terminal {
            state.events.push_back(event);
        } else if state.events.len() == EVENTS {
            state
                .events
                .push_back(FetchEvent::Failed(FetchFailure::Internal(
                    "fetch event queue exhausted".into(),
                )));
        } else {
            state.events.push_back(event);
            return true;
        }
        self.stop(state);
        false
    }

    fn stop(&self, state: &mut State) {
        state.stopped = true;
        state.upload_queue.clear();
        if let Some(socket) = state.socket.take() {
            let _ = socket.shutdown(Shutdown::Both);
        }
        self.ready.notify_all();
    }

    fn credit(&self, bytes: usize) {
        let mut state = self.state.lock().unwrap();
        if state.stopped || state.upload_finished {
            return;
        }
        let available = state
            .upload_remaining
            .unwrap_or(u64::MAX)
            .saturating_sub(state.upload_credit as u64);
        let bytes = (bytes as u64).min(available) as usize;
        if bytes != 0 && self.emit_locked(&mut state, FetchEvent::UploadCredit(bytes as u64)) {
            state.upload_credit += bytes;
        }
    }

    fn read_budget(&self, max: usize) -> usize {
        let state = self
            .ready
            .wait_while(self.state.lock().unwrap(), |state| {
                !state.stopped && state.download == 0
            })
            .unwrap();
        if state.stopped {
            0
        } else {
            state.download.min(max as u64) as usize
        }
    }

    fn data(&self, bytes: Vec<u8>) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.stopped {
            return false;
        }
        state.download -= bytes.len() as u64;
        self.emit_locked(&mut state, FetchEvent::Data(bytes))
    }
}

struct LoopbackRegistration {
    live: Arc<Mutex<BTreeMap<OperationToken, Arc<Exchange>>>>,
    token: OperationToken,
    exchange: Arc<Exchange>,
}

impl FetchRegistration for LoopbackRegistration {
    fn send_upload(&mut self, bytes: Vec<u8>) -> Result<(), HostFault> {
        let mut state = self.exchange.state.lock().unwrap();
        if state.canceled || state.upload_finished || bytes.len() > state.upload_credit {
            return Err(fault("upload closed or exceeds credit"));
        }
        if !bytes.is_empty() {
            state.upload_credit -= bytes.len();
            if let Some(remaining) = &mut state.upload_remaining {
                *remaining -= bytes.len() as u64;
            }
            // A terminal event can overtake an already-issued credit. Accept
            // it without retaining more data or writing to a closed socket.
            if !state.stopped {
                state.upload_queue.push_back(bytes);
            }
            self.exchange.ready.notify_all();
        }
        Ok(())
    }

    fn finish_upload(&mut self) -> Result<(), HostFault> {
        let mut state = self.exchange.state.lock().unwrap();
        if state.canceled || state.upload_remaining.is_some_and(|len| len != 0) {
            return Err(fault("upload closed or incomplete"));
        }
        state.upload_finished = true;
        self.exchange.ready.notify_all();
        Ok(())
    }

    fn grant_download(&mut self, bytes: u64) -> Result<(), HostFault> {
        let mut state = self.exchange.state.lock().unwrap();
        if state.canceled {
            return Err(fault("download closed"));
        }
        // A response with no body can finish before the VM handles its Head.
        // The queued terminal event, not this racing grant, ends the exchange.
        if state.stopped {
            return Ok(());
        }
        state.download = state
            .download
            .checked_add(bytes)
            .ok_or_else(|| fault("download credit overflow"))?;
        self.exchange.ready.notify_all();
        Ok(())
    }

    fn cancel(&mut self) -> Result<(), HostFault> {
        let mut state = self.exchange.state.lock().unwrap();
        state.canceled = true;
        state.events.clear();
        self.exchange.stop(&mut state);
        Ok(())
    }
}

impl Drop for LoopbackRegistration {
    fn drop(&mut self) {
        let _ = self.cancel();
        self.live.lock().unwrap().remove(&self.token);
    }
}

fn fault(message: &str) -> HostFault {
    HostFault(message.into())
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn run(exchange: Arc<Exchange>, endpoint: SocketAddr, request: HopRequest) {
    match transfer(&exchange, endpoint, &request) {
        Ok(()) => exchange.emit(FetchEvent::Eof),
        Err(error) => exchange.emit(FetchEvent::Failed(FetchFailure::Network(error.to_string()))),
    };
}

fn transfer(
    exchange: &Arc<Exchange>,
    endpoint: SocketAddr,
    request: &HopRequest,
) -> io::Result<()> {
    let head = request_head(endpoint, request)?;
    if exchange.state.lock().unwrap().stopped {
        return Ok(());
    }
    let mut socket = TcpStream::connect_timeout(&endpoint, Duration::from_secs(5))?;
    {
        let mut state = exchange.state.lock().unwrap();
        if state.stopped {
            return Ok(());
        }
        state.socket = Some(socket.try_clone()?);
    }
    socket.write_all(&head)?;
    // Read independently of uploads: a server may return a final head before
    // consuming the request body, including while the writer is blocked.
    if request.upload != UploadBody::None {
        let mut writer = socket.try_clone()?;
        let chunked = request.upload == UploadBody::Unknown;
        let upload = exchange.clone();
        thread::Builder::new().spawn(move || {
            if let Err(error) = write_upload(&upload, &mut writer, chunked) {
                upload.emit(FetchEvent::Failed(FetchFailure::Network(error.to_string())));
            }
        })?;
    }
    let mut reader = BufReader::new(socket);
    loop {
        let head = response_head(&mut reader)?;
        if (100..200).contains(&head.status) && head.status != 101 {
            if !exchange.emit(FetchEvent::Informational(head)) {
                return Ok(());
            }
            continue;
        }
        let framing = framing(&request.method, &head)?;
        if !exchange.emit(FetchEvent::Head(head)) {
            return Ok(());
        }
        return read_body(exchange, &mut reader, framing);
    }
}

fn is_token(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}

fn request_head(endpoint: SocketAddr, request: &HopRequest) -> io::Result<Vec<u8>> {
    if request.method.is_empty() || !request.method.bytes().all(is_token) {
        return Err(invalid("invalid request method"));
    }
    let path = request.url.path();
    let mut head = Vec::new();
    write!(
        head,
        "{} {}",
        request.method,
        if path.is_empty() { "/" } else { path }
    )?;
    if let Some(query) = request.url.query() {
        write!(head, "?{query}")?;
    }
    write!(
        head,
        " HTTP/1.1\r\nHost: {endpoint}\r\nConnection: close\r\n"
    )?;
    for (name, value) in &request.headers {
        // The transport owns request routing, persistence and framing headers.
        if [
            b"host".as_slice(),
            b"connection",
            b"content-length",
            b"transfer-encoding",
            b"upgrade",
            b"proxy-connection",
        ]
        .iter()
        .any(|reserved| name.eq_ignore_ascii_case(reserved))
        {
            continue;
        }
        if request.origin.is_some() && name.eq_ignore_ascii_case(b"origin") {
            continue;
        }
        append_header(&mut head, name, value)?;
    }
    if let Some(origin) = &request.origin {
        append_header(&mut head, b"Origin", origin.as_bytes())?;
    }
    match request.upload {
        UploadBody::None => {}
        UploadBody::Known(len) => write!(head, "Content-Length: {len}\r\n")?,
        UploadBody::Unknown => head.extend_from_slice(b"Transfer-Encoding: chunked\r\n"),
    }
    head.extend_from_slice(b"\r\n");
    Ok(head)
}

fn append_header(head: &mut Vec<u8>, name: &[u8], value: &[u8]) -> io::Result<()> {
    if name.is_empty()
        || !name.iter().copied().all(is_token)
        || !value.iter().all(|&b| b == b'\t' || (b >= b' ' && b != 127))
    {
        return Err(invalid("invalid request header"));
    }
    head.extend_from_slice(name);
    head.extend_from_slice(b": ");
    head.extend_from_slice(value);
    head.extend_from_slice(b"\r\n");
    Ok(())
}

fn write_upload(exchange: &Exchange, socket: &mut TcpStream, chunked: bool) -> io::Result<()> {
    exchange.credit(WINDOW);
    loop {
        let bytes = {
            let mut state = exchange
                .ready
                .wait_while(exchange.state.lock().unwrap(), |state| {
                    !state.stopped && !state.upload_finished && state.upload_queue.is_empty()
                })
                .unwrap();
            if state.stopped {
                return Ok(());
            }
            state.upload_queue.pop_front()
        };
        let Some(bytes) = bytes else {
            if chunked {
                socket.write_all(b"0\r\n\r\n")?;
            }
            return Ok(());
        };
        if chunked {
            write!(socket, "{:x}\r\n", bytes.len())?;
        }
        socket.write_all(&bytes)?;
        if chunked {
            socket.write_all(b"\r\n")?;
        }
        exchange.credit(bytes.len());
    }
}

fn line(reader: &mut impl BufRead, out: &mut Vec<u8>, limit: usize) -> io::Result<()> {
    let offset = out.len();
    reader.take(limit as u64).read_until(b'\n', out)?;
    if out[offset..].ends_with(b"\r\n") {
        Ok(())
    } else {
        Err(invalid("truncated or oversized HTTP line"))
    }
}

fn response_head(reader: &mut impl BufRead) -> io::Result<ResponseHead> {
    let mut raw = Vec::new();
    loop {
        let offset = raw.len();
        line(reader, &mut raw, HEAD_LIMIT - offset)?;
        if raw[offset..] == *b"\r\n" && offset != 0 {
            break;
        }
    }
    let mut headers = [httparse::EMPTY_HEADER; 128];
    let mut response = httparse::Response::new(&mut headers);
    if response.parse(&raw).map_err(io::Error::other)? != httparse::Status::Complete(raw.len()) {
        return Err(invalid("incomplete response head"));
    }
    // httparse exposes the reason as UTF-8 only. Preserve its original bytes.
    let status_line = raw
        .split(|&b| b == b'\n')
        .find(|line| line.starts_with(b"HTTP/"))
        .ok_or_else(|| invalid("missing status line"))?;
    let status_text = status_line
        .strip_suffix(b"\r")
        .unwrap_or(status_line)
        .get(13..)
        .unwrap_or_default()
        .to_vec();
    Ok(ResponseHead {
        status: response
            .code
            .ok_or_else(|| invalid("missing response status"))?,
        status_text,
        headers: response
            .headers
            .iter()
            .map(|header| (header.name.as_bytes().to_vec(), header.value.to_vec()))
            .collect(),
    })
}

enum Framing {
    Empty,
    Length(u64),
    Chunked,
    Close,
}

fn framing(method: &str, head: &ResponseHead) -> io::Result<Framing> {
    if method == "HEAD"
        || matches!(head.status, 101 | 204 | 205 | 304)
        || (method == "CONNECT" && (200..300).contains(&head.status))
    {
        return Ok(Framing::Empty);
    }
    let mut length = None;
    let mut chunked = false;
    for (name, value) in &head.headers {
        if name.eq_ignore_ascii_case(b"transfer-encoding") {
            if chunked || !value.trim_ascii().eq_ignore_ascii_case(b"chunked") {
                return Err(invalid("unsupported transfer encoding"));
            }
            chunked = true;
        } else if name.eq_ignore_ascii_case(b"content-length") {
            for item in value.split(|&byte| byte == b',') {
                let item = item.trim_ascii();
                let len = std::str::from_utf8(item)
                    .ok()
                    .filter(|item| item.bytes().all(|b| b.is_ascii_digit()))
                    .and_then(|item| item.parse::<u64>().ok())
                    .ok_or_else(|| invalid("invalid content length"))?;
                if length.replace(len).is_some_and(|old| old != len) {
                    return Err(invalid("conflicting content lengths"));
                }
            }
        }
    }
    match (chunked, length) {
        (true, Some(_)) => Err(invalid("ambiguous response framing")),
        (true, None) => Ok(Framing::Chunked),
        (false, Some(len)) => Ok(Framing::Length(len)),
        (false, None) => Ok(Framing::Close),
    }
}

fn read_body(exchange: &Exchange, reader: &mut impl BufRead, framing: Framing) -> io::Result<()> {
    match framing {
        Framing::Empty => {}
        Framing::Length(len) => {
            read_data(exchange, reader, Some(len))?;
        }
        Framing::Close => {
            read_data(exchange, reader, None)?;
        }
        Framing::Chunked => {
            let mut raw = Vec::new();
            loop {
                raw.clear();
                line(reader, &mut raw, HEAD_LIMIT)?;
                let len = match httparse::parse_chunk_size(&raw)
                    .map_err(|_| invalid("invalid chunk size"))?
                {
                    httparse::Status::Complete((end, len)) if end == raw.len() => len,
                    _ => return Err(invalid("invalid chunk line")),
                };
                if len == 0 {
                    // Consume bounded trailers; they are not part of ResponseHead.
                    raw.clear();
                    loop {
                        let offset = raw.len();
                        line(reader, &mut raw, HEAD_LIMIT - offset)?;
                        if raw[offset..] == *b"\r\n" {
                            break;
                        }
                    }
                    let mut trailers = [httparse::EMPTY_HEADER; 128];
                    if !matches!(httparse::parse_headers(&raw, &mut trailers), Ok(httparse::Status::Complete((end, _))) if end == raw.len())
                    {
                        return Err(invalid("invalid chunk trailers"));
                    }
                    break;
                }
                if !read_data(exchange, reader, Some(len))? {
                    break;
                }
                let mut crlf = [0; 2];
                reader.read_exact(&mut crlf)?;
                if crlf != *b"\r\n" {
                    return Err(invalid("missing chunk terminator"));
                }
            }
        }
    }
    Ok(())
}

// False means canceled or stopped. For close framing, look for EOF even at
// zero credit; BufReader can prefetch at most its fixed buffer capacity.
fn read_data(
    exchange: &Exchange,
    reader: &mut impl BufRead,
    mut remaining: Option<u64>,
) -> io::Result<bool> {
    while remaining != Some(0) {
        if remaining.is_none() && reader.fill_buf()?.is_empty() {
            return Ok(true);
        }
        let n = exchange.read_budget(remaining.unwrap_or(u64::MAX).min(WINDOW as u64) as usize);
        if n == 0 {
            return Ok(false);
        }
        let mut data = vec![0; n];
        let n = reader.read(&mut data)?;
        if n == 0 {
            return Err(invalid("premature HTTP body EOF"));
        }
        data.truncate(n);
        if let Some(remaining) = &mut remaining {
            *remaining -= n as u64;
        }
        if !exchange.data(data) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// A call recorded by the in-memory fixture, tagged by its operation token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FetchCall {
    Start(OperationToken, Box<HopRequest>),
    SendUpload(OperationToken, Vec<u8>),
    FinishUpload(OperationToken),
    GrantDownload(OperationToken, u64),
    Cancel(OperationToken),
}

/// Test fixture that deliberately does not enforce protocol ordering or credit.
/// Calls and events are independently driven so tests can exercise VM errors.
#[derive(Default)]
pub struct ControllableFetchService(Arc<Mutex<Control>>);

#[derive(Default)]
struct Control {
    calls: Vec<FetchCall>,
    live: BTreeMap<OperationToken, Vec<FetchEvent>>,
}

impl ControllableFetchService {
    /// Remove all recorded calls in invocation order.
    pub fn take_calls(&self) -> Vec<FetchCall> {
        std::mem::take(&mut self.0.lock().unwrap().calls)
    }

    /// Queue an arbitrary event for a registration; faults for an unknown token
    /// or overflow. Explicit cancellation is recorded, not simulated, so tests
    /// can queue late events while they still hold the registration.
    pub fn push(&self, token: OperationToken, event: FetchEvent) -> Result<(), HostFault> {
        let mut state = self.0.lock().unwrap();
        let queue = state
            .live
            .get_mut(&token)
            .ok_or_else(|| fault("unknown fetch token"))?;
        if queue.len() == EVENTS {
            return Err(fault("fetch event queue exhausted"));
        }
        queue.push(event);
        Ok(())
    }

    pub fn take(&self, token: OperationToken) -> Vec<FetchEvent> {
        self.0
            .lock()
            .unwrap()
            .live
            .get_mut(&token)
            .map_or_else(Vec::new, std::mem::take)
    }

    pub fn drain(&self, token: OperationToken, deliver: impl Fn(FetchEvent)) {
        self.take(token).into_iter().for_each(deliver);
    }
}

impl FetchService for ControllableFetchService {
    fn start(
        &self,
        token: OperationToken,
        request: HopRequest,
    ) -> Result<Box<dyn FetchRegistration>, HostFault> {
        {
            use std::collections::btree_map::Entry;
            let mut state = self.0.lock().unwrap();
            state.calls.push(FetchCall::Start(token, Box::new(request)));
            match state.live.entry(token) {
                Entry::Vacant(entry) => {
                    entry.insert(Vec::new());
                }
                Entry::Occupied(_) => return Err(fault("duplicate fetch token")),
            }
        }
        Ok(Box::new(ControlledRegistration {
            control: self.0.clone(),
            token,
        }))
    }
}

struct ControlledRegistration {
    control: Arc<Mutex<Control>>,
    token: OperationToken,
}

impl ControlledRegistration {
    fn record(&self, call: FetchCall) -> Result<(), HostFault> {
        self.control.lock().unwrap().calls.push(call);
        Ok(())
    }
}

impl FetchRegistration for ControlledRegistration {
    fn send_upload(&mut self, bytes: Vec<u8>) -> Result<(), HostFault> {
        self.record(FetchCall::SendUpload(self.token, bytes))
    }
    fn finish_upload(&mut self) -> Result<(), HostFault> {
        self.record(FetchCall::FinishUpload(self.token))
    }
    fn grant_download(&mut self, bytes: u64) -> Result<(), HostFault> {
        self.record(FetchCall::GrantDownload(self.token, bytes))
    }
    fn cancel(&mut self) -> Result<(), HostFault> {
        self.record(FetchCall::Cancel(self.token))
    }
}

impl Drop for ControlledRegistration {
    fn drop(&mut self) {
        self.control.lock().unwrap().live.remove(&self.token);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::time::Instant;

    fn request(url: &str) -> HopRequest {
        HopRequest {
            url: url.parse().unwrap(),
            method: "GET".into(),
            headers: Vec::new(),
            upload: UploadBody::None,
            origin: None,
            credentials: yield_vm::fetch::CredentialsMode::Omit,
            partition: String::new(),
            authorization: String::new(),
        }
    }

    #[test]
    fn fetch_transport_policy_requires_explicit_loopback_http() {
        let service = LoopbackFetchService::new(["127.0.0.1:80", "localhost:80"]);
        assert!(service.endpoint(&request("http://127.0.0.1/")).is_some());
        for url in [
            "https://127.0.0.1:80/",
            "http://127.0.0.1:81/",
            "http://localhost/",
            "http://attacker@127.0.0.1/",
        ] {
            assert!(service.endpoint(&request(url)).is_none(), "{url}");
        }
    }

    fn next_event(exchange: &Exchange) -> FetchEvent {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(event) = exchange.state.lock().unwrap().events.pop_front() {
                return event;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for fetch event"
            );
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn fetch_transport_chunked_redirect_is_one_credit_gated_exchange() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = listener.local_addr().unwrap();
        let request = request(&format!("http://{endpoint}/path?q=1#fragment"));
        let server = thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(socket);
            let mut head = Vec::new();
            loop {
                let offset = head.len();
                line(&mut reader, &mut head, HEAD_LIMIT - offset).unwrap();
                if head[offset..] == *b"\r\n" {
                    break;
                }
            }
            assert_eq!(
                head,
                format!("GET /path?q=1 HTTP/1.1\r\nHost: {endpoint}\r\nConnection: close\r\n\r\n")
                    .into_bytes()
            );
            reader.get_mut().write_all(b"HTTP/1.1 103 Early Hints\r\n\r\nHTTP/1.1 302 Moved\r\nLocation: /next\r\nSet-Cookie: a=1\r\nSet-Cookie: b=2\r\nTransfer-Encoding: chunked\r\n\r\n5;ext=x\r\nhello\r\n0\r\nTrailer: ignored\r\n\r\n").unwrap();
        });
        let exchange = Arc::new(Exchange::new(&request.upload));
        let worker = exchange.clone();
        let client = thread::spawn(move || run(worker, endpoint, request));
        assert!(
            matches!(next_event(&exchange), FetchEvent::Informational(head) if head.status == 103)
        );
        let FetchEvent::Head(head) = next_event(&exchange) else {
            panic!("expected response head")
        };
        assert_eq!(head.status, 302);
        assert_eq!(head.status_text, b"Moved");
        assert_eq!(head.headers[1], (b"Set-Cookie".to_vec(), b"a=1".to_vec()));
        assert_eq!(head.headers[2], (b"Set-Cookie".to_vec(), b"b=2".to_vec()));
        for (credit, expected) in [(2, &b"he"[..]), (3, &b"llo"[..])] {
            {
                let mut state = exchange.state.lock().unwrap();
                assert!(state.events.is_empty());
                state.download += credit;
                exchange.ready.notify_all();
            }
            let mut received = Vec::new();
            while received.len() < expected.len() {
                let FetchEvent::Data(data) = next_event(&exchange) else {
                    panic!("expected data")
                };
                received.extend(data);
            }
            assert_eq!(received, expected);
        }
        assert_eq!(next_event(&exchange), FetchEvent::Eof);
        client.join().unwrap();
        server.join().unwrap();
    }

    #[test]
    fn fetch_transport_conflicting_lengths_fail() {
        let mut raw = &b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\n"[..];
        assert!(framing("GET", &response_head(&mut raw).unwrap()).is_err());
    }
}

#[cfg(test)]
mod framing_tests {
    use super::*;

    #[test]
    fn fetch_transport_length_is_exact_and_truncation_faults() {
        let exchange = Exchange::new(&UploadBody::None);
        exchange.state.lock().unwrap().download = 5;
        let mut raw = &b"helloextra"[..];
        read_body(&exchange, &mut raw, Framing::Length(5)).unwrap();
        assert_eq!(raw, b"extra");
        let mut state = exchange.state.lock().unwrap();
        assert_eq!(
            state.events.pop_front(),
            Some(FetchEvent::Data(b"hello".to_vec()))
        );
        state.download = 6;
        drop(state);
        assert_eq!(
            read_body(&exchange, &mut &b"short"[..], Framing::Length(6))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn fetch_transport_queue_reserves_one_terminal_slot() {
        let exchange = Exchange::new(&UploadBody::Unknown);
        for _ in 0..EVENTS {
            assert!(exchange.emit(FetchEvent::UploadCredit(1)));
        }
        assert!(!exchange.emit(FetchEvent::UploadCredit(1)));
        assert!(!exchange.emit(FetchEvent::Eof));
        let state = exchange.state.lock().unwrap();
        assert!(state.stopped);
        assert_eq!(state.events.len(), EVENTS + 1);
        assert!(matches!(
            state.events.back(),
            Some(FetchEvent::Failed(FetchFailure::Internal(_)))
        ));
    }
}
