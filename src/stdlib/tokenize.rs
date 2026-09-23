// SPDX-License-Identifier: MIT OR Apache-2.0

//! tokenize, tokenize_console, tokenizebyseparator, argv and friends (docs/spec/strings.md).

use crate::builtins::Builtins;
use crate::host::Host;

/// Registers this module's builtins.
pub(crate) fn register<H: Host>(b: &mut Builtins<H>) {
    let _ = b;
}

/// The current token list (per VM; FTE shares one per process).
#[derive(Clone, Debug, Default)]
pub(crate) struct Tokens {}
