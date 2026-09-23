// SPDX-License-Identifier: MIT OR Apache-2.0

//! Number/string conversions: ftos, vtos, etos, itos, htos, stoi, stoh, ftoi, itof, stof, stov (docs/spec/strings.md B.1).

use crate::builtins::Builtins;
use crate::host::Host;

/// Registers this module's builtins.
pub(crate) fn register<H: Host>(b: &mut Builtins<H>) {
    let _ = b;
}
