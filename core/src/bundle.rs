//! App bundle layout next to the executable: extra `metadata/*.bin` from app and
//! plugin native code, and their shared libraries in `lib/`.

use nativescript_metadata::{MetaFile, MetaSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

fn exe_dir() -> Option<PathBuf> {
    Some(std::env::current_exe().ok()?.parent()?.to_path_buf())
}

/// The metadata files to load: `--metadata`, then `$NS_METADATA` (both a
/// `:`-separated list, like the Node-API addon), then `metadata-<arch>.bin` next
/// to the executable or in the working directory. Release builds (`overrides`
/// off, the envOverrides fuse) only use the one next to the executable.
pub fn find_metadata(explicit: Option<OsString>, overrides: bool) -> Option<Vec<PathBuf>> {
    let file = format!("metadata-{}.bin", std::env::consts::ARCH);
    let next_to_exe = || Some(exe_dir()?.join(&file)).filter(|p| p.exists());
    if !overrides {
        return next_to_exe().map(|p| vec![p]);
    }
    if let Some(list) = explicit.or_else(|| std::env::var_os("NS_METADATA")) {
        let files: Vec<PathBuf> = std::env::split_paths(&list)
            .filter(|p| !p.as_os_str().is_empty())
            .collect();
        if !files.is_empty() {
            return Some(files);
        }
    }
    next_to_exe()
        .or_else(|| Some(PathBuf::from(&file)).filter(|p| p.exists()))
        .map(|p| vec![p])
}

/// `files` (the SDK first) followed by the bundle's `metadata/*.bin` (sorted,
/// so loading is stable). Also registers the bundle's `lib/` for metadata sonames.
/// A name in several files resolves to the first.
pub fn load_metadata(files: &[PathBuf]) -> Result<MetaSet, String> {
    let open = |p: &Path| MetaFile::open(p).map_err(|e| format!("{}: {e}", p.display()));
    let (first, rest) = files.split_first().ok_or("no metadata file")?;
    let mut set = MetaSet::from(open(first)?);
    for path in rest {
        set.push(open(path)?);
    }
    let Some(dir) = exe_dir() else { return Ok(set) };
    crate::set_library_dirs(vec![dir.join("lib")]);
    let mut extra: Vec<PathBuf> = std::fs::read_dir(dir.join("metadata"))
        .map(|entries| {
            entries
                .filter_map(|e| Some(e.ok()?.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "bin"))
                .collect()
        })
        .unwrap_or_default();
    extra.sort();
    for path in extra {
        set.push(open(&path)?);
    }
    Ok(set)
}
