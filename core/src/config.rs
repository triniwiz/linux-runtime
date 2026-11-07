//! App settings the CLI writes into the app's package.json (from nativescript.config).

use crate::nsbundle::{self as vfs, fuses, Fuse};
use std::path::Path;

/// package.json of the app `entry` (its folder, or a script inside it).
pub fn package_json(entry: &Path) -> Option<serde_json::Value> {
    let dir = if vfs::is_dir(entry) {
        entry
    } else {
        entry.parent()?
    };
    crate::resolve::read_json(&dir.join("package.json"))
}

/// `linux.v8Flags`, plus `$NS_V8_FLAGS` where env overrides are allowed.
pub fn v8_flags(entry: Option<&Path>) -> String {
    let mut flags = entry
        .and_then(package_json)
        .and_then(|j| j["linux"]["v8Flags"].as_str().map(str::to_owned))
        .unwrap_or_default();
    if fuses::get().is_on(Fuse::EnvOverrides) {
        if let Ok(env) = std::env::var("NS_V8_FLAGS") {
            flags.push(' ');
            flags.push_str(&env);
        }
    }
    flags.trim().to_owned()
}
