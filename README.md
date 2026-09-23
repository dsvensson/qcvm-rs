<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# qcvm

A re-entrant, reusable QuakeC virtual machine for Rust.

- Runs progs compiled by `fteqcc` for any FTE target: version 6, FTE version 7 (16- and 32-bit
  records), KK7, uHexen2 and QTest formats, with **every opcode FTE executes**.
- Host-registered builtins, re-entrant calls (builtins can call back into QuakeC), multiple
  progs per VM, and QuakeC threads (`fork`/`sleep`).
- Ships every builtin that does not need an engine: maths and vectors, strings and formatting
  (`sprintf`, `ftos`, tokenizers, info strings, …), entity search, hash tables, string buffers,
  VM memory, JSON, digests, and more — with FTE's builtin numbering for CSQC, SSQC and menu progs.
- Built for untrusted, server-supplied progs: `#![forbid(unsafe_code)]`, no panics on malformed
  input, hard resource limits.

The design lives in [docs/design.md](docs/design.md); the behaviour it implements is specified in
[docs/spec](docs/spec), including a list of [deliberate deviations](docs/spec/deviations.md) from
FTE.

## Testing

`cargo test` runs the self-contained suite. Some tests need external tools and are skipped (with a
notice) when those are missing:

| Variable | Used for |
|---|---|
| `FTEQCC` | QuakeC compiler for the `.qc` fixtures, when neither `fteqcc64` nor `fteqcc` is on `PATH` |
| `FTE_QCVM` | FTE's standalone `qcvm` runner, used as a black-box oracle for differential tests |
| `QCVM_CSPROGS` | path to a KTX `csprogs.dat` for the CSQC integration test |

`scripts/build-fte-tools.ps1` builds `fteqcc` and `qcvm` from an FTE checkout. They are GPL
programs and are only ever *run* by the tests — never linked or redistributed.

## Credits

qcvm started as a port of [QCVM](https://github.com/erysdren/QCVM) by erysdren (MIT). FTEQW's
behaviour served as the specification for the extended instruction set and builtins; no FTE code
is used.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in
the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any
additional terms or conditions.
