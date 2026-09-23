// SPDX-License-Identifier: MIT OR Apache-2.0

//! String buffers (docs/spec/builtins.md).
//!
//! A buffer is a vector of optional strings: its size is the highest set index + 1 and unset
//! entries (holes) read as null. Handles are floats counted from 1. Invalid handles are ignored;
//! where FTE then leaves the return value untouched, qcvm returns 0 or null. Entries are copies,
//! so they outlive the temp strings they came from.

use crate::builtins::Builtins;
use crate::error::VmError;
use crate::host::Host;
use crate::vm::Vm;
use crate::vm::num::f2i;

use super::util::{arg_int, opt_f32};

/// Registers this module's builtins.
pub(crate) fn register<H: Host>(b: &mut Builtins<H>) {
    b.set("buf_create", buf_create::<H>);
    b.set("buf_del", buf_del::<H>);
    b.set("buf_getsize", buf_getsize::<H>);
    b.set("buf_copy", buf_copy::<H>);
    b.set("buf_sort", buf_sort::<H>);
    b.set("buf_implode", buf_implode::<H>);
    b.set("bufstr_get", bufstr_get::<H>);
    b.set("bufstr_set", bufstr_set::<H>);
    b.set("bufstr_add", bufstr_add::<H>);
    b.set("bufstr_free", bufstr_free::<H>);
    b.set("bufstr_find", bufstr_find::<H>);
    b.set("buf_cvarlist", buf_cvarlist::<H>);
    b.set("buf_loadfile", buf_loadfile::<H>);
}

/// FTE refuses `bufstr_set` indices above this.
const MAX_SET_INDEX: usize = 1 << 20;

/// One string buffer.
#[derive(Clone, Debug, Default)]
struct StrBuf {
    /// The entries; the length is the buffer's size.
    strings: Vec<Option<Box<[u8]>>>,
}

impl StrBuf {
    /// Stores `s` at `i`, growing the buffer.
    fn set(&mut self, i: usize, s: &[u8]) {
        if i >= self.strings.len() {
            self.strings.resize(i.saturating_add(1), None);
        }
        if let Some(slot) = self.strings.get_mut(i) {
            *slot = Some(s.into());
        }
    }
}

/// The VM's string buffers.
#[derive(Clone, Debug, Default)]
pub(crate) struct Buffers {
    /// Buffers by handle − 1.
    bufs: Vec<Option<StrBuf>>,
}

impl Buffers {
    fn get(&self, i: usize) -> Option<&StrBuf> {
        self.bufs.get(i)?.as_ref()
    }

    fn get_mut(&mut self, i: usize) -> Option<&mut StrBuf> {
        self.bufs.get_mut(i)?.as_mut()
    }
}

/// The buffer index argument `i` designates, if valid (FTE subtracts 1 from the float handle,
/// then truncates).
fn buf_arg<H: Host>(vm: &Vm<H>, i: usize) -> Option<usize> {
    let index = usize::try_from(f2i(vm.arg_f32(i) - 1.0)).ok()?;
    vm.core.std.bufs.get(index).map(|_| index)
}

/// An entry index argument (negative indices are invalid).
fn index_arg<H: Host>(vm: &Vm<H>, i: usize) -> Option<usize> {
    usize::try_from(arg_int(vm, i)).ok()
}

/// The largest number of entries a buffer may hold.
fn entry_limit<H: Host>(vm: &Vm<H>) -> usize {
    usize::try_from(vm.core.config.limits.string_buffer_entries).unwrap_or(usize::MAX)
}

/// `strbuf buf_create(optional string type = "string", optional float flags = 1)`: a new empty
/// buffer, or −1 for another type or when the buffer limit is reached.
pub fn buf_create<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let is_string = vm.argc() == 0 || vm.arg_str(0).eq_ignore_ascii_case(b"string");
    let limit = usize::try_from(vm.core.config.limits.string_buffers).unwrap_or(usize::MAX);
    let bufs = &mut vm.core.std.bufs.bufs;
    let live = bufs.iter().filter(|b| b.is_some()).count();
    if !is_string || live >= limit {
        vm.ret_f32(-1.0);
        return Ok(());
    }
    let index = match bufs.iter().position(Option::is_none) {
        Some(i) => {
            if let Some(slot) = bufs.get_mut(i) {
                *slot = Some(StrBuf::default());
            }
            i
        }
        None => {
            bufs.push(Some(StrBuf::default()));
            bufs.len().saturating_sub(1)
        }
    };
    vm.ret_f32(index.saturating_add(1) as f32);
    Ok(())
}

