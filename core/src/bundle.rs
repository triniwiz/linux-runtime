//! App bundle layout next to the executable: extra `metadata/*.bin` from app and
//! plugin native code, and their shared libraries in `lib/`.

use nativescript_metadata::{MetaFile, MetaSet};
use std::path::{Path, PathBuf};

fn exe_dir() -> Option<PathBuf> {
    Some(std::env::current_exe().ok()?.parent()?.to_path_buf())
}

/// `sdk` followed by the bundle's metadata files (sorted, so loading is stable).
/// Also registers the bundle's `lib/` for metadata sonames.
pub fn load_metadata(sdk: &Path) -> Result<MetaSet, String> {
    let open = |p: &Path| MetaFile::open(p).map_err(|e| format!("{}: {e}", p.display()));
    let mut set = MetaSet::from(open(sdk)?);
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
