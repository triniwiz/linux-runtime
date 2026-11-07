//! `ns-metadata-filter <in.bin> <out.bin> [--whitelist <file>] [--blacklist <file>]`

use nativescript_metadata::filter::{filter, Filter};
use nativescript_metadata::MetaFile;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let (mut paths, mut whitelist, mut blacklist) = (Vec::new(), None, None);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--whitelist" => whitelist = args.next(),
            "--blacklist" => blacklist = args.next(),
            _ => paths.push(arg),
        }
    }
    let [input, output] = paths.as_slice() else {
        eprintln!("usage: ns-metadata-filter <in.bin> <out.bin> [--whitelist <file>] [--blacklist <file>]");
        return ExitCode::from(2);
    };
    let read = |p: Option<String>| -> Result<Option<String>, String> {
        p.map(|p| std::fs::read_to_string(&p).map_err(|e| format!("{p}: {e}")))
            .transpose()
    };
    let result = (|| -> Result<String, String> {
        let file = MetaFile::open(input).map_err(|e| format!("{input}: {e}"))?;
        let rules = Filter::new(read(whitelist)?.as_deref(), read(blacklist)?.as_deref());
        let (bytes, stats) = filter(&file, &rules);
        std::fs::write(output, &bytes).map_err(|e| format!("{output}: {e}"))?;
        Ok(format!(
            "{} of {} declarations ({} referenced records), {} -> {} bytes",
            stats.kept,
            stats.declarations,
            stats.dependencies,
            file.bytes().len(),
            bytes.len()
        ))
    })();
    match result {
        Ok(summary) => {
            println!("{summary}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("ns-metadata-filter: {e}");
            ExitCode::FAILURE
        }
    }
}
