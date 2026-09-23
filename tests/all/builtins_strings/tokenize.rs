// SPDX-License-Identifier: MIT OR Apache-2.0

//! tokenize, tokenize_console, tokenizebyseparator, argv and friends (docs/spec/strings.md).

use qcvm::Arg;

use super::{eq, harness, opt_s};
use crate::support::harness::{Harness, b, f, s};

/// Tokenizes with `name` and returns every token with its start/end offsets.
fn toks(h: &mut Harness, name: &str, args: &[Arg<'_>]) -> Vec<(Vec<u8>, f32, f32)> {
    let n = h.f(name, args);
    assert_eq!(h.f("argc", &[]), n);
    (0..n as i32)
        .map(|k| {
            let k = k as f32;
            let text = opt_s(h, "argv", &[f(k)]).expect("token");
            (text, h.f("argv_start_index", &[f(k)]), h.f("argv_end_index", &[f(k)]))
        })
        .collect()
}

fn texts(h: &mut Harness, name: &str, text: &str) -> Vec<String> {
    toks(h, name, &[s(text)]).into_iter().map(|(t, _, _)| String::from_utf8(t).unwrap()).collect()
}

#[test]
fn tokenize_examples() {
    let mut h = harness();
    let mut t = |text: &str| texts(&mut h, "tokenize", text);
    assert_eq!(t("say hello world"), ["say", "hello", "world"]);
    assert_eq!(t("  a   b "), ["a", "b"]);
    assert!(t("").is_empty());
    assert!(t("   ").is_empty());
    assert_eq!(t("\"hello world\" foo"), ["hello world", "foo"]);
    assert_eq!(t("a\"b c\""), ["a\"b", "c\""]);
    assert_eq!(t("\"a\"\"b\""), ["a\"b"]);
    assert_eq!(t("\"\""), [""]);
    assert_eq!(t("f(x,y)"), ["f", "(", "x", ",", "y", ")"]);
    assert_eq!(t("key:value"), ["key", ":", "value"]);
    assert_eq!(t("{a;b}"), ["{", "a", ";", "b", "}"]);
    assert_eq!(t("'1 2 3'"), ["1 2 3"]);
    assert_eq!(t("a // b"), ["a"]);
    assert_eq!(t("a //x\nb"), ["a", "\n", "b"]);
    assert_eq!(t("a\nb"), ["a", "b"]);
    assert_eq!(t("a//b"), ["a//b"]);
    assert_eq!(t("http://foo"), ["http", ":"]);
    assert_eq!(t("a /*b*/ c"), ["a", "/*b*/", "c"]);
    assert_eq!(t("x=1 [2]"), ["x=1", "[", "2", "]"]);
    assert_eq!(t("\"unterminated here"), ["unterminated here"]);
    assert_eq!(t("\\\"a\\n\\x41\\q\\\"b\" c"), ["a\nA?\"b", "c"]);
    assert_eq!(t("\\\"line\\\ncont\""), ["linecont"]);
    assert_eq!(t("\\\"a\\x00b\" c"), ["a", "b\"", "c"]);
}

#[test]
fn single_quotes_are_bounded() {
    // Fixed: FTE reads past the end of an unterminated single-quoted token and mangles `''`.
    let mut h = harness();
    let mut t = |text: &str| toks(&mut h, "tokenize", &[s(text)]);
    assert_eq!(t("'abc"), [(b"abc".to_vec(), 0.0, 4.0)]);
    assert_eq!(t("'a''b' c"), [(b"a'b".to_vec(), 0.0, 6.0), (b"c".to_vec(), 7.0, 8.0)]);
    assert_eq!(t("''''"), [(b"'".to_vec(), 0.0, 4.0)]);
    assert_eq!(t("'"), [(Vec::new(), 0.0, 1.0)]);
}

#[test]
fn tokenize_console_examples() {
    let mut h = harness();
    let mut t = |text: &str| texts(&mut h, "tokenize_console", text);
    assert_eq!(t("say 'hi there'"), ["say", "'hi", "there'"]);
    assert_eq!(t("f(x,y)"), ["f(x,y)"]);
    assert_eq!(t("http://foo"), ["http://foo"]);
    assert_eq!(t("a /*b*/ c"), ["a", "c"]);
    assert_eq!(t("a /*b c"), ["a"]);
    assert_eq!(t("a //x\nb"), ["a", "\n", "b"]);
    assert_eq!(t("\"a b\" c"), ["a b", "c"]);
    assert_eq!(t("\\\"a\\tb\""), ["a\tb"]);
}

#[test]
fn token_offsets() {
    let mut h = harness();
    let t = toks(&mut h, "tokenize", &[s("\"hello world\" foo")]);
    assert_eq!((t[0].1, t[0].2), (0.0, 13.0));
    assert_eq!((t[1].1, t[1].2), (14.0, 17.0));
    let t = toks(&mut h, "tokenize", &[s("  ab \"c d\" e")]);
    let spans: Vec<_> = t.iter().map(|x| (x.1, x.2)).collect();
    assert_eq!(spans, [(2.0, 4.0), (5.0, 10.0), (11.0, 12.0)]);
    let t = toks(&mut h, "tokenize", &[s("a //x\nb")]);
    let spans: Vec<_> = t.iter().map(|x| (x.1, x.2)).collect();
    assert_eq!(spans, [(0.0, 1.0), (2.0, 6.0), (6.0, 7.0)]);
    let t = toks(&mut h, "tokenize", &[s("f(x)")]);
    let spans: Vec<_> = t.iter().map(|x| (x.1, x.2)).collect();
    assert_eq!(spans, [(0.0, 1.0), (1.0, 2.0), (2.0, 3.0), (3.0, 4.0)]);
    let t = toks(&mut h, "tokenize", &[s("\"abc")]);
    assert_eq!((t[0].1, t[0].2), (0.0, 4.0));
    let t = toks(&mut h, "tokenize_console", &[s("a /*b*/ c")]);
    assert_eq!((t[1].1, t[1].2), (2.0, 9.0));
}

#[test]
fn long_tokens_continue_as_the_next_token() {
    let mut h = harness();
    let word = "w".repeat(65_540);
    let t = toks(&mut h, "tokenize", &[s(&word)]);
    assert_eq!(t.len(), 2);
    assert_eq!(t[0].0.len(), 65_535);
    assert_eq!((t[1].0.len(), t[1].1, t[1].2), (5, 65_535.0, 65_540.0));
}

#[test]
fn argv_indexing() {
    let mut h = harness();
    assert_eq!(h.f("tokenize", &[s("a b c")]), 3.0);
    eq(opt_s(&mut h, "argv", &[f(0.0)]).unwrap(), "a");
    eq(opt_s(&mut h, "argv", &[f(2.9)]).unwrap(), "c");
    eq(opt_s(&mut h, "argv", &[f(-1.0)]).unwrap(), "c");
    eq(opt_s(&mut h, "argv", &[f(-3.0)]).unwrap(), "a");
    assert_eq!(opt_s(&mut h, "argv", &[f(3.0)]), None);
    assert_eq!(opt_s(&mut h, "argv", &[f(-4.0)]), None);
    assert_eq!(opt_s(&mut h, "argv", &[f(f32::NAN)]), None);
    assert_eq!(h.f("argv_start_index", &[f(-1.0)]), 4.0);
    assert_eq!(h.f("argv_end_index", &[f(-1.0)]), 5.0);
    assert_eq!(h.f("argv_start_index", &[f(3.0)]), -1.0);
    assert_eq!(h.f("argv_end_index", &[f(-4.0)]), -1.0);
    // Each call returns a new copy.
    let r1 = h.call("argv", &[f(0.0)]).unwrap().str_ref();
    let r2 = h.call("argv", &[f(0.0)]).unwrap().str_ref();
    assert_ne!(r1, r2);
    // An empty token is a non-null empty string.
    h.f("tokenize", &[s("\"\"")]);
    eq(opt_s(&mut h, "argv", &[f(0.0)]).unwrap(), "");
    // A new tokenize replaces the list.
    assert_eq!(h.f("tokenize", &[s("")]), 0.0);
    assert_eq!(h.f("argc", &[]), 0.0);
    assert_eq!(opt_s(&mut h, "argv", &[f(0.0)]), None);
}

#[test]
fn tokenizebyseparator_examples() {
    let mut h = harness();
    let mut t = |args: &[Arg<'_>]| -> Vec<(String, f32, f32)> {
        toks(&mut h, "tokenizebyseparator", args)
            .into_iter()
            .map(|(t, a, z)| (String::from_utf8(t).unwrap(), a, z))
            .collect()
    };
    let names = |v: Vec<(String, f32, f32)>| v.into_iter().map(|x| x.0).collect::<Vec<_>>();
    assert_eq!(names(t(&[s("a,b,,c"), s(",")])), ["a", "b", "", "c"]);
    assert_eq!(names(t(&[s("a,"), s(",")])), ["a", ""]);
    assert_eq!(names(t(&[s(","), s(",")])), ["", ""]);
    assert_eq!(names(t(&[s("abc"), s(",")])), ["abc"]);
    assert!(t(&[s(""), s(",")]).is_empty());
    assert_eq!(names(t(&[s("a::b:c"), s("::"), s(":")])), ["a", "b", "c"]);
    assert_eq!(names(t(&[s("a::b"), s(":"), s("::")])), ["a", "", "b"]);
    assert_eq!(names(t(&[s("abc")])), ["abc"]);
    assert_eq!(names(t(&[s(" a , b "), s(",")])), [" a ", " b "]);
    // Fixed: an empty separator made FTE loop forever; it is ignored.
    assert_eq!(names(t(&[s("a,b"), s(""), s(",")])), ["a", "b"]);
    assert_eq!(names(t(&[s("ab"), s("")])), ["ab"]);
    let spans = t(&[s("a::b:c"), s("::"), s(":")]);
    let spans: Vec<_> = spans.into_iter().map(|x| (x.1, x.2)).collect();
    assert_eq!(spans, [(0.0, 1.0), (3.0, 4.0), (5.0, 6.0)]);
    // Up to seven separators.
    let seven: Vec<Arg<'_>> = std::iter::once(s("1a2b3c4d5e6f7g8"))
        .chain(["a", "b", "c", "d", "e", "f", "g"].map(s))
        .collect();
    assert_eq!(names(t(&seven)), ["1", "2", "3", "4", "5", "6", "7", "8"]);
    // Bytes are compared as they are.
    assert_eq!(t(&[b(b"x\xFFy"), b(b"\xFF")]).len(), 2);
}
