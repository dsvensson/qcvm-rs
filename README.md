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

## Usage

```rust
use std::sync::Arc;

use qcvm::{Arg, Builtins, Host, Numbering, Program, Vm, VmConfig, VmError};

/// The engine: builtins get it as `&mut`, and it receives prints, warnings, cvar lookups, ….
#[derive(Default)]
struct Client {
    frames: u32,
}

impl Host for Client {
    fn print(&mut self, text: &[u8]) {
        print!("{}", String::from_utf8_lossy(text));
    }
}

/// An engine builtin, declared in QuakeC as `float() framecount = #500;`.
fn framecount(vm: &mut Vm<Client>, host: &mut Client) -> Result<(), VmError> {
    vm.ret_f32(host.frames as f32);
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Programs are immutable and can be shared between VMs.
    let program = Arc::new(Program::parse(&std::fs::read("csprogs.dat")?)?);
    let mut builtins = Builtins::standard(Numbering::Csqc);
    builtins.set_numbered(500, "framecount", framecount);
    let mut vm = Vm::new(program, Arc::new(builtins), VmConfig::csqc())?;
    let mut client = Client::default();

    let time = vm.global::<f32>("time")?;
    vm.set(time, 1.5);
    let init = vm.find_function("CSQC_Init").ok_or("no CSQC_Init")?;
    vm.call(&mut client, init, &[Arg::Float(0.0), Arg::Bytes(b"qualia"), Arg::Float(1.0)])?;
    Ok(())
}
```

Builtins may call back into QuakeC (`vm.call`), which is how engines implement things like the
CSQC `addentities` walk ([`qcvm::csqc::add_entities`](src/csqc.rs)). Further progs are loaded into
a running VM with `Vm::add_progs` (FTE's multiprogs: shared fields and globals, cross-progs calls
and extern linking), and threads suspended by `sleep`/`fork` resume from `Vm::run_threads`.

The design lives in [docs/design.md](docs/design.md); the behaviour it implements is specified in
[docs/spec](docs/spec), including a list of [deliberate deviations](docs/spec/deviations.md) from
FTE.

## Cargo features

| Feature | Default | Enables |
|---|---|---|
| `digests` | yes | MD4, MD5, SHA-1 and SHA-2 for `digest_hex`/`digest_ptr` (RustCrypto crates) |
| `json` | yes | the `json_*` builtins (strict JSON, parsed with `serde_json`) |

## Testing

`cargo test` runs the self-contained suite. Some tests need external tools and are skipped (with a
notice) when those are missing:

| Variable | Used for |
|---|---|
| `FTEQCC` | QuakeC compiler for the `.qc` fixtures, when neither `fteqcc64` nor `fteqcc` is on `PATH` |
| `FTE_QCVM` | FTE's standalone `qcvm` runner, used as a black-box oracle for differential tests |
| `QCVM_CSPROGS` | path to a KTX `csprogs.dat` for the CSQC integration test and benchmark |
| `QCVM_FUZZ_CASES` | number of random programs the execution fuzz test runs (default 256) |

`cargo bench` runs the interpreter benchmarks, and `scripts/bench-fte.ps1` times qcvm against
FTE's runner. The interpreter is much slower unoptimised; a project depending on qcvm can keep it
fast in debug builds with

```toml
[profile.dev.package.qcvm]
opt-level = 3
```

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
