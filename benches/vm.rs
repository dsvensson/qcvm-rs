// SPDX-License-Identifier: MIT OR Apache-2.0

//! Interpreter benchmarks on assembled programs.

#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::panic
)]

#[path = "../tests/all/support/asm.rs"]
#[allow(dead_code, unreachable_pub)]
mod asm;

use std::hint::black_box;
use std::sync::Arc;

use asm::{Asm, OFS_RETURN, parm, ty};
use criterion::{Criterion, criterion_group, criterion_main};
use qcvm::{Arg, Builtins, Host, Numbering, Op, Program, ProgsFormat, Vm, VmConfig, VmError};

#[derive(Default)]
struct BenchHost;
impl Host for BenchHost {}

fn vm(asm: &Asm, builtins: Builtins<BenchHost>) -> Vm<BenchHost> {
    let program = Arc::new(Program::parse(&asm.build(ProgsFormat::Fte16)).unwrap());
    Vm::new(program, Arc::new(builtins), VmConfig::default()).unwrap()
}

/// `for (i = 0; i < n; i++) total += i * 0.5;`
fn float_loop() -> Asm {
    let mut asm = Asm::new();
    let (zero, one, half) = (asm.float(0.0), asm.float(1.0), asm.float(0.5));
    let f = asm.function("main", &[1], 4);
    let (n, i, total, t) = (f.local(0), f.local(1), f.local(2), f.local(3));
    asm.emit(Op::StoreF, zero, i, 0);
    asm.emit(Op::StoreF, zero, total, 0);
    let top = asm.here();
    asm.emit(Op::LtF, i, n, t);
    let exit = asm.emit(Op::IfNotI, t, 0, 0);
    asm.emit(Op::MulF, i, half, t);
    asm.emit(Op::AddF, total, t, total);
    asm.emit(Op::AddF, i, one, i);
    let back = asm.emit(Op::Goto, 0, 0, 0);
    asm.patch_jump(back, 0, top);
    let end = asm.emit(Op::Return, total, 0, 0);
    asm.patch_jump(exit, 1, end);
    asm
}

/// Recursive Fibonacci, as fteqcc compiles it (Hexen 2 style calls).
fn fib() -> Asm {
    let mut asm = Asm::new();
    let (one, two) = (asm.float(1.0), asm.float(2.0));
    let fib_g = asm.global("fib_g", ty::FUNCTION, &[0]);
    let f = asm.function("main", &[1], 2);
    let (n, saved, t) = (f.local(0), f.local(1), f.local(2));
    asm.emit(Op::LtF, n, two, t);
    let br = asm.emit(Op::IfNotI, t, 0, 0);
    asm.emit(Op::Return, n, 0, 0);
    let rec = asm.here();
    asm.patch_jump(br, 1, rec);
    asm.emit(Op::SubF, n, one, t);
    asm.emit(Op::Call1H, fib_g, t, 0);
    asm.emit(Op::SubF, n, two, t);
    asm.emit(Op::StoreF, OFS_RETURN, saved, 0);
    asm.emit(Op::Call1H, fib_g, t, 0);
    asm.emit(Op::AddF, saved, OFS_RETURN, t);
    asm.emit(Op::Return, t, 0, 0);
    asm.set_global(fib_g, f.index);
    asm
}

/// How an entity-field loop writes `e.health`.
#[derive(Clone, Copy)]
enum FieldStore {
    /// `e.health = e.health + e.armor`: LOAD_F, LOAD_F, ADD_F, STOREF_F (what fteqcc emits).
    Direct,
    /// The same through ADDRESS and STOREP_F.
    Pointer,
    /// `e.health += e.armor`: LOAD_F, ADDRESS, ADDSTOREP_F.
    Compound,
}

