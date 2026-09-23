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
fn keys_and_strings_are_unescaped() {
    let mut h = h();
    let root = parse(&mut h, "{\"a\\\"b\": \"tab\\t quote\\\" \x5cu00e9 \x5cud83d\x5cude00 \\/\"}");
    assert_eq!(len(&mut h, root), 1);
    let n = at(&mut h, root, 0);
    assert_eq!(name(&mut h, n), Some(b"a\"b".to_vec()));
    assert_eq!(string(&mut h, n), Some("tab\t quote\" \u{e9} \u{1F600} /".as_bytes().to_vec()));
    // A NUL would end a QuakeC string, so it is stored as the overlong C0 80, in keys too.
    let root = parse(&mut h, "{\"k\x5cu0000\": \"a\x5cu0000b\"}");
    let b = at(&mut h, root, 0);
    assert_eq!(name(&mut h, b), Some(b"k\xC0\x80".to_vec()));
    assert_eq!(string(&mut h, b), Some(b"a\xC0\x80b".to_vec()));
    // String values are temp strings that outlive the tree's collection roots.
    h.vm.collect_garbage().unwrap();
    assert_eq!(string(&mut h, b), Some(b"a\xC0\x80b".to_vec()));
}

#[test]
fn members_keep_their_order_and_duplicates() {
    let mut h = h();
    let root = parse(&mut h, r#"{"z": 1, "a": 2, "z": 3}"#);
    assert_eq!(len(&mut h, root), 3);
    let names: Vec<_> = (0..3)
        .map(|k| {
            let n = at(&mut h, root, k);
            (name(&mut h, n).unwrap(), h.f("json_get_float", &[w(n)]))
        })
        .collect();
    assert_eq!(names, [(b"z".to_vec(), 1.0), (b"a".to_vec(), 2.0), (b"z".to_vec(), 3.0)]);
    // A lookup by name finds the first.
    let z = child(&mut h, root, "z");
    assert_eq!(h.f("json_get_float", &[w(z)]), 1.0);
}

#[test]
fn documents_must_be_strict_json() {
    let mut h = h();
    // FTE's parser accepts all of these; strict JSON does not.
    for text in [
        "// comment\n[1]",
        "[1, /* block */ 2]",
        "[1, 2, ]",
        r#"{"a": 1,}"#,
        "[Infinity]",
        "[0x10]",
        "[abc]",
        "[TRUE]",
        "[Null]",
        r#"{"a": 1, "b" , "c": 2}"#,
        r#"["\q"]"#,
    ] {
        assert_eq!(parse(&mut h, text), 0, "{text:?}");
    }
    // A byte-order mark is skipped, and scalars may stand at the root.
    let root = parse(&mut h, "\u{feff}[true, null, false, 1e2]");
    let types: Vec<i32> = (0..4)
        .map(|k| {
            let n = at(&mut h, root, k);
            ty(&mut h, n)
        })
        .collect();
    assert_eq!(types, [TRUE, NULL, FALSE, NUMBER]);
    for (text, want) in [(" 42 ", NUMBER), ("\"text\"", STRING), ("[]", ARRAY), ("{}", OBJECT)] {
        let n = parse(&mut h, text);
        assert_eq!((ty(&mut h, n), len(&mut h, n)), (want, 0), "{text}");
    }
    // Numbers beyond 64-bit integers are doubles.
    let n = parse(&mut h, "123456789012345678901234567890");
    assert_eq!(h.f("json_get_float", &[w(n)]), 1.234_567_9e29);
    assert_eq!(h.i("json_get_integer", &[w(n)]), i32::MIN, "x86 conversion");
}

#[test]
fn invalid_documents_parse_to_null() {
    let mut h = h();
    for text in ["", "   ", "{", "[1 2]", "{} x", "\"unterminated", "[,]", "{\"a\" 1}", "]"] {
        assert_eq!(parse(&mut h, text), 0, "{text:?}");
    }
    let deep = "[".repeat(200) + &"]".repeat(200);
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

/// A document whose tree cannot fit the heap is refused after a pass that allocates nothing,
/// however wide it is; documents that fit still parse.
#[test]
fn documents_too_large_for_the_heap_are_refused_up_front() {
    let limits = qcvm::Limits { heap_bytes: 64 * 1024, ..qcvm::Limits::default() };
    let config = VmConfig { limits, developer: true, ..VmConfig::default() };
    let mut h = Harness::with(Numbering::Csqc, config, Harness::named(&["json_parse"]));
    let wide = format!("[{}]", "0,".repeat(200_000));
    let started = std::time::Instant::now();
    assert_eq!(parse(&mut h, &wide), 0);
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    assert!(!warnings(&h).is_empty(), "developer mode reports the builtin error as a warning");
    assert_ne!(parse(&mut h, "[1, 2, {\"a\": \"b\"}]"), 0);
}

/// Walks a parsed tree, checking each container's children; returns the node count.
fn walk(h: &mut Harness, n: u32, depth: u32) -> usize {
    assert!(depth <= 130);
    let t = ty(h, n);
    if t != OBJECT && t != ARRAY {
        return 1;
    }
    let mut count = 1;
    for k in 0..len(h, n) {
        let c = at(h, n, k);
        assert_ne!(c, 0);
        if t == ARRAY {
            assert_eq!(name(h, c), Some(k.to_string().into_bytes()));
        }
        count += walk(h, c, depth + 1);
    }
    count
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(128))]

    /// Any mix of JSON tokens either parses to a consistent tree or to null.
    #[test]
    fn token_soup_parses_consistently(parts in proptest::collection::vec(
        proptest::sample::select(vec![
            "[", "]", "{", "}", ",", ":", "\"k\"", r#""\u0000""#, "1", "-2.5e3", "null", "true", " ",
        ]),
        0..48,
    )) {
        let text = parts.concat();
        let mut h = h();
        let root = parse(&mut h, &text);
        if root != 0 {
            let nodes = walk(&mut h, root, 0);
            proptest::prop_assert!(nodes >= 1);
        }
    }
}
