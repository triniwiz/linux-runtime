//! `app.nsbundle` writer.

use super::container::{KeyMode, Kind, ALIGN, FLAG_SEALED, HEADER_LEN, MAGIC, RECORD_LEN};
use super::crypto;
use std::fs;
use std::io::{self, Seek, SeekFrom, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// Always unpacked: native code is loaded by path.
pub const NATIVE_UNPACK: [&str; 3] = ["*.node", "*.so", "*.so.*"];
pub const DEFAULT_UNPACK: [&str; 1] = ["fonts/**"];

#[derive(Default)]
pub struct PackOptions {
    /// globs (relative to the input folder) of files written next to the bundle
    pub unpack: Vec<String>,
    pub seal: Option<(KeyMode, [u8; 32])>,
}

pub struct PackReport {
    pub entries: usize,
    pub unpacked: usize,
    pub sealed: bool,
    pub toc_hash: [u8; 32],
    pub unpacked_dir: PathBuf,
}

/// `*`, `?` within a segment, `**` across segments, `{a,b}`. Patterns without `/` match the file name.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    if let Some(open) = pattern.find('{') {
        if let Some(close) = pattern[open..].find('}').map(|c| open + c) {
            let (head, tail) = (&pattern[..open], &pattern[close + 1..]);
            return pattern[open + 1..close]
                .split(',')
                .any(|alt| glob_match(&format!("{head}{alt}{tail}"), path));
        }
    }
    let pattern = pattern.trim_start_matches("./");
    if !pattern.contains('/') {
        let name = path.rsplit('/').next().unwrap_or(path);
        return segment_match(pattern.as_bytes(), name.as_bytes());
    }
    let pat: Vec<&str> = pattern.split('/').collect();
    let segs: Vec<&str> = path.split('/').collect();
    segments_match(&pat, &segs)
}

fn segments_match(pat: &[&str], segs: &[&str]) -> bool {
    match pat.split_first() {
        None => segs.is_empty(),
        Some((&"**", rest)) => (0..=segs.len()).any(|i| segments_match(rest, &segs[i..])),
        Some((p, rest)) => segs.split_first().is_some_and(|(s, srest)| {
            segment_match(p.as_bytes(), s.as_bytes()) && segments_match(rest, srest)
        }),
    }
}

fn segment_match(p: &[u8], s: &[u8]) -> bool {
    match p.split_first() {
        None => s.is_empty(),
        Some((b'*', rest)) => (0..=s.len()).any(|i| segment_match(rest, &s[i..])),
        Some((b'?', rest)) => !s.is_empty() && segment_match(rest, &s[1..]),
        Some((c, rest)) => s.first() == Some(c) && segment_match(rest, &s[1..]),
    }
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let meta = fs::metadata(&path)?;
        if meta.is_dir() {
            collect(root, &path, out)?;
        } else if meta.is_file() {
            let rel = path.strip_prefix(root).unwrap().to_str().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{}: not UTF-8", path.display()),
                )
            })?;
            out.push((rel.to_owned(), path));
        }
    }
    Ok(())
}

fn align(n: u64) -> u64 {
    n.div_ceil(ALIGN) * ALIGN
}

