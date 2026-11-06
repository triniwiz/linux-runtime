//! Runs against metadata produced by `metadata-generator/tests/test-mdg-executable.sh`.
//! Set `NS_TEST_METADATA` to point at a different file. Skipped when missing.

use nativescript_metadata::{MetaFile, MetaKind, Type};
use std::path::PathBuf;

fn metadata() -> Option<MetaFile> {
    let path = std::env::var_os("NS_TEST_METADATA").map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../metadata-generator/tests/TestOutput/metadata-{}.bin",
            std::env::consts::ARCH
        ))
    });
    if !path.exists() {
        eprintln!("skipping: {} not found, run metadata-generator/tests/test-mdg-executable.sh", path.display());
        return None;
    }
    Some(MetaFile::open(&path).expect("open metadata"))
}

#[test]
fn every_declaration_is_reachable_by_name() {
    let Some(file) = metadata() else { return };
    let mut count = 0;
    for meta in file.iter() {
        count += 1;
        let found = file
            .find(meta.js_name())
            .unwrap_or_else(|| panic!("{} not found by js name, hash mismatch?", meta.js_name()));
        assert_eq!(found.js_name(), meta.js_name());
    }
    assert!(count > 5000, "only {count} declarations");
}

#[test]
fn modules_and_libraries() {
    let Some(file) = metadata() else { return };
    let libc = file.modules().find(|m| m.name() == "libc").expect("libc module");
    assert!(libc.is_system());
    let libs: Vec<_> = libc.libraries().map(|l| l.name()).collect();
    assert_eq!(libs, ["libc.so.6", "libm.so.6"]);

    let puts = file.find("puts").unwrap();
    assert_eq!(puts.module().name(), "libc");
}

#[test]
fn functions() {
    let Some(file) = metadata() else { return };
    let strlen = file.find("strlen").unwrap();
    assert_eq!(strlen.kind(), MetaKind::Function);
    assert_eq!(strlen.signature().unwrap(), vec![Type::ULong, Type::CString]);
    assert!(!strlen.is_variadic());

    let printf = file.find("printf").unwrap();
    assert!(printf.is_variadic());
    assert_eq!(printf.signature().unwrap(), vec![Type::Int, Type::CString]);

    let qsort = file.find("qsort").unwrap();
    let sig = qsort.signature().unwrap();
    assert_eq!(sig.len(), 5);
    assert!(matches!(&sig[4], Type::FunctionPointer(cmp) if cmp.len() == 3 && cmp[0] == Type::Int));

    let strtold = file.find("strtold").unwrap();
    assert_eq!(strtold.signature().unwrap()[0], Type::LongDouble);
}

#[test]
fn records() {
    let Some(file) = metadata() else { return };
    let gvalue = file.find("_GValue").unwrap();
    assert_eq!(gvalue.kind(), MetaKind::Struct);
    let fields = gvalue.fields().unwrap();
    assert_eq!(fields[0].0, "g_type");
    assert_eq!(fields[1].0, "data");
    match &fields[1].1 {
        Type::ConstantArray(2, inner) => {
            assert!(matches!(&**inner, Type::AnonymousUnion(f) if f.iter().any(|(n, _)| n == "v_pointer")))
        }
        other => panic!("unexpected GValue.data type {other:?}"),
    }

    let tm = file.find("tm").unwrap();
    assert_eq!(tm.kind(), MetaKind::Struct);
    assert_eq!(tm.fields().unwrap()[0], ("tm_sec".into(), Type::Int));
}

#[test]
fn vars_and_constants() {
    let Some(file) = metadata() else { return };
    let stdout = file.find("stdout").unwrap();
    assert_eq!(stdout.kind(), MetaKind::Var);
    assert!(matches!(stdout.var_type().unwrap(), Type::Pointer(_)));

    let level = file.find("GLogLevelFlags").unwrap();
    assert_eq!(level.kind(), MetaKind::JsCode);
    assert!(level.js_code().unwrap().starts_with("__tsEnum({"));

    let error = file.find("G_LOG_LEVEL_ERROR").unwrap();
    assert_eq!(error.js_code(), Some("4"));
}