/// `void buf_del(strbuf buf)`: deletes a buffer.
pub fn buf_del<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    if let Some(i) = buf_arg(vm, 0)
        && let Some(slot) = vm.core.std.bufs.bufs.get_mut(i)
    {
        *slot = None;
    }
    Ok(())
}

/// `float buf_getsize(strbuf buf)`: the highest set index + 1 (0 for an invalid handle).
pub fn buf_getsize<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let size = buf_arg(vm, 0).and_then(|i| vm.core.std.bufs.get(i)).map_or(0, |b| b.strings.len());
    vm.ret_f32(size as f32);
    Ok(())
}

/// `void buf_copy(strbuf from, strbuf to)`: replaces `to`'s contents with a copy of `from`'s
/// (holes included). Both must be valid and different.
pub fn buf_copy<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let (Some(from), Some(to)) = (buf_arg(vm, 0), buf_arg(vm, 1)) else { return Ok(()) };
    if from == to {
        return Ok(());
    }
    let copy = vm.core.std.bufs.get(from).cloned();
    if let (Some(copy), Some(dst)) = (copy, vm.core.std.bufs.get_mut(to)) {
        *dst = copy;
    }
    Ok(())
}

/// `void buf_sort(strbuf buf, float prefixlen, float backward)`: removes the holes, then sorts
/// by the first `prefixlen` bytes (all if ≤ 0), descending if `backward`.
pub fn buf_sort<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let Some(i) = buf_arg(vm, 0) else { return Ok(()) };
    let prefix = arg_int(vm, 1);
    let prefix =
        if prefix <= 0 { usize::MAX } else { usize::try_from(prefix).unwrap_or(usize::MAX) };
    let backward = vm.arg_f32(2) != 0.0;
    let Some(buf) = vm.core.std.bufs.get_mut(i) else { return Ok(()) };
    let mut strings: Vec<Box<[u8]>> =
        std::mem::take(&mut buf.strings).into_iter().flatten().collect();
    strings.sort_by(|a, b| {
        let ord = prefix_of(a, prefix).cmp(prefix_of(b, prefix));
        if backward { ord.reverse() } else { ord }
    });
    buf.strings = strings.into_iter().map(Some).collect();
    Ok(())
}

/// The first `n` bytes of `s` (`strncmp` compares only these).
fn prefix_of(s: &[u8], n: usize) -> &[u8] {
    s.get(..s.len().min(n)).unwrap_or_default()
}

/// `string buf_implode(strbuf buf, string glue)`: the set entries joined, with `glue` between
/// them (only once the output is non-empty). Null for an invalid handle.
pub fn buf_implode<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let Some(i) = buf_arg(vm, 0) else {
        vm.ret_raw([0; 3]);
        return Ok(());
    };
    let glue = vm.arg_str(1).to_vec();
    let mut out = Vec::new();
    for s in
        vm.core.std.bufs.get(i).map(|b| b.strings.as_slice()).unwrap_or_default().iter().flatten()
    {
        if !out.is_empty() {
            out.extend_from_slice(&glue);
        }
        out.extend_from_slice(s);
    }
    vm.ret_str(&out)
}

/// `string bufstr_get(strbuf buf, float index)`: a copy of the entry, or null for holes and
/// invalid indices or handles.
pub fn bufstr_get<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let entry = buf_arg(vm, 0)
        .zip(index_arg(vm, 1))
        .and_then(|(b, i)| vm.core.std.bufs.get(b)?.strings.get(i)?.clone());
    match entry {
        Some(s) => vm.ret_str(&s),
        None => {
            vm.ret_raw([0; 3]);
            Ok(())
        }
    }
}

