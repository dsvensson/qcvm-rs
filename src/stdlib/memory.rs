// SPDX-License-Identifier: MIT OR Apache-2.0

//! QuakeC heap and pointer builtins, base64 (docs/spec/builtins.md).
//!
//! `memalloc` blocks live in the VM heap (region H), so a pointer is `h_base` plus the block's
//! offset and works with every pointer opcode. The copy and fill builtins also accept temp
//! buffers (`createbuffer`, temp strings), which grow when written past their end, as in FTE.

use crate::builtins::Builtins;
use crate::bytes::usize_from;
use crate::error::{ErrorKind, VmError, WarningKind};
use crate::host::Host;
use crate::vm::Vm;
use crate::vm::core::Core;
use crate::vm::memory::{Loc, WriteError};
use crate::vm::num::f2i;
use crate::vm::strings::{MAX_TEMP_GROWTH, StrKind, classify};

use super::introspect::{set_arg_word, soft_error};

/// Registers this module's builtins.
pub(crate) fn register<H: Host>(b: &mut Builtins<H>) {
    b.set("memalloc", memalloc::<H>);
    b.set("memfree", memfree::<H>);
    b.set("memrealloc", memrealloc::<H>);
    b.set("memcpy", memcpy::<H>);
    b.set("memfill8", memfill8::<H>);
    b.set("memgetval", memgetval::<H>);
    b.set("memsetval", memsetval::<H>);
    b.set("memptradd", memptradd::<H>);
    b.set("memcmp", memcmp::<H>);
    b.set("createbuffer", createbuffer::<H>);
    b.set("base64encode", base64encode::<H>);
    b.set("base64decode", base64decode::<H>);
}

/// Largest block `memalloc` hands out (FTE's limit).
const MAX_ALLOC: i32 = 0x0100_0000;

// ---- heap -------------------------------------------------------------------------------------

/// Allocates `n` zeroed bytes on the VM heap; returns the pointer.
pub(crate) fn heap_alloc(core: &mut Core, n: u32) -> Option<u32> {
    let off = core.mem.heap.alloc(n)?;
    core.mem.h_base.checked_add(off)
}

/// The heap block offset of pointer `p`, if `p` points into the heap.
fn heap_offset(core: &Core, p: u32) -> Option<u32> {
    p.checked_sub(core.mem.h_base)
}

/// Frees the heap block starting at pointer `p`; `false` if there is none.
pub(crate) fn heap_free(core: &mut Core, p: u32) -> bool {
    heap_offset(core, p).is_some_and(|off| core.mem.heap.free(off))
}

/// Writes bytes into the heap at pointer `p` (which must lie inside it).
fn heap_write(core: &mut Core, p: u32, bytes: &[u8]) -> bool {
    let Some(off) = heap_offset(core, p) else { return false };
    let start = usize_from(off);
    let Some(end) = start.checked_add(bytes.len()) else { return false };
    match core.mem.heap.data.get_mut(start..end) {
        Some(dst) => {
            dst.copy_from_slice(bytes);
            true
        }
        None => false,
    }
}

/// The `memalloc`/`memrealloc` size rule: 0 means 1; negative or above 16 MiB is refused.
fn alloc_size(size: i32) -> Option<u32> {
    let size = if size == 0 { 1 } else { size };
    if size > MAX_ALLOC { None } else { u32::try_from(size).ok() }
}

// ---- pointer ranges ---------------------------------------------------------------------------

/// `ptr + ofs` for a linear pointer.
fn linear(p: u32, ofs: i32) -> Option<u32> {
    u32::try_from(i64::from(p).checked_add(i64::from(ofs))?).ok()
}

/// Reads `n` bytes at `ptr + ofs` (a linear pointer or a temp buffer, which must be large
/// enough).
pub(crate) fn read_range(core: &Core, ptr: u32, ofs: i32, n: usize) -> Option<Vec<u8>> {
    match classify(ptr) {
        StrKind::Temp(slot) => {
            let start = usize::try_from(ofs).ok()?;
            let data = core.strings.temp_raw(slot)?;
            data.get(start..start.checked_add(n)?).map(<[u8]>::to_vec)
        }
        StrKind::Static(_) => None,
        StrKind::Linear(p) => {
            let addr = linear(p, ofs)?;
            if addr == 0 && n > 1 {
                return None;
            }
            let loc = core.mem.locate(addr, u32::try_from(n).ok()?)?;
            let (data, o) = match loc {
                Loc::S(o) => (&core.mem.s, o),
                Loc::E(o, _) => (&core.mem.e, o),
                Loc::H(o) => (&core.mem.heap.data, o),
            };
            data.get(o..o.checked_add(n)?).map(<[u8]>::to_vec)
        }
    }
}

