use nativescript_runtime::{MetaFile, Runtime};
use std::path::PathBuf;
use std::process::Command;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/napi")
}

fn build_addon() -> Option<PathBuf> {
    let out = fixtures().join("build");
    std::fs::create_dir_all(&out).ok()?;
    let addon = out.join("addon.node");
    let status = Command::new("cc")
        .args(["-shared", "-fPIC", "-O2", "-DNODE_GYP_MODULE_NAME=addon", "-I"])
        .arg(fixtures().join("include"))
        .arg(fixtures().join("addon.c"))
        .arg("-o")
        .arg(&addon)
        .arg("-lpthread")
        .status()
        .ok()?;
    status.success().then_some(addon)
}

#[test]
fn addon() {
    let meta = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../metadata-generator/tests/TestOutput/metadata-{}.bin", std::env::consts::ARCH));
    if !meta.exists() || build_addon().is_none() {
        eprintln!("skipping: metadata or C compiler missing");
        return;
    }
    let mut rt = Runtime::new(MetaFile::open(meta).unwrap()).unwrap();
    rt.run_file(fixtures().join("test.js")).unwrap();
    rt.run_event_loop().unwrap();
    let json: serde_json::Value = serde_json::from_str(&rt.eval_to_string("JSON.stringify(globalThis.napiResults)").unwrap()).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "add": 5.5,
            "greet": "hello napi",
            "callback": 42,
            "error": ["boom from C", "E_BOOM"],
            "jsError": "from js",
            "counter": [3, 3, true],
            "zeroCopy": ["zero-copy", "Zero-copy"],
            "sum": [6, 99],
            "reference": { "kept": true },
            "describe": [2, -4],
            "async": 42,
            "thread": [1, 2, 3]
        })
    );
}
