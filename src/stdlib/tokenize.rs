// SPDX-License-Identifier: MIT OR Apache-2.0

//! tokenize, tokenize_console, tokenizebyseparator, argv and friends (docs/spec/strings.md).
//!
//! Each tokenizer replaces the VM's token list (FTE keeps one per process; qcvm one per VM).
//! Tokens remember the byte offsets they came from, for `argv_start_index`/`argv_end_index`.

use crate::builtins::Builtins;
use crate::error::VmError;
use crate::host::Host;
use crate::stdlib::util::arg_int;
use crate::value::StrRef;
use crate::vm::Vm;

/// Registers this module's builtins.
pub(crate) fn register<H: Host>(b: &mut Builtins<H>) {
    b.set("tokenize", tokenize::<H>);
    b.set("tokenize_console", tokenize_console::<H>);
    b.set("tokenizebyseparator", tokenizebyseparator::<H>);
    b.set("argv", argv::<H>);
    b.set("argc", argc::<H>);
    b.set("argv_start_index", argv_start_index::<H>);
    b.set("argv_end_index", argv_end_index::<H>);
}

/// One token: its text and the byte range of the input it was read from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Token {
    pub(crate) text: Vec<u8>,
    pub(crate) start: usize,
    pub(crate) end: usize,
}

/// The current token list (per VM; FTE shares one per process).
#[derive(Clone, Debug, Default)]
pub(crate) struct Tokens {
    pub(crate) list: Vec<Token>,
}

impl Tokens {
    /// The token at a QuakeC index: negative indices count from the end.
    fn get(&self, index: i32) -> Option<&Token> {
        let n = i32::try_from(self.list.len()).unwrap_or(i32::MAX);
        let i = if index < 0 { index.wrapping_add(n) } else { index };
        usize::try_from(i).ok().and_then(|i| self.list.get(i))
    }
}

/// Longest token FTE's 64 KiB token buffer holds (a longer word continues as the next token).
const TOKEN_MAX: usize = 65_535;
/// Longest `\"…"` C-string token.
const CSTRING_MAX: usize = 65_534;

/// Characters that are tokens by themselves in QuakeC mode (and end words).
fn is_single(c: u8) -> bool {
    matches!(c, b'\n' | b'{' | b'}' | b'(' | b')' | b'[' | b']' | b'\'' | b':' | b',' | b';')
}

/// Parses a `\"…"` token starting at the `"` (`at`): C escapes (`\n \t \r \xHH \" \\ \' \$`,
/// backslash-newline continues the line, anything else is `?`); a `\x` that yields NUL or `"`
/// ends the token. Returns the text and the end offset.
fn parse_cstring(s: &[u8], at: usize) -> (Vec<u8>, usize) {
    let mut out = Vec::new();
    let mut p = at.saturating_add(1);
    loop {
        if out.len() >= CSTRING_MAX {
            return (out, p);
        }
        let Some(&c) = s.get(p) else { return (out, s.len()) };
        p = p.saturating_add(1);
        let c = if c == b'\\' {
            let Some(&e) = s.get(p) else { return (out, s.len()) };
            p = p.saturating_add(1);
            match e {
                b'\r' | b'\n' => {
                    if e == b'\r' && s.get(p) == Some(&b'\n') {
                        p = p.saturating_add(1);
                    }
                    continue;
                }
                b'n' => b'\n',
                b't' => b'\t',
                b'r' => b'\r',
                b'x' => {
                    let mut v = 0u8;
                    for _ in 0..2 {
                        let Some(d) = s.get(p).and_then(|&h| char::from(h).to_digit(16)) else {
                            break;
                        };
                        v = v.wrapping_shl(4) | d as u8;
                        p = p.saturating_add(1);
                    }
                    v
                }
                b'$' | b'\\' | b'\'' => e,
                b'"' => {
                    out.push(b'"');
                    continue;
                }
                _ => b'?',
            }
        } else {
            c
        };
        if c == b'"' || c == 0 {
            return (out, p);
        }
        out.push(c);
    }
}

