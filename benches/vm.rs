// SPDX-License-Identifier: MIT OR Apache-2.0

//! Interpreter benchmarks (filled in with the interpreter milestone).

#![allow(missing_docs)]

use criterion::{Criterion, criterion_group, criterion_main};

fn placeholder(c: &mut Criterion) {
    c.bench_function("noop", |b| b.iter(|| std::hint::black_box(0u32)));
}

criterion_group!(benches, placeholder);
criterion_main!(benches);
