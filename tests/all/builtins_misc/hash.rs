// SPDX-License-Identifier: MIT OR Apache-2.0

//! Hash tables.

use std::collections::BTreeSet;

use qcvm::{ErrorKind, Numbering, Op, StrRef, Vec3, VmConfig};

use super::{w, warnings};
use crate::support::asm::{Asm, parm, ty};
use crate::support::harness::{Harness, f, s, v};

const EV_STRING: f32 = 1.0;
const EV_FLOAT: f32 = 2.0;
const EV_VECTOR: f32 = 3.0;
const HASH_REPLACE: f32 = 256.0;
const HASH_ADD: f32 = 512.0;

/// Adds `cb(string key, vector value)`, which prints `key` and a newline, counts its calls in
/// `cb_count` and keeps the last value in `cb_value`.
fn setup(asm: &mut Asm) {
    // Declared by number: FTE numbers hash_getcb #293 although its extension dump does not.
    asm.builtin("hash_getcb", 293, 3);
    let print = asm.builtin("print", 339, 8);
    let print_g = asm.global("print_fn", ty::FUNCTION, &[print]);
    let count = asm.global("cb_count", ty::FLOAT, &[]);
    let value = asm.global("cb_value", ty::VECTOR, &[]);
    let one = asm.float(1.0);
    let newline = asm.str_const("\n");
    let cb = asm.function("cb", &[1, 3], 0);
    asm.emit(Op::AddF, count, one, count);
    asm.emit(Op::StoreV, cb.local(1), value, 0);
    asm.emit(Op::StoreS, cb.local(0), parm(0), 0);
    asm.emit(Op::StoreS, newline, parm(1), 0);
    asm.emit(Op::Call2, print_g, 0, 0);
    asm.emit(Op::Done, 0, 0, 0);
}

fn h() -> Harness {
    Harness::with(Numbering::Csqc, VmConfig::default(), setup)
}

