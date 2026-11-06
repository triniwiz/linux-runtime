use nativescript_runtime::{MetaFile, Runtime};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;

fn runtime() -> Option<Runtime> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../metadata-generator/tests/TestOutput/metadata-{}.bin", std::env::consts::ARCH));
    path.exists().then(|| Runtime::new(MetaFile::open(path).unwrap()).unwrap())
}

const PRELUDE: &str = r#"
  function eq(actual, expected, what) {
    const a = JSON.stringify(actual), e = JSON.stringify(expected);
    if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
  }
"#;

/// Minimal HTTP/1.1 server, returns its base URL.
fn http_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let mut parts = line.split_whitespace();
                let (method, path) = (parts.next().unwrap_or("").to_owned(), parts.next().unwrap_or("").to_owned());
                let mut length = 0;
                let mut headers = Vec::new();
                loop {
                    let mut h = String::new();
                    reader.read_line(&mut h).unwrap();
                    if h.trim().is_empty() {
                        break;
                    }
                    if let Some((k, v)) = h.trim().split_once(": ") {
                        if k.eq_ignore_ascii_case("content-length") {
                            length = v.parse().unwrap();
                        }
                        headers.push((k.to_lowercase(), v.to_owned()));
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let (status, content_type, extra, payload) = match path.as_str() {
                    "/json" => ("200 OK", "application/json", "x-test: yes\r\n", br#"{"hello":"world"}"#.to_vec()),
                    "/echo" => {
                        let header = headers.iter().find(|(k, _)| k == "x-echo").map(|(_, v)| v.clone()).unwrap_or_default();
                        let out = format!("{method} {header} {}", String::from_utf8_lossy(&body));
                        ("200 OK", "text/plain", "", out.into_bytes())
                    }
                    "/redirect" => ("302 Found", "text/plain", "location: /json\r\n", vec![]),
                    "/mod/main.js" => ("200 OK", "text/javascript", "", b"import { dep } from './dep.js'; export const value = dep * 2;".to_vec()),
                    "/mod/dep.js" => ("200 OK", "text/javascript", "", b"export const dep = 21;".to_vec()),
                    _ => ("404 Not Found", "text/plain", "", b"missing".to_vec()),
                };
                let mut stream = stream;
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\n{extra}content-length: {}\r\nconnection: close\r\n\r\n",
                    payload.len()
                );
                let _ = stream.write_all(&payload);
            });
        }
    });
    format!("http://{addr}")
}

fn ws_echo_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let mut ws = tungstenite::accept(stream).unwrap();
                while let Ok(msg) = ws.read() {
                    if msg.is_close() {
                        break;
                    }
                    if msg.is_text() || msg.is_binary() {
                        ws.send(msg).unwrap();
                    }
                }
            });
        }
    });
    format!("ws://{addr}")
}

fn run(source: &str) {
    let Some(mut rt) = runtime() else { return };
    rt.run_script(&format!("{PRELUDE}\n{source}"), "net.js").unwrap();
    rt.run_event_loop().unwrap();
}

#[test]
fn fetch() {
    let base = http_server();
    run(&format!(
        r#"
      const base = '{base}';
      (async () => {{
        const r = await fetch(base + '/json');
        eq([r.ok, r.status, r.headers.get('X-Test'), await r.json()], [true, 200, 'yes', {{ hello: 'world' }}], 'GET json');

        const post = await fetch(base + '/echo', {{ method: 'POST', headers: {{ 'x-echo': 'hi' }}, body: 'payload' }});
        eq(await post.text(), 'POST hi payload', 'POST');

        const bytes = await fetch(base + '/echo', {{ method: 'PUT', body: new Uint8Array([97, 98]) }});
        eq(await bytes.text(), 'PUT  ab', 'binary body');

        const missing = await fetch(base + '/nope');
        eq([missing.ok, missing.status, await missing.text()], [false, 404, 'missing'], '404');

        const redirected = await fetch(base + '/redirect');
        eq([redirected.redirected, redirected.url.endsWith('/json')], [true, true], 'redirect');

        let aborted = false;
        const c = new AbortController();
        const p = fetch(base + '/json', {{ signal: c.signal }}).catch((e) => (aborted = e.name));
        c.abort();
        await p;
        eq(aborted, 'AbortError', 'abort');

        let failed = false;
        try {{ await fetch('http://127.0.0.1:1/'); }} catch (e) {{ failed = e instanceof TypeError; }}
        eq(failed, true, 'network error');
      }})();
    "#
    ));
}

#[test]
fn websocket() {
    let url = ws_echo_server();
    run(&format!(
        r#"
      const ws = new WebSocket('{url}');
      ws.binaryType = 'arraybuffer';
      const got = [];
      ws.onopen = () => {{
        eq(ws.readyState, WebSocket.OPEN, 'open');
        ws.send('hello');
        ws.send(new Uint8Array([1, 2, 3]));
      }};
      ws.addEventListener('message', (e) => {{
        got.push(typeof e.data === 'string' ? e.data : Array.from(new Uint8Array(e.data)));
        if (got.length === 2) ws.close(1000, 'done');
      }});
      ws.onclose = (e) => {{
        eq(got, ['hello', [1, 2, 3]], 'echo');
        eq(ws.readyState, WebSocket.CLOSED, 'closed');
      }};
    "#
    ));
}

#[test]
fn url_imports() {
    let base = http_server();
    run(&format!(
        r#"
      import('{base}/mod/main.js').then((m) => eq(m.value, 42, 'remote module with relative import'));
    "#
    ));
}
