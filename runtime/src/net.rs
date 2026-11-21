//! fetch and WebSocket transports (worker threads, completions on the JS thread).

use crate::event_loop::{begin_pending, post};
use crate::state::{state, State};
use crate::throw_error;
use nativescript_core::glib::{g_main_context_iteration, g_main_context_wakeup, gpointer};
use std::cell::Cell;
use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

type Args<'s> = v8::FunctionCallbackArguments<'s>;
type Rv<'s> = v8::ReturnValue<'s, v8::Value>;

fn str<'s>(scope: &mut v8::PinScope<'s, '_>, s: &str) -> v8::Local<'s, v8::String> {
    v8::String::new(scope, s).unwrap()
}

fn bytes_of(v: v8::Local<v8::Value>) -> Option<Vec<u8>> {
    if let Ok(view) = v8::Local::<v8::ArrayBufferView>::try_from(v) {
        return Some(
            unsafe { std::slice::from_raw_parts(view.data() as *const u8, view.byte_length()) }
                .to_vec(),
        );
    }
    if let Ok(buf) = v8::Local::<v8::ArrayBuffer>::try_from(v) {
        return Some(match buf.data() {
            Some(p) => {
                unsafe { std::slice::from_raw_parts(p.as_ptr() as *const u8, buf.byte_length()) }
                    .to_vec()
            }
            None => Vec::new(),
        });
    }
    None
}

fn array_buffer<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    bytes: Vec<u8>,
) -> v8::Local<'s, v8::ArrayBuffer> {
    let store = v8::ArrayBuffer::new_backing_store_from_vec(bytes).make_shared();
    v8::ArrayBuffer::with_backing_store(scope, &store)
}

struct HttpResponse {
    status: u16,
    status_text: String,
    url: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

fn agent(follow: bool) -> &'static ureq::Agent {
    static FOLLOW: OnceLock<ureq::Agent> = OnceLock::new();
    static MANUAL: OnceLock<ureq::Agent> = OnceLock::new();
    let cell = if follow { &FOLLOW } else { &MANUAL };
    cell.get_or_init(|| {
        // idle timeouts, not a total one: slow downloads keep going while bytes arrive
        ureq::AgentBuilder::new()
            .redirects(if follow { 20 } else { 0 })
            .timeout_connect(Duration::from_secs(30))
            .timeout_read(Duration::from_secs(120))
            .timeout_write(Duration::from_secs(120))
            .build()
    })
}

fn http(
    method: &str,
    url: &str,
    headers: &[(String, String)],
    body: Option<&[u8]>,
    follow: bool,
) -> Result<HttpResponse, String> {
    if url.starts_with("file://") {
        // app files come from the bundle when one is mounted
        let path = nativescript_core::resolve::file_url_to_path(url);
        let body = nativescript_core::nsbundle::read(&path)?.to_vec();
        return Ok(HttpResponse {
            status: 200,
            status_text: "OK".into(),
            url: url.into(),
            headers: vec![],
            body,
        });
    }
    let mut request = agent(follow).request(method, url);
    for (k, v) in headers {
        request = request.set(k, v);
    }
    let result = match body {
        Some(b) => request.send_bytes(b),
        None => request.call(),
    };
    let response = match result {
        Ok(r) => r,
        Err(ureq::Error::Status(_, r)) => r,
        Err(e) => return Err(e.to_string()),
    };
    let mut headers = Vec::new();
    for name in response.headers_names() {
        for value in response.all(&name) {
            headers.push((name.clone(), value.to_owned()));
        }
    }
    let (status, status_text, url) = (
        response.status(),
        response.status_text().to_owned(),
        response.get_url().to_owned(),
    );
    let mut body = Vec::new();
    response
        .into_reader()
        .read_to_end(&mut body)
        .map_err(|e| e.to_string())?;
    Ok(HttpResponse {
        status,
        status_text,
        url,
        headers,
        body,
    })
}

