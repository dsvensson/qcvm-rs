// SPDX-License-Identifier: MIT OR Apache-2.0

//! String buffers.

use qcvm::{Limits, Numbering, StrRef, VmConfig};

use super::warnings;
use crate::support::harness::{Harness, f, s};

fn h() -> Harness {
    Harness::new()
}

fn get(h: &mut Harness, b: f32, i: f32) -> Option<Vec<u8>> {
    let r = h.call("bufstr_get", &[f(b), f(i)]).unwrap().str_ref();
    (!r.is_null()).then(|| h.vm.str(r).to_vec())
}

fn contents(h: &mut Harness, b: f32) -> Vec<Option<String>> {
    let n = h.f("buf_getsize", &[f(b)]) as usize;
    (0..n).map(|i| get(h, b, i as f32).map(|v| String::from_utf8(v).unwrap())).collect()
}

fn set(h: &mut Harness, b: f32, i: f32, text: &str) {
    h.call("bufstr_set", &[f(b), f(i), s(text)]).unwrap();
}

fn some(v: &[&str]) -> Vec<Option<String>> {
    v.iter().map(|s| Some((*s).to_owned())).collect()
}

#[test]
fn create_set_get_and_holes() {
    let mut h = h();
    assert_eq!(h.f("buf_create", &[]), 1.0);
    assert_eq!(h.f("buf_create", &[s("STRING"), f(0.0)]), 2.0);
    assert_eq!(h.f("buf_create", &[s("float")]), -1.0, "only string buffers exist");
    assert_eq!(h.f("buf_getsize", &[f(1.0)]), 0.0);
    set(&mut h, 1.0, 2.0, "two");
    assert_eq!(h.f("buf_getsize", &[f(1.0)]), 3.0);
    assert_eq!(contents(&mut h, 1.0), [None, None, Some("two".to_owned())]);
    assert_eq!(get(&mut h, 1.0, 3.0), None);
    assert_eq!(get(&mut h, 1.0, -1.0), None);
    // Entries are copies that outlive the temp strings they came from.
    h.vm.collect_garbage().unwrap();
    assert_eq!(get(&mut h, 1.0, 2.0), Some(b"two".to_vec()));
    // Deleted handles are reused.
    h.call("buf_del", &[f(1.0)]).unwrap();
    assert_eq!(h.f("buf_create", &[]), 1.0);
    assert_eq!(h.f("buf_getsize", &[f(1.0)]), 0.0);
}

#[test]
fn add_free_and_copy() {
    let mut h = h();
    let b = h.f("buf_create", &[]);
    assert_eq!(h.f("bufstr_add", &[f(b), s("a"), f(1.0)]), 0.0);
    assert_eq!(h.f("bufstr_add", &[f(b), s("b"), f(1.0)]), 1.0);
    assert_eq!(h.f("bufstr_add", &[f(b), s("c"), f(1.0)]), 2.0);
    h.call("bufstr_free", &[f(b), f(1.0)]).unwrap();
    assert_eq!(h.f("buf_getsize", &[f(b)]), 3.0, "freeing leaves a hole");
    // Unordered adds fill the first hole, ordered ones append.
    assert_eq!(h.f("bufstr_add", &[f(b), s("d"), f(1.0)]), 3.0);
    assert_eq!(h.f("bufstr_add", &[f(b), s("e"), f(0.0)]), 1.0);
    assert_eq!(contents(&mut h, b), some(&["a", "e", "c", "d"]));
    h.call("bufstr_free", &[f(b), f(9.0)]).unwrap();
    h.call("bufstr_free", &[f(b), f(2.0)]).unwrap();

    let c = h.f("buf_create", &[]);
    set(&mut h, c, 7.0, "gone");
    h.call("buf_copy", &[f(b), f(c)]).unwrap();
    let want = vec![Some("a".to_owned()), Some("e".to_owned()), None, Some("d".to_owned())];
    assert_eq!(contents(&mut h, c), want);
    // A deep copy.
    set(&mut h, b, 0.0, "changed");
    assert_eq!(get(&mut h, c, 0.0), Some(b"a".to_vec()));
    // Copying onto itself or from/to invalid handles does nothing.
    h.call("buf_copy", &[f(c), f(c)]).unwrap();
    h.call("buf_copy", &[f(c), f(99.0)]).unwrap();
    assert_eq!(contents(&mut h, c), want);
}