/// `void bufstr_set(strbuf buf, float index, string s)`: stores a copy of `s` at `index`,
/// growing the buffer. Indices above 1,048,576 (or beyond the configured entry limit) are refused
/// with a warning.
pub fn bufstr_set<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let (Some(b), Some(i)) = (buf_arg(vm, 0), index_arg(vm, 1)) else { return Ok(()) };
    if i > MAX_SET_INDEX || i >= entry_limit(vm) {
        vm.warn("bufstr_set: index outside sanity range");
        return Ok(());
    }
    let s = vm.arg_str(2).to_vec();
    if let Some(buf) = vm.core.std.bufs.get_mut(b) {
        buf.set(i, &s);
    }
    Ok(())
}

/// `float bufstr_add(strbuf buf, string s, float ordered)`: stores a copy of `s` at the end (if
/// `ordered`) or in the first hole, and returns the index. Returns 0 for an invalid handle and
/// −1 (with a warning) when the buffer is full.
pub fn bufstr_add<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let Some(b) = buf_arg(vm, 0) else {
        vm.ret_f32(0.0);
        return Ok(());
    };
    let s = vm.arg_str(1).to_vec();
    let ordered = arg_int(vm, 2) != 0;
    let limit = entry_limit(vm);
    let Some(buf) = vm.core.std.bufs.get_mut(b) else { return Ok(()) };
    let len = buf.strings.len();
    let i = if ordered { len } else { buf.strings.iter().position(Option::is_none).unwrap_or(len) };
    if i >= limit {
        vm.warn("bufstr_add: string buffer is full");
        vm.ret_f32(-1.0);
        return Ok(());
    }
    buf.set(i, &s);
    vm.ret_f32(i as f32);
    Ok(())
}

/// `void bufstr_free(strbuf buf, float index)`: turns the entry into a hole (the size does not
/// change).
pub fn bufstr_free<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let (Some(b), Some(i)) = (buf_arg(vm, 0), index_arg(vm, 1)) else { return Ok(()) };
    if let Some(slot) = vm.core.std.bufs.get_mut(b).and_then(|buf| buf.strings.get_mut(i)) {
        *slot = None;
    }
    Ok(())
}

/// FTE's `wildcmp`: `?` matches any one byte, `*` any run of bytes other than `/` and `\`, and
/// letters compare ASCII case-insensitively.
pub(crate) fn wildcmp(pattern: &[u8], s: &[u8]) -> bool {
    // ok[j]: pattern[..i] matches s[..j]; computed row by row over the pattern.
    let mut ok = vec![false; s.len().saturating_add(1)];
    if let Some(first) = ok.first_mut() {
        *first = true;
    }
    for &p in pattern {
        let mut next = vec![false; ok.len()];
        if p == b'*' {
            let mut run = false;
            for (j, n) in next.iter_mut().enumerate() {
                // `*` extends over s[j - 1] only if that byte is not a separator.
                let extend = j > 0
                    && run
                    && s.get(j.wrapping_sub(1)).is_some_and(|&c| c != b'/' && c != b'\\');
                run = ok.get(j).copied().unwrap_or(false) || extend;
                *n = run;
            }
        } else {
            for (j, n) in next.iter_mut().enumerate().skip(1) {
                let c = s.get(j.wrapping_sub(1)).copied().unwrap_or(0);
                *n = ok.get(j.wrapping_sub(1)).copied().unwrap_or(false)
                    && (p == b'?' || p.eq_ignore_ascii_case(&c));
            }
        }
        ok = next;
    }
    ok.last().copied().unwrap_or(false)
}

/// Whether entry `s` matches `pattern` under `bufstr_find` rule `rule`.
fn matches_rule(s: &[u8], pattern: &[u8], rule: i32) -> bool {
    match rule {
        1 => s == pattern,
        2 => s.starts_with(pattern),
        3 => s.ends_with(pattern),
        4 => pattern.is_empty() || s.windows(pattern.len()).any(|w| w == pattern),
        _ => wildcmp(pattern, s),
    }
}

