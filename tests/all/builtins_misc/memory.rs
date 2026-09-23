// SPDX-License-Identifier: MIT OR Apache-2.0

//! The QuakeC heap, pointer helpers, temp buffers and base64.

use qcvm::{ErrorKind, Limits, Numbering, Ptr, VmConfig};

use super::{add_peek, parm_word, peek, w, warnings};
use crate::support::asm::ty;
use crate::support::harness::{Harness, f, i, s};

fn h_with(config: VmConfig) -> Harness {
    Harness::with(Numbering::Csqc, config, |asm| {
        add_peek(asm);
        asm.global("scratch", ty::FLOAT, &[]);
        Harness::named(&["memrealloc", "memcmp", "base64encode", "base64decode"])(asm);
    })
}

fn h() -> Harness {
    h_with(VmConfig::default())
}

fn alloc(h: &mut Harness, n: i32) -> u32 {
    let p = h.call("memalloc", &[i(n)]).unwrap().0[0];
    assert_ne!(p, 0);
    p
}

fn bytes(h: &Harness, p: u32, n: usize) -> Vec<u8> {
    h.vm.read_mem(Ptr(p), n).unwrap()
}

fn write(h: &mut Harness, p: u32, data: &[u8]) {
    assert!(h.vm.write_mem(Ptr(p), data));
}

#[track_caller]
fn assert_builtin_error(r: Result<qcvm::Ret, qcvm::VmError>, what: &str) {
    let err = r.unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::Builtin(m) if m.contains(what)), "{err}");
}

#[test]
fn memalloc_gives_zeroed_blocks_and_memfree_releases_them() {
    let mut h = h();
    let a = alloc(&mut h, 10);
    let b = alloc(&mut h, 3);
    assert_ne!(a, b);
    assert_eq!(bytes(&h, a, 10), [0; 10]);
    write(&mut h, a, b"0123456789");
    h.call("memfree", &[w(a)]).unwrap();
    // First fit: the freed block comes back, zeroed again.
    assert_eq!(alloc(&mut h, 8), a);
    assert_eq!(bytes(&h, a, 8), [0; 8]);
    // Size 0 still gives a block; null is ignored by memfree.
    let z = alloc(&mut h, 0);
    h.call("memfree", &[w(z)]).unwrap();
    h.call("memfree", &[w(0)]).unwrap();
    assert!(warnings(&h).is_empty());
    // Freeing twice, or something that is not a block, warns.
    h.call("memfree", &[w(z)]).unwrap();
    h.call("memfree", &[w(a + 4)]).unwrap();
    assert_eq!(warnings(&h).len(), 2);
}

#[test]
fn memalloc_refuses_bad_sizes() {
    let mut h = h();
    assert_builtin_error(h.call("memalloc", &[i(-1)]), "memalloc");
    assert_builtin_error(h.call("memalloc", &[i(0x0100_0001)]), "memalloc");
    assert!(h.call("memalloc", &[i(0x0100_0000)]).unwrap().0[0] != 0);
    // Developer mode: null and a warning.
    h.vm.set_developer(true);
    assert_eq!(h.call("memalloc", &[i(-5)]).unwrap().0[0], 0);
    assert_eq!(warnings(&h), ["memalloc: failure (size -5)"]);
    // The heap limit.
    let limits = Limits { heap_bytes: 256, ..Limits::default() };
    let mut h = h_with(VmConfig { limits, ..VmConfig::default() });
    alloc(&mut h, 200);
    assert_builtin_error(h.call("memalloc", &[i(100)]), "memalloc");
}