fn create(h: &mut Harness, args: &[qcvm::Arg<'_>]) -> f32 {
    h.f("hash_createtab", args)
}

#[test]
fn tables_are_numbered_from_one_and_reused() {
    let mut h = h();
    assert_eq!(create(&mut h, &[f(16.0)]), 1.0);
    assert_eq!(create(&mut h, &[f(0.0)]), 2.0);
    h.call("hash_destroytab", &[f(1.0)]).unwrap();
    assert_eq!(create(&mut h, &[f(1e9)]), 1.0);
    assert_eq!(create(&mut h, &[f(4.0)]), 3.0);
}

#[test]
fn the_table_limit_is_enforced() {
    let config = VmConfig {
        limits: qcvm::Limits { hash_tables: 2, ..qcvm::Limits::default() },
        ..VmConfig::default()
    };
    let mut h = Harness::with(Numbering::Csqc, config, setup);
    assert_eq!(create(&mut h, &[f(8.0)]), 1.0);
    assert_eq!(create(&mut h, &[f(8.0)]), 2.0);
    assert_eq!(create(&mut h, &[f(8.0)]), 0.0);
    h.call("hash_destroytab", &[f(2.0)]).unwrap();
    assert_eq!(create(&mut h, &[f(8.0)]), 2.0);
}

#[test]
fn values_default_to_vectors() {
    let mut h = h();
    let t = create(&mut h, &[f(8.0)]);
    h.call("hash_add", &[f(t), s("a"), v(1.0, 2.0, 3.0)]).unwrap();
    assert_eq!(h.v("hash_get", &[f(t), s("a")]), [1.0, 2.0, 3.0]);
    assert_eq!(h.v("hash_get", &[f(t), s("A")]), [0.0; 3], "keys are case-sensitive");
    assert_eq!(h.v("hash_get", &[f(t), s("b"), v(7.0, 8.0, 9.0)]), [7.0, 8.0, 9.0]);
    // An explicit type 0 also means EV_VECTOR.
    let t = create(&mut h, &[f(8.0), f(0.0)]);
    h.call("hash_add", &[f(t), s("a"), v(4.0, 5.0, 6.0)]).unwrap();
    assert_eq!(h.v("hash_get", &[f(t), s("a"), v(0.0, 0.0, 0.0), f(EV_VECTOR)]), [4.0, 5.0, 6.0]);
}

#[test]
fn string_values_are_copied() {
    let mut h = h();
    let t = create(&mut h, &[f(8.0), f(EV_STRING)]);
    h.call("hash_add", &[f(t), s("greeting"), s("hello")]).unwrap();
    // The temp string passed in is gone after a collection; the table kept a copy.
    h.vm.collect_garbage().unwrap();
    let r = h.call("hash_get", &[f(t), s("greeting")]).unwrap();
    assert_eq!(h.vm.str(r.str_ref()), b"hello");
    assert!(h.vm.is_temp(r.str_ref()));
    assert_eq!(&r.0[1..], [0, 0]);
    // A string default comes back as passed.
    let def = h.vm.intern(b"none");
    let r = h.call("hash_get", &[f(t), s("missing"), w(def.0)]).unwrap();
    assert_eq!(r.str_ref(), def);
}

#[test]
fn types_can_be_required() {
    let mut h = h();
    let t = create(&mut h, &[f(8.0)]);
    h.call("hash_add", &[f(t), s("k"), f(5.0), f(HASH_ADD + EV_FLOAT)]).unwrap();
    h.call("hash_add", &[f(t), s("k"), s("text"), f(HASH_ADD + EV_STRING)]).unwrap();
    assert_eq!(h.f("hash_get", &[f(t), s("k"), f(-1.0), f(EV_FLOAT)]), 5.0);
    assert_eq!(h.s("hash_get", &[f(t), s("k"), f(-1.0), f(EV_STRING)]), b"text");
    assert_eq!(h.f("hash_get", &[f(t), s("k"), f(-1.0), f(EV_VECTOR)]), -1.0);
    // Without a required type the newest entry wins.
    assert_eq!(h.s("hash_get", &[f(t), s("k")]), b"text");
}

#[test]
fn replace_removes_only_the_newest_entry() {
    let mut h = h();
    let t = create(&mut h, &[f(8.0), f(EV_FLOAT)]);
    // Plain adds replace.
    h.call("hash_add", &[f(t), s("x"), f(1.0)]).unwrap();
    h.call("hash_add", &[f(t), s("x"), f(2.0)]).unwrap();
    assert_eq!(h.f("hash_get", &[f(t), s("x")]), 2.0);
    assert_eq!(h.f("hash_get", &[f(t), s("x"), f(-1.0), f(0.0), f(1.0)]), -1.0);
    // HASH_ADD keeps duplicates, reachable by index (newest first).
    h.call("hash_add", &[f(t), s("y"), f(1.0), f(HASH_ADD)]).unwrap();
    h.call("hash_add", &[f(t), s("y"), f(2.0), f(HASH_ADD)]).unwrap();
    let get =
        |h: &mut Harness, index: f32| h.f("hash_get", &[f(t), s("y"), f(-1.0), f(0.0), f(index)]);
    assert_eq!((get(&mut h, 0.0), get(&mut h, 1.0), get(&mut h, 2.0)), (2.0, 1.0, -1.0));
    // HASH_REPLACE wins over HASH_ADD and removes just the newest duplicate.
    h.call("hash_add", &[f(t), s("y"), f(3.0), f(HASH_ADD + HASH_REPLACE)]).unwrap();
    assert_eq!((get(&mut h, 0.0), get(&mut h, 1.0), get(&mut h, 2.0)), (3.0, 1.0, -1.0));
    // Empty keys are ignored.
    h.call("hash_add", &[f(t), s(""), f(9.0)]).unwrap();
    assert_eq!(h.f("hash_get", &[f(t), s(""), f(-1.0)]), -1.0);
}

#[test]
fn delete_returns_the_newest_value() {
    let mut h = h();
    let t = create(&mut h, &[f(8.0)]);
    h.call("hash_add", &[f(t), s("k"), v(1.0, 1.0, 1.0), f(HASH_ADD)]).unwrap();
    h.call("hash_add", &[f(t), s("k"), s("two"), f(HASH_ADD + EV_STRING)]).unwrap();
    assert_eq!(h.s("hash_delete", &[f(t), s("k")]), b"two");
    assert_eq!(h.v("hash_delete", &[f(t), s("k")]), [1.0; 3]);
    assert_eq!(h.call("hash_delete", &[f(t), s("k")]).unwrap().0, [0; 3]);
    assert_eq!(h.v("hash_get", &[f(t), s("k"), v(4.0, 4.0, 4.0)]), [4.0; 3]);
}

#[test]
fn getkey_enumerates_every_entry() {
    let mut h = h();
    let t = create(&mut h, &[f(4.0)]);
    let keys = ["alpha", "beta", "gamma", "delta", "epsilon"];
    for k in keys {
        h.call("hash_add", &[f(t), s(k), f(1.0)]).unwrap();
    }
    let got: BTreeSet<Vec<u8>> = (0..5).map(|i| h.s("hash_getkey", &[f(t), f(i as f32)])).collect();
    let want: BTreeSet<Vec<u8>> = keys.iter().map(|k| k.as_bytes().to_vec()).collect();
    assert_eq!(got, want);
    for i in [5.0, -1.0] {
        assert_eq!(h.call("hash_getkey", &[f(t), f(i)]).unwrap().str_ref(), StrRef::NULL);
    }
}

#[test]
fn destroyed_and_invalid_tables_are_builtin_errors() {
    let mut h = h();
    let t = create(&mut h, &[f(8.0)]);
    h.call("hash_destroytab", &[f(t)]).unwrap();
    for (name, args) in [
        ("hash_get", vec![f(t), s("k")]),
        ("hash_add", vec![f(t), s("k"), f(1.0)]),
        ("hash_delete", vec![f(5.0), s("k")]),
        ("hash_getkey", vec![f(-1.0), f(0.0)]),
        ("hash_destroytab", vec![f(9.0)]),
    ] {
        let err = h.call(name, &args).unwrap_err();
        assert!(matches!(err.kind(), ErrorKind::Builtin(m) if m.contains("invalid")), "{name}");
    }
    // In developer mode FTE warns and carries on: hash_get returns its default.
    h.vm.set_developer(true);
    assert_eq!(h.v("hash_get", &[f(t), s("k"), v(1.0, 2.0, 3.0)]), [1.0, 2.0, 3.0]);
    assert_eq!(warnings(&h), ["hash: invalid hash table"]);
}

#[test]
fn table_zero_is_the_persistent_gamestate_table() {
    let mut h = h();
    // It exists without being created, holds strings by default and cannot be destroyed.
    h.call("hash_add", &[f(0.0), s("map"), s("e1m1")]).unwrap();
    h.call("hash_destroytab", &[f(0.0)]).unwrap();
    assert_eq!(h.s("hash_get", &[f(0.0), s("map")]), b"e1m1");
    assert_eq!(create(&mut h, &[f(8.0)]), 1.0, "never handed out by hash_createtab");
}

fn printed_keys(h: &mut Harness) -> BTreeSet<Vec<u8>> {
    let text = std::mem::take(&mut h.host.printed);
    text.split(|&b| b == b'\n').filter(|k| !k.is_empty()).map(<[u8]>::to_vec).collect()
}

fn cb_count(h: &Harness) -> f32 {
    h.vm.get(h.vm.global::<f32>("cb_count").unwrap())
}

#[test]
fn getcb_calls_back_for_every_entry() {
    let mut h = h();
    let t = create(&mut h, &[f(4.0)]);
    for (k, x) in [("a", 1.0), ("b", 2.0), ("c", 3.0)] {
        h.call("hash_add", &[f(t), s(k), v(x, x, x)]).unwrap();
    }
    let cb = h.vm.find_function("cb").unwrap();
    // FTE ships this builtin as a no-op; qcvm calls the callback as documented.
    h.call("hash_getcb", &[f(t), w(cb.0)]).unwrap();
    assert_eq!(cb_count(&h), 3.0);
    let want: BTreeSet<Vec<u8>> = [b"a".to_vec(), b"b".to_vec(), b"c".to_vec()].into();
    assert_eq!(printed_keys(&mut h), want);
    // Only the entries for one key.
    h.call("hash_add", &[f(t), s("b"), v(5.0, 5.0, 5.0), f(HASH_ADD)]).unwrap();
    h.call("hash_getcb", &[f(t), w(cb.0), s("b")]).unwrap();
    assert_eq!(cb_count(&h), 5.0);
    assert_eq!(printed_keys(&mut h), [b"b".to_vec()].into());
    // Newest first: the last call saw the older value.
    assert_eq!(h.vm.get(h.vm.global::<Vec3>("cb_value").unwrap()), [2.0; 3]);
    // String values arrive as strings.
    h.call("hash_add", &[f(0.0), s("s"), s("text")]).unwrap();
    h.call("hash_getcb", &[f(0.0), w(cb.0)]).unwrap();
    let value = h.vm.get(h.vm.global::<Vec3>("cb_value").unwrap());
    let r = StrRef(value[0].to_bits());
    assert_eq!(h.vm.str(r), b"text");
}

/// Filling a bucket and deleting everything in it frees its storage as well as the budget it
/// was charged: churn cannot accumulate storage the budget no longer counts, and every round
/// fits as many entries as the first.
#[test]
fn deleted_entries_give_back_their_storage() {
    let limits = qcvm::Limits { container_bytes: 16 * 1024, ..qcvm::Limits::default() };
    let config = VmConfig { limits, ..VmConfig::default() };
    let mut h = Harness::with(Numbering::Csqc, config, |_| {});
    let t = h.f("hash_createtab", &[f(4.0), f(1.0)]);
    // HASH_ADD (512) with EV_STRING: every entry lands in the same bucket under one key.
    let fill = |h: &mut Harness| {
        let mut n = 0usize;
        loop {
            h.call("hash_add", &[f(t), s("k"), s("v"), f(513.0)]).unwrap();
            let nth = [f(t), s("k"), f(0.0), f(0.0), f(n as f32)];
            if h.call("hash_get", &nth).unwrap().0[0] == 0 {
                return n;
            }
            n += 1;
            assert!(n < 100_000);
        }
    };
    let first = fill(&mut h);
    assert!(first > 20, "{first}");
    for _ in 0..4 {
        for _ in 0..first {
            h.call("hash_delete", &[f(t), s("k")]).unwrap();
        }
        assert_eq!(h.call("hash_get", &[f(t), s("k")]).unwrap().0[0], 0);
        assert_eq!(fill(&mut h), first);
    }
}
