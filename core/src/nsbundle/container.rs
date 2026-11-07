//! `app.nsbundle` reader (format NSB2, see mod.rs). Mapped once; stored entries are
//! slices of the mapping, sealed ones are decrypted on read.

use super::crypto;
use std::collections::HashMap;
use std::fs::File;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub const MAGIC: &[u8; 4] = b"NSB2";
pub const HEADER_LEN: usize = 64;
pub const RECORD_LEN: usize = 88;
pub const ALIGN: u64 = 16;
pub const FLAG_SEALED: u32 = 1;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Kind {
    Stored = 0,
    Sealed = 1,
    /// written to `<bundle>.unpacked/<name>`
    Unpacked = 2,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum KeyMode {
    None = 0,
    /// the shared default pepper
    Default = 1,
    /// the per-build key embedded in the runtime binary
    Embedded = 2,
}

#[derive(Clone, Debug)]
pub struct Entry {
    /// range in `Container::names`
    name: (usize, usize),
    pub kind: Kind,
    pub mode: u32,
    offset: u64,
    stored: u64,
    pub size: u64,
    nonce: [u8; 12],
    /// of the content; of the ciphertext for sealed entries
    pub sha256: [u8; 32],
}

/// Entry bytes: borrowed from the mapping, or decrypted / read from disk.
#[derive(Clone)]
pub enum Bytes<'a> {
    Mapped(&'a [u8]),
    Owned(Arc<Vec<u8>>),
}

impl Deref for Bytes<'_> {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Bytes::Mapped(b) => b,
            Bytes::Owned(v) => v,
        }
    }
}

pub struct Container {
    // private copy-on-write mapping: JS may write to zero-copy buffers without faulting
    map: memmap2::MmapMut,
    entries: Vec<Entry>,
    // copied out: the file can change under the mapping
    names: String,
    data_at: usize,
    pub key_mode: KeyMode,
    key: Option<[u8; 32]>,
    toc_hash: [u8; 32],
    unpacked: PathBuf,
    verify: bool,
    verified: Vec<AtomicBool>,
    /// last decrypted entry (format sniffing and loading read a module twice) and package.json files
    cache: Mutex<(Option<(usize, Arc<Vec<u8>>)>, HashMap<usize, Arc<Vec<u8>>>)>,
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

/// Relative, `/` separated, no `.`/`..` or empty segments.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .split('/')
            .all(|s| !s.is_empty() && s != "." && s != "..")
}

impl Container {
    /// `key` opens `KeyMode::Embedded` containers; `verify` checks stored and unpacked
    /// entries against their SHA-256 on first read.
    pub fn open(path: &Path, key: Option<[u8; 32]>, verify: bool) -> Result<Self, String> {
        let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let map = unsafe { memmap2::MmapOptions::new().map_copy(&file) }
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let mut unpacked = path.as_os_str().to_owned();
        unpacked.push(".unpacked");
        Self::parse(map, key, verify, PathBuf::from(unpacked))
            .map_err(|e| format!("{}: {e}", path.display()))
    }

