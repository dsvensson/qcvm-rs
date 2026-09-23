// SPDX-License-Identifier: MIT OR Apache-2.0

//! JSON parsing into VM memory.

use qcvm::{ErrorKind, Numbering, Ptr, StrRef, VmConfig};

use super::{w, warnings};
use crate::support::harness::{Harness, i, s};

const STRING: i32 = 0;
const NUMBER: i32 = 1;
const OBJECT: i32 = 2;
const ARRAY: i32 = 3;
const TRUE: i32 = 4;
const FALSE: i32 = 5;
const NULL: i32 = 6;

fn h() -> Harness {
    Harness::with(
        Numbering::Csqc,
        VmConfig::default(),
        Harness::named(&[
            "json_parse",
            "json_free",
            "json_get_value_type",
            "json_get_name",
            "json_get_integer",
            "json_get_float",
            "json_get_string",
            "json_find_object_child",
            "json_get_length",
            "json_get_child_at_index",
        ]),
    )
}

fn parse(h: &mut Harness, text: &str) -> u32 {
    h.call("json_parse", &[s(text)]).unwrap().0[0]
}

fn ty(h: &mut Harness, n: u32) -> i32 {
    h.i("json_get_value_type", &[w(n)])
}

fn child(h: &mut Harness, n: u32, key: &str) -> u32 {
    h.call("json_find_object_child", &[w(n), s(key)]).unwrap().0[0]
}

fn at(h: &mut Harness, n: u32, index: i32) -> u32 {
    h.call("json_get_child_at_index", &[w(n), i(index)]).unwrap().0[0]
}

fn len(h: &mut Harness, n: u32) -> i32 {
    h.i("json_get_length", &[w(n)])
}

fn name(h: &mut Harness, n: u32) -> Option<Vec<u8>> {
    let r = h.call("json_get_name", &[w(n)]).unwrap().str_ref();
    (!r.is_null()).then(|| h.vm.str(r).to_vec())
}

fn string(h: &mut Harness, n: u32) -> Option<Vec<u8>> {
    let r = h.call("json_get_string", &[w(n)]).unwrap().str_ref();
    (!r.is_null()).then(|| h.vm.str(r).to_vec())
}

fn words(h: &Harness, p: u32) -> [u32; 4] {
    let b = h.vm.read_mem(Ptr(p), 16).unwrap();
    std::array::from_fn(|k| u32::from_le_bytes(b[k * 4..k * 4 + 4].try_into().unwrap()))
}

const DOC: &str = r#"{"name": "Ranger", "hp": 100, "pos": [1, 2.5, -3], "alive": true,
    "dead": false, "none": null, "nested": {"k": "v"}}"#;