#[test]
fn memrealloc_keeps_the_contents() {
    let mut h = h();
    let a = alloc(&mut h, 4);
    write(&mut h, a, b"abcd");
    let b = h.call("memrealloc", &[w(a), i(64)]).unwrap().0[0];
    assert_ne!(b, a);
    assert_eq!(bytes(&h, b, 8), b"abcd\0\0\0\0");
    // The old block was freed.
    h.call("memfree", &[w(a)]).unwrap();
    assert_eq!(warnings(&h).len(), 1);
    let c = h.call("memrealloc", &[w(b), i(2)]).unwrap().0[0];
    assert_eq!(bytes(&h, c, 2), b"ab");
    // A null pointer allocates.
    let d = h.call("memrealloc", &[w(0), i(16)]).unwrap().0[0];
    assert_eq!(bytes(&h, d, 16), [0; 16]);
    assert_builtin_error(h.call("memrealloc", &[w(c + 1), i(8)]), "memrealloc");
    assert_builtin_error(h.call("memrealloc", &[w(c), i(-1)]), "memrealloc");
}

#[test]
fn memgetval_counts_words_and_memptradd_bytes() {
    let mut h = h();
    let p = alloc(&mut h, 16);
    // Heap pointers are large: FTE computes `p + ofs * 4` through a float, which would lose
    // precision here; qcvm is exact.
    assert!(p > 1 << 24);
    h.call("memsetval", &[w(p), f(1.0), i(42)]).unwrap();
    h.call("memsetval", &[w(p), f(2.0), f(1.5)]).unwrap();
    assert_eq!(bytes(&h, p + 4, 4), 42u32.to_le_bytes());
    assert_eq!(h.i("memgetval", &[w(p), f(1.0)]), 42);
    assert_eq!(h.f("memgetval", &[w(p), f(2.0)]), 1.5);
    // memptradd adds bytes: +8 bytes is word 2.
    let q = h.call("memptradd", &[w(p), f(8.0)]).unwrap().0[0];
    assert_eq!(q, p + 8);
    assert_eq!(h.f("memgetval", &[w(q), f(0.0)]), 1.5);
    assert_eq!(h.i("memgetval", &[w(q), f(-1.0)]), 42);
    assert!(warnings(&h).is_empty());
    // Globals are memory too.
    let g = h.vm.global::<f32>("scratch").unwrap();
    h.call("memsetval", &[w(g.ptr().0), f(0.0), f(2.5)]).unwrap();
    assert_eq!(h.vm.get(g), 2.5);
}

#[test]
fn memgetval_warns_about_odd_offsets_and_fails_outside_memory() {
    let mut h = h();
    let p = alloc(&mut h, 16);
    write(&mut h, p, &[1, 2, 3, 4, 5, 6, 7, 8]);
    // Half a word: a non-integral offset and a misaligned pointer, both only warnings.
    assert_eq!(h.i("memgetval", &[w(p), f(0.5)]), i32::from_le_bytes([3, 4, 5, 6]));
    let warned = warnings(&h);
    assert_eq!(warned.len(), 2);
    assert!(warned[0].contains("non-integer"));
    assert!(warned[1].contains("misaligned"));
    let err = h.call("memgetval", &[w(0x7FFF_0000), f(0.0)]).unwrap_err();
    assert_eq!(*err.kind(), ErrorKind::BadPointerRead(0x7FFF_0000));
    let err = h.call("memsetval", &[w(0x7FFF_0000), f(1.0), i(1)]).unwrap_err();
    assert_eq!(*err.kind(), ErrorKind::BadPointerWrite(0x7FFF_0004));
    let err = h.call("memsetval", &[w(0), f(0.0), i(1)]).unwrap_err();
    assert_eq!(*err.kind(), ErrorKind::NullPointerWrite);
    // Negative results and temp buffers are outside linear memory.
    assert!(h.call("memgetval", &[w(p), f(-1e9)]).is_err());
    let buf = h.call("createbuffer", &[i(8)]).unwrap().0[0];
    assert!(h.call("memgetval", &[w(buf), f(0.0)]).is_err());
    // Developer mode does not make these fatal errors warnings.
    h.vm.set_developer(true);
    assert!(h.call("memgetval", &[w(0x7FFF_0000), f(0.0)]).is_err());
}

