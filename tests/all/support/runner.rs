// SPDX-License-Identifier: MIT OR Apache-2.0

//! A host reproducing the builtins of FTE's standalone `qcvm` test runner, so QuakeC fixtures
//! produce the same output under both VMs.
//!
//! The runner's builtins were characterised as a black box: `#1`/`#20` puts (concatenate string
//! arguments), `#2` ftos (`%g`), `#3` spawn, `#4` remove, `#5` vtos (`'%g %g %g'`), `#6` error,
//! `#7` vlen, `#8` etos (the entity number), `#9` stof, `#10` strcat, `#11` strcmp (sign),
//! `#12` normalize, `#13` sqrt, `#14` floor, `#15` pow, `#16` stov, `#17` itos (hex), `#18` ltos
//! (64-bit hex), `#19` dtos (`%g`), `#21` putv (`%f %f %f\n`), `#22` putf (`%f\n`) and `#23`
//! printf (`%d` `%i` `%f` `%g` `%s`).

use qcvm::{Builtins, ErrorKind, Host, Numbering, Vm, VmError};

/// Collects everything the fixture prints.
#[derive(Default)]
pub struct RunnerHost {
    pub out: Vec<u8>,
}

impl Host for RunnerHost {}

type R = Result<(), VmError>;

/// C's `%g` (precision 6).
pub fn c_g(v: f64) -> String {
    if v.is_nan() {
        return if v.is_sign_negative() { "-nan".into() } else { "nan".into() };
    }
    if v.is_infinite() {
        return if v < 0.0 { "-inf".into() } else { "inf".into() };
    }
    if v == 0.0 {
        return if v.is_sign_negative() { "-0".into() } else { "0".into() };
    }
    let sci = format!("{v:.5e}");
    let (mant, exp) = sci.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    let strip = |s: String| {
        if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s }
    };
    if !(-4..6).contains(&exp) {
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{}e{sign}{:02}", strip(mant.to_string()), exp.abs())
    } else {
        strip(format!("{:.*}", (5 - exp) as usize, v))
    }
}

/// C's `%f`.
pub fn c_f(v: f64) -> String {
    format!("{v:.6}")
}

fn puts(vm: &mut Vm<RunnerHost>, host: &mut RunnerHost) -> R {
    for i in 0..vm.argc() {
        host.out.extend_from_slice(vm.arg_str(i));
    }
    Ok(())
}

fn ftos(vm: &mut Vm<RunnerHost>, _: &mut RunnerHost) -> R {
    let s = c_g(f64::from(vm.arg_f32(0)));
    vm.ret_str(s.as_bytes())
}

fn spawn(vm: &mut Vm<RunnerHost>, _: &mut RunnerHost) -> R {
    let e = vm.spawn()?;
    vm.ret_ent(e);
    Ok(())
}

fn remove(vm: &mut Vm<RunnerHost>, _: &mut RunnerHost) -> R {
    let e = vm.arg_ent(0);
    vm.remove(e, false);
    Ok(())
}

fn vtos(vm: &mut Vm<RunnerHost>, _: &mut RunnerHost) -> R {
    let v = vm.arg_vec(0).map(|c| c_g(f64::from(c)));
    let s = format!("'{} {} {}'", v[0], v[1], v[2]);
    vm.ret_str(s.as_bytes())
}

fn error(vm: &mut Vm<RunnerHost>, _: &mut RunnerHost) -> R {
    let mut msg = Vec::new();
    for i in 0..vm.argc() {
        msg.extend_from_slice(vm.arg_str(i));
    }
    Err(VmError::new(ErrorKind::QcError(msg.into())))
}