/// A validated destination for a write.
#[derive(Clone, Copy, Debug)]
enum Dest {
    Linear(u32),
    Temp { slot: u32, start: usize },
}

/// Validates a write of `n` bytes at `ptr + ofs`: linear memory (not address 0), or a temp
/// buffer that may grow up to 1 MiB.
fn dest(core: &Core, ptr: u32, ofs: i32, n: usize) -> Option<Dest> {
    match classify(ptr) {
        StrKind::Temp(slot) => {
            core.strings.temp_raw(slot)?;
            let start = usize::try_from(ofs).ok()?;
            (start.checked_add(n)? <= MAX_TEMP_GROWTH).then_some(Dest::Temp { slot, start })
        }
        StrKind::Static(_) => None,
        StrKind::Linear(p) => {
            let addr = linear(p, ofs)?;
            match core.mem.check_write(addr, u32::try_from(n).ok()?) {
                Ok(_) | Err(WriteError::Protected(_)) => Some(Dest::Linear(addr)),
                Err(_) => None,
            }
        }
    }
}

/// Writes `bytes` to a validated destination. Protected entities are skipped with a warning.
/// Returns `false` if a temp buffer could not grow.
fn write_dest(core: &mut Core, d: Dest, bytes: &[u8]) -> bool {
    match d {
        Dest::Linear(addr) => match core.mem.write(addr, bytes) {
            Ok(()) => true,
            Err(WriteError::Protected(e)) => {
                core.warn(WarningKind::ReadOnlyEntity(e));
                true
            }
            Err(_) => false,
        },
        Dest::Temp { slot, start } => {
            let Some(end) = start.checked_add(bytes.len()) else { return false };
            match core.strings.temp_raw_mut(slot, end).and_then(|d| d.get_mut(start..end)) {
                Some(dst) => {
                    dst.copy_from_slice(bytes);
                    true
                }
                None => false,
            }
        }
    }
}

fn opt_i32<H: Host>(vm: &Vm<H>, i: usize) -> i32 {
    if vm.argc() > i { vm.arg_i32(i) } else { 0 }
}

// ---- builtins ---------------------------------------------------------------------------------

/// `__variant *memalloc(int size)`: a zeroed heap block (size 0 counts as 1). Negative sizes,
/// sizes above 16 MiB and an exhausted heap give null and a builtin error.
pub fn memalloc<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let size = vm.arg_i32(0);
    match alloc_size(size).and_then(|n| heap_alloc(&mut vm.core, n)) {
        Some(p) => {
            vm.ret_raw([p, 0, 0]);
            Ok(())
        }
        None => {
            vm.ret_raw([0; 3]);
            soft_error(vm, format!("memalloc: failure (size {size})"))
        }
    }
}

/// `void memfree(__variant *ptr)`: frees a heap block (null is ignored; anything that is not a
/// block start only warns).
pub fn memfree<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let p = vm.arg_u32(0);
    if p != 0 && !heap_free(&mut vm.core, p) {
        vm.warn(format!("memfree: {p:#x} is not an allocated block"));
    }
    Ok(())
}

/// `__variant *memrealloc(__variant *ptr, int size)`: a new block holding the old block's
/// contents (the rest zeroed); the old block is freed. A null `ptr` allocates.
pub fn memrealloc<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let p = vm.arg_u32(0);
    let size = vm.arg_i32(1);
    let core = &mut vm.core;
    let old = if p == 0 { Ok(None) } else { heap_offset(core, p).map(Some).ok_or(()) };
    let new = match (alloc_size(size), old) {
        (Some(n), Ok(old)) => {
            core.mem.heap.realloc(old, n).and_then(|o| core.mem.h_base.checked_add(o))
        }
        _ => None,
    };
    match new {
        Some(p) => {
            vm.ret_raw([p, 0, 0]);
            Ok(())
        }
        None => {
            vm.ret_raw([0; 3]);
            soft_error(vm, format!("memrealloc: failure (size {size})"))
        }
    }
}

