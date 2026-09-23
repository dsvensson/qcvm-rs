// SPDX-License-Identifier: MIT OR Apache-2.0

//! The integration test-suite, built as a single binary (linking many test binaries is slow on
//! Windows).

#![allow(
    missing_docs,
    unreachable_pub,
    clippy::arithmetic_side_effects,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used
)]

mod support;

mod builtins_misc;
mod builtins_strings;
mod differential;
mod license;
mod loader;
mod opcodes;
mod vm_basic;