/// Packs every file under `input` into `output`; unpacked files go to `<output>.unpacked/`.
pub fn pack_dir(input: &Path, output: &Path, options: &PackOptions) -> io::Result<PackReport> {
    let mut files = Vec::new();
    collect(input, input, &mut files)?;
    files.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    if files.len() > u32::MAX as usize {
        return Err(io::Error::other("too many files"));
    }

    let mut unpacked_dir = output.as_os_str().to_owned();
    unpacked_dir.push(".unpacked");
    let unpacked_dir = PathBuf::from(unpacked_dir);
    if unpacked_dir.exists() {
        fs::remove_dir_all(&unpacked_dir)?;
    }
    fs::create_dir_all(&unpacked_dir)?;

    let sealed = options.seal.is_some();
    let unpack = |rel: &str| {
        NATIVE_UNPACK
            .iter()
            .map(|p| p.to_string())
            .chain(options.unpack.iter().cloned())
            .any(|p| glob_match(&p, rel))
    };

    let toc_len = files.len() * RECORD_LEN + files.iter().map(|f| f.0.len()).sum::<usize>();
    if toc_len > u32::MAX as usize {
        return Err(io::Error::other("table of contents too large"));
    }
    let data_at = align((HEADER_LEN + toc_len) as u64);
    let tmp = output.with_extension("nsbundle.part");
    let mut out = io::BufWriter::new(fs::File::create(&tmp)?);

    let mut names = Vec::new();
    let mut records = Vec::with_capacity(files.len() * RECORD_LEN);
    let mut offset = 0u64;
    let mut unpacked = 0;
    for (rel, path) in &files {
        let mut data = fs::read(path)?;
        let mode = fs::metadata(path)?.permissions().mode();
        let size = data.len() as u64;
        let hash = crypto::sha256(&data);
        let (kind, nonce, digest) = if unpack(rel) {
            let target = unpacked_dir.join(rel);
            fs::create_dir_all(target.parent().unwrap())?;
            fs::write(&target, &data)?;
            fs::set_permissions(&target, fs::Permissions::from_mode(mode & 0o7777))?;
            unpacked += 1;
            data.clear();
            (Kind::Unpacked, [0u8; 12], hash)
        } else if let Some((_, key)) = &options.seal {
            let nonce = crypto::random::<12>();
            crypto::seal(key, nonce, rel.as_bytes(), &mut data);
            // a plaintext hash would let anyone confirm guesses of the content
            (Kind::Sealed, nonce, [0u8; 32])
        } else {
            (Kind::Stored, [0u8; 12], hash)
        };
        let stored = data.len() as u64;
        let at = if stored == 0 { 0 } else { offset };
        let mut r = [0u8; RECORD_LEN];
        r[0..4].copy_from_slice(&(names.len() as u32).to_le_bytes());
        r[4..8].copy_from_slice(&(rel.len() as u32).to_le_bytes());
        r[8] = kind as u8;
        r[12..16].copy_from_slice(&mode.to_le_bytes());
        r[16..24].copy_from_slice(&at.to_le_bytes());
        r[24..32].copy_from_slice(&stored.to_le_bytes());
        r[32..40].copy_from_slice(&size.to_le_bytes());
        r[40..52].copy_from_slice(&nonce);
        r[56..88].copy_from_slice(&digest);
        records.extend_from_slice(&r);
        names.extend_from_slice(rel.as_bytes());
        if stored > 0 {
            out.seek(SeekFrom::Start(data_at + at))?;
            out.write_all(&data)?;
            offset = align(at + stored);
        }
    }
    let mut header = [0u8; HEADER_LEN];
    header[..4].copy_from_slice(MAGIC);
    header[4..8].copy_from_slice(&(if sealed { FLAG_SEALED } else { 0 }).to_le_bytes());
    header[8] = options.seal.as_ref().map_or(KeyMode::None, |s| s.0) as u8;
    header[12..16].copy_from_slice(&(files.len() as u32).to_le_bytes());
    header[16..20].copy_from_slice(&(toc_len as u32).to_le_bytes());
    header[24..32].copy_from_slice(&data_at.to_le_bytes());
    if let Some((_, key)) = &options.seal {
        header[32..48].copy_from_slice(&crypto::key_check(key));
    }
    let mut toc = Vec::with_capacity(HEADER_LEN + toc_len);
    toc.extend_from_slice(&header);
    toc.extend_from_slice(&records);
    toc.extend_from_slice(&names);
    let toc_hash = crypto::sha256(&toc);

    out.seek(SeekFrom::Start(0))?;
    out.write_all(&toc)?;
    out.into_inner()?.set_len(data_at + offset)?;
    fs::rename(&tmp, output)?;
    Ok(PackReport {
        entries: files.len(),
        unpacked,
        sealed,
        toc_hash,
        unpacked_dir,
    })
}