/// Updates an entity field in a loop.
fn fields(store: FieldStore) -> Asm {
    let mut asm = Asm::new();
    let (_, health) = asm.field("health", ty::FLOAT);
    let (_, armor) = asm.field("armor", ty::FLOAT);
    let (zero, one) = (asm.float(0.0), asm.float(1.0));
    let f = asm.function("main", &[1, 1], 5);
    let (e, n, i, t, p) = (f.local(0), f.local(1), f.local(2), f.local(3), f.local(4));
    asm.emit(Op::StoreF, zero, i, 0);
    let top = asm.here();
    asm.emit(Op::LtF, i, n, t);
    let exit = asm.emit(Op::IfNotI, t, 0, 0);
    match store {
        FieldStore::Direct => {
            asm.emit(Op::LoadF, e, health, t);
            asm.emit(Op::LoadF, e, armor, p);
            asm.emit(Op::AddF, t, p, t);
            asm.emit(Op::StoreFieldF, e, health, t);
        }
        FieldStore::Pointer => {
            asm.emit(Op::LoadF, e, health, t);
            asm.emit(Op::LoadF, e, armor, p);
            asm.emit(Op::AddF, t, p, t);
            asm.emit(Op::Address, e, health, p);
            asm.emit(Op::StorePF, t, p, 0);
        }
        FieldStore::Compound => {
            asm.emit(Op::LoadF, e, armor, t);
            asm.emit(Op::Address, e, health, p);
            asm.emit(Op::AddStorePF, t, p, t);
        }
    }
    asm.emit(Op::AddF, i, one, i);
    let back = asm.emit(Op::Goto, 0, 0, 0);
    asm.patch_jump(back, 0, top);
    let end = asm.emit(Op::Done, 0, 0, 0);
    asm.patch_jump(exit, 1, end);
    asm
}

/// Calls a trivial builtin in a loop.
fn builtin_loop() -> Asm {
    let mut asm = Asm::new();
    let b = asm.builtin("nop", 1, 1);
    let bg = asm.global("nop_g", ty::FUNCTION, &[b]);
    let (zero, one) = (asm.float(0.0), asm.float(1.0));
    let f = asm.function("main", &[1], 2);
    let (n, i, t) = (f.local(0), f.local(1), f.local(2));
    asm.emit(Op::StoreF, zero, i, 0);
    let top = asm.here();
    asm.emit(Op::LtF, i, n, t);
    let exit = asm.emit(Op::IfNotI, t, 0, 0);
    asm.emit(Op::StoreF, i, parm(0), 0);
    asm.emit(Op::Call1, bg, 0, 0);
    asm.emit(Op::AddF, i, one, i);
    let back = asm.emit(Op::Goto, 0, 0, 0);
    asm.patch_jump(back, 0, top);
    let end = asm.emit(Op::Done, 0, 0, 0);
    asm.patch_jump(exit, 1, end);
    asm
}

/// Vector maths in a loop: `v = v * 0.5 + w; d += v * w`.
fn vectors() -> Asm {
    let mut asm = Asm::new();
    let (zero, one, half) = (asm.float(0.0), asm.float(1.0), asm.float(0.5));
    let w = asm.vector([1.0, 2.0, 3.0]);
    let f = asm.function("main", &[1], 8);
    let (n, i, t, d) = (f.local(0), f.local(1), f.local(2), f.local(3));
    let v = f.local(4);
    asm.emit(Op::StoreF, zero, i, 0);
    let top = asm.here();
    asm.emit(Op::LtF, i, n, t);
    let exit = asm.emit(Op::IfNotI, t, 0, 0);
    asm.emit(Op::MulVF, v, half, v);
    asm.emit(Op::AddV, v, w, v);
    asm.emit(Op::MulV, v, w, t);
    asm.emit(Op::AddF, d, t, d);
    asm.emit(Op::AddF, i, one, i);
    let back = asm.emit(Op::Goto, 0, 0, 0);
    asm.patch_jump(back, 0, top);
    let end = asm.emit(Op::Return, d, 0, 0);
    asm.patch_jump(exit, 1, end);
    asm
}