#[test]
fn sort_compacts_holes_first() {
    let mut h = h();
    let b = h.f("buf_create", &[]);
    for (i, t) in [(0.0, "pear"), (2.0, "apple"), (3.0, "fig"), (5.0, "apricot")] {
        set(&mut h, b, i, t);
    }
    h.call("buf_sort", &[f(b), f(0.0), f(0.0)]).unwrap();
    assert_eq!(contents(&mut h, b), some(&["apple", "apricot", "fig", "pear"]));
    h.call("buf_sort", &[f(b), f(0.0), f(1.0)]).unwrap();
    assert_eq!(contents(&mut h, b), some(&["pear", "fig", "apricot", "apple"]));
    // Only the first byte counts: "apricot" and "apple" compare equal (the sort is stable here,
    // unspecified in FTE).
    h.call("buf_sort", &[f(b), f(1.0), f(0.0)]).unwrap();
    assert_eq!(contents(&mut h, b), some(&["apricot", "apple", "fig", "pear"]));
}

#[test]
fn implode_glues_only_after_output() {
    let mut h = h();
    let b = h.f("buf_create", &[]);
    for (i, t) in [(0.0, ""), (1.0, "a"), (3.0, "b"), (4.0, "")] {
        set(&mut h, b, i, t);
    }
    // The empty first entry produces no output, so no glue before "a"; the hole is skipped.
    assert_eq!(h.s("buf_implode", &[f(b), s(", ")]), b"a, b, ");
    let e = h.f("buf_create", &[]);
    let r = h.call("buf_implode", &[f(e), s(",")]).unwrap().str_ref();
    assert!(!r.is_null());
    assert_eq!(h.vm.str(r), b"");
}

#[test]
fn invalid_handles_return_zero_or_null() {
    let mut h = h();
    let b = h.f("buf_create", &[]);
    h.call("buf_del", &[f(b)]).unwrap();
    // FTE leaves the return value untouched; qcvm returns 0 / null.
    for bad in [0.0, b, 7.0, -1.0] {
        assert_eq!(h.f("buf_getsize", &[f(bad)]), 0.0);
        let r = h.call("buf_implode", &[f(bad), s(",")]).unwrap().str_ref();
        assert_eq!(r, StrRef::NULL);
        assert_eq!(h.f("bufstr_add", &[f(bad), s("x"), f(1.0)]), 0.0);
        assert_eq!(get(&mut h, bad, 0.0), None);
        assert_eq!(h.f("bufstr_find", &[f(bad), s("x"), f(1.0)]), -1.0);
        set(&mut h, bad, 0.0, "x");
        h.call("bufstr_free", &[f(bad), f(0.0)]).unwrap();
        h.call("buf_sort", &[f(bad), f(0.0), f(0.0)]).unwrap();
        h.call("buf_del", &[f(bad)]).unwrap();
    }
    assert!(warnings(&h).is_empty());
}

#[test]
fn set_refuses_absurd_indices() {
    let mut h = h();
    let b = h.f("buf_create", &[]);
    set(&mut h, b, 2_000_000.0, "far");
    assert_eq!(h.f("buf_getsize", &[f(b)]), 0.0);
    assert_eq!(warnings(&h), ["bufstr_set: index outside sanity range"]);
}

#[test]
fn limits_are_enforced() {
    let limits = Limits { string_buffers: 1, string_buffer_entries: 3, ..Limits::default() };
    let config = VmConfig { limits, ..VmConfig::default() };
    let mut h = Harness::with(Numbering::Csqc, config, |_| {});
    let b = h.f("buf_create", &[]);
    assert_eq!(h.f("buf_create", &[]), -1.0);
    set(&mut h, b, 3.0, "no");
    assert_eq!(h.f("buf_getsize", &[f(b)]), 0.0);
    for i in 0..3 {
        assert_eq!(h.f("bufstr_add", &[f(b), s("x"), f(1.0)]), i as f32);
    }
    assert_eq!(h.f("bufstr_add", &[f(b), s("x"), f(1.0)]), -1.0);
    assert_eq!(h.f("buf_getsize", &[f(b)]), 3.0);
    assert_eq!(warnings(&h).len(), 2);
    h.call("buf_del", &[f(b)]).unwrap();
    assert_eq!(h.f("buf_create", &[]), 1.0);
}

