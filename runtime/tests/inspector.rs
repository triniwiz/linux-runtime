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
        let mut http = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(http, "GET /json/list HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
        let mut body = String::new();
        http.read_to_string(&mut body).unwrap();
        let json: serde_json::Value =
            serde_json::from_str(body.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        let url = json[0]["webSocketDebuggerUrl"].as_str().unwrap().to_owned();

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
        let url = format!("ws://127.0.0.1:{port}/session");
        let (mut ws, _) = tungstenite::connect(url).unwrap();
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
