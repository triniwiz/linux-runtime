//! Chrome DevTools Protocol over V8's inspector (`--inspect`).

use crate::event_loop::{post, post_state, with_js};
use crate::state::State;
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Sender, TryRecvError};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use v8::inspector::{
    Channel, ChannelImpl, StringBuffer, StringView, V8Inspector, V8InspectorClient, V8InspectorClientImpl,
    V8InspectorClientTrustLevel, V8InspectorSession,
};

const GROUP: i32 = 1;

/// Messages from the DevTools frontend, read by the JS thread.
#[derive(Default)]
struct Inbox {
    queue: Mutex<VecDeque<String>>,
    ready: Condvar,
    connected: AtomicBool,
}

pub struct Inspector {
    // the session must drop before the inspector
    session: Option<Rc<V8InspectorSession>>,
    inspector: V8Inspector,
    inbox: Arc<Inbox>,
    outbox: Arc<Mutex<Option<Sender<String>>>>,
    waiting: Rc<std::cell::Cell<bool>>,
}

struct Client {
    state: *const State,
    /// leaked Global slot of the main context, usable as a Local
    context: std::ptr::NonNull<v8::Context>,
    paused: Rc<std::cell::Cell<bool>>,
    waiting: Rc<std::cell::Cell<bool>>,
}

impl V8InspectorClientImpl for Client {
    /// Blocks the JS thread at a breakpoint, serving DevTools messages only.
    fn run_message_loop_on_pause(&self, _group: i32) {
        let st = unsafe { &*self.state };
        self.paused.set(true);
        while self.paused.get() {
            let Some((session, inbox)) = session_of(st) else { break };
            let message = {
                let mut queue = inbox.queue.lock().unwrap();
                loop {
                    if let Some(m) = queue.pop_front() {
                        break Some(m);
                    }
                    if !inbox.connected.load(Ordering::SeqCst) {
                        break None;
                    }
                    queue = inbox.ready.wait_timeout(queue, Duration::from_millis(100)).unwrap().0;
                }
            };
            match message {
                Some(m) => session.dispatch_protocol_message(StringView::from(m.as_bytes())),
                None => break,
            }
        }
        self.paused.set(false);
    }

    fn quit_message_loop_on_pause(&self) {
        self.paused.set(false);
    }

    fn run_if_waiting_for_debugger(&self, _group: i32) {
        self.waiting.set(false);
    }

