use nativescript_runtime::{MetaFile, Runtime};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn get(port: u16, path: &str, host: &str) -> String {
    let mut http = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(http, "GET {path} HTTP/1.1\r\nHost: {host}\r\n\r\n").unwrap();
    let mut response = String::new();
    http.read_to_string(&mut response).unwrap();
    response
}

fn ws_url(port: u16) -> String {
    let body = get(port, "/json/list", "localhost");
    let json: serde_json::Value =
        serde_json::from_str(body.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    json[0]["webSocketDebuggerUrl"].as_str().unwrap().to_owned()
}

#[test]
fn devtools_protocol() {
    let meta = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../metadata-generator/tests/TestOutput/metadata-{}.bin",
        std::env::consts::ARCH
    ));
    if !meta.exists() {
        return;
    }
    let mut rt = Runtime::new(MetaFile::open(meta).unwrap()).unwrap();
    let port = free_port();
    rt.start_inspector(port, false, "test").unwrap();
    rt.run_script(
        "globalThis.answer = 41 + 1; setTimeout(() => {}, 1500);",
        "inspected.js",
    )
    .unwrap();

    let client = std::thread::spawn(move || {
        // DNS rebinding: a page's own host name
        assert!(get(port, "/json/list", "evil.example:80").starts_with("HTTP/1.1 403"));
        // only the advertised (random) target id upgrades
        assert!(tungstenite::connect(format!("ws://127.0.0.1:{port}/session")).is_err());
        let url = ws_url(port);
        let (mut ws, _) = tungstenite::connect(url).unwrap();
        let mut call = |id: u64, method: &str, params: serde_json::Value| -> serde_json::Value {
            let msg = serde_json::json!({ "id": id, "method": method, "params": params });
            ws.send(tungstenite::Message::text(msg.to_string()))
                .unwrap();
            loop {
                let reply: serde_json::Value =
                    serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
                if reply["id"] == id {
                    return reply;
                }
            }
        };
        call(1, "Runtime.enable", serde_json::json!({}));
        call(3, "Debugger.enable", serde_json::json!({}));
        let r = call(
            2,
            "Runtime.evaluate",
            serde_json::json!({ "expression": "answer + strlen('abc')" }),
        );
        r["result"]["result"]["value"].clone()
    });

    rt.run_event_loop().unwrap();
    assert_eq!(client.join().unwrap(), serde_json::json!(45));
}

#[test]
fn breakpoints_pause_and_resume() {
    let meta = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../metadata-generator/tests/TestOutput/metadata-{}.bin",
        std::env::consts::ARCH
    ));
    if !meta.exists() {
        return;
    }
    let mut rt = Runtime::new(MetaFile::open(meta).unwrap()).unwrap();
    let port = free_port();
    rt.start_inspector(port, false, "pause").unwrap();
    rt.run_script(
        "globalThis.after = 0; setTimeout(() => { const local = 7; debugger; globalThis.after = local * 6; }, 300);",
        "paused.js",
    )
    .unwrap();

    let client = std::thread::spawn(move || {
        let (mut ws, _) = tungstenite::connect(ws_url(port)).unwrap();
        let send = |ws: &mut tungstenite::WebSocket<_>,
                    id: u64,
                    method: &str,
                    params: serde_json::Value| {
            let msg = serde_json::json!({ "id": id, "method": method, "params": params });
            ws.send(tungstenite::Message::text(msg.to_string()))
                .unwrap();
        };
        send(&mut ws, 1, "Runtime.enable", serde_json::json!({}));
        send(&mut ws, 2, "Debugger.enable", serde_json::json!({}));
        let mut frame = None;
        let mut local = None;
        loop {
            let msg: serde_json::Value =
                serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
            if msg["method"] == "Debugger.paused" {
                frame = Some(
                    msg["params"]["callFrames"][0]["callFrameId"]
                        .as_str()
                        .unwrap()
                        .to_owned(),
                );
                send(
                    &mut ws,
                    3,
                    "Debugger.evaluateOnCallFrame",
                    serde_json::json!({ "callFrameId": frame, "expression": "local" }),
                );
            }
            if msg["id"] == 3 {
                local = Some(msg["result"]["result"]["value"].clone());
                send(&mut ws, 4, "Debugger.resume", serde_json::json!({}));
            }
            if msg["id"] == 4 {
                break;
            }
        }
        (frame.is_some(), local)
    });

    rt.run_event_loop().unwrap();
    assert_eq!(client.join().unwrap(), (true, Some(serde_json::json!(7))));
    assert_eq!(rt.eval_to_string("after").unwrap(), "42");
}