/// `float bufstr_find(strbuf buf, string pattern, float rule, float start = 0, float step = 1)`:
/// the first index `start + k·step` whose entry matches, or −1. Rules: 1 exact, 2 prefix,
/// 3 suffix, 4 substring, anything else (0, 5) a wildcard pattern.
pub fn bufstr_find<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    vm.ret_f32(-1.0);
    let Some(b) = buf_arg(vm, 0) else { return Ok(()) };
    let rule = arg_int(vm, 2);
    let start = f2i(opt_f32(vm, 3, 0.0));
    let step = f2i(opt_f32(vm, 4, 1.0));
    let (Ok(start), Ok(step)) = (usize::try_from(start), usize::try_from(step)) else {
        return Ok(());
    };
    if step == 0 {
        return Ok(());
    }
    let pattern = vm.arg_str(1).to_vec();
    let strings = vm.core.std.bufs.get(b).map(|b| b.strings.as_slice()).unwrap_or_default();
    let found = (start..strings.len()).step_by(step).find(|&i| {
        strings.get(i).and_then(Option::as_deref).is_some_and(|s| matches_rule(s, &pattern, rule))
    });
    if let Some(i) = found {
        vm.ret_f32(i as f32);
    }
    Ok(())
}

/// `void buf_cvarlist(strbuf buf, string pattern, string antipattern)`: replaces the buffer's
/// contents with the sorted names of the cvars [`Host::cvar_list`] reports.
pub fn buf_cvarlist<H: Host>(vm: &mut Vm<H>, host: &mut H) -> Result<(), VmError> {
    let Some(b) = buf_arg(vm, 0) else { return Ok(()) };
    let mut names = host.cvar_list(vm.arg_str(1), vm.arg_str(2));
    names.sort();
    names.truncate(entry_limit(vm));
    if let Some(buf) = vm.core.std.bufs.get_mut(b) {
        buf.strings = names.into_iter().map(|n| Some(n.into_boxed_slice())).collect();
    }
    Ok(())
}

/// Splits file contents into lines like FTE's `VFS_GETS`: at `\n`, dropping one trailing `\r`
/// from each terminated line; a final unterminated line is kept as is. Lines end at a NUL.
fn file_lines(data: &[u8]) -> Vec<&[u8]> {
    let mut lines = Vec::new();
    let mut rest = data;
    while !rest.is_empty() {
        let (line, next) = match rest.iter().position(|&c| c == b'\n') {
            Some(n) => {
                let line = rest.get(..n).unwrap_or_default();
                (line.strip_suffix(b"\r").unwrap_or(line), rest.get(n.saturating_add(1)..))
            }
            None => (rest, None),
        };
        let end = line.iter().position(|&c| c == 0).unwrap_or(line.len());
        lines.push(line.get(..end).unwrap_or_default());
        rest = next.unwrap_or_default();
    }
    lines
}

/// `float buf_loadfile(string path, strbuf buf)`: appends each line of the file
/// ([`Host::read_file`]) to the buffer. Returns 1 if the file was read, else 0.
pub fn buf_loadfile<H: Host>(vm: &mut Vm<H>, host: &mut H) -> Result<(), VmError> {
    vm.ret_f32(0.0);
    let Some(b) = buf_arg(vm, 1) else { return Ok(()) };
    let Some(data) = host.read_file(vm.arg_str(0)) else { return Ok(()) };
    let limit = entry_limit(vm);
    if let Some(buf) = vm.core.std.bufs.get_mut(b) {
        for line in file_lines(&data) {
            let i = buf.strings.len();
            if i >= limit {
                break;
            }
            buf.set(i, line);
        }
    }
    vm.ret_f32(1.0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcards() {
        assert!(wildcmp(b"*", b""));
        assert!(wildcmp(b"*", b"abc"));
        assert!(wildcmp(b"a*c", b"ABC"));
        assert!(wildcmp(b"a?c", b"axc"));
        assert!(!wildcmp(b"a?c", b"ac"));
        assert!(!wildcmp(b"a*", b"a/b"));
        assert!(wildcmp(b"a*/b", b"ax/b"));
        assert!(wildcmp(b"a?b", b"a/b"));
        assert!(wildcmp(b"**x", b"abx"));
        assert!(!wildcmp(b"", b"x"));
        assert!(wildcmp(b"", b""));
    }

    #[test]
    fn lines() {
        assert_eq!(file_lines(b"a\r\nb\n\nc\r"), [&b"a"[..], b"b", b"", b"c\r"]);
        assert_eq!(file_lines(b"x\0y\n"), [&b"x"[..]]);
        assert!(file_lines(b"").is_empty());
    }
}