    fn ensure_default_context_in_group(&self, _group: i32) -> Option<v8::Local<'_, v8::Context>> {
        Some(unsafe { std::mem::transmute::<std::ptr::NonNull<v8::Context>, v8::Local<'_, v8::Context>>(self.context) })
    }

    fn resource_name_to_url(&self, name: &StringView) -> Option<v8::UniquePtr<StringBuffer>> {
        let name = name.to_string();
        if !name.starts_with('/') {
            return None;
        }
        let url = crate::modules::path_to_file_url(std::path::Path::new(&name));
        Some(StringBuffer::create(StringView::from(url.as_bytes())))
    }
}

struct Out(Arc<Mutex<Option<Sender<String>>>>);

impl Out {
    fn send(&self, message: v8::UniquePtr<StringBuffer>) {
        let Some(message) = message.as_ref() else { return };
        if let Some(tx) = self.0.lock().unwrap().as_ref() {
            let _ = tx.send(message.string().to_string());
        }
    }
}

impl ChannelImpl for Out {
    fn send_response(&self, _call_id: i32, message: v8::UniquePtr<StringBuffer>) {
        self.send(message);
    }
    fn send_notification(&self, message: v8::UniquePtr<StringBuffer>) {
        self.send(message);
    }
    fn flush_protocol_notifications(&self) {}
}

fn session_of(st: &State) -> Option<(Rc<V8InspectorSession>, Arc<Inbox>)> {
    let inspector = st.inspector.borrow();
    let i = inspector.as_ref()?;
    Some((i.session.clone()?, i.inbox.clone()))
}

/// Dispatches queued frontend messages (outside of pauses).
fn pump(st: &State) {
    let Some((session, inbox)) = session_of(st) else { return };
    loop {
        let Some(m) = inbox.queue.lock().unwrap().pop_front() else { break };
        session.dispatch_protocol_message(StringView::from(m.as_bytes()));
    }
}

fn connect(st: &State) {
    let mut inspector = st.inspector.borrow_mut();
    let Some(i) = inspector.as_mut().filter(|i| i.session.is_none()) else { return };
    let channel = Channel::new(Box::new(Out(i.outbox.clone())));
    let session = i.inspector.connect(GROUP, channel, StringView::empty(), V8InspectorClientTrustLevel::FullyTrusted);
    i.session = Some(Rc::new(session));
}

/// Starts the inspector server on `port`; with `wait` the caller should call
/// [`wait_for_debugger`] before running user code.
pub fn start(st: &Rc<State>, port: u16, title: &str) -> Result<(), String> {
    let isolate = unsafe { &mut *st.isolate.get() };
    let paused = Rc::new(std::cell::Cell::new(false));
    let waiting = Rc::new(std::cell::Cell::new(false));
    let context = st.context.borrow().clone().ok_or("no context")?.into_raw();
    let client = V8InspectorClient::new(Box::new(Client { state: Rc::as_ptr(st), context, paused, waiting: waiting.clone() }));
    let inspector = V8Inspector::create(isolate, client);
    with_js(st, |scope| {
        let context = scope.get_current_context();
        let name = format!("NativeScript {title}");
        inspector.context_created(context, GROUP, StringView::from(name.as_bytes()), StringView::empty());
    });
    let inbox = Arc::new(Inbox::default());
    let outbox = Arc::new(Mutex::new(None));
    *st.inspector.borrow_mut() = Some(Inspector { inspector, session: None, inbox: inbox.clone(), outbox: outbox.clone(), waiting });

    let listener = TcpListener::bind(("127.0.0.1", port)).map_err(|e| format!("inspector port {port}: {e}"))?;
    let id = format!("{:016x}", (std::process::id() as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    eprintln!("Debugger listening on ws://127.0.0.1:{port}/{id}");
    eprintln!("Open chrome://inspect or devtools://devtools/bundled/js_app.html?ws=127.0.0.1:{port}/{id}");
    let context = st.main_context.context;
    let title = title.to_owned();
    std::thread::Builder::new()
        .name("inspector".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                serve(stream, port, &id, &title, context, &inbox, &outbox);
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// `--inspect-brk`: waits for a frontend and pauses on the first statement.
pub fn wait_for_debugger(st: &State) {
    let Some(inbox) = st.inspector.borrow().as_ref().map(|i| i.inbox.clone()) else { return };
    eprintln!("Waiting for the debugger to attach...");
    if let Some(i) = st.inspector.borrow().as_ref() {
        i.waiting.set(true);
    }
    while st.inspector.borrow().as_ref().is_some_and(|i| i.waiting.get()) {
        let message = {
            let mut queue = inbox.queue.lock().unwrap();
            while queue.is_empty() {
                queue = inbox.ready.wait(queue).unwrap();
            }
            queue.pop_front()
        };
        if st.inspector.borrow().as_ref().is_some_and(|i| i.session.is_none()) {
            connect(st);
        }
        if let (Some(m), Some((session, _))) = (message, session_of(st)) {
            with_js(st, |_| session.dispatch_protocol_message(StringView::from(m.as_bytes())));
        }
    }
    if let Some((session, _)) = session_of(st) {
        let reason = b"Break on start";
        session.schedule_pause_on_next_statement(StringView::from(&reason[..]), StringView::empty());
    }
}

fn http(stream: &mut TcpStream, status: &str, body: &str) {
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: application/json; charset=UTF-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

fn serve(mut stream: TcpStream, port: u16, id: &str, title: &str, context: usize, inbox: &Arc<Inbox>, outbox: &Arc<Mutex<Option<Sender<String>>>>) {
    let mut head = [0u8; 2048];
    let n = stream.peek(&mut head).unwrap_or(0);
    let request = String::from_utf8_lossy(&head[..n]).to_ascii_lowercase();
    let path = request.split_whitespace().nth(1).unwrap_or("/").to_owned();
    if !request.contains("upgrade: websocket") {
        let mut buf = vec![0; n];
        let _ = stream.read(&mut buf);
        let target = format!("127.0.0.1:{port}/{id}");
        match path.trim_end_matches('/') {
            "/json" | "/json/list" => {
                let body = serde_json::json!([{
                    "description": "NativeScript Linux runtime",
                    "devtoolsFrontendUrl": format!("devtools://devtools/bundled/js_app.html?experiments=true&v8only=true&ws={target}"),
                    "id": id,
                    "title": title,
                    "type": "node",
                    "url": "file://",
                    "webSocketDebuggerUrl": format!("ws://{target}"),
                }]);
                http(&mut stream, "200 OK", &body.to_string());
            }
            "/json/version" => http(&mut stream, "200 OK", r#"{"Browser":"NativeScript-Linux/0.1","Protocol-Version":"1.3"}"#),
            _ => http(&mut stream, "404 Not Found", "{}"),
        }
        return;
    }
    if inbox.connected.swap(true, Ordering::SeqCst) {
        return; // one frontend at a time
    }
    let Ok(mut ws) = tungstenite::accept(stream.try_clone().unwrap()) else {
        inbox.connected.store(false, Ordering::SeqCst);
        return;
    };
    let (tx, rx) = channel::<String>();
    *outbox.lock().unwrap() = Some(tx);
    // create the session on the JS thread
    post_state(context, Box::new(|st| connect(st)));
    let _ = stream.set_read_timeout(Some(Duration::from_millis(10)));
    loop {
        loop {
            match rx.try_recv() {
                Ok(m) => {
                    if ws.send(tungstenite::Message::text(m)).is_err() {
                        break;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }
        match ws.read() {
            Ok(tungstenite::Message::Text(t)) => {
                inbox.queue.lock().unwrap().push_back(t.to_string());
                inbox.ready.notify_all();
                post(context, false, Box::new(|_, st| pump(st)));
            }
            Ok(tungstenite::Message::Close(_)) => break,
            Ok(_) => {}
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(_) => break,
        }
    }
    *outbox.lock().unwrap() = None;
    inbox.connected.store(false, Ordering::SeqCst);
    inbox.ready.notify_all();
    post_state(
        context,
        Box::new(|st| {
            if let Some(i) = st.inspector.borrow_mut().as_mut() {
                i.session = None;
            }
        }),
    );
}