    fn parse(
        map: memmap2::MmapMut,
        key: Option<[u8; 32]>,
        verify: bool,
        unpacked: PathBuf,
    ) -> Result<Self, String> {
        let b: &[u8] = &map;
        if b.len() < HEADER_LEN || &b[..4] != MAGIC {
            return Err("not an app.nsbundle (bad magic)".into());
        }
        let flags = u32_at(b, 4);
        let key_mode = match b[8] {
            0 => KeyMode::None,
            1 => KeyMode::Default,
            2 => KeyMode::Embedded,
            m => return Err(format!("unknown key mode {m}")),
        };
        let count = u32_at(b, 12) as usize;
        let toc_len = u32_at(b, 16) as usize;
        let data_at = u64_at(b, 24);
        let toc_end = HEADER_LEN
            .checked_add(toc_len)
            .filter(|&e| e <= b.len())
            .ok_or("truncated table of contents")?;
        let names_at = count
            .checked_mul(RECORD_LEN)
            .and_then(|r| r.checked_add(HEADER_LEN))
            .filter(|&n| n <= toc_end)
            .ok_or("bad entry count")?;
        if data_at < toc_end as u64 || data_at > b.len() as u64 {
            return Err("bad data offset".into());
        }
        let data_at = data_at as usize;
        let data_len = (b.len() - data_at) as u64;
        let sealed = flags & FLAG_SEALED != 0;
        if sealed != (key_mode != KeyMode::None) {
            return Err("inconsistent seal flags".into());
        }

        let key = match key_mode {
            KeyMode::None => None,
            KeyMode::Default => Some(crypto::default_key()),
            KeyMode::Embedded => {
                Some(key.ok_or("sealed with a per-build key, but the runtime has none")?)
            }
        };
        if let Some(k) = &key {
            if crypto::key_check(k)[..] != b[32..48] {
                return Err("wrong key for this bundle".into());
            }
        }

        let mut entries = Vec::with_capacity(count);
        let names_len = toc_end - names_at;
        for i in 0..count {
            let r = &b[HEADER_LEN + i * RECORD_LEN..][..RECORD_LEN];
            let (off, len) = (u32_at(r, 0) as usize, u32_at(r, 4) as usize);
            if off.checked_add(len).is_none_or(|e| e > names_len) {
                return Err(format!("entry {i}: name out of range"));
            }
            let kind = match r[8] {
                0 => Kind::Stored,
                1 if sealed => Kind::Sealed,
                2 => Kind::Unpacked,
                k => return Err(format!("entry {i}: bad kind {k}")),
            };
            let e = Entry {
                name: (names_at + off, len),
                kind,
                mode: u32_at(r, 12),
                offset: u64_at(r, 16),
                stored: u64_at(r, 24),
                size: u64_at(r, 32),
                nonce: r[40..52].try_into().unwrap(),
                sha256: r[56..88].try_into().unwrap(),
            };
            let expected = match kind {
                Kind::Stored => Some(e.size),
                Kind::Sealed => e.size.checked_add(crypto::TAG_LEN as u64),
                Kind::Unpacked => Some(0),
            };
            if Some(e.stored) != expected
                || e.offset
                    .checked_add(e.stored)
                    .is_none_or(|end| end > data_len)
            {
                return Err(format!("entry {i}: data out of range"));
            }
            entries.push(e);
        }
        let mut names = String::with_capacity(names_len);
        let mut previous = 0..0;
        for (i, e) in entries.iter_mut().enumerate() {
            let n = std::str::from_utf8(&b[e.name.0..e.name.0 + e.name.1])
                .map_err(|_| format!("entry {i}: name isn't UTF-8"))?;
            if !valid_name(n) {
                return Err(format!("entry {i}: invalid name {n:?}"));
            }
            // sorted and unique: lookups are binary searches
            if i > 0 && names[previous.clone()].as_bytes() >= n.as_bytes() {
                return Err("entries aren't sorted".into());
            }
            previous = names.len()..names.len() + n.len();
            e.name = (names.len(), n.len());
            names.push_str(n);
        }
        let toc_hash = crypto::sha256(&b[..toc_end]);
        let verified = (0..count).map(|_| AtomicBool::new(false)).collect();
        Ok(Container {
            map,
            entries,
            names,
            data_at,
            key_mode,
            key,
            toc_hash,
            unpacked,
            verify,
            verified,
            cache: Mutex::default(),
        })
    }

    /// SHA-256 of the header and table of contents (the integrity fuse's value).
    pub fn toc_hash(&self) -> [u8; 32] {
        self.toc_hash
    }

    pub fn sealed(&self) -> bool {
        self.key_mode != KeyMode::None
    }

