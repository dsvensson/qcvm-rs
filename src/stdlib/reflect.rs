// SPDX-License-Identifier: MIT OR Apache-2.0

//! Entity field reflection and the field-value formatter used by eprint/coredump (docs/spec/builtins.md).
//!
//! The field table is the VM-wide one: every field definition of the progs (vectors appear four
//! times: `v`, `v_x`, `v_y`, `v_z`), plus fields the host added. The text formats follow FTE's
//! savegame writer ("ugly value strings").

use std::fmt::Write as _;

use crate::builtins::Builtins;
use crate::bytes::usize_from;
use crate::error::VmError;
use crate::host::Host;
use crate::progs::Type;
use crate::vm::Vm;
use crate::vm::core::Core;
use crate::vm::num::{f2i, f2u};

/// Registers this module's builtins.
pub(crate) fn register<H: Host>(b: &mut Builtins<H>) {
    b.set("numentityfields", numentityfields::<H>);
    b.set("entityfieldname", entityfieldname::<H>);
    b.set("entityfieldtype", entityfieldtype::<H>);
    b.set("findentityfield", findentityfield::<H>);
    b.set("entityfieldref", entityfieldref::<H>);
}

// ---- builtins ---------------------------------------------------------------------------------

/// `float numentityfields()`: the number of named entity fields (vectors count four times).
pub fn numentityfields<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let n = vm.core.fields.entries.len();
    vm.ret_f32(n as f32);
    Ok(())
}

/// `string entityfieldname(float index)`: the field's name, or null if out of range.
pub fn entityfieldname<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let i = usize_from(f2u(vm.arg_f32(0)));
    match vm.core.fields.entries.get(i).map(|f| f.name.to_vec()) {
        Some(name) => vm.ret_str(&name),
        None => {
            vm.ret_raw([0; 3]);
            Ok(())
        }
    }
}

/// `float entityfieldtype(float index)`: the field's `EV_*` type, or 0 if out of range.
pub fn entityfieldtype<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let i = usize_from(f2u(vm.arg_f32(0)));
    let ty = vm.core.fields.entries.get(i).map_or(0, |f| f.ty.code());
    vm.ret_f32(ty as f32);
    Ok(())
}

/// `float findentityfield(string name)`: the index of the first field with that exact name, or 0.
pub fn findentityfield<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let name = vm.arg_str(0);
    let i = vm.core.fields.entries.iter().position(|f| &*f.name == name).unwrap_or(0);
    vm.ret_f32(i as f32);
    Ok(())
}

/// `field_t entityfieldref(float index)`: the field reference (offset) of field `index`, or 0.
pub fn entityfieldref<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let i = usize_from(f2u(vm.arg_f32(0)));
    let ofs = vm.core.fields.entries.get(i).map_or(0, |f| f.ofs);
    vm.ret_raw([ofs, 0, 0]);
    Ok(())
}

// ---- formatting -------------------------------------------------------------------------------

/// C's `%f` (six decimals), with C's spellings of infinities and NaNs.
pub(crate) fn fmt_f(x: f64) -> String {
    special(x).unwrap_or_else(|| format!("{x:.6}"))
}

/// C's `%g` (six significant digits, trailing zeros removed).
pub(crate) fn fmt_g(x: f64) -> String {
    if let Some(s) = special(x) {
        return s;
    }
    if x == 0.0 {
        return if x.is_sign_negative() { "-0".into() } else { "0".into() };
    }
    let sci = format!("{x:.5e}");
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    if (-4..6).contains(&exp) {
        let decimals = usize::try_from(5i32.saturating_sub(exp)).unwrap_or(0);
        trim_zeros(&format!("{x:.decimals$}"))
    } else {
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{}e{sign}{:02}", trim_zeros(mantissa), exp.unsigned_abs())
    }
}

fn special(x: f64) -> Option<String> {
    if x.is_nan() {
        Some(if x.is_sign_negative() { "-nan" } else { "nan" }.into())
    } else if x.is_infinite() {
        Some(if x < 0.0 { "-inf" } else { "inf" }.into())
    } else {
        None
    }
}

fn trim_zeros(s: &str) -> String {
    if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').into() } else { s.into() }
}

/// C's `%#x`.
fn fmt_alt_hex(v: u32) -> String {
    if v == 0 { "0".into() } else { format!("{v:#x}") }
}

/// The name of function `f` as `progs:name`, as FTE writes function values.
fn function_name(core: &Core, f: u32) -> String {
    let pr = f >> 24;
    let Some(ps) = core.progs.get(usize_from(pr)) else {
        return format!("BAD FUNCTION INDEX: {}", fmt_alt_hex(f));
    };
    match ps.program.function(f & 0x00FF_FFFF) {
        Some(info) => format!("{pr}:{}", String::from_utf8_lossy(info.name)),
        None => format!("{pr}:CORRUPT FUNCTION POINTER"),
    }
}

