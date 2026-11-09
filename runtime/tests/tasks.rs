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
    rt.run_script(source, "tasks.js").unwrap();
    rt.run_event_loop().unwrap();
}

/// V8 resolves async compiles from its worker threads; the idle loop must wake for it.
#[test]
fn background_results_wake_the_loop() {
    run(r#"
      // (func (export "ans") (result i32) i32.const 42)
      const bytes = new Uint8Array([
        0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00,
        0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7f,
        0x03, 0x02, 0x01, 0x00,
        0x07, 0x07, 0x01, 0x03, 0x61, 0x6e, 0x73, 0x00, 0x00,
        0x0a, 0x06, 0x01, 0x04, 0x00, 0x41, 0x2a, 0x0b,
      ]);
      const timeout = setTimeout(() => { throw new Error('compile result not delivered'); }, 5000);
      WebAssembly.instantiate(bytes).then(({ instance }) => {
        clearTimeout(timeout);
        if (instance.exports.ans() !== 42) throw new Error('wrong result');
      });
    "#);
}