fn vlen(vm: &mut Vm<RunnerHost>, _: &mut RunnerHost) -> R {
    let v = vm.arg_vec(0);
    vm.ret_f32((v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt());
    Ok(())
}

fn etos(vm: &mut Vm<RunnerHost>, _: &mut RunnerHost) -> R {
    let s = vm.arg_ent(0).0.to_string();
    vm.ret_str(s.as_bytes())
}

/// The numeric prefix of `s`, like C's `atof`.
fn atof(s: &[u8]) -> f64 {
    let s = String::from_utf8_lossy(s);
    let t = s.trim_start();
    let mut end = 0;
    for (i, c) in t.char_indices() {
        let ok = c.is_ascii_digit() || c == '.' || ((c == '-' || c == '+') && i == 0);
        if !ok {
            break;
        }
        end = i + c.len_utf8();
    }
    t[..end].parse().unwrap_or(0.0)
}

fn stof(vm: &mut Vm<RunnerHost>, _: &mut RunnerHost) -> R {
    let v = atof(vm.arg_str(0)) as f32;
    vm.ret_f32(v);
    Ok(())
}

fn strcat(vm: &mut Vm<RunnerHost>, _: &mut RunnerHost) -> R {
    let mut out = Vec::new();
    for i in 0..vm.argc() {
        out.extend_from_slice(vm.arg_str(i));
    }
    vm.ret_str(&out)
}

fn strcmp(vm: &mut Vm<RunnerHost>, _: &mut RunnerHost) -> R {
    let o = vm.arg_str(0).cmp(vm.arg_str(1));
    vm.ret_f32(o as i32 as f32);
    Ok(())
}

fn normalize(vm: &mut Vm<RunnerHost>, _: &mut RunnerHost) -> R {
    let v = vm.arg_vec(0);
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    let r = if len == 0.0 { [0.0; 3] } else { v.map(|c| c / len) };
    vm.ret_vec(r);
    Ok(())
}

fn sqrt(vm: &mut Vm<RunnerHost>, _: &mut RunnerHost) -> R {
    let v = vm.arg_f32(0).sqrt();
    vm.ret_f32(v);
    Ok(())
}

fn floor(vm: &mut Vm<RunnerHost>, _: &mut RunnerHost) -> R {
    let v = vm.arg_f32(0).floor();
    vm.ret_f32(v);
    Ok(())
}

fn pow(vm: &mut Vm<RunnerHost>, _: &mut RunnerHost) -> R {
    let v = f64::from(vm.arg_f32(0)).powf(f64::from(vm.arg_f32(1))) as f32;
    vm.ret_f32(v);
    Ok(())
}

fn stov(vm: &mut Vm<RunnerHost>, _: &mut RunnerHost) -> R {
    let s = String::from_utf8_lossy(vm.arg_str(0)).replace('\'', " ");
    let mut v = [0.0f32; 3];
    for (slot, part) in v.iter_mut().zip(s.split_whitespace()) {
        *slot = atof(part.as_bytes()) as f32;
    }
    vm.ret_vec(v);
    Ok(())
}

fn itos(vm: &mut Vm<RunnerHost>, _: &mut RunnerHost) -> R {
    let s = format!("{:x}", vm.arg_u32(0));
    vm.ret_str(s.as_bytes())
}

fn ltos(vm: &mut Vm<RunnerHost>, _: &mut RunnerHost) -> R {
    let w = vm.arg_raw(0);
    let s = format!("{:x}", u64::from(w[0]) | (u64::from(w[1]) << 32));
    vm.ret_str(s.as_bytes())
}

fn dtos(vm: &mut Vm<RunnerHost>, _: &mut RunnerHost) -> R {
    let w = vm.arg_raw(0);
    let s = c_g(f64::from_bits(u64::from(w[0]) | (u64::from(w[1]) << 32)));
    vm.ret_str(s.as_bytes())
}

fn putv(vm: &mut Vm<RunnerHost>, host: &mut RunnerHost) -> R {
    let v = vm.arg_vec(0).map(|c| c_f(f64::from(c)));
    host.out.extend_from_slice(format!("{} {} {}\n", v[0], v[1], v[2]).as_bytes());
    Ok(())
}

fn putf(vm: &mut Vm<RunnerHost>, host: &mut RunnerHost) -> R {
    host.out.extend_from_slice(format!("{}\n", c_f(f64::from(vm.arg_f32(0)))).as_bytes());
    Ok(())
}

fn printf(vm: &mut Vm<RunnerHost>, host: &mut RunnerHost) -> R {
    let fmt = vm.arg_str(0).to_vec();
    let mut arg = 1;
    let mut out = Vec::new();
    let mut it = fmt.iter().copied();
    while let Some(c) = it.next() {
        if c != b'%' {
            out.push(c);
            continue;
        }
        let Some(conv) = it.next() else { break };
        let text = match conv {
            b'd' => format!("{}", vm.arg_f32(arg) as i32),
            b'i' => format!("{}", vm.arg_i32(arg)),
            b'f' => c_f(f64::from(vm.arg_f32(arg))),
            b'g' => c_g(f64::from(vm.arg_f32(arg))),
            b's' => String::from_utf8_lossy(vm.arg_str(arg)).into_owned(),
            b'%' => {
                out.push(b'%');
                continue;
            }
            other => format!("%{}", other as char),
        };
        arg += 1;
        out.extend_from_slice(text.as_bytes());
    }
    host.out.extend_from_slice(&out);
    Ok(())
}

/// The runner's builtin table.
pub fn builtins() -> Builtins<RunnerHost> {
    let mut b = Builtins::empty(Numbering::None);
    let table: [(u32, &str, qcvm::BuiltinFn<RunnerHost>); 23] = [
        (1, "puts", puts),
        (2, "ftos", ftos),
        (3, "spawn", spawn),
        (4, "remove", remove),
        (5, "vtos", vtos),
        (6, "error", error),
        (7, "vlen", vlen),
        (8, "etos", etos),
        (9, "stof", stof),
        (10, "strcat", strcat),
        (11, "strcmp", strcmp),
        (12, "normalize", normalize),
        (13, "sqrt", sqrt),
        (14, "floor", floor),
        (15, "pow", pow),
        (16, "stov", stov),
        (17, "itos", itos),
        (18, "ltos", ltos),
        (19, "dtos", dtos),
        (20, "puts2", puts),
        (21, "putv", putv),
        (22, "putf", putf),
        (23, "printf", printf),
    ];
    for (n, name, f) in table {
        b.set_numbered(n, name, f);
    }
    b
}

#[test]
fn c_style_g() {
    assert_eq!(c_g(1.5), "1.5");
    assert_eq!(c_g(0.1), "0.1");
    assert_eq!(c_g(-0.0), "-0");
    assert_eq!(c_g(1e10), "1e+10");
    assert_eq!(c_g(123456.0), "123456");
    assert_eq!(c_g(1234567.0), "1.23457e+06");
    assert_eq!(c_g(0.0001), "0.0001");
    assert_eq!(c_g(0.00001), "1e-05");
    assert_eq!(c_g(f64::from(1.0f32 / 3.0)), "0.333333");
    assert_eq!(c_g(2.0f64.sqrt()), "1.41421");
}
