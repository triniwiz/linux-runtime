//! fetch and WebSocket transports (worker threads, completions on the JS thread).

use crate::event_loop::{begin_pending, post};
use crate::state::{state, State};
use crate::throw_error;
use std::io::Read;
use std::sync::mpsc::{channel, Sender, TryRecvError};
use std::sync::OnceLock;
use std::time::Duration;

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

fn ws_thread(
    context: usize,
    id: u32,
    url: String,
    protocols: Vec<String>,
    commands: std::sync::mpsc::Receiver<WsCommand>,
) {
    use tungstenite::client::IntoClientRequest;
    use tungstenite::stream::MaybeTlsStream;
    use tungstenite::Message;

    let connect = || -> Result<_, String> {
        let mut request = url
            .as_str()
            .into_client_request()
            .map_err(|e| e.to_string())?;
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
            dispatch_ws(context, id, WsEvent::Error(e));
            dispatch_ws(context, id, WsEvent::Close(1006, String::new(), false));
            return;
        }
    };
    // short read timeout so outgoing commands are picked up
    let timeout = Some(Duration::from_millis(20));
    match socket.get_mut() {
        MaybeTlsStream::Plain(s) => {
            let _ = s.set_read_timeout(timeout);
        }
        MaybeTlsStream::Rustls(s) => {
            let _ = s.get_mut().set_read_timeout(timeout);
        }
        _ => {}
    }
    let protocol = response
        .headers()
        .get("Sec-WebSocket-Protocol")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    dispatch_ws(context, id, WsEvent::Open(protocol));

    loop {
        loop {
            let sent = match commands.try_recv() {
                Ok(WsCommand::Text(t)) => socket.send(Message::text(t)),
                Ok(WsCommand::Binary(b)) => socket.send(Message::binary(b)),
                Ok(WsCommand::Close(code, reason)) => {
                    socket.close(Some(tungstenite::protocol::CloseFrame {
                        code: code.into(),
                        reason: reason.into(),
                    }))
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => socket.close(None),
            };
            if let Err(e) = sent {
                if !matches!(
                    e,
                    tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed
                ) {
                    dispatch_ws(context, id, WsEvent::Error(e.to_string()));
                }
            }
        }
        match socket.read() {
            Ok(Message::Text(t)) => dispatch_ws(context, id, WsEvent::Text(t.to_string())),
            Ok(Message::Binary(b)) => dispatch_ws(context, id, WsEvent::Binary(b.to_vec())),
            Ok(Message::Close(frame)) => {
                let (code, reason) = frame
                    .map(|f| (u16::from(f.code), f.reason.to_string()))
                    .unwrap_or((1005, String::new()));
                // tungstenite replies to the close frame on the next read/flush
                let _ = socket.flush();
                dispatch_ws(context, id, WsEvent::Close(code, reason, true));
                return;
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(e))
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                dispatch_ws(context, id, WsEvent::Close(1000, String::new(), true));
                return;
            }
            Err(e) => {
                dispatch_ws(context, id, WsEvent::Error(e.to_string()));
                dispatch_ws(context, id, WsEvent::Close(1006, String::new(), false));
                return;
            }
        }
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
    let id = st.next_request.get() + 1;
    st.next_request.set(id);
    let (tx, rx) = channel();
    st.websockets.borrow_mut().insert(id, tx);
    begin_pending(&st);
    let context = st.main_context.context;
    std::thread::spawn(move || ws_thread(context, id, url, protocols, rx));
    rv.set(v8::Integer::new_from_unsigned(scope, id).into());
}

fn ws_command(st: &State, id: u32, command: WsCommand) -> bool {
    st.websockets
        .borrow()
        .get(&id)
        .map(|tx: &Sender<WsCommand>| tx.send(command).is_ok())
        .unwrap_or(false)
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

/// Blocking GET used for URL module imports.
pub fn fetch_text(url: &str) -> Result<String, String> {
    let r = http("GET", url, &[], None, true)?;
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
