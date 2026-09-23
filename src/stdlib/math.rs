// SPDX-License-Identifier: MIT OR Apache-2.0

//! Scalar maths and random numbers (docs/spec/builtins.md).

use crate::builtins::Builtins;
use crate::host::Host;

/// Registers this module's builtins.
pub(crate) fn register<H: Host>(b: &mut Builtins<H>) {
    let _ = b;
}