fn runtime() -> Option<Runtime> {
    let meta = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../metadata-generator/tests/TestOutput/metadata-{}.bin",
        std::env::consts::ARCH
    ));
    meta.exists()
        .then(|| Runtime::new(MetaFile::open(meta).unwrap()).unwrap())
}

type Ws = tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<TcpStream>>;

fn send(ws: &mut Ws, id: u64, method: &str) {
    let msg = serde_json::json!({ "id": id, "method": method, "params": {} });
    ws.send(tungstenite::Message::text(msg.to_string()))
        .unwrap();
}

/// Reads until `Debugger.paused`, then evaluates `expression` there and resumes.
fn on_pause(ws: &mut Ws, expression: &str) -> serde_json::Value {
    let mut value = None;
    loop {
        let msg: serde_json::Value =
            serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
        if msg["method"] == "Debugger.paused" {
            let frame = msg["params"]["callFrames"][0]["callFrameId"].clone();
            let eval = serde_json::json!({ "id": 90, "method": "Debugger.evaluateOnCallFrame",
                "params": { "callFrameId": frame, "expression": expression } });
            ws.send(tungstenite::Message::text(eval.to_string()))
                .unwrap();
        }
        if msg["id"] == 90 {
            value = Some(msg["result"]["result"]["value"].clone());
            send(ws, 91, "Debugger.resume");
        }
        if msg["id"] == 91 {
            return value.unwrap();
        }
    }
}

fn connect_when_up(port: u16) -> Ws {
    for _ in 0..100 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    tungstenite::connect(ws_url(port)).unwrap().0
}

#[test]
fn inspect_brk_stops_before_app_code() {
    let Some(mut rt) = runtime() else { return };
    let port = free_port();
    let client = std::thread::spawn(move || {
        let mut ws = connect_when_up(port);
        send(&mut ws, 1, "Runtime.enable");
        send(&mut ws, 2, "Debugger.enable");
        send(&mut ws, 3, "Runtime.runIfWaitingForDebugger");
        on_pause(&mut ws, "typeof globalThis.ran")
    });
    let started = std::time::Instant::now();
    rt.start_inspector(port, true, "brk").unwrap();
    rt.run_script("globalThis.ran = true;", "app.js").unwrap();
    assert_eq!(client.join().unwrap(), serde_json::json!("undefined"));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "didn't wait for the fallback"
    );
    assert_eq!(rt.eval_to_string("ran").unwrap(), "true");
}

#[test]
fn inspect_brk_releases_frontends_without_run_if_waiting() {
    let Some(mut rt) = runtime() else { return };
    let port = free_port();
    let client = std::thread::spawn(move || {
        let mut ws = connect_when_up(port);
        send(&mut ws, 1, "Debugger.enable");
        on_pause(&mut ws, "typeof globalThis.ran")
    });
    rt.start_inspector(port, true, "brk").unwrap();
    rt.run_script("globalThis.ran = true;", "app.js").unwrap();
    assert_eq!(client.join().unwrap(), serde_json::json!("undefined"));
}

#[test]
fn debugger_pause_interrupts_busy_js() {
    let Some(mut rt) = runtime() else { return };
    let port = free_port();
    rt.start_inspector(port, false, "busy").unwrap();
    rt.run_script(
        "setTimeout(() => { globalThis.spins = 0; const end = Date.now() + 4000; \
         while (Date.now() < end) globalThis.spins++; }, 300);",
        "busy.js",
    )
    .unwrap();
    let client = std::thread::spawn(move || {
        let mut ws = tungstenite::connect(ws_url(port)).unwrap().0;
        send(&mut ws, 1, "Debugger.enable");
        std::thread::sleep(std::time::Duration::from_millis(1200));
        send(&mut ws, 2, "Debugger.pause");
        on_pause(&mut ws, "spins > 0")
    });
    let started = std::time::Instant::now();
    rt.run_event_loop().unwrap();
    assert_eq!(client.join().unwrap(), serde_json::json!(true));
    assert!(started.elapsed() >= std::time::Duration::from_secs(4));
}