#[test]
fn memptradd_rejects_odd_offsets() {
    let mut h = h();
    let p = alloc(&mut h, 16);
    assert_builtin_error(h.call("memptradd", &[w(p), f(4.5)]), "non-integer");
    assert_builtin_error(h.call("memptradd", &[w(p), f(2.0)]), "aligned");
    assert_builtin_error(h.call("memptradd", &[w(p), f(-4.0)]), "negative");
    // FTE carries on in developer mode.
    h.vm.set_developer(true);
    assert_eq!(h.call("memptradd", &[w(p), f(-4.0)]).unwrap().0[0], p - 4);
    assert_eq!(warnings(&h), ["memptradd: negative offset"]);
}

#[test]
fn memcpy_and_memfill8() {
    let mut h = h();
    let a = alloc(&mut h, 16);
    let b = alloc(&mut h, 16);
    write(&mut h, a, b"abcdefgh");
    h.call("memcpy", &[w(b), w(a), i(8)]).unwrap();
    assert_eq!(bytes(&h, b, 8), b"abcdefgh");
    // Offsets are bytes; the source offset comes first (as FTE implements it).
    h.call("memcpy", &[w(b), w(a), i(2), i(6), i(1)]).unwrap();
    assert_eq!(bytes(&h, b, 8), b"aghdefgh");
    // Overlapping copies behave like memmove.
    h.call("memcpy", &[w(a + 2), w(a), i(6)]).unwrap();
    assert_eq!(bytes(&h, a, 8), b"ababcdef");
    h.call("memcpy", &[w(a), w(a + 2), i(6)]).unwrap();
    assert_eq!(bytes(&h, a, 8), b"abcdefef");
    // Size 0 does nothing, even with null pointers.
    h.call("memcpy", &[w(0), w(0), i(0)]).unwrap();
    assert_builtin_error(h.call("memcpy", &[w(b), w(a), i(-1)]), "invalid size");
    assert_builtin_error(h.call("memcpy", &[w(0), w(a), i(4)]), "invalid dest");
    assert_builtin_error(h.call("memcpy", &[w(b), w(0x7FFF_0000), i(4)]), "invalid source");

    h.call("memfill8", &[w(b), i(0x1FF), i(3), i(1)]).unwrap();
    assert_eq!(bytes(&h, b, 5), [b'a', 0xFF, 0xFF, 0xFF, b'e']);
    h.call("memfill8", &[w(b), i(0), i(0)]).unwrap();
    assert_builtin_error(h.call("memfill8", &[w(b), i(0), i(-1)]), "invalid dest");
    assert_builtin_error(h.call("memfill8", &[w(0), i(0), i(4)]), "invalid dest");
}

#[test]
fn memcmp_compares_bytes() {
    let mut h = h();
    let a = alloc(&mut h, 16);
    let b = alloc(&mut h, 16);
    write(&mut h, a, b"hello world");
    write(&mut h, b, b"help");
    assert_eq!(h.i("memcmp", &[w(a), w(b), i(3)]), 0);
    assert_eq!(h.i("memcmp", &[w(a), w(b), i(4)]), i32::from(b'l') - i32::from(b'p'));
    assert_eq!(h.i("memcmp", &[w(b), w(a), i(4)]), i32::from(b'p') - i32::from(b'l'));
    assert_eq!(h.i("memcmp", &[w(a), w(b), i(0)]), 0);
    // Offsets: a + 2 ("llo") against b + 2 ("lp").
    assert_eq!(h.i("memcmp", &[w(a), w(b), i(1), i(2), i(2)]), 0);
    assert_eq!(h.i("memcmp", &[w(a), w(b), i(2), i(2), i(2)]), i32::from(b'l') - i32::from(b'p'));
    // The first offset applies to the first pointer (as documented; FTE's implementation swaps
    // them): a + 2 ("l") against b ("h").
    assert_eq!(h.i("memcmp", &[w(a), w(b), i(1), i(2), i(0)]), i32::from(b'l') - i32::from(b'h'));
    assert_builtin_error(h.call("memcmp", &[w(a), w(b), i(-1)]), "invalid size");
    assert_builtin_error(h.call("memcmp", &[w(0x7FFF_0000), w(b), i(1)]), "invalid");
}

