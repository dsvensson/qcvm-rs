// SPDX-License-Identifier: MIT OR Apache-2.0

//! Character decoding and encoding for the Quake, UTF-8 and ISO-8859-1 schemes (docs/spec/strings.md).

use crate::builtins::Builtins;
use crate::host::Host;

/// Registers this module's builtins.
pub(crate) fn register<H: Host>(b: &mut Builtins<H>) {
    let _ = b;
}