/// `void memcpy(__variant *dst, __variant *src, int size, optional int srcofs, optional int
/// dstofs)`: copies `size` bytes (overlap-safe). The offsets are bytes; like FTE's
/// implementation, the source offset comes first.
pub fn memcpy<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let (dst, src, size) = (vm.arg_u32(0), vm.arg_u32(1), vm.arg_i32(2));
    let (sofs, dofs) = (opt_i32(vm, 3), opt_i32(vm, 4));
    let Ok(n) = usize::try_from(size) else {
        return soft_error(vm, format!("memcpy: invalid size {size:#x}"));
    };
    if n == 0 {
        return Ok(());
    }
    let Some(d) = dest(&vm.core, dst, dofs, n) else {
        return soft_error(vm, format!("memcpy: invalid dest ({dst:#x}+{dofs:#x})"));
    };
    let Some(bytes) = read_range(&vm.core, src, sofs, n) else {
        return soft_error(vm, format!("memcpy: invalid source ({src:#x}+{sofs:#x})"));
    };
    if !write_dest(&mut vm.core, d, &bytes) {
        return soft_error(vm, format!("memcpy: invalid dest ({dst:#x}+{dofs:#x})"));
    }
    Ok(())
}

/// `void memfill8(__variant *dst, int value, int size, optional int dstofs)`: sets `size` bytes
/// to the low byte of `value`.
pub fn memfill8<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let (dst, value, size) = (vm.arg_u32(0), vm.arg_u32(1), vm.arg_i32(2));
    let dofs = opt_i32(vm, 3);
    let target = usize::try_from(size).ok().and_then(|n| Some((dest(&vm.core, dst, dofs, n)?, n)));
    let Some((d, n)) = target else {
        return soft_error(vm, "memfill8: invalid dest");
    };
    let byte = value.to_le_bytes().first().copied().unwrap_or(0);
    let fill = vec![byte; n];
    if !write_dest(&mut vm.core, d, &fill) {
        return soft_error(vm, "memfill8: invalid dest");
    }
    Ok(())
}

/// The address `memgetval`/`memsetval` access: `ptr + ofs * 4` (the offset counts words). A
/// non-integral offset and a misaligned result only warn.
fn word_address<H: Host>(vm: &mut Vm<H>, name: &str) -> Option<u32> {
    let p = vm.arg_i32(0);
    let ofs = vm.arg_f32(1);
    if ofs != f2i(ofs) as f32 {
        vm.warn(format!("{name}: non-integer offset"));
    }
    // Exact (FTE rounds the sum through a float, losing precision above 2^24).
    let addr = crate::vm::num::d2i64(f64::from(p) + f64::from(ofs) * 4.0);
    let addr = u32::try_from(addr).ok()?;
    if addr & 3 != 0 {
        vm.warn(format!("{name}: misaligned pointer ({addr:#x})"));
    }
    Some(addr)
}

/// `__variant memgetval(__variant *ptr, float ofs)`: the 32-bit word at `ptr + ofs * 4`. Reading
/// outside VM memory is a fatal error.
pub fn memgetval<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let Some(addr) = word_address(vm, "memgetval") else {
        return Err(ErrorKind::BadPointerRead(vm.arg_u32(0)).into());
    };
    match vm.core.mem.read_u32(addr) {
        Some(v) => {
            vm.ret_raw([v, 0, 0]);
            Ok(())
        }
        None => Err(ErrorKind::BadPointerRead(addr).into()),
    }
}

/// `void memsetval(__variant *ptr, float ofs, __variant value)`: writes the 32-bit word at
/// `ptr + ofs * 4`. Writing outside VM memory is a fatal error; a protected entity is skipped
/// with a warning.
pub fn memsetval<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let value = vm.arg_u32(2);
    let Some(addr) = word_address(vm, "memsetval") else {
        return Err(ErrorKind::BadPointerWrite(vm.arg_u32(0)).into());
    };
    match vm.core.mem.write_u32(addr, value) {
        Ok(()) => Ok(()),
        Err(WriteError::Protected(e)) => {
            vm.core.warn(WarningKind::ReadOnlyEntity(e));
            Ok(())
        }
        Err(WriteError::Null) => Err(ErrorKind::NullPointerWrite.into()),
        Err(WriteError::Invalid(_)) => Err(ErrorKind::BadPointerWrite(addr).into()),
    }
}

