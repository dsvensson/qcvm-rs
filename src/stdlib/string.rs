// SPDX-License-Identifier: MIT OR Apache-2.0

//! String builtins: strlen, strcat, substring, strconv, strpad, strreplace, strncmp, info strings, uri escaping, decolorizing, … (docs/spec/strings.md B.3–B.9).

use crate::builtins::Builtins;
use crate::host::Host;

/// Registers this module's builtins.
pub(crate) fn register<H: Host>(b: &mut Builtins<H>) {
    let _ = b;
}
