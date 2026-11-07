use super::container::{HEADER_LEN, RECORD_LEN};
use super::fuses::{Fuses, SENTINEL, SLOT_LEN};
use super::pack::{glob_match, pack_dir, PackOptions, DEFAULT_UNPACK};
use super::*;
use std::fs;
use std::sync::atomic::{AtomicU32, Ordering};

fn scratch(name: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "nsbundle-{name}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// app/ with code, json, an image, a font and an addon; returns (app dir, bundle path).
fn sample(name: &str) -> (PathBuf, PathBuf) {
    let root = scratch(name);
    let app = root.join("src");
    for (rel, data) in [
        ("bundle.js", &b"console.log('hi');"[..]),
        ("package.json", br#"{"main":"bundle.js"}"#),
        ("assets/logo.png", &[0x89, b'P', b'N', b'G', 0, 1, 2, 3]),
        ("assets/deep/a.txt", b"a"),
        ("assets.txt", b"sibling sorts between"),
        ("fonts/Ahem.ttf", b"font bytes"),
        ("native/addon.node", b"\x7fELF"),
        ("empty.txt", b""),
    ] {
        let p = app.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, data).unwrap();
    }
    (app, root.join("app.nsbundle"))
}

fn options(seal: Option<(KeyMode, [u8; 32])>) -> PackOptions {
    PackOptions {
        unpack: DEFAULT_UNPACK.iter().map(|s| s.to_string()).collect(),
        seal,
    }
}

fn plain_fuses() -> Fuses {
    Fuses::parse(&{
        let mut s = [0u8; SLOT_LEN];
        s[..32].copy_from_slice(&SENTINEL);
        s[32] = 1;
        s[33] = 4;
        s[34..42].copy_from_slice(b"0011rrrr");
        s
    })
    .unwrap()
}

#[test]
fn pack_and_read() {
    let (app, out) = sample("plain");
    let report = pack_dir(&app, &out, &options(None)).unwrap();
    assert_eq!((report.entries, report.unpacked), (8, 2));
    let c = Container::open(&out, None, true).unwrap();
    assert!(!c.sealed());
    assert_eq!(c.toc_hash(), report.toc_hash);

    let i = c.find("bundle.js").unwrap();
    let bytes = c.read(i).unwrap();
    assert!(
        matches!(bytes, Bytes::Mapped(_)),
        "stored entries are zero-copy"
    );
    assert_eq!(&*bytes, b"console.log('hi');");
    assert_eq!(bytes.as_ptr() as usize % 16, 0, "entries are aligned");
    assert_eq!(&*c.read(c.find("empty.txt").unwrap()).unwrap(), b"");

    assert!(c.is_dir("") && c.is_dir("assets") && c.is_dir("assets/deep"));
    assert!(!c.is_dir("assets.txt") && !c.is_dir("asset"));
    assert!(c.find("assets").is_none());
    assert_eq!(
        c.list("").unwrap(),
        // byte order: '.' sorts before '/'
        vec![
            ("assets.txt", false),
            ("assets", true),
            ("bundle.js", false),
            ("empty.txt", false),
            ("fonts", true),
            ("native", true),
            ("package.json", false),
        ]
    );
    assert_eq!(
        c.list("assets").unwrap(),
        vec![("deep", true), ("logo.png", false)]
    );
    assert!(c.list("bundle.js").is_none());
}

#[test]
fn unpacked_entries() {
    let (app, out) = sample("unpacked");
    pack_dir(&app, &out, &options(None)).unwrap();
    let c = Container::open(&out, None, true).unwrap();
    let dir = out.with_file_name("app.nsbundle.unpacked");
    assert_eq!(c.unpacked_dir(), dir);
    for name in ["fonts/Ahem.ttf", "native/addon.node"] {
        let i = c.find(name).unwrap();
        assert_eq!(c.entry(i).kind, Kind::Unpacked);
        assert_eq!(c.unpacked_path(i).unwrap(), dir.join(name));
        assert_eq!(&*c.read(i).unwrap(), &fs::read(app.join(name)).unwrap()[..]);
    }
    assert!(c.unpacked_path(c.find("bundle.js").unwrap()).is_none());
    assert!(!dir.join("bundle.js").exists());

    // a replaced unpacked file fails its hash check
    fs::write(dir.join("fonts/Ahem.ttf"), b"other").unwrap();
    let c = Container::open(&out, None, true).unwrap();
    assert!(c.read(c.find("fonts/Ahem.ttf").unwrap()).is_err());
    let c = Container::open(&out, None, false).unwrap();
    assert_eq!(
        &*c.read(c.find("fonts/Ahem.ttf").unwrap()).unwrap(),
        b"other"
    );
}

#[test]
fn seal_and_open() {
    let (app, out) = sample("sealed");
    let key = crypto::random::<32>();
    pack_dir(&app, &out, &options(Some((KeyMode::Embedded, key)))).unwrap();
    let raw = fs::read(&out).unwrap();
    assert!(
        !raw.windows(7).any(|w| w == b"console"),
        "no plaintext in a sealed bundle"
    );

    let c = Container::open(&out, Some(key), true).unwrap();
    assert!(c.sealed());
    let i = c.find("bundle.js").unwrap();
    assert_eq!(c.entry(i).kind, Kind::Sealed);
    assert_eq!(&*c.read(i).unwrap(), b"console.log('hi');");
    // cached: the second read doesn't decrypt again
    let (a, b) = (c.read(i).unwrap(), c.read(i).unwrap());
    assert_eq!(a.as_ptr(), b.as_ptr());
    assert_eq!(&*c.read(c.find("empty.txt").unwrap()).unwrap(), b"");
    // unpacked files stay plain
    assert_eq!(
        c.entry(c.find("fonts/Ahem.ttf").unwrap()).kind,
        Kind::Unpacked
    );

    let err = Container::open(&out, Some([7; 32]), false).err().unwrap();
    assert!(err.contains("wrong key"), "{err}");
    assert!(Container::open(&out, None, false).is_err());
}

#[test]
fn default_key_mode() {
    let (app, out) = sample("pepper");
    pack_dir(
        &app,
        &out,
        &options(Some((KeyMode::Default, crypto::default_key()))),
    )
    .unwrap();
    // opens without an embedded key
    let c = Container::open(&out, None, false).unwrap();
    assert_eq!(c.key_mode, KeyMode::Default);
    assert_eq!(
        &*c.read(c.find("bundle.js").unwrap()).unwrap(),
        b"console.log('hi');"
    );
}

#[test]
fn tampered_sealed_entry() {
    let (app, out) = sample("tamper-sealed");
    let key = [3u8; 32];
    pack_dir(&app, &out, &options(Some((KeyMode::Embedded, key)))).unwrap();
    let c = Container::open(&out, Some(key), false).unwrap();
    let i = c.find("bundle.js").unwrap();
    let at = {
        let p = c.read(i).unwrap();
        drop(p);
        // data offset from the record
        let raw = fs::read(&out).unwrap();
        let rec = &raw[HEADER_LEN + i * RECORD_LEN..];
        let data_at = u64::from_le_bytes(raw[24..32].try_into().unwrap());
        (data_at + u64::from_le_bytes(rec[16..24].try_into().unwrap())) as usize
    };
    let mut raw = fs::read(&out).unwrap();
    raw[at] ^= 1;
    fs::write(&out, &raw).unwrap();
    let c = Container::open(&out, Some(key), false).unwrap();
    assert!(c.read(c.find("bundle.js").unwrap()).is_err());
    assert!(c.read(c.find("package.json").unwrap()).is_ok());
}

#[test]
fn integrity_fuse() {
    let (app, out) = sample("integrity");
    let report = pack_dir(&app, &out, &options(None)).unwrap();
    let mut fuses = plain_fuses();
    fuses.set(Fuse::BundleIntegrity, true);
    // on without a hash: refused
    assert!(open(&out, &fuses).is_err());
    fuses.integrity = Some(report.toc_hash);
    let c = open(&out, &fuses).unwrap();
    assert_eq!(
        &*c.read(c.find("bundle.js").unwrap()).unwrap(),
        b"console.log('hi');"
    );

    // a changed TOC field (mode bits of the first entry) no longer matches
    let mut raw = fs::read(&out).unwrap();
    raw[HEADER_LEN + 12] ^= 0o100;
    fs::write(&out, &raw).unwrap();
    let err = open(&out, &fuses).err().unwrap();
    assert!(err.contains("integrity"), "{err}");
    fuses.set(Fuse::BundleIntegrity, false);
    assert!(open(&out, &fuses).is_ok());

    // changed content of a stored entry fails on read
    let raw = fs::read(&out).unwrap();
    let c = Container::open(&out, None, false).unwrap();
    let i = c.find("bundle.js").unwrap();
    let data_at = u64::from_le_bytes(raw[24..32].try_into().unwrap());
    let rec = &raw[HEADER_LEN + i * RECORD_LEN..];
    let at = (data_at + u64::from_le_bytes(rec[16..24].try_into().unwrap())) as usize;
    let mut raw = raw.clone();
    raw[at] = b'C';
    fs::write(&out, &raw).unwrap();
    let c = Container::open(&out, None, true).unwrap();
    assert!(c.read(c.find("bundle.js").unwrap()).is_err());
    assert!(c.read(c.find("package.json").unwrap()).is_ok());
}

#[test]
fn corrupt_headers_fail_cleanly() {
    let (app, out) = sample("corrupt");
    pack_dir(&app, &out, &options(None)).unwrap();
    let good = fs::read(&out).unwrap();
    let cases: [(usize, &[u8]); 5] = [
        (0, b"NSB1"),
        (12, &u32::MAX.to_le_bytes()),
        (16, &u32::MAX.to_le_bytes()),
        (24, &u64::MAX.to_le_bytes()),
        // first record's data offset
        (HEADER_LEN + 16, &u64::MAX.to_le_bytes()),
    ];
    for (at, bytes) in cases {
        let mut raw = good.clone();
        raw[at..at + bytes.len()].copy_from_slice(bytes);
        fs::write(&out, &raw).unwrap();
        assert!(Container::open(&out, None, false).is_err(), "patch at {at}");
    }
    fs::write(&out, &good[..10]).unwrap();
    assert!(Container::open(&out, None, false).is_err());
}

#[test]
fn globs() {
    assert!(glob_match("fonts/**", "fonts/Ahem.ttf"));
    assert!(glob_match("fonts/**", "fonts/a/b.otf"));
    assert!(!glob_match("fonts/**", "assets/fonts.ttf"));
    assert!(glob_match("*.node", "a/b/c.node"));
    assert!(glob_match("*.so.*", "lib/libx.so.1"));
    assert!(glob_match("**/*.{ttf,otf}", "x/y.otf"));
    assert!(glob_match("**/*.ttf", "y.ttf"));
    assert!(glob_match("assets/?.png", "assets/a.png"));
    assert!(!glob_match("assets/?.png", "assets/ab.png"));
    assert!(glob_match("./data/*", "data/x.bin"));
}

#[test]
fn fuse_slot_patching() {
    let mut binary = vec![0xAAu8; 1000];
    binary.extend_from_slice(&{
        let mut s = [0u8; SLOT_LEN];
        s[..32].copy_from_slice(&SENTINEL);
        s[32] = 1;
        s[33] = 4;
        s[34..42].copy_from_slice(b"0011rrrr");
        s
    });
    binary.extend_from_slice(&[0x55; 64]);
    let before = fuses::read(&binary).unwrap();
    assert_eq!(before, plain_fuses());
    assert!(before.key.is_none() && before.is_on(Fuse::Inspector));

    let mut f = before.clone();
    f.set(Fuse::Inspector, false);
    f.set(Fuse::BundleIntegrity, true);
    f.key = Some([9; 32]);
    f.integrity = Some([4; 32]);
    fuses::write(&mut binary, &f).unwrap();
    assert_eq!(fuses::read(&binary).unwrap(), f);
    // the key isn't stored as is
    assert!(!binary.windows(32).any(|w| w == [9u8; 32]));
    assert_eq!(binary.len(), 1000 + SLOT_LEN + 64);

    let copy = binary.clone();
    binary.extend_from_slice(&copy);
    assert!(fuses::find(&binary).is_err(), "two slots are ambiguous");
    assert!(fuses::find(&[0u8; 300]).is_err());
}

#[test]
fn this_binary_has_default_fuses() {
    let f = fuses::get();
    assert!(!f.is_on(Fuse::BundleIntegrity) && !f.is_on(Fuse::OnlyLoadAppFromBundle));
    assert!(f.is_on(Fuse::Inspector) && f.is_on(Fuse::EnvOverrides));
    assert!(f.key.is_none());
}

#[test]
fn mounted_lookups() {
    let (app, out) = sample("mount");
    pack_dir(&app, &out, &options(None)).unwrap();
    let root = out.with_file_name("app");
    mount(&out, &root).unwrap();
    assert!(is_file(&root.join("bundle.js")));
    assert!(is_file(&root.join("assets/../bundle.js")));
    assert!(is_file(&root.join("assets/.").join("logo.png")));
    assert!(is_dir(&root) && is_dir(&root.join("assets")));
    assert!(!is_file(&root.join("missing.js")));
    assert_eq!(
        &*read(&root.join("bundle.js")).unwrap(),
        b"console.log('hi');"
    );
    assert_eq!(
        read_to_string(&root.join("package.json")).unwrap(),
        r#"{"main":"bundle.js"}"#
    );
    assert_eq!(
        read_dir(&root.join("assets")).unwrap(),
        vec![("deep".to_string(), true), ("logo.png".to_string(), false)]
    );
    let s = stat(&root.join("assets/logo.png")).unwrap();
    assert!(s.bundled && !s.dir && !s.unpacked && s.size == 8);
    let unpacked = out.with_file_name("app.nsbundle.unpacked");
    assert_eq!(
        real_path(&root.join("fonts/Ahem.ttf")).unwrap(),
        unpacked.join("fonts/Ahem.ttf")
    );
    assert_eq!(
        real_path(&root.join("fonts")).unwrap(),
        unpacked.join("fonts")
    );
    assert!(real_path(&root.join("bundle.js")).is_none());
    assert_eq!(
        native_path(&root.join("native/addon.node")).unwrap(),
        unpacked.join("native/addon.node")
    );
    assert!(native_path(&root.join("bundle.js")).is_err());
    assert!(in_bundle(&root.join("x")) && !in_bundle(&app));
    assert_eq!(app_path("~/fonts/Ahem.ttf"), root.join("fonts/Ahem.ttf"));
    assert_eq!(app_path("bundle.js"), root.join("bundle.js"));
    // outside the root: the disk
    assert!(is_file(&app.join("bundle.js")));
    // the resolver goes through the bundle
    let resolved = crate::resolve::resolve("./", &root, crate::resolve::Kind::Require).unwrap();
    assert_eq!(resolved, root.join("bundle.js"));
}

fn data_offset(raw: &[u8], i: usize) -> usize {
    let data_at = u64::from_le_bytes(raw[24..32].try_into().unwrap());
    let rec = &raw[HEADER_LEN + i * RECORD_LEN..];
    (data_at + u64::from_le_bytes(rec[16..24].try_into().unwrap())) as usize
}

#[test]
fn resealed_entry_fails_integrity() {
    // the key is recoverable from the runtime binary: re-sealing with it must not pass
    let (app, out) = sample("reseal");
    let key = [5u8; 32];
    let report = pack_dir(&app, &out, &options(Some((KeyMode::Embedded, key)))).unwrap();
    let mut fuses = plain_fuses();
    fuses.set(Fuse::BundleIntegrity, true);
    fuses.integrity = Some(report.toc_hash);
    fuses.key = Some(key);

    let mut raw = fs::read(&out).unwrap();
    let c = Container::open(&out, Some(key), false).unwrap();
    let i = c.find("bundle.js").unwrap();
    let rec = &raw[HEADER_LEN + i * RECORD_LEN..][..RECORD_LEN];
    let nonce: [u8; 12] = rec[40..52].try_into().unwrap();
    let mut evil = b"console.log('XX');".to_vec();
    crypto::seal(&key, nonce, b"bundle.js", &mut evil);
    let at = data_offset(&raw, i);
    raw[at..at + evil.len()].copy_from_slice(&evil);
    fs::write(&out, &raw).unwrap();

    let c = open(&out, &fuses).unwrap();
    let err = c.read(c.find("bundle.js").unwrap()).err().unwrap();
    assert!(err.contains("integrity"), "{err}");
    assert!(c.read(c.find("package.json").unwrap()).is_ok());
    // without the fuse, GCM still opens it
    let c = Container::open(&out, Some(key), false).unwrap();
    assert_eq!(&*c.read(i).unwrap(), b"console.log('XX');");
}

#[test]
fn unpacked_entries_are_checked_on_every_read() {
    let (app, out) = sample("unpacked-recheck");
    pack_dir(&app, &out, &options(None)).unwrap();
    let c = Container::open(&out, None, true).unwrap();
    let i = c.find("fonts/Ahem.ttf").unwrap();
    assert!(c.read(i).is_ok());
    fs::write(c.unpacked_path(i).unwrap(), b"swapped").unwrap();
    assert!(c.read(i).is_err());
}

#[test]
fn packed_modes_drop_special_and_write_bits() {
    use std::os::unix::fs::PermissionsExt;
    let (app, out) = sample("modes");
    let addon = app.join("native/addon.node");
    fs::set_permissions(&addon, fs::Permissions::from_mode(0o6777)).unwrap();
    pack_dir(&app, &out, &options(None)).unwrap();
    let c = Container::open(&out, None, false).unwrap();
    let i = c.find("native/addon.node").unwrap();
    assert_eq!(c.entry(i).mode, 0o755);
    let disk = fs::metadata(c.unpacked_path(i).unwrap()).unwrap();
    assert_eq!(disk.permissions().mode() & 0o7777, 0o755);
}

#[test]
fn mutated_tables_never_panic() {
    for seal in [None, Some((KeyMode::Embedded, [6u8; 32]))] {
        let (app, out) = sample("fuzz");
        pack_dir(&app, &out, &options(seal)).unwrap();
        let good = fs::read(&out).unwrap();
        let data_at = u64::from_le_bytes(good[24..32].try_into().unwrap()) as usize;
        let key = seal.map(|s| s.1);
        let count = u32::from_le_bytes(good[12..16].try_into().unwrap()) as usize;
        let mut cases: Vec<(usize, Vec<u8>)> = Vec::new();
        for at in 0..data_at {
            for value in [0u8, 1, 0x7f, 0x80, 0xff, good[at] ^ 0x10] {
                cases.push((at, vec![value]));
            }
        }
        // offset, stored and size fields near u64::MAX
        for i in 0..count {
            for field in [16, 24, 32] {
                for v in [u64::MAX, u64::MAX - 8] {
                    cases.push((HEADER_LEN + i * RECORD_LEN + field, v.to_le_bytes().to_vec()));
                }
            }
        }
        for (at, bytes) in cases {
            {
                let mut raw = good.clone();
                raw[at..at + bytes.len()].copy_from_slice(&bytes);
                fs::write(&out, &raw).unwrap();
                let Ok(c) = Container::open(&out, key, true) else {
                    continue;
                };
                for i in 0..c.len() {
                    let _ = c.read(i);
                    let _ = c.list(c.name(i));
                }
            }
        }
    }
}