/// httpRequest(method, url, [[name, value]], body | null, redirect) -> Promise<{status, statusText, url, headers, body}>
fn http_request<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, mut rv: Rv<'s>) {
    let st = state(scope);
    let method = args.get(0).to_rust_string_lossy(scope);
    let url = args.get(1).to_rust_string_lossy(scope);
    let mut headers = Vec::new();
    if let Ok(list) = v8::Local::<v8::Array>::try_from(args.get(2)) {
        for i in 0..list.length() {
            let Some(pair) = list
                .get_index(scope, i)
                .and_then(|p| v8::Local::<v8::Array>::try_from(p).ok())
            else {
                continue;
            };
            let k = pair
                .get_index(scope, 0)
                .unwrap()
                .to_rust_string_lossy(scope);
            let v = pair
                .get_index(scope, 1)
                .unwrap()
                .to_rust_string_lossy(scope);
            headers.push((k, v));
        }
    }
    let body = bytes_of(args.get(3));
    let follow = args.get(4).to_rust_string_lossy(scope) != "manual";

    let Some(resolver) = v8::PromiseResolver::new(scope) else {
        return;
    };
    rv.set(resolver.get_promise(scope).into());
    let id = st.next_request.get() + 1;
    st.next_request.set(id);
    st.requests
        .borrow_mut()
        .insert(id, v8::Global::new(scope, resolver));
    begin_pending(&st);
    let context = st.main_context.context;
    std::thread::spawn(move || {
        let result = http(&method, &url, &headers, body.as_deref(), follow);
        post(
            context,
            true,
            Box::new(move |scope, st| {
                let Some(resolver) = st.requests.borrow_mut().remove(&id) else {
                    return;
                };
                let resolver = v8::Local::new(scope, resolver);
                match result {
                    Ok(r) => {
                        let object = v8::Object::new(scope);
                        let pairs: Vec<v8::Local<v8::Value>> = r
                            .headers
                            .iter()
                            .map(|(k, v)| {
                                let items = [str(scope, k).into(), str(scope, v).into()];
                                v8::Array::new_with_elements(scope, &items).into()
                            })
                            .collect();
                        let fields: [(&str, v8::Local<v8::Value>); 5] = [
                            ("status", v8::Integer::new(scope, r.status as i32).into()),
                            ("statusText", str(scope, &r.status_text).into()),
                            ("url", str(scope, &r.url).into()),
                            (
                                "headers",
                                v8::Array::new_with_elements(scope, &pairs).into(),
                            ),
                            ("body", array_buffer(scope, r.body).into()),
                        ];
                        for (k, v) in fields {
                            let k = str(scope, k);
                            object.set(scope, k.into(), v);
                        }
                        resolver.resolve(scope, object.into());
                    }
                    Err(e) => {
                        let message = str(scope, &format!("fetch failed: {e}"));
                        let error = v8::Exception::type_error(scope, message);
                        resolver.reject(scope, error);
                    }
                }
            }),
        );
    });
}

pub enum WsCommand {
    Text(String),
    Binary(Vec<u8>),
    Close(u16, String),
}

/// The JS side of a socket: commands for its thread, and a wakeup for that thread's poll().
pub struct WsHandle {
    commands: Sender<WsCommand>,
    wake: UnixStream,
}

enum WsEvent {
    Open(String),
    Text(String),
    Binary(Vec<u8>),
    Error(String),
    Close(u16, String, bool),
}

fn dispatch_ws(context: usize, id: u32, event: WsEvent) {
    let closing = matches!(event, WsEvent::Close(..));
    post(
        context,
        closing,
        Box::new(move |scope, st| {
            let Some(dispatch) = st.ws_dispatch.borrow().clone() else {
                return;
            };
            let dispatch = v8::Local::new(scope, dispatch);
            let id_v = v8::Integer::new_from_unsigned(scope, id).into();
            let args: Vec<v8::Local<v8::Value>> = match event {
                WsEvent::Open(protocol) => vec![
                    id_v,
                    str(scope, "open").into(),
                    str(scope, &protocol).into(),
                ],
                WsEvent::Text(t) => vec![id_v, str(scope, "message").into(), str(scope, &t).into()],
                WsEvent::Binary(b) => vec![
                    id_v,
                    str(scope, "message").into(),
                    array_buffer(scope, b).into(),
                ],
                WsEvent::Error(e) => vec![id_v, str(scope, "error").into(), str(scope, &e).into()],
                WsEvent::Close(code, reason, clean) => {
                    st.websockets.borrow_mut().remove(&id);
                    vec![
                        id_v,
                        str(scope, "close").into(),
                        v8::Integer::new(scope, code as i32).into(),
                        str(scope, &reason).into(),
                        v8::Boolean::new(scope, clean).into(),
                    ]
                }
            };
            let recv = v8::undefined(scope).into();
            dispatch.call(scope, recv, &args);
        }),
    );
}

type WsSocket = tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<TcpStream>>;

/// (code, reason, wasClean) of the close event.
type WsClose = (u16, String, bool);