/// Struct-array access through pointers: sums `arr[i]` via GLOBALADDRESS + LOADP.
fn pointers() -> Asm {
    let mut asm = Asm::new();
    let arr = asm.alloc(64, &(0..64).map(|x| (x as f32).to_bits()).collect::<Vec<_>>());
    let (zero, one, mask) = (asm.int(0), asm.int(1), asm.int(63));
    let f = asm.function("main", &[1], 5);
    let (n, i, t, s, p) = (f.local(0), f.local(1), f.local(2), f.local(3), f.local(4));
    asm.emit(Op::StoreI, zero, i, 0);
    let top = asm.here();
    asm.emit(Op::LtI, i, n, t);
    let exit = asm.emit(Op::IfNotI, t, 0, 0);
    asm.emit(Op::BitAndI, i, mask, t);
    asm.emit(Op::GlobalAddress, arr, t, p);
    asm.emit(Op::LoadPF, p, zero, t);
    asm.emit(Op::AddF, s, t, s);
    asm.emit(Op::AddI, i, one, i);
    let back = asm.emit(Op::Goto, 0, 0, 0);
    asm.patch_jump(back, 0, top);
    let end = asm.emit(Op::Return, s, 0, 0);
    asm.patch_jump(exit, 1, end);
    asm
}

fn nop(_vm: &mut Vm<BenchHost>, _h: &mut BenchHost) -> Result<(), VmError> {
    Ok(())
}

fn temp(vm: &mut Vm<BenchHost>, _h: &mut BenchHost) -> Result<(), VmError> {
    vm.ret_str(b"a temp string")
}

fn run(c: &mut Criterion, name: &str, asm: &Asm, builtins: Builtins<BenchHost>, args: &[Arg<'_>]) {
    let mut vm = vm(asm, builtins);
    let mut host = BenchHost;
    let main = vm.find_function("main").unwrap();
    c.bench_function(name, |b| b.iter(|| black_box(vm.call(&mut host, main, args).unwrap())));
}

/// Checks that the assembled programs compute what they should, so a benchmark never times a
/// broken program.
fn sanity() {
    let none = || Builtins::empty(Numbering::None);
    let mut host = BenchHost;
    let mut fib_vm = vm(&fib(), none());
    let main = fib_vm.find_function("main").unwrap();
    assert_eq!(fib_vm.call(&mut host, main, &[Arg::Float(20.0)]).unwrap().f32(), 6765.0);
    for store in [FieldStore::Direct, FieldStore::Pointer, FieldStore::Compound] {
        let mut vm = vm(&fields(store), none());
        let e = vm.spawn().unwrap();
        let armor = vm.field::<f32>("armor").unwrap();
        vm.set_field(e, armor, 2.0);
        let main = vm.find_function("main").unwrap();
        vm.call(&mut host, main, &[Arg::Ent(e), Arg::Float(10.0)]).unwrap();
        let health = vm.field::<f32>("health").unwrap();
        assert_eq!(vm.get_field(e, health), Some(20.0));
    }
}

fn benches(c: &mut Criterion) {
    sanity();
    let none = || Builtins::empty(Numbering::None);
    run(c, "float_loop_10k", &float_loop(), none(), &[Arg::Float(10_000.0)]);
    run(c, "fib_20", &fib(), none(), &[Arg::Float(20.0)]);
    for (name, store) in [
        ("fields_10k", FieldStore::Direct),
        ("fields_ptr_10k", FieldStore::Pointer),
        ("fields_add_10k", FieldStore::Compound),
    ] {
        let asm = fields(store);
        let mut vm = vm(&asm, none());
        let e = vm.spawn().unwrap();
        let mut host = BenchHost;
        let main = vm.find_function("main").unwrap();
        c.bench_function(name, |b| {
            b.iter(|| {
                black_box(vm.call(&mut host, main, &[Arg::Ent(e), Arg::Float(10_000.0)]).unwrap())
            })
        });
    }
    let mut b = none();
    b.set_numbered(1, "nop", nop);
    run(c, "builtin_10k", &builtin_loop(), b, &[Arg::Float(10_000.0)]);
    run(c, "vectors_10k", &vectors(), none(), &[Arg::Float(10_000.0)]);
    run(c, "pointers_10k", &pointers(), none(), &[Arg::Int(10_000)]);
    let mut b = none();
    b.set_numbered(1, "nop", temp);
    run(c, "temp_strings_10k", &builtin_loop(), b, &[Arg::Float(10_000.0)]);
}

criterion_group!(vm_benches, benches);
criterion_main!(vm_benches);