#[test]
fn createbuffer_gives_growable_temp_buffers() {
    let mut h = h();
    assert_eq!(h.call("createbuffer", &[i(0)]).unwrap().0[0], 0);
    assert_eq!(h.call("createbuffer", &[i(-3)]).unwrap().0[0], 0);
    let r = h.call("createbuffer", &[i(8)]).unwrap();
    assert!(h.vm.is_temp(r.str_ref()));
    let buf = r.0[0];
    assert_eq!(peek(&mut h, buf, 0), 0);
    let a = alloc(&mut h, 16);
    write(&mut h, a, &7i32.to_le_bytes());
    h.call("memcpy", &[w(buf), w(a), i(4), i(0), i(4)]).unwrap();
    assert_eq!(peek(&mut h, buf, 1), 7);
    // Writing past the end grows the buffer.
    h.call("memcpy", &[w(buf), w(a), i(4), i(0), i(100)]).unwrap();
    assert_eq!(peek(&mut h, buf, 25), 7);
    h.call("memfill8", &[w(buf), i(1), i(4), i(200)]).unwrap();
    assert_eq!(peek(&mut h, buf, 50), 0x0101_0101);
    // Reading from it too.
    h.call("memcpy", &[w(a), w(buf), i(4), i(100)]).unwrap();
    assert_eq!(bytes(&h, a, 4), 7i32.to_le_bytes());
    assert_builtin_error(h.call("memcpy", &[w(a), w(buf), i(4), i(10_000)]), "invalid source");
    // Not freeable.
    h.call("memfree", &[w(buf)]).unwrap();
    assert_eq!(warnings(&h).len(), 1);
}

#[test]
fn base64_round_trip_through_the_heap() {
    let mut h = h();
    let a = alloc(&mut h, 8);
    write(&mut h, a, b"foobar");
    assert_eq!(h.s("base64encode", &[w(a), i(6)]), b"Zm9vYmFy");
    assert_eq!(h.s("base64encode", &[w(a), i(4)]), b"Zm9vYg==");
    assert_eq!(h.s("base64encode", &[w(a), i(0)]), b"");
    // Temp strings are readable memory too.
    assert_eq!(h.s("base64encode", &[s("hi"), i(2)]), b"aGk=");
    assert_builtin_error(h.call("base64encode", &[w(0x7FFF_0000), i(4)]), "invalid");
    assert_builtin_error(h.call("base64encode", &[w(a), i(-1)]), "invalid");

    let p = h.call("base64decode", &[s("Zm9vYmFy"), i(0)]).unwrap().0[0];
    assert_eq!(parm_word(&h, 1), 6);
    assert_eq!(bytes(&h, p, 7), b"foobar\0");
    h.call("memfree", &[w(p)]).unwrap();
    // FTE's lenient decoder: line breaks skipped, URL-safe symbols, padding ends the data.
    let p = h.call("base64decode", &[s("Zm9v\nYg==trailing"), i(0)]).unwrap().0[0];
    assert_eq!(parm_word(&h, 1), 4);
    assert_eq!(bytes(&h, p, 4), b"foob");
    let p = h.call("base64decode", &[s("-_8"), i(0)]).unwrap().0[0];
    assert_eq!((parm_word(&h, 1), bytes(&h, p, 2)), (2, vec![0xFB, 0xFF]));
    // An empty string still gives a (NUL-terminated) block.
    let p = h.call("base64decode", &[s(""), i(9)]).unwrap().0[0];
    assert_ne!(p, 0);
    assert_eq!(parm_word(&h, 1), 0);
    assert!(warnings(&h).is_empty());
}