/// How long close() waits for the server's reply (Firefox's network.websocket.timeout.close).
const WS_CLOSE_TIMEOUT: Duration = Duration::from_secs(20);

fn abnormal() -> WsClose {
    (1006, String::new(), false)
}

fn would_block(e: &tungstenite::Error) -> bool {
    matches!(e, tungstenite::Error::Io(e) if e.kind() == ErrorKind::WouldBlock)
}

/// Waits up to `ms` (-1: forever) for `fd` to be ready for `events`.
fn poll_fds(fds: &mut [libc::pollfd], ms: i32) {
    let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, ms) };
    if n < 0 {
        for f in fds.iter_mut() {
            f.revents = 0;
        }
    }
}

/// Gives queued frames (a close reply) up to `limit` to go out.
fn flush_for(socket: &mut WsSocket, fd: RawFd, limit: Duration) {
    let deadline = Instant::now() + limit;
    loop {
        match socket.flush() {
            Err(e) if would_block(&e) => {}
            _ => return,
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return;
        }
        let mut fds = [libc::pollfd {
            fd,
            events: libc::POLLOUT,
            revents: 0,
        }];
        poll_fds(&mut fds, left.as_millis().clamp(1, i32::MAX as u128) as i32);
    }
}

fn ws_thread(
    context: usize,
    id: u32,
    url: String,
    protocols: Vec<String>,
    commands: Receiver<WsCommand>,
    wake: UnixStream,
) {
    /// JS is owed exactly one close event, even if this thread panics.
    struct Owed {
        context: usize,
        id: u32,
    }
    impl Drop for Owed {
        fn drop(&mut self) {
            dispatch_ws(
                self.context,
                self.id,
                WsEvent::Error("WebSocket thread failed".into()),
            );
            let (code, reason, clean) = abnormal();
            dispatch_ws(self.context, self.id, WsEvent::Close(code, reason, clean));
        }
    }
    let owed = Owed { context, id };
    let (code, reason, clean) = ws_run(context, id, &url, &protocols, &commands, &wake);
    std::mem::forget(owed);
    dispatch_ws(context, id, WsEvent::Close(code, reason, clean));
}