    pub fn unpacked_dir(&self) -> &Path {
        &self.unpacked
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entry(&self, i: usize) -> &Entry {
        &self.entries[i]
    }

    pub fn name(&self, i: usize) -> &str {
        self.name_of(&self.entries[i])
    }

    fn lower_bound(&self, key: &str) -> usize {
        self.entries
            .partition_point(|e| self.name_of(e).as_bytes() < key.as_bytes())
    }

    fn name_of(&self, e: &Entry) -> &str {
        &self.names[e.name.0..e.name.0 + e.name.1]
    }

    /// Index of the file `name` (relative, `/` separated).
    pub fn find(&self, name: &str) -> Option<usize> {
        let i = self.lower_bound(name);
        (i < self.entries.len() && self.name(i) == name).then_some(i)
    }

    /// Whether `name` is a directory ("" is the root).
    pub fn is_dir(&self, name: &str) -> bool {
        if name.is_empty() {
            return true;
        }
        let prefix = format!("{name}/");
        let i = self.lower_bound(&prefix);
        i < self.entries.len() && self.name(i).starts_with(&prefix)
    }

    /// Children of directory `name`: (child name, is directory), sorted.
    pub fn list(&self, name: &str) -> Option<Vec<(&str, bool)>> {
        if !self.is_dir(name) {
            return None;
        }
        let prefix = if name.is_empty() {
            String::new()
        } else {
            format!("{name}/")
        };
        let mut out: Vec<(&str, bool)> = Vec::new();
        for i in self.lower_bound(&prefix)..self.entries.len() {
            let Some(rest) = self.name(i).strip_prefix(prefix.as_str()) else {
                break;
            };
            let child = match rest.split_once('/') {
                Some((dir, _)) => (dir, true),
                None => (rest, false),
            };
            // a directory's entries are contiguous
            if out.last() != Some(&child) {
                out.push(child);
            }
        }
        Some(out)
    }

    /// Path on disk of an unpacked entry.
    pub fn unpacked_path(&self, i: usize) -> Option<PathBuf> {
        (self.entries[i].kind == Kind::Unpacked).then(|| self.unpacked.join(self.name(i)))
    }

    fn check(&self, i: usize, bytes: &[u8]) -> Result<(), String> {
        if !self.verify || self.verified[i].load(Ordering::Relaxed) {
            return Ok(());
        }
        if crypto::sha256(bytes) != self.entries[i].sha256 {
            return Err(format!(
                "{}: content doesn't match the bundle (integrity check failed)",
                self.name(i)
            ));
        }
        // unpacked files are read from disk again each time
        if self.entries[i].kind != Kind::Unpacked {
            self.verified[i].store(true, Ordering::Relaxed);
        }
        Ok(())
    }

    /// Contents of entry `i`: zero-copy for stored entries.
    pub fn read(&self, i: usize) -> Result<Bytes<'_>, String> {
        let e = &self.entries[i];
        match e.kind {
            Kind::Stored => {
                let at = self.data_at + e.offset as usize;
                let bytes = &self.map[at..at + e.stored as usize];
                self.check(i, bytes)?;
                Ok(Bytes::Mapped(bytes))
            }
            Kind::Unpacked => {
                let path = self.unpacked.join(self.name(i));
                let bytes =
                    std::fs::read(&path).map_err(|err| format!("{}: {err}", path.display()))?;
                self.check(i, &bytes)?;
                Ok(Bytes::Owned(Arc::new(bytes)))
            }
            Kind::Sealed => self.decrypt(i).map(Bytes::Owned),
        }
    }

    fn decrypt(&self, i: usize) -> Result<Arc<Vec<u8>>, String> {
        {
            let cache = self.cache.lock().unwrap();
            if let Some((_, v)) = cache.0.as_ref().filter(|(at, _)| *at == i) {
                return Ok(v.clone());
            }
            if let Some(v) = cache.1.get(&i) {
                return Ok(v.clone());
            }
        }
        let e = &self.entries[i];
        let at = self.data_at + e.offset as usize;
        let key = self.key.as_ref().ok_or("no key")?;
        let name = self.name(i);
        let sealed = &self.map[at..at + e.stored as usize];
        // the key ships in the runtime binary, so GCM alone doesn't stop re-sealed content
        self.check(i, sealed)?;
        let plain = crypto::open(key, e.nonce, name.as_bytes(), sealed)
            .ok_or_else(|| format!("{name}: can't decrypt (tampered bundle or wrong key)"))?;
        let plain = Arc::new(plain);
        let mut cache = self.cache.lock().unwrap();
        if name == "package.json" || name.ends_with("/package.json") {
            cache.1.insert(i, plain.clone());
        } else {
            cache.0 = Some((i, plain.clone()));
        }
        Ok(plain)
    }
}
