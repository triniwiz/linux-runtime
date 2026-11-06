use nativescript_runtime::{MetaFile, Runtime};
use std::path::PathBuf;

fn runtime() -> Option<Runtime> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../metadata-generator/tests/TestOutput/metadata-{}.bin", std::env::consts::ARCH));
    path.exists().then(|| Runtime::new(MetaFile::open(path).unwrap()).unwrap())
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/modules").join(name)
}

fn results(rt: &mut Runtime, global: &str) -> String {
    rt.eval_to_string(&format!("JSON.stringify(globalThis.{global})")).unwrap()
}

#[test]
fn es_modules() {
    let Some(mut rt) = runtime() else { return };
    rt.run_file(fixture("main.mjs")).unwrap();
    rt.run_event_loop().unwrap();
    let json: serde_json::Value = serde_json::from_str(&results(&mut rt, "results")).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "add": 5, "value": 42, "json": "fixture", "cjsDefault": "commonjs",
            "cjsNamed": "hello esm from legacy.cjs", "pkg": "pkg:esm", "scoped": "scoped",
            "dirIndex": "index", "metaUrl": true, "metaResolve": true, "dynamic": "dynamic",
            "tla": "awaited", "missing": true
        })
    );
}

#[test]
fn commonjs() {
    let Some(mut rt) = runtime() else { return };
    rt.run_file(fixture("entry.cjs")).unwrap();
    let json: serde_json::Value = serde_json::from_str(&results(&mut rt, "cjsResults")).unwrap();
    assert_eq!(json, serde_json::json!({ "pkg": "pkg:cjs", "util": "scoped", "json": "fixture", "esm": 2, "resolved": true }));
}

#[test]
fn errors_are_reported() {
    let Some(mut rt) = runtime() else { return };
    let dir = std::env::temp_dir().join(format!("ns-mod-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("bad.mjs"), "import './nope.mjs';").unwrap();
    std::fs::write(dir.join("throws.mjs"), "throw new Error('top level');").unwrap();
    assert!(rt.run_file(dir.join("bad.mjs")).unwrap_err().contains("Cannot find module"));
    assert!(rt.run_file(dir.join("throws.mjs")).unwrap_err().contains("top level"));
}
