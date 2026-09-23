// SPDX-License-Identifier: MIT OR Apache-2.0

//! Discovery of the optional external tools some tests use.
//!
//! Nothing is hard-coded: the QuakeC compiler is looked up on `PATH` (`fteqcc64`, then `fteqcc`)
//! and otherwise taken from the `FTEQCC` environment variable. FTE's standalone runner comes from
//! `FTE_QCVM`, and a KTX `csprogs.dat` from `QCVM_CSPROGS`. Tests that need a missing tool print a
//! notice and return early instead of failing.

use std::env;
use std::path::{Path, PathBuf};

/// Finds an executable called `name` on `PATH`, trying the platform's executable suffix too.
fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path).find_map(|dir| {
        [name.to_owned(), format!("{name}{}", env::consts::EXE_SUFFIX)]
            .into_iter()
            .map(|file| dir.join(file))
            .find(|candidate| candidate.is_file())
    })
}

/// Reads an environment variable naming an existing file.
fn file_from_env(var: &str) -> Option<PathBuf> {
    let value = env::var_os(var)?;
    let path = PathBuf::from(value);
    path.is_file().then_some(path)
}

/// The QuakeC compiler: `fteqcc64`/`fteqcc` on `PATH`, else `$FTEQCC`.
pub fn fteqcc() -> Option<PathBuf> {
    find_on_path("fteqcc64").or_else(|| find_on_path("fteqcc")).or_else(|| file_from_env("FTEQCC"))
}

/// FTE's standalone `qcvm` runner (`$FTE_QCVM`), used as a black-box oracle.
pub fn fte_qcvm() -> Option<PathBuf> {
    file_from_env("FTE_QCVM")
}

/// A KTX `csprogs.dat` (`$QCVM_CSPROGS`).
pub fn csprogs() -> Option<PathBuf> {
    file_from_env("QCVM_CSPROGS")
}

/// Prints the standard "skipped" notice for a test that needs an unavailable tool.
pub fn skip(test: &str, what: &str) {
    eprintln!("note: skipping {test}: {what} not available (see README.md, Testing)");
}

/// The repository root.
pub fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn report_tool_discovery() {
    let show =
        |p: Option<PathBuf>| p.map_or_else(|| "not found".to_owned(), |p| p.display().to_string());
    eprintln!("fteqcc:   {}", show(fteqcc()));
    eprintln!("FTE qcvm: {}", show(fte_qcvm()));
    eprintln!("csprogs:  {}", show(csprogs()));
    if fteqcc().is_none() {
        skip("report_tool_discovery", "fteqcc");
    }
    assert!(repo_root().join("Cargo.toml").is_file());
}