#[test]
fn objects_arrays_and_scalars() {
    let mut h = h();
    let root = parse(&mut h, DOC);
    assert_ne!(root, 0);
    assert_eq!((ty(&mut h, root), len(&mut h, root), name(&mut h, root)), (OBJECT, 7, None));

    let hp = child(&mut h, root, "hp");
    assert_eq!(ty(&mut h, hp), NUMBER);
    assert_eq!(h.f("json_get_float", &[w(hp)]), 100.0);
    assert_eq!(h.i("json_get_integer", &[w(hp)]), 100);
    assert_eq!(string(&mut h, hp), None);
    assert_eq!(name(&mut h, hp), Some(b"hp".to_vec()));

    let n = child(&mut h, root, "name");
    assert_eq!((ty(&mut h, n), string(&mut h, n)), (STRING, Some(b"Ranger".to_vec())));
    assert_eq!(h.f("json_get_float", &[w(n)]), 0.0);

    let pos = child(&mut h, root, "pos");
    assert_eq!((ty(&mut h, pos), len(&mut h, pos)), (ARRAY, 3));
    let second = at(&mut h, pos, 1);
    assert_eq!(h.f("json_get_float", &[w(second)]), 2.5);
    assert_eq!(h.i("json_get_integer", &[w(second)]), 2);
    // Array elements are named by their index, so they can be found by name too.
    assert_eq!(name(&mut h, second), Some(b"1".to_vec()));
    let third = child(&mut h, pos, "2");
    assert_eq!(h.f("json_get_float", &[w(third)]), -3.0);

    for (key, want, as_int) in [("alive", TRUE, 1), ("dead", FALSE, 0), ("none", NULL, 0)] {
        let n = child(&mut h, root, key);
        assert_eq!(ty(&mut h, n), want, "{key}");
        assert_eq!(h.i("json_get_integer", &[w(n)]), as_int, "{key}");
        assert_eq!(h.f("json_get_float", &[w(n)]), as_int as f32, "{key}");
        assert_eq!(len(&mut h, n), 0);
    }
    let nested = child(&mut h, root, "nested");
    let k = child(&mut h, nested, "k");
    assert_eq!(string(&mut h, k), Some(b"v".to_vec()));

    // Misses.
    assert_eq!(child(&mut h, root, "missing"), 0);
    assert_eq!(child(&mut h, root, "HP"), 0);
    assert_eq!(child(&mut h, hp, "x"), 0);
    assert_eq!(at(&mut h, pos, 3), 0);
    assert_eq!(at(&mut h, pos, -1), 0);
    assert_eq!(at(&mut h, hp, 0), 0);
}

#[test]
fn nodes_are_laid_out_in_one_block() {
    let mut h = h();
    let root = parse(&mut h, DOC);
    // Root: object, no name, 7 children right after it.
    assert_eq!(words(&h, root), [OBJECT as u32, 0, root + 16, 7]);
    let first = words(&h, root + 16);
    assert_eq!(first[0], STRING as u32);
    assert_eq!(h.vm.str(StrRef(first[1])), b"name");
    assert_eq!(h.vm.str(StrRef(first[2])), b"Ranger");
    // The children of containers follow in pre-order: pos's 3 elements, then nested's child.
    let pos = child(&mut h, root, "pos");
    assert_eq!(pos, root + 3 * 16);
    assert_eq!(words(&h, pos)[2..], [root + 8 * 16, 3]);
    let nested = child(&mut h, root, "nested");
    assert_eq!(words(&h, nested)[2..], [root + 11 * 16, 1]);
    // Numbers are doubles in the second half.
    let hp = words(&h, root + 2 * 16);
    assert_eq!(f64::from_bits(u64::from(hp[2]) | (u64::from(hp[3]) << 32)), 100.0);
    // The names are stored after the nodes, in the same block.
    let name_ptr = first[1];
    assert!(name_ptr >= root + 12 * 16);
    // One free releases it all.
    h.call("json_free", &[w(root)]).unwrap();
    assert!(warnings(&h).is_empty());
    h.call("json_free", &[w(root)]).unwrap();
    assert_eq!(warnings(&h).len(), 1);
}

#[test]
fn strings_decode_escapes_but_keys_do_not() {
    let mut h = h();
    let root = parse(&mut h, "{\"a\\\"b\": \"tab\\t quote\\\" \x5cu00e9 \x5cud83d\x5cude00 \\/\"}");
    assert_eq!(len(&mut h, root), 1);
    let n = at(&mut h, root, 0);
    assert_eq!(name(&mut h, n), Some(b"a\\\"b".to_vec()));
    assert_eq!(string(&mut h, n), Some("tab\t quote\" \u{e9} \u{1F600} /".as_bytes().to_vec()));
    // Unknown escapes are kept; a NUL becomes the overlong C0 80.
    let root = parse(&mut h, "[\"\\q\", \"a\x5cu0000b\"]");
    let a = at(&mut h, root, 0);
    assert_eq!(string(&mut h, a), Some(b"\\q".to_vec()));
    let b = at(&mut h, root, 1);
    assert_eq!(string(&mut h, b), Some(b"a\xC0\x80b".to_vec()));
    // String values are temp strings that outlive the tree's collection roots.
    h.vm.collect_garbage().unwrap();
    assert_eq!(string(&mut h, b), Some(b"a\xC0\x80b".to_vec()));
}

