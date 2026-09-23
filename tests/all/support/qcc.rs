// SPDX-License-Identifier: MIT OR Apache-2.0

//! Compiling QuakeC test fixtures with fteqcc at test time.
//!
//! Compiled programs are cached under `CARGO_TARGET_TMPDIR`, keyed by the source, the compiler
//! flags and the compiler binary, so repeated test runs do not recompile.

use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::support::tools;

/// A compiled fixture.
pub struct Compiled {
    pub dat: Vec<u8>,
    pub path: PathBuf,
}

/// The fixture source directory.
pub fn fixture_dir() -> PathBuf {
    tools::repo_root().join("tests").join("qc")
}

/// Compiles `sources` (fixture file names, compiled in order) with `flags`. Returns `None` (and
/// prints a notice) if no compiler is available; panics with the compiler output on errors.
pub fn compile(sources: &[&str], flags: &[&str]) -> Option<Compiled> {
    let Some(compiler) = tools::fteqcc() else {
        tools::skip(sources.first().copied().unwrap_or("fixture"), "fteqcc");
        return None;
    };
    let mut hasher = DefaultHasher::new();
    for name in sources {
        name.hash(&mut hasher);
        fs::read(fixture_dir().join(name)).unwrap().hash(&mut hasher);
    }
    flags.hash(&mut hasher);
    compiler.hash(&mut hasher);
    if let Ok(meta) = fs::metadata(&compiler) {
        meta.len().hash(&mut hasher);
        meta.modified().ok().hash(&mut hasher);
    }
    let dir =
        Path::new(env!("CARGO_TARGET_TMPDIR")).join("qc").join(format!("{:016x}", hasher.finish()));
    let dat = dir.join("out.dat");
    if let Ok(bytes) = fs::read(&dat) {
        return Some(Compiled { dat: bytes, path: dat });
    }
    fs::create_dir_all(&dir).unwrap();
    let mut src = String::from("out.dat\n");
    for name in sources {
        let text = fs::read(fixture_dir().join(name)).unwrap();
        fs::write(dir.join(name), text).unwrap();
        src.push_str(name);
        src.push('\n');
    }
    fs::write(dir.join("progs.src"), src).unwrap();
    let output = Command::new(&compiler)
        .current_dir(&dir)
        .arg("-srcfile")
        .arg("progs.src")
        .args(flags)
        .output()
        .unwrap();
    let log = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let bytes = fs::read(&dat)
        .unwrap_or_else(|_| panic!("fteqcc failed for {sources:?} {flags:?}:\n{log}"));
    Some(Compiled { dat: bytes, path: dat })
}