/// Runs a connection until it closes; dispatches everything but the close event.
fn ws_run(
    context: usize,
    id: u32,
    url: &str,
    protocols: &[String],
    commands: &Receiver<WsCommand>,
    wake: &UnixStream,
) -> WsClose {
    use tungstenite::client::IntoClientRequest;
    use tungstenite::stream::MaybeTlsStream;
    use tungstenite::{Error, Message};

    let error = |e: String| dispatch_ws(context, id, WsEvent::Error(e));
    let connect = || -> Result<_, String> {
        let mut request = url.into_client_request().map_err(|e| e.to_string())?;
        if !protocols.is_empty() {
            let value = protocols
                .join(", ")
                .parse()
                .map_err(|_| "invalid protocol")?;
            request
                .headers_mut()
                .insert("Sec-WebSocket-Protocol", value);
        }
        tungstenite::connect(request).map_err(|e| e.to_string())
    };
    let (mut socket, response) = match connect() {
        Ok(s) => s,
        Err(e) => {
            error(e);
            return abnormal();
        }
    };
    let tcp = match socket.get_ref() {
        MaybeTlsStream::Plain(s) => s,
        MaybeTlsStream::Rustls(s) => s.get_ref(),
        _ => {
            error("unsupported WebSocket stream".into());
            return abnormal();
        }
    };
    // Never block: a write the network can't take yet (a stalled uplink) must not stop
    // reads, or pings go unanswered and the server's FIN/RST is never seen. Writes that
    // would block stay queued in tungstenite and go out when poll() says writable.
    if let Err(e) = tcp.set_nonblocking(true) {
        error(e.to_string());
        return abnormal();
    }
    // small frames (game input, pongs) go out now, as browsers do
    let _ = tcp.set_nodelay(true);
    let fd = tcp.as_raw_fd();
    let protocol = response
        .headers()
        .get("Sec-WebSocket-Protocol")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    dispatch_ws(context, id, WsEvent::Open(protocol));

    let mut detached = false;
    let mut closing: Option<Instant> = None;
    loop {
        // drain wakeups first: a command sent after this is followed by a new wakeup
        let mut sink = [0u8; 64];
        while !detached && matches!((&*wake).read(&mut sink), Ok(n) if n > 0) {}

        // JS -> socket (queued by tungstenite, flushed below)
        loop {
            let queued = match commands.try_recv() {
                Ok(WsCommand::Text(t)) => socket.write(Message::text(t)),
                Ok(WsCommand::Binary(b)) => socket.write(Message::binary(b)),
                Ok(WsCommand::Close(code, reason)) => {
                    closing.get_or_insert_with(|| Instant::now() + WS_CLOSE_TIMEOUT);
                    socket.close(Some(tungstenite::protocol::CloseFrame {
                        code: code.into(),
                        reason: reason.into(),
                    }))
                }
                Err(TryRecvError::Empty) => break,
                // the runtime is gone: close politely, stop listening for commands
                Err(TryRecvError::Disconnected) => {
                    if detached {
                        break;
                    }
                    detached = true;
                    closing.get_or_insert_with(|| Instant::now() + WS_CLOSE_TIMEOUT);
                    socket.close(None)
                }
            };
            match queued {
                Ok(()) => {}
                Err(e) if would_block(&e) => {}
                Err(Error::ConnectionClosed | Error::AlreadyClosed) => {}
                Err(Error::Io(e)) => {
                    error(e.to_string());
                    return abnormal();
                }
                Err(e) => error(e.to_string()),
            }
        }

        // socket -> JS: read until it would block (whole frames may sit in tungstenite's or
        // rustls' buffers, where poll() can't see them)
        loop {
            match socket.read() {
                Ok(Message::Text(t)) => dispatch_ws(context, id, WsEvent::Text(t.to_string())),
                Ok(Message::Binary(b)) => dispatch_ws(context, id, WsEvent::Binary(b.to_vec())),
                Ok(Message::Close(frame)) => {
                    let (code, reason) = frame
                        .map(|f| (u16::from(f.code), f.reason.to_string()))
                        .unwrap_or((1005, String::new()));
                    // tungstenite queued the close reply
                    flush_for(&mut socket, fd, Duration::from_secs(1));
                    return (code, reason, true);
                }
                // pings: tungstenite queues the pong, flushed below
                Ok(_) => {}
                Err(e) if would_block(&e) => break,
                Err(Error::Io(e)) if e.kind() == ErrorKind::Interrupted => {}
                Err(Error::ConnectionClosed | Error::AlreadyClosed) => {
                    return (1000, String::new(), true);
                }
                Err(e) => {
                    error(e.to_string());
                    return abnormal();
                }
            }
        }

        // frames from JS, pongs, close frames
        let want_write = match socket.flush() {
            Ok(()) => false,
            Err(e) if would_block(&e) => true,
            Err(Error::ConnectionClosed | Error::AlreadyClosed) => false,
            Err(Error::Io(e)) => {
                error(e.to_string());
                return abnormal();
            }
            Err(e) => {
                error(e.to_string());
                false
            }
        };

        // a peer that never answers our close frame
        if closing.is_some_and(|deadline| Instant::now() >= deadline) {
            return abnormal();
        }

        let mut fds = [
            libc::pollfd {
                fd,
                events: libc::POLLIN | if want_write { libc::POLLOUT } else { 0 },
                revents: 0,
            },
            libc::pollfd {
                // negative: ignored once the JS side is gone (its end would poll as hung up)
                fd: if detached { -1 } else { wake.as_raw_fd() },
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        poll_fds(&mut fds, 1000);
    }
}

/// wsConnect(url, protocols) -> id
fn ws_connect<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, mut rv: Rv<'s>) {
    let st = state(scope);
    let url = args.get(0).to_rust_string_lossy(scope);
    let mut protocols = Vec::new();
    if let Ok(list) = v8::Local::<v8::Array>::try_from(args.get(1)) {
        for i in 0..list.length() {
            protocols.push(
                list.get_index(scope, i)
                    .unwrap()
                    .to_rust_string_lossy(scope),
            );
        }
    }
    let (wake, wake_rx) = match UnixStream::pair().and_then(|(a, b)| {
        a.set_nonblocking(true)?;
        b.set_nonblocking(true)?;
        Ok((a, b))
    }) {
        Ok(pair) => pair,
        Err(e) => return throw_error(scope, &format!("WebSocket: {e}")),
    };
    let id = st.next_request.get() + 1;
    st.next_request.set(id);
    let (tx, rx) = channel();
    st.websockets
        .borrow_mut()
        .insert(id, WsHandle { commands: tx, wake });
    begin_pending(&st);
    let context = st.main_context.context;
    std::thread::spawn(move || ws_thread(context, id, url, protocols, rx, wake_rx));
    rv.set(v8::Integer::new_from_unsigned(scope, id).into());
}

/// False for an unknown socket. A socket whose thread has finished takes commands
/// silently: its close event is already on the way.
fn ws_command(st: &State, id: u32, command: WsCommand) -> bool {
    let sockets = st.websockets.borrow();
    let Some(handle) = sockets.get(&id) else {
        return false;
    };
    if handle.commands.send(command).is_ok() {
        // full: a wakeup is already pending
        let _ = (&handle.wake).write(&[1]);
    }
    true
}

/// wsSend(id, string | buffer)
fn ws_send<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, _rv: Rv<'s>) {
    let st = state(scope);
    let id = args.get(0).uint32_value(scope).unwrap_or(0);
    let data = args.get(1);
    let command = match bytes_of(data) {
        Some(b) => WsCommand::Binary(b),
        None => WsCommand::Text(data.to_rust_string_lossy(scope)),
    };
    if !ws_command(&st, id, command) {
        throw_error(scope, "WebSocket is not open");
    }
}

