// SPDX-License-Identifier: MIT OR Apache-2.0

//! Frames of KTX's weapon-prediction `csprogs.dat` (located through `QCVM_CSPROGS`) against the
//! stub engine of the csprogs tests: the interpreter on a real workload, builtins, re-entrant
//! predraw calls and temp strings included. Skipped when `QCVM_CSPROGS` is unset.

#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::panic
)]

#[path = "../tests/all/support/csqc_engine.rs"]
#[allow(dead_code, unreachable_pub)]
mod csqc_engine;

use std::path::PathBuf;

use criterion::{Criterion, criterion_group, criterion_main};
use csqc_engine::{Client, Engine, builtins, load_program};

fn benches(c: &mut Criterion) {
    let Some(path) = std::env::var_os("QCVM_CSPROGS").map(PathBuf::from) else {
        eprintln!("csprogs benchmark skipped: set QCVM_CSPROGS to KTX's csprogs.dat");
        return;
    };
    let mut client = Client::new(load_program(&path), builtins(), Engine::new());
    // Past the first weapon snapshot, so prediction, local rockets and the view weapon are live.
    for _ in 0..60 {
        client.step(true);
    }
    c.bench_function("csprogs_frame", |b| {
        b.iter(|| {
            client.step(true);
            // Keep the workload steady: the server never runs out of rockets, and the stub
            // engine's records of past frames don't pile up.
            client.server.shots = 0;
            let frame = client.frame;
            client.host.inputs.retain(|&f, _| f + 64 >= frame);
            client.host.log.clear();
            client.host.rendered.clear();
            client.host.sounds.clear();
        });
    });
}

criterion_group!(csprogs_benches, benches);
criterion_main!(csprogs_benches);
