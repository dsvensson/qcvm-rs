// SPDX-License-Identifier: MIT OR Apache-2.0

//! sprintf and the C printf engine it is built on (docs/spec/strings.md).

use crate::builtins::Builtins;
use crate::host::Host;

/// Registers this module's builtins.
pub(crate) fn register<H: Host>(b: &mut Builtins<H>) {
    let _ = b;
}