/// Parses one token at `p` (FTE's `COM_StringParse`): `None` when only white space and comments
/// remain. `qc` selects tokenize's QuakeC mode (single-character tokens and single-quoted
/// strings) over the console's (`/* */` comments).
fn parse_token(s: &[u8], mut p: usize, qc: bool) -> Option<(Vec<u8>, usize)> {
    loop {
        while s.get(p).is_some_and(|&c| c <= b' ' && c != b'\n') {
            p = p.saturating_add(1);
        }
        let c = *s.get(p)?;
        if c == b'\n' {
            return Some((vec![b'\n'], p.saturating_add(1)));
        }
        let next = s.get(p.saturating_add(1)).copied();
        if c == b'/' && next == Some(b'/') {
            while s.get(p).is_some_and(|&c| c != b'\n') {
                p = p.saturating_add(1);
            }
            continue;
        }
        if !qc && c == b'/' && next == Some(b'*') {
            p = p.saturating_add(2);
            p = s
                .get(p..)
                .and_then(|r| r.windows(2).position(|w| w == b"*/"))
                .map_or(s.len(), |k| p.saturating_add(k).saturating_add(2));
            continue;
        }
        break;
    }
    let c = *s.get(p)?;
    let next = s.get(p.saturating_add(1)).copied();
    if c == b'\\' && next == Some(b'"') {
        return Some(parse_cstring(s, p.saturating_add(1)));
    }
    let mut out = Vec::new();
    if c == b'"' || (qc && c == b'\'') {
        // Quoted: a doubled quote is a literal quote; the end of the string ends the token.
        p = p.saturating_add(1);
        loop {
            if out.len() >= TOKEN_MAX {
                return Some((out, p));
            }
            let Some(&d) = s.get(p) else { return Some((out, s.len())) };
            p = p.saturating_add(1);
            if d == c {
                if s.get(p) != Some(&c) {
                    return Some((out, p));
                }
                p = p.saturating_add(1);
            }
            out.push(d);
        }
    }
    if qc && is_single(c) {
        return Some((vec![c], p.saturating_add(1)));
    }
    loop {
        if out.len() >= TOKEN_MAX {
            return Some((out, p));
        }
        out.push(s.get(p).copied().unwrap_or(0));
        p = p.saturating_add(1);
        match s.get(p) {
            Some(&c) if c > b' ' && !(qc && is_single(c)) => {}
            _ => return Some((out, p)),
        }
    }
}

/// Splits `s` like FTE's `tokenize` (`qc`) or `tokenize_console`.
pub(crate) fn split(s: &[u8], qc: bool) -> Vec<Token> {
    let mut list = Vec::new();
    let mut p = 0usize;
    loop {
        while s.get(p).is_some_and(|&c| c <= b' ') {
            p = p.saturating_add(1);
        }
        if p >= s.len() {
            break;
        }
        let Some((text, end)) = parse_token(s, p, qc) else { break };
        list.push(Token { text, start: p, end });
        p = end;
    }
    list
}

/// Splits `s` at any of `seps` (tried in order at each byte; empty separators are ignored).
/// An empty string has no tokens; otherwise the end of the string ends the last token.
pub(crate) fn split_by(s: &[u8], seps: &[&[u8]]) -> Vec<Token> {
    let mut list = Vec::new();
    if s.is_empty() {
        return list;
    }
    let token = |start: usize, end: usize| Token {
        text: s.get(start..end).unwrap_or_default().to_vec(),
        start,
        end,
    };
    let (mut start, mut p) = (0usize, 0usize);
    while p < s.len() {
        let rest = s.get(p..).unwrap_or_default();
        match seps.iter().find(|sep| !sep.is_empty() && rest.starts_with(sep)) {
            Some(sep) => {
                list.push(token(start, p));
                p = p.saturating_add(sep.len());
                start = p;
            }
            None => p = p.saturating_add(1),
        }
    }
    list.push(token(start, s.len()));
    list
}

fn set_tokens<H: Host>(vm: &mut Vm<H>, list: Vec<Token>) {
    let n = list.len();
    vm.core.std.tokens.list = list;
    vm.ret_f32(n as f32);
}