#[test]
fn find_by_rule() {
    let mut h = h();
    let b = h.f("buf_create", &[]);
    for (i, t) in [
        (0.0, "maps/e1m1.bsp"),
        (1.0, "maps/E1M2.bsp"),
        (3.0, "progs/player.mdl"),
        (4.0, "maps/sub/e1m3.bsp"),
        (5.0, "e1m1"),
    ] {
        set(&mut h, b, i, t);
    }
    let find = |h: &mut Harness, pat: &str, rule: f32, extra: &[f32]| {
        let mut args = vec![f(b), s(pat), f(rule)];
        args.extend(extra.iter().map(|&x| f(x)));
        h.f("bufstr_find", &args)
    };
    assert_eq!(find(&mut h, "e1m1", 1.0, &[]), 5.0);
    assert_eq!(find(&mut h, "maps/", 2.0, &[]), 0.0);
    assert_eq!(find(&mut h, ".mdl", 3.0, &[]), 3.0);
    assert_eq!(find(&mut h, "player", 4.0, &[]), 3.0);
    assert_eq!(find(&mut h, "", 4.0, &[]), 0.0);
    // Wildcards: case-insensitive, `*` does not cross a slash.
    assert_eq!(find(&mut h, "maps/e1m?.bsp", 5.0, &[1.0]), 1.0);
    assert_eq!(find(&mut h, "maps/*.bsp", 0.0, &[2.0]), -1.0, "not maps/sub/e1m3.bsp");
    assert_eq!(find(&mut h, "maps/*/*", 5.0, &[]), 4.0);
    assert_eq!(find(&mut h, "*", 5.0, &[]), 5.0);
    assert_eq!(find(&mut h, "zzz", 9.0, &[]), -1.0, "unknown rules are wildcards");
    // start and step.
    assert_eq!(find(&mut h, "maps/", 2.0, &[1.0]), 1.0);
    assert_eq!(find(&mut h, "maps/", 2.0, &[2.0, 2.0]), 4.0, "2 is a hole");
    assert_eq!(find(&mut h, "maps/", 2.0, &[1.0, 3.0]), 1.0);
    assert_eq!(find(&mut h, "e1m", 2.0, &[0.0, 4.0]), -1.0);
    assert_eq!(find(&mut h, "maps/", 2.0, &[-1.0]), -1.0);
    assert_eq!(find(&mut h, "maps/", 2.0, &[0.0, 0.0]), -1.0);
    assert_eq!(find(&mut h, "maps/", 2.0, &[9.0]), -1.0);
}

#[test]
fn cvarlist_and_loadfile_use_the_host() {
    let mut h = h();
    for (k, v) in [("sv_gravity", "800"), ("cl_yawspeed", "140"), ("sv_friction", "4")] {
        h.host.cvars.insert(k.as_bytes().to_vec(), v.as_bytes().to_vec());
    }
    h.host.files.insert(b"lines.txt".to_vec(), b"one\r\ntwo\n\nthree".to_vec());
    let b = h.f("buf_create", &[]);
    set(&mut h, b, 5.0, "old");
    h.call("buf_cvarlist", &[f(b), s("sv_"), s("")]).unwrap();
    assert_eq!(contents(&mut h, b), some(&["sv_friction", "sv_gravity"]));

    assert_eq!(h.f("buf_loadfile", &[s("missing.txt"), f(b)]), 0.0);
    assert_eq!(h.f("buf_loadfile", &[s("lines.txt"), f(b)]), 1.0);
    assert_eq!(
        contents(&mut h, b),
        some(&["sv_friction", "sv_gravity", "one", "two", "", "three"]),
        "lines are appended, without their line breaks"
    );
    assert_eq!(h.f("buf_loadfile", &[s("lines.txt"), f(99.0)]), 0.0);
}
