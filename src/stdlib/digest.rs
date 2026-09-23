// SPDX-License-Identifier: MIT OR Apache-2.0

//! crc16, digest_hex and digest_ptr (docs/spec/strings.md).

use crate::builtins::Builtins;
use crate::host::Host;

/// Registers this module's builtins.
pub(crate) fn register<H: Host>(b: &mut Builtins<H>) {
    let _ = b;
}
