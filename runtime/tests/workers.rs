use nativescript_runtime::{MetaFile, Runtime};
use std::path::PathBuf;

fn run(source: &str) {
    let meta = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../metadata-generator/tests/TestOutput/metadata-{}.bin",
        std::env::consts::ARCH
    ));
    if !meta.exists() {
        return;
    }
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/workers");
    let mut rt = Runtime::new(MetaFile::open(meta).unwrap()).unwrap();
    let prelude = format!(
        r#"
      const fixtures = '{}';
      function eq(actual, expected, what) {{
        const a = JSON.stringify(actual), e = JSON.stringify(expected);
        if (a !== e) throw new Error(`${{what}}: expected ${{e}}, got ${{a}}`);
      }}
    "#,
        fixtures.display()
    );
    rt.run_script(&format!("{prelude}\n{source}"), "workers.js")
        .unwrap();
    rt.run_event_loop().unwrap();
}

#[test]
fn messages_round_trip() {
    run(r#"
      const w = new Worker(`${fixtures}/echo.js`);
      const replies = [];
      // posted before the worker started: queued
      w.postMessage({ n: 1, map: new Map([[1, 2]]), date: new Date(Date.UTC(2020, 0, 1)), bytes: new Uint8Array([7]) });
      w.onmessage = (e) => {
        replies.push(e.data);
        if (replies.length === 1) w.postMessage({ n: 2, map: new Map(), date: new Date(0), last: true });
      };
      w.addEventListener('exit', () => {});
      setTimeout(function check() {
        if (replies.length < 2) return setTimeout(check, 10);
        eq(replies.map((r) => [r.echo.n, r.isMap, r.year, r.len]), [[1, true, 2020, 13], [2, true, 1970, 13]], 'replies');
        eq(Array.from(replies[0].echo.bytes), [7], 'typed array');
      }, 10);
    "#);
}

#[test]
fn terminate_and_errors() {
    run(r#"
      const spin = new Worker(`${fixtures}/spin.js`);
      spin.onmessage = () => spin.terminate();
      const bad = new Worker(`${fixtures}/throws.mjs`);
      let error = null;
      bad.onerror = (e) => (error = e.message);
      setTimeout(function check() {
        if (error === null) return setTimeout(check, 10);
        eq(error.includes('worker failed'), true, 'error event');
      }, 10);
    "#);
}

#[test]
fn message_channel() {
    run(r#"
      const { port1, port2 } = new MessageChannel();
      let got = null;
      port2.onmessage = (e) => (got = e.data);
      port1.postMessage({ a: [1, 2] });
      setTimeout(() => eq(got, { a: [1, 2] }, 'channel'), 20);
    "#);
}