/// `__variant *memptradd(__variant *ptr, float ofs)`: `ptr + ofs` (the offset counts bytes).
/// Non-integral, misaligned and negative offsets are builtin errors.
pub fn memptradd<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let p = vm.arg_u32(0);
    let f = vm.arg_f32(1);
    let ofs = f2i(f);
    if ofs as f32 != f {
        soft_error(vm, "memptradd: non-integer offset")?;
    }
    if ofs & 3 != 0 {
        soft_error(vm, "memptradd: offset is not 32-bit aligned")?;
    }
    if ofs < 0 {
        soft_error(vm, "memptradd: negative offset")?;
    }
    vm.ret_raw([p.wrapping_add(ofs.cast_unsigned()), 0, 0]);
    Ok(())
}

/// `int memcmp(__variant *a, __variant *b, int size, optional int aofs, optional int bofs)`:
/// compares `size` bytes; the difference of the first differing bytes, or 0.
pub fn memcmp<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    vm.ret_raw([0; 3]);
    let (a, b, size) = (vm.arg_u32(0), vm.arg_u32(1), vm.arg_i32(2));
    let (aofs, bofs) = (opt_i32(vm, 3), opt_i32(vm, 4));
    let Ok(n) = usize::try_from(size) else {
        return soft_error(vm, "memcmp: invalid size");
    };
    if n == 0 {
        return Ok(());
    }
    let Some(x) = read_range(&vm.core, a, aofs, n) else {
        return soft_error(vm, "memcmp: invalid first pointer");
    };
    let Some(y) = read_range(&vm.core, b, bofs, n) else {
        return soft_error(vm, "memcmp: invalid second pointer");
    };
    let diff = x
        .iter()
        .zip(&y)
        .map(|(&p, &q)| i32::from(p).wrapping_sub(i32::from(q)))
        .find(|&d| d != 0)
        .unwrap_or(0);
    vm.ret_i32(diff);
    Ok(())
}

/// `void *createbuffer(int size)`: a zeroed temporary buffer of `size + 1` bytes (a temp-string
/// handle usable as a pointer; collected like temp strings, not freeable). Null if `size ≤ 0`.
pub fn createbuffer<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let size = vm.arg_i32(0);
    let Some(n) = usize::try_from(size).ok().filter(|&n| n > 0) else {
        vm.ret_raw([0; 3]);
        return Ok(());
    };
    if n > vm.core.config.limits.temp_string_bytes {
        vm.ret_raw([0; 3]);
        return soft_error(vm, format!("createbuffer: {n} bytes is too large"));
    }
    let r = vm.temp(&vec![0; n])?;
    vm.ret_str_ref(r);
    Ok(())
}

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with `=` padding.
fn base64_encode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len().div_ceil(3).saturating_mul(4));
    for chunk in data.chunks(3) {
        let b = |i: usize| u32::from(chunk.get(i).copied().unwrap_or(0));
        let v = (b(0) << 16) | (b(1) << 8) | b(2);
        let sym = |shift: u32| BASE64.get(usize_from((v >> shift) & 63)).copied().unwrap_or(b'A');
        out.push(sym(18));
        out.push(sym(12));
        out.push(if chunk.len() > 1 { sym(6) } else { b'=' });
        out.push(if chunk.len() > 2 { sym(0) } else { b'=' });
    }
    out
}

/// The value of a base64 symbol, FTE-style: `-` and `_` are accepted for 62 and 63, anything
/// else (padding included) counts as 0.
fn base64_value(c: u8) -> u32 {
    u32::from(match c {
        b'A'..=b'Z' => c.wrapping_sub(b'A'),
        b'a'..=b'z' => c.wrapping_sub(b'a').wrapping_add(26),
        b'0'..=b'9' => c.wrapping_sub(b'0').wrapping_add(52),
        b'+' | b'-' => 62,
        b'/' | b'_' => 63,
        _ => 0,
    })
}

