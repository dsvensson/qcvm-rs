// SPDX-License-Identifier: MIT OR Apache-2.0

//! Tests of the string builtins (see `src/stdlib`: convert, format, string, charset, tokenize,
//! digest, strftime), following the examples of `docs/spec/strings.md` with the fixes listed in
//! `docs/spec/deviations.md`.

use qcvm::vm::{CharScheme, Charset};
use qcvm::{Arg, Numbering, StrRef, VmConfig};

use crate::support::harness::Harness;

mod charset;
mod convert;
mod digest;
mod format;
mod strftime;
mod string;
mod tokenize;

/// Builtins FTE resolves by name that its CSQC declarations dump does not list.
const EXTRA: &[&str] = &["argc", "instr", "ftou", "utof", "strcmp"];

/// A CSQC harness with QuakeWorld's charset defaults (`utf8_enable 0`, Quake scheme).
fn harness() -> Harness {
    with_charset(false, CharScheme::Quake)
}

/// A CSQC harness with the given charset settings.
fn with_charset(utf8: bool, scheme: CharScheme) -> Harness {
    let config = VmConfig { charset: Charset { utf8, scheme }, ..VmConfig::default() };
    Harness::with(Numbering::Csqc, config, Harness::named(EXTRA))
}

/// A menu QuakeC harness (for the menu-only builtins).
fn menu() -> Harness {
    Harness::with(Numbering::Menu, VmConfig::menu(), |_| {})
}

/// Calls a builtin returning a string: `None` for a null reference.
fn opt_s(h: &mut Harness, name: &str, args: &[Arg<'_>]) -> Option<Vec<u8>> {
    let r = h.call(name, args).unwrap_or_else(|e| panic!("{name}: {e}")).str_ref();
    (r != StrRef(0)).then(|| h.vm.str(r).to_vec())
}

/// Asserts byte-string equality with a readable failure message.
#[track_caller]
fn eq(got: impl AsRef<[u8]>, want: impl AsRef<[u8]>) {
    let (got, want) = (got.as_ref(), want.as_ref());
    assert!(got == want, "got b\"{}\", want b\"{}\"", got.escape_ascii(), want.escape_ascii());
}

/// Number of warnings the host received.
fn warnings(h: &Harness) -> usize {
    h.host.warnings.len()
}
