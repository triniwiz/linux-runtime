//! Runs against metadata produced by `metadata-generator/tests/test-mdg-executable.sh`.
//! Set `NS_TEST_METADATA` to point at a different file. Skipped when missing.

use nativescript_metadata::{MetaFile, MetaKind, Type};
use std::path::PathBuf;

fn metadata() -> Option<MetaFile> {
    let path = std::env::var_os("NS_TEST_METADATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
                "../metadata-generator/tests/TestOutput/metadata-{}.bin",
                std::env::consts::ARCH
            ))
        });
    if !path.exists() {
        eprintln!(
            "skipping: {} not found, run metadata-generator/tests/test-mdg-executable.sh",
            path.display()
        );
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
    let libc = file
        .modules()
        .find(|m| m.name() == "libc")
        .expect("libc module");
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
    assert_eq!(
        strlen.signature().unwrap(),
        vec![Type::ULong, Type::ConstCString]
    );
    assert!(!strlen.is_variadic());

    let printf = file.find("printf").unwrap();
    assert!(printf.is_variadic());
    assert_eq!(
        printf.signature().unwrap(),
        vec![Type::Int, Type::ConstCString]
    );

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
    let gvalue = file.find_global("_GValue").unwrap();
    assert_eq!(gvalue.kind(), MetaKind::Struct);
    let fields = gvalue.fields().unwrap();
    assert_eq!(fields[0].0, "g_type");
    assert_eq!(fields[1].0, "data");
    match &fields[1].1 {
        Type::ConstantArray(2, inner) => {
            assert!(
                matches!(&**inner, Type::AnonymousUnion(f, _) if f.iter().any(|(n, _)| n == "v_pointer"))
            )
        }
        other => panic!("unexpected GValue.data type {other:?}"),
    }

    let tm = file.find("tm").unwrap();
    assert_eq!(tm.kind(), MetaKind::Struct);
    assert_eq!(tm.fields().unwrap()[0], ("tm_sec".into(), Type::Int));
}

#[test]
fn records_take_their_typedef_name() {
    let Some(file) = metadata() else { return };
    // typedef struct _GdkRGBA GdkRGBA; ... struct _GdkRGBA { ... };
    for (typedef, tag) in [
        ("GdkRGBA", "_GdkRGBA"),
        ("graphene_rect_t", "_graphene_rect_t"),
        ("GValue", "_GValue"),
    ] {
        let Some(meta) = file.find(typedef) else {
            panic!("{typedef} not found by its typedef name");
        };
        assert_eq!(meta.kind(), MetaKind::Struct);
        assert_eq!(meta.name(), tag);
        assert!(file.find(tag).is_none(), "{tag} is not a js name");
        let by_tag = file.find_global(tag).expect(tag);
        assert_eq!(by_tag.js_name(), typedef);
    }
    // a typedef next to its definition was already the name
    assert_eq!(file.find("div_t").unwrap().kind(), MetaKind::Struct);
    // no typedef: the tag
    assert_eq!(file.find("tm").unwrap().name(), "tm");
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

#[test]
fn record_layouts_and_const_strings() {
    let Some(file) = metadata() else { return };
    let tm = file.find("tm").unwrap().record_layout().unwrap();
    assert_eq!((tm.size, tm.align), (56, 8));
    assert_eq!(tm.fields[1].offset_bits, 32);
    assert_eq!(tm.fields[9].offset_bits, 320); // tm_gmtoff after tm_isdst + padding

    // struct _GList { gpointer data; GList *next; GList *prev; }
    let list = file.find_global("_GList").unwrap().record_layout().unwrap();
    assert_eq!(list.size, 24);

    // GTK uses bitfields, e.g. struct _GdkRGBA has none but _GtkBorder has none either:
    // find any record with a bitfield and check widths are sane
    let with_bits = file
        .iter()
        .filter_map(|m| m.record_layout())
        .find(|l| l.fields.iter().any(|f| f.bit_width > 0))
        .expect("some record with bitfields");
    assert!(with_bits.fields.iter().all(|f| f.bit_width <= 64));

    let name = file.find("g_get_user_name").unwrap();
    assert_eq!(name.signature().unwrap()[0], Type::ConstCString);
    let dup = file.find("g_strdup").unwrap();
    assert_eq!(dup.signature().unwrap()[0], Type::CString);
}

fn same(a: &nativescript_metadata::Meta, b: &nativescript_metadata::Meta) {
    assert_eq!(a.kind(), b.kind());
    assert_eq!(a.js_name(), b.js_name());
    assert_eq!(a.name(), b.name());
    assert_eq!(a.demangled_name(), b.demangled_name());
    assert_eq!(a.flags(), b.flags());
    assert_eq!(a.module().name(), b.module().name());
    assert_eq!(
        a.module().libraries().map(|l| l.name()).collect::<Vec<_>>(),
        b.module().libraries().map(|l| l.name()).collect::<Vec<_>>()
    );
    assert_eq!(a.signature(), b.signature(), "{}", a.js_name());
    assert_eq!(a.var_type(), b.var_type(), "{}", a.js_name());
    assert_eq!(a.js_code(), b.js_code());
    assert_eq!(a.fields(), b.fields(), "{}", a.js_name());
    assert_eq!(a.record_layout(), b.record_layout(), "{}", a.js_name());
}

#[test]
fn unfiltered_copy_is_identical() {
    use nativescript_metadata::filter::{filter, Filter};
    let Some(file) = metadata() else { return };
    let (bytes, stats) = filter(&file, &Filter::new(None, None));
    let copy = MetaFile::from_vec(bytes).unwrap();
    assert_eq!(stats.kept, stats.declarations);
    assert_eq!(copy.iter().count(), file.iter().count());
    for meta in file.iter() {
        let found = copy.find(meta.js_name()).expect(meta.js_name());
        // duplicate js names resolve to the first, like the source
        same(&file.find(meta.js_name()).unwrap(), &found);
        assert!(copy.find_native(meta.name()).is_some(), "{}", meta.name());
    }
}

#[test]
fn filter_keeps_referenced_records() {
    use nativescript_metadata::filter::{filter, Filter};
    let Some(file) = metadata() else { return };
    if file.find("gtk_widget_snapshot_child").is_none() {
        return;
    }
    let (bytes, stats) = filter(
        &file,
        &Filter::new(
            Some("Gtk:gtk_widget_*\nGLib:G_PRIORITY_*"),
            Some("Gtk:gtk_widget_show"),
        ),
    );
    let small = MetaFile::from_vec(bytes).unwrap();
    assert!(stats.kept < stats.declarations / 10);
    assert!(stats.dependencies > 0);
    same(
        &file.find("gtk_widget_measure").unwrap(),
        &small.find("gtk_widget_measure").unwrap(),
    );
    assert!(small.find("gtk_widget_show").is_none());
    assert!(small.find("gtk_button_new").is_none());
    assert!(small.find("g_signal_connect_data").is_some());
    assert_eq!(
        small.find("G_PRIORITY_DEFAULT").and_then(|m| m.js_code()),
        Some("0")
    );
    // GtkAllocation from gtk_widget_size_allocate's signature
    for meta in small.iter() {
        for ty in meta.signature().unwrap_or_default() {
            if let Type::Pointer(inner) = ty {
                if let Type::Struct(name) = *inner {
                    assert!(small.find(&name).is_some(), "{name} missing");
                }
            }
        }
    }
}