/// Decodes base64 like FTE: control characters before the first two symbols of a group are
/// skipped, `=` or the end of input after two or three symbols ends the data, and invalid symbols
/// decode as zero bits. `cap` bounds the output (FTE's size estimate).
fn base64_decode(s: &[u8], cap: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0usize;
    let at = |i: usize| s.get(i).copied().unwrap_or(0);
    let skip_ctrl = |mut i: usize| {
        while (1..b' ').contains(&at(i)) {
            i = i.saturating_add(1);
        }
        i
    };
    let mut room = cap;
    while room > 1 {
        i = skip_ctrl(i);
        if i >= s.len() {
            break;
        }
        let mut v = base64_value(at(i)) << 18;
        i = skip_ctrl(i.saturating_add(1));
        if i >= s.len() {
            break;
        }
        v |= base64_value(at(i)) << 12;
        i = i.saturating_add(1);
        out.extend_from_slice(v.to_be_bytes().get(1..2).unwrap_or_default());
        if i >= s.len() || at(i) == b'=' || room < 2 {
            break;
        }
        v |= base64_value(at(i)) << 6;
        i = i.saturating_add(1);
        out.extend_from_slice(v.to_be_bytes().get(2..3).unwrap_or_default());
        if i >= s.len() || at(i) == b'=' || room < 3 {
            break;
        }
        v |= base64_value(at(i));
        i = i.saturating_add(1);
        out.extend_from_slice(v.to_be_bytes().get(3..4).unwrap_or_default());
        room = room.saturating_sub(3);
    }
    out
}

/// FTE's upper estimate of the decoded size of `len` base64 characters, plus a NUL.
fn base64_capacity(len: usize) -> usize {
    len.saturating_add(3).div_euclid(4).saturating_mul(3).saturating_add(1)
}

/// `string base64encode(__variant *ptr, int size)`: `size` bytes of memory as base64 (with `+`,
/// `/` and `=` padding). Null and a builtin error if the range is not readable.
pub fn base64encode<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let (p, size) = (vm.arg_u32(0), vm.arg_i32(1));
    let data = match usize::try_from(size) {
        Ok(0) => Some(Vec::new()),
        Ok(n) if p != 0 => read_range(&vm.core, p, 0, n),
        _ => None,
    };
    match data {
        Some(d) => vm.ret_str(&base64_encode(&d)),
        None => {
            vm.ret_raw([0; 3]);
            soft_error(vm, "base64encode: invalid pointer")
        }
    }
}

/// `__variant *base64decode(string s, __out int size)`: decodes into a new heap block (free it
/// with `memfree`; it has a NUL after the data) and stores the decoded length in `size`.
pub fn base64decode<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let s = vm.arg_str(0).to_vec();
    let cap = base64_capacity(s.len());
    let data = base64_decode(&s, cap);
    let block = u32::try_from(cap).ok().and_then(|n| heap_alloc(&mut vm.core, n));
    let Some(p) = block else {
        set_arg_word(vm, 1, 0);
        vm.ret_raw([0; 3]);
        return soft_error(vm, "base64decode: out of memory");
    };
    heap_write(&mut vm.core, p, &data);
    set_arg_word(vm, 1, u32::try_from(data.len()).unwrap_or(0));
    vm.ret_raw([p, 0, 0]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trip() {
        assert_eq!(base64_encode(b""), b"");
        assert_eq!(base64_encode(b"f"), b"Zg==");
        assert_eq!(base64_encode(b"fo"), b"Zm8=");
        assert_eq!(base64_encode(b"foo"), b"Zm9v");
        assert_eq!(base64_encode(b"foobar"), b"Zm9vYmFy");
        for s in [&b"f"[..], b"fo", b"foo", b"foob", b"fooba", b"foobar"] {
            let enc = base64_encode(s);
            let cap = base64_capacity(enc.len());
            assert_eq!(base64_decode(&enc, cap), s);
        }
    }

    #[test]
    fn base64_quirks() {
        // URL-safe symbols, embedded line breaks and invalid symbols.
        assert_eq!(base64_decode(b"-_-_", 4), [0xFB, 0xFF, 0xBF]);
        assert_eq!(base64_decode(b"Zm9v\nYmFy", 10), b"foobar");
        assert_eq!(base64_decode(b"Z!==", 4), [0x64]);
        assert_eq!(base64_decode(b"Z", 4), b"");
    }
}