/// A value as FTE's savegame writer spells it (`PR_UglyValueString`). Strings have newlines,
/// quotes and backslashes escaped.
pub(crate) fn format_value(core: &Core, ty: Type, words: [u32; 3]) -> Vec<u8> {
    let [w0, w1, w2] = words;
    let text = match ty {
        Type::String => {
            let mut out = Vec::new();
            for &b in core.str_bytes(w0).unwrap_or_default() {
                match b {
                    b'\n' => out.extend_from_slice(b"\\n"),
                    b'"' => out.extend_from_slice(b"\\\""),
                    b'\\' => out.extend_from_slice(b"\\\\"),
                    _ => out.push(b),
                }
            }
            return out;
        }
        Type::Entity | Type::Integer => w0.cast_signed().to_string(),
        Type::Function => function_name(core, w0),
        Type::Field => match core.fields.entries.iter().find(|f| f.ofs == w0) {
            Some(f) => String::from_utf8_lossy(&f.name).into_owned(),
            None => format!("bad field {w0}"),
        },
        Type::Void => "void".into(),
        Type::Float => {
            let v = f32::from_bits(w0);
            let i = f2i(v);
            if v == i as f32 { i.to_string() } else { fmt_f(f64::from(v)) }
        }
        Type::Double => {
            let v = f64::from_bits(u64::from(w0) | (u64::from(w1) << 32));
            let i = crate::vm::num::d2i64(v);
            if v == i as f64 { i.to_string() } else { fmt_f(v) }
        }
        Type::UInt => w0.to_string(),
        Type::Int64 => (u64::from(w0) | (u64::from(w1) << 32)).cast_signed().to_string(),
        Type::UInt64 => (u64::from(w0) | (u64::from(w1) << 32)).to_string(),
        Type::Vector => {
            let v = [w0, w1, w2].map(f32::from_bits);
            let ints = v.map(f2i);
            if v.iter().zip(ints).all(|(&c, i)| c == i as f32) {
                let [x, y, z] = ints;
                format!("{x} {y} {z}")
            } else {
                let [x, y, z] = v.map(|c| fmt_g(f64::from(c)));
                format!("{x} {y} {z}")
            }
        }
        Type::Pointer => fmt_alt_hex(w0),
        Type::Other(code) => format!("bad type {code}"),
    };
    text.into_bytes()
}

/// Whether a field or global with this name is a vector component (`v_x`, `v_y`, `v_z`).
fn is_component(name: &[u8]) -> bool {
    name.len() > 2 && matches!(name, [.., b'_', b'x' | b'y' | b'z'])
}

/// Appends `"name" "value"` lines for entity `e`'s fields that are not all zero.
fn entity_fields(core: &Core, e: u32, out: &mut Vec<u8>) {
    for f in &core.fields.entries {
        if f.name.is_empty() || is_component(&f.name) || f.name.windows(2).any(|w| w == b"::") {
            continue;
        }
        let n = f.ty.words().unwrap_or(1).clamp(1, 3);
        let Some(at) = core.mem.field_offset(e, f.ofs, n) else { continue };
        let mut words = [0u32; 3];
        for (k, w) in (0u32..n).zip(words.iter_mut()) {
            *w = core.mem.ent_word(at.wrapping_add(usize_from(k).wrapping_mul(4)));
        }
        if words.iter().all(|&w| w == 0) {
            continue;
        }
        quoted_pair(out, &f.name, &format_value(core, f.ty, words));
    }
}

fn quoted_pair(out: &mut Vec<u8>, name: &[u8], value: &[u8]) {
    out.push(b'"');
    out.extend_from_slice(name);
    out.extend_from_slice(b"\" \"");
    out.extend_from_slice(value);
    out.extend_from_slice(b"\"\n");
}

/// Entity `e` as a savegame block: `{`, one `"field" "value"` line per non-zero field, `}`.
pub(crate) fn entity_block(core: &Core, e: u32) -> Vec<u8> {
    let mut out = b"{\n".to_vec();
    entity_fields(core, e, &mut out);
    out.extend_from_slice(b"}\n");
    out
}

/// A human-readable core dump like FTE's `coredump()`: general information, the call stack, the
/// saved globals and every entity in use.
pub(crate) fn coredump_text(core: &Core) -> Vec<u8> {
    let mut head = String::new();
    let _ = write!(
        head,
        "general {{\n\"maxprogs\" \"{}\"\n\"numentities\" \"{}\"\n}}\n",
        core.config.limits.progs,
        core.mem.num_edicts()
    );
    for (pr, ps) in core.progs.iter().enumerate() {
        let _ = write!(head, "progs {pr} {{\n\"crc\" \"{}\"\n}}\n", ps.program.crc());
    }
    head.push_str("stacktrace {\n");
    head.push_str(&core.backtrace().to_string());
    head.push_str("}\n");
    let mut out = head.into_bytes();

    for (pr, ps) in core.progs.iter().enumerate() {
        out.extend_from_slice(format!("globals {pr} {{\n").as_bytes());
        for d in ps.program.global_defs() {
            if d.name.is_empty() || is_component(d.name) || !d.save {
                continue;
            }
            let at = usize_from(ps.gbase).saturating_add(usize_from(d.offset).saturating_mul(4));
            let words = [0usize, 4, 8].map(|k| core.mem.g(at.wrapping_add(k)));
            match d.ty {
                Type::Function => {
                    // Functions still holding their own name's function are not interesting.
                    let f = words.first().copied().unwrap_or(0);
                    let own = ps.program.function(f & 0x00FF_FFFF).map(|i| i.name);
                    if f >> 24 == u32::try_from(pr).unwrap_or(0) && own == Some(d.name) {
                        continue;
                    }
                }
                Type::String
                | Type::Float
                | Type::Double
                | Type::Integer
                | Type::UInt
                | Type::Int64
                | Type::UInt64
                | Type::Entity
                | Type::Vector => {}
                _ => continue,
            }
            quoted_pair(&mut out, d.name, &format_value(core, d.ty, words));
        }
        out.extend_from_slice(b"}\n");
    }

    for e in 0..core.mem.num_edicts() {
        if !core.mem.in_use(e) {
            continue;
        }
        out.extend_from_slice(format!("entity {e}{{\n").as_bytes());
        entity_fields(core, e, &mut out);
        out.extend_from_slice(b"}\n");
    }
    out
}