/// wsClose(id, code, reason)
fn ws_close<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, _rv: Rv<'s>) {
    let st = state(scope);
    let id = args.get(0).uint32_value(scope).unwrap_or(0);
    let code = args.get(1).uint32_value(scope).unwrap_or(1000) as u16;
    let reason = args.get(2).to_rust_string_lossy(scope);
    ws_command(&st, id, WsCommand::Close(code, reason));
}

/// wsSetDispatch(fn(id, type, ...data))
fn ws_set_dispatch<'s>(scope: &mut v8::PinScope<'s, '_>, args: Args<'s>, _rv: Rv<'s>) {
    let st = state(scope);
    if let Ok(f) = v8::Local::<v8::Function>::try_from(args.get(0)) {
        *st.ws_dispatch.borrow_mut() = Some(v8::Global::new(scope, f));
    }
}

static DEV_BOOT_COMPLETE: AtomicBool = AtomicBool::new(false);

thread_local! {
    static PUMPING: Cell<bool> = const { Cell::new(false) };
}

/// `ns:module` `setDevBootComplete`: stops pumping the loop during URL imports.
pub fn set_dev_boot_complete(done: bool) {
    DEV_BOOT_COMPLETE.store(done, Ordering::Relaxed);
}

/// Until dev boot completes, fetches off-thread and keeps the loop running meanwhile.
fn get_module(url: &str) -> Result<HttpResponse, String> {
    let st = crate::event_loop::current();
    let Some(st) = st.filter(|_| !DEV_BOOT_COMPLETE.load(Ordering::Relaxed) && !PUMPING.get())
    else {
        return http("GET", url, &[], None, true);
    };
    let context = st.main_context.context;
    let (tx, rx) = channel();
    let owned = url.to_owned();
    std::thread::spawn(move || {
        let _ = tx.send(http("GET", &owned, &[], None, true));
        unsafe { g_main_context_wakeup(context as gpointer) };
    });
    PUMPING.set(true);
    let result = loop {
        match rx.try_recv() {
            Ok(r) => break r,
            Err(TryRecvError::Empty) => unsafe {
                g_main_context_iteration(context as gpointer, 1);
            },
            Err(TryRecvError::Disconnected) => break Err(format!("{url}: fetch failed")),
        }
    };
    PUMPING.set(false);
    result
}

/// Blocking GET used for URL module imports.
pub fn fetch_text(url: &str) -> Result<String, String> {
    let r = get_module(url)?;
    // an https module may redirect to http
    nativescript_core::resolve::check_remote(&r.url)?;
    if r.status >= 400 {
        return Err(format!("{url}: HTTP {}", r.status));
    }
    String::from_utf8(r.body).map_err(|_| format!("{url}: not UTF-8"))
}

pub fn install<'s>(scope: &mut v8::PinScope<'s, '_>, natives: v8::Local<'s, v8::Object>) {
    macro_rules! set {
        ($name:literal, $f:expr) => {{
            let f = v8::Function::new(scope, $f).unwrap();
            let k = str(scope, $name);
            natives.set(scope, k.into(), f.into());
        }};
    }
    set!("httpRequest", http_request);
    set!("wsConnect", ws_connect);
    set!("wsSend", ws_send);
    set!("wsClose", ws_close);
    set!("wsSetDispatch", ws_set_dispatch);
}
