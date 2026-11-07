use nativescript_runtime::{MetaFile, Runtime};
use std::path::PathBuf;

fn run(source: &str) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../metadata-generator/tests/TestOutput/metadata-{}.bin",
        std::env::consts::ARCH
    ));
    if !path.exists() {
        return;
    }
    let mut rt = Runtime::new(MetaFile::open(path).unwrap()).unwrap();
    let prelude = r#"
      function eq(actual, expected, what) {
        const a = JSON.stringify(actual), e = JSON.stringify(expected);
        if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
      }
    "#;
    rt.run_script(&format!("{prelude}\n{source}"), "web.js")
        .unwrap();
    rt.run_event_loop().unwrap();
}

#[test]
fn url() {
    run(r#"
      const u = new URL('../b/c?x=1&y=two words#frag', 'https://user:pw@example.com:8080/a/index.html');
      eq([u.href, u.origin, u.host, u.pathname, u.search, u.hash, u.username],
         ['https://user:pw@example.com:8080/b/c?x=1&y=two%20words#frag', 'https://example.com:8080', 'example.com:8080', '/b/c', '?x=1&y=two%20words', '#frag', 'user'], 'parse');
      eq(u.searchParams.get('y'), 'two words', 'searchParams');
      u.searchParams.append('z', 'a&b');
      eq(u.search, '?x=1&y=two+words&z=a%26b', 'params update url');
      u.pathname = '/new path';
      u.port = '';
      eq(u.href, 'https://user:pw@example.com/new%20path?x=1&y=two+words&z=a%26b#frag', 'setters');
      eq(URL.canParse('nope'), false, 'canParse');
      let threw = false; try { new URL('not a url'); } catch (e) { threw = e instanceof TypeError; }
      eq(threw, true, 'invalid');
      eq(new URLSearchParams({ a: 1, b: 'x y' }).toString(), 'a=1&b=x+y', 'URLSearchParams');
      eq(new URL('file:///tmp/a b.txt').pathname, '/tmp/a%20b.txt', 'file url');
    "#);
}

#[test]
fn encoding() {
    run(r#"
      const bytes = new TextEncoder().encode('héllo €');
      eq(Array.from(bytes), [104, 195, 169, 108, 108, 111, 32, 226, 130, 172], 'encode');
      eq(new TextDecoder().decode(bytes), 'héllo €', 'decode');
      eq(new TextDecoder().decode(bytes.buffer), 'héllo €', 'decode buffer');
      let threw = false; try { new TextDecoder('utf-8', { fatal: true }).decode(new Uint8Array([0xff])); } catch { threw = true; }
      eq(threw, true, 'fatal');
      const dest = new Uint8Array(4);
      eq(new TextEncoder().encodeInto('a€', dest), { read: 2, written: 4 }, 'encodeInto');
      eq([btoa('hello'), atob('aGVsbG8=')], ['aGVsbG8=', 'hello'], 'base64');
    "#);
}

#[test]
fn events_and_abort() {
    run(r#"
      const target = new EventTarget();
      const seen = [];
      target.addEventListener('ping', (e) => seen.push(e.detail), { once: true });
      target.dispatchEvent(new CustomEvent('ping', { detail: 1 }));
      target.dispatchEvent(new CustomEvent('ping', { detail: 2 }));
      eq(seen, [1], 'once');

      const controller = new AbortController();
      let aborted = null;
      controller.signal.addEventListener('abort', () => (aborted = controller.signal.reason.name));
      controller.abort();
      eq([controller.signal.aborted, aborted], [true, 'AbortError'], 'abort');

      const timeout = AbortSignal.timeout(5);
      setTimeout(() => eq(timeout.reason?.name, 'TimeoutError', 'timeout signal'), 30);
    "#);
}

#[test]
fn misc_globals() {
    run(r#"
      const a = { d: new Date(0), m: new Map([[1, { x: 1 }]]), t: new Uint8Array([1, 2]) };
      a.self = a;
      const c = structuredClone(a);
      eq([c !== a, c.self === c, c.d.getTime(), c.m.get(1).x, Array.from(c.t)], [true, true, 0, 1, [1, 2]], 'structuredClone');
      eq(/^[0-9a-f-]{36}$/.test(crypto.randomUUID()), true, 'randomUUID');
      eq(crypto.getRandomValues(new Uint32Array(4)).length, 4, 'getRandomValues');
      const t0 = performance.now();
      eq(typeof t0 === 'number' && t0 >= 0 && performance.timeOrigin > 1e12, true, 'performance');
      let immediate = false;
      setImmediate(() => (immediate = true));
      setTimeout(() => eq(immediate, true, 'setImmediate'), 5);
    "#);
}

#[test]
fn process() {
    run(r#"
      eq([process.platform, typeof process.pid, Array.isArray(process.argv)], ['linux', 'number', true], 'process');
      process.env.NS_TEST_VAR = 'value';
      eq([process.env.NS_TEST_VAR, 'NS_TEST_VAR' in process.env, typeof getenv('NS_TEST_VAR')], ['value', true, 'object'], 'env visible to C');
      delete process.env.NS_TEST_VAR;
      eq(process.env.NS_TEST_VAR, undefined, 'env delete');
      eq(process.cwd().startsWith('/'), true, 'cwd');
      eq(typeof process.hrtime.bigint(), 'bigint', 'hrtime');
      let ticked = false;
      process.nextTick(() => (ticked = true));
      Promise.resolve().then(() => eq(ticked, true, 'nextTick'));
    "#);
}
