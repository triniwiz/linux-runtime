//! Packs app folders into `app.nsbundle` and patches runtime fuses (the Linux `nsbundle_pack`).

use nativescript_core::nsbundle::{crypto, fuses, pack, Container, Fuse, KeyMode, Kind};
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "usage:
  ns-bundle pack --input <dir> --output <app.nsbundle> [--unpack <glob>]... [--no-default-unpack]
                 [--seal [--key-hex <64 hex> | --write-key <file>]]
                 [--exe <runtime executable> [--fuse <name>=on|off]...]
  ns-bundle fuses <runtime executable> [--fuse <name>=on|off]...
  ns-bundle list <app.nsbundle> [--key-hex <64 hex> | --exe <runtime executable>]

Sealing embeds the key (random per run unless --key-hex) in --exe; without --exe the
shared default key is used. --exe also embeds the bundle's integrity hash.
Fuses: bundleIntegrity, onlyLoadAppFromBundle, inspector, envOverrides.";

fn fail(message: impl std::fmt::Display) -> ExitCode {
    eprintln!("ns-bundle: {message}");
    ExitCode::FAILURE
}

fn parse_fuse(arg: &str) -> Result<(Fuse, bool), String> {
    let (name, value) = arg
        .split_once('=')
        .ok_or(format!("--fuse {arg}: expected name=on|off"))?;
    let fuse = fuses::FUSES
        .iter()
        .find(|(_, n)| n.eq_ignore_ascii_case(name))
        .map(|(f, _)| *f)
        .ok_or(format!("unknown fuse {name}"))?;
    let on = match value {
        "on" | "1" | "true" => true,
        "off" | "0" | "false" => false,
        _ => return Err(format!("--fuse {arg}: expected on or off")),
    };
    Ok((fuse, on))
}

fn print_fuses(f: &fuses::Fuses) {
    for (fuse, name) in fuses::FUSES {
        println!("{name}: {}", if f.is_on(fuse) { "on" } else { "off" });
    }
    println!("key: {}", if f.key.is_some() { "embedded" } else { "none" });
    match f.integrity {
        Some(h) => println!("integrity: {}", crypto::to_hex(&h)),
        None => println!("integrity: none"),
    }
}

fn patch(exe: &PathBuf, update: impl FnOnce(&mut fuses::Fuses)) -> Result<fuses::Fuses, String> {
    let mut binary = std::fs::read(exe).map_err(|e| format!("{}: {e}", exe.display()))?;
    let mut f = fuses::read(&binary).map_err(|e| format!("{}: {e}", exe.display()))?;
    update(&mut f);
    fuses::write(&mut binary, &f)?;
    // keep the inode (and its permissions): write in place
    std::fs::write(exe, &binary).map_err(|e| format!("{}: {e}", exe.display()))?;
    Ok(f)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let (mut input, mut output, mut exe, mut key_hex, mut write_key) =
        (None, None, None, None, None);
    let (mut unpack, mut set, mut seal, mut default_unpack) = (Vec::new(), Vec::new(), false, true);
    let mut positional = Vec::new();
    let mut it = args[1..].iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().cloned();
        match a.as_str() {
            "--input" => input = value().map(PathBuf::from),
            "--output" => output = value().map(PathBuf::from),
            "--exe" => exe = value().map(PathBuf::from),
            "--unpack" => unpack.extend(value()),
            "--no-default-unpack" => default_unpack = false,
            "--seal" => seal = true,
            "--key-hex" => key_hex = value(),
            "--write-key" => write_key = value().map(PathBuf::from),
            "--fuse" => match value().as_deref().map(parse_fuse) {
                Some(Ok(f)) => set.push(f),
                Some(Err(e)) => return fail(e),
                None => return fail("--fuse needs a value"),
            },
            "-h" | "--help" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            other if other.starts_with("--") => return fail(format!("unknown option {other}")),
            other => positional.push(PathBuf::from(other)),
        }
    }
    let key = match key_hex.as_deref().map(crypto::parse_hex_key) {
        Some(None) => return fail("--key-hex must be 64 hex characters (32 bytes)"),
        Some(Some(k)) => Some(k),
        None => None,
    };

    match command.as_str() {
        "pack" => {
            let (Some(input), Some(output)) = (input, output) else {
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            };
            if !input.is_dir() {
                return fail(format!("{} is not a directory", input.display()));
            }
            let seal = seal.then(|| match (&exe, key) {
                (_, Some(k)) if exe.is_some() => (KeyMode::Embedded, k),
                (Some(_), _) => (KeyMode::Embedded, crypto::random::<32>()),
                // nowhere to embed a key
                (None, _) => (KeyMode::Default, crypto::default_key()),
            });
            if seal.is_some_and(|s| s.0 == KeyMode::Default) {
                eprintln!("ns-bundle: warning: no --exe, sealing with the shared default key");
            }
            if let (Some(path), Some((KeyMode::Embedded, k))) = (&write_key, &seal) {
                if let Err(e) = std::fs::write(path, crypto::to_hex(k)) {
                    return fail(format!("{}: {e}", path.display()));
                }
            }
            if default_unpack {
                unpack.extend(pack::DEFAULT_UNPACK.iter().map(|s| s.to_string()));
            }
            let options = pack::PackOptions { unpack, seal };
            let report = match pack::pack_dir(&input, &output, &options) {
                Ok(r) => r,
                Err(e) => return fail(format!("packing {}: {e}", input.display())),
            };
            println!(
                "{} -> {} ({} files, {} unpacked{})",
                input.display(),
                output.display(),
                report.entries,
                report.unpacked,
                if report.sealed { ", sealed" } else { "" }
            );
            if let Some(exe) = &exe {
                let result = patch(exe, |f| {
                    f.integrity = Some(report.toc_hash);
                    f.key = options
                        .seal
                        .as_ref()
                        .filter(|s| s.0 == KeyMode::Embedded)
                        .map(|s| s.1);
                    for (fuse, on) in &set {
                        f.set(*fuse, *on);
                    }
                });
                match result {
                    Ok(f) => print_fuses(&f),
                    Err(e) => return fail(e),
                }
            }
            ExitCode::SUCCESS
        }
        "fuses" => {
            let Some(exe) = positional.first() else {
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            };
            match patch(exe, |f| {
                for (fuse, on) in &set {
                    f.set(*fuse, *on);
                }
            }) {
                Ok(f) => print_fuses(&f),
                Err(e) => return fail(e),
            }
            ExitCode::SUCCESS
        }
        "list" => {
            let Some(path) = positional.first() else {
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            };
            // the key can come from the patched runtime
            let key = key.or_else(|| {
                let binary = std::fs::read(exe.as_ref()?).ok()?;
                fuses::read(&binary).ok()?.key
            });
            let c = match Container::open(path, key, false) {
                Ok(c) => c,
                Err(e) => return fail(e),
            };
            println!("integrity: {}", crypto::to_hex(&c.toc_hash()));
            for i in 0..c.len() {
                let e = c.entry(i);
                let kind = match e.kind {
                    Kind::Stored => "stored",
                    Kind::Sealed => "sealed",
                    Kind::Unpacked => "unpacked",
                };
                println!("{:>10} {kind:8} {}", e.size, c.name(i));
            }
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}