#[test]
fn the_parser_is_lenient_like_ftes() {
    let mut h = h();
    let root = parse(
        &mut h,
        "\u{feff} // comment\n[ /* block */ Infinity, 0x10, abc, TRUE, Null, fAlse, 1e2, ]",
    );
    assert_ne!(root, 0);
    assert_eq!(len(&mut h, root), 7);
    let types: Vec<i32> = (0..7)
        .map(|k| {
            let n = at(&mut h, root, k);
            ty(&mut h, n)
        })
        .collect();
    assert_eq!(types, [NUMBER, NUMBER, NUMBER, TRUE, NULL, FALSE, NUMBER]);
    let n = at(&mut h, root, 0);
    assert_eq!(h.f("json_get_float", &[w(n)]), f32::INFINITY);
    assert_eq!(h.i("json_get_integer", &[w(n)]), i32::MIN, "x86 conversion");
    let n = at(&mut h, root, 1);
    assert_eq!(h.f("json_get_float", &[w(n)]), 16.0);
    let n = at(&mut h, root, 2);
    assert_eq!(h.f("json_get_float", &[w(n)]), 0.0);
    // Trailing commas in objects, a key without a value, scalars at the root.
    let root = parse(&mut h, r#"{"a": 1, "b" , "c": 2,}"#);
    assert_eq!(len(&mut h, root), 2);
    for (text, want) in [(" 42 ", NUMBER), ("\"text\"", STRING), ("[]", ARRAY), ("{}", OBJECT)] {
        let n = parse(&mut h, text);
        assert_eq!((ty(&mut h, n), len(&mut h, n)), (want, 0), "{text}");
    }
}

#[test]
fn invalid_documents_parse_to_null() {
    let mut h = h();
    for text in ["", "   ", "{", "[1 2]", "{} x", "\"unterminated", "[,]", "{\"a\" 1}", "]"] {
        assert_eq!(parse(&mut h, text), 0, "{text:?}");
    }
    let deep = "[".repeat(300) + &"]".repeat(300);
    assert_eq!(parse(&mut h, &deep), 0, "nesting is bounded");
    let ok = "[".repeat(100) + &"]".repeat(100);
    assert_ne!(parse(&mut h, &ok), 0);
}

#[test]
fn strings_convert_like_atoi_and_atof() {
    let mut h = h();
    let root = parse(&mut h, r#"["42abc", " 1.5e1x", "-2.9", "x"]"#);
    let get = |h: &mut Harness, k: i32| {
        let n = at(h, root, k);
        (h.i("json_get_integer", &[w(n)]), h.f("json_get_float", &[w(n)]))
    };
    assert_eq!(get(&mut h, 0), (42, 42.0));
    assert_eq!(get(&mut h, 1), (1, 15.0));
    assert_eq!(get(&mut h, 2), (-2, -2.9));
    assert_eq!(get(&mut h, 3), (0, 0.0));
    let n = parse(&mut h, "3.99");
    assert_eq!(h.i("json_get_integer", &[w(n)]), 3);
}

#[test]
fn null_and_bad_nodes() {
    let mut h = h();
    // A null node reads as JSON null.
    assert_eq!(ty(&mut h, 0), NULL);
    assert_eq!((len(&mut h, 0), string(&mut h, 0), name(&mut h, 0)), (0, None, None));
    assert_eq!(at(&mut h, 0, 0), 0);
    // A pointer outside VM memory is a builtin error...
    let err = h.call("json_get_value_type", &[w(0x7FFF_FFF0)]).unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::Builtin(m) if m.contains("bad node")));
    // ...after which FTE carries on with a null node in developer mode.
    h.vm.set_developer(true);
    assert_eq!(ty(&mut h, 0x7FFF_FFF0), NULL);
    assert_eq!(warnings(&h).len(), 1);
}