/// `float tokenize(string)`: splits into tokens the QuakeC way: white space separates,
/// `"…"`, `'…'` and C-style `\"…"` strings are single tokens, `{ } ( ) [ ] : , ;` are tokens by
/// themselves, `//` starts a comment (a newline after it is a token). Returns the count.
///
/// # Errors
/// Never.
pub fn tokenize<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let list = split(vm.arg_str(0), true);
    set_tokens(vm, list);
    Ok(())
}

/// `float tokenize_console(string)`: splits like the console: white space separates, `"…"` and
/// `\"…"` strings are single tokens, `//` and `/* */` are comments. Returns the count.
///
/// # Errors
/// Never.
pub fn tokenize_console<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let list = split(vm.arg_str(0), false);
    set_tokens(vm, list);
    Ok(())
}

/// `float tokenizebyseparator(string s, string separator...)`: splits at up to seven separators
/// (no trimming; empty tokens are kept). Returns the count.
///
/// # Errors
/// Never.
pub fn tokenizebyseparator<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let seps: Vec<&[u8]> = (1..vm.argc().min(8)).map(|i| vm.arg_str(i)).collect();
    let list = split_by(vm.arg_str(0), &seps);
    set_tokens(vm, list);
    Ok(())
}

/// `string argv(float index)`: a copy of a token (negative indices count from the end), or
/// null when out of range.
///
/// # Errors
/// Only if the result string cannot be allocated.
pub fn argv<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let index = arg_int(vm, 0);
    match vm.core.std.tokens.get(index).map(|t| t.text.clone()) {
        Some(text) => vm.ret_str(&text),
        None => {
            vm.ret_str_ref(StrRef(0));
            Ok(())
        }
    }
}

/// `float argc()`: the number of tokens.
///
/// # Errors
/// Never.
pub fn argc<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let n = vm.core.std.tokens.list.len();
    vm.ret_f32(n as f32);
    Ok(())
}

/// `float argv_start_index(float index)`: the byte offset where a token started, or -1.
///
/// # Errors
/// Never.
pub fn argv_start_index<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let index = arg_int(vm, 0);
    let r = vm.core.std.tokens.get(index).map_or(-1.0, |t| t.start as f32);
    vm.ret_f32(r);
    Ok(())
}

/// `float argv_end_index(float index)`: the byte offset just past a token, or -1.
///
/// # Errors
/// Never.
pub fn argv_end_index<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let index = arg_int(vm, 0);
    let r = vm.core.std.tokens.get(index).map_or(-1.0, |t| t.end as f32);
    vm.ret_f32(r);
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn texts(list: &[Token]) -> Vec<&[u8]> {
        list.iter().map(|t| t.text.as_slice()).collect()
    }

    #[test]
    fn qc_mode() {
        assert_eq!(texts(&split(b"say hello world", true)), [&b"say"[..], b"hello", b"world"]);
        assert_eq!(texts(&split(b"f(x,y)", true)).len(), 6);
        assert_eq!(texts(&split(b"a //x\nb", true)), [&b"a"[..], b"\n", b"b"]);
        assert_eq!(texts(&split(b"'a''b' c", true)), [&b"a'b"[..], b"c"]);
        assert_eq!(texts(&split(b"'abc", true)), [&b"abc"[..]]);
        assert_eq!(texts(&split(b"\\\"a\\tb\\x41\\q\" c", true)), [&b"a\tbA?"[..], b"c"]);
        assert_eq!(texts(&split(b"\\\"a\\x22b\"", true)), [&b"a"[..], b"b\""]);
    }

    #[test]
    fn long_words_split() {
        let s = vec![b'a'; TOKEN_MAX + 3];
        let list = split(&s, false);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].text.len(), TOKEN_MAX);
        assert_eq!((list[1].start, list[1].end), (TOKEN_MAX, TOKEN_MAX + 3));
    }

    #[test]
    fn separators() {
        assert_eq!(texts(&split_by(b"a,b,,c", &[b","])), [&b"a"[..], b"b", b"", b"c"]);
        assert_eq!(texts(&split_by(b"a::b", &[b":", b"::"])), [&b"a"[..], b"", b"b"]);
        assert_eq!(texts(&split_by(b"ab", &[b""])), [&b"ab"[..]]);
    }
}
