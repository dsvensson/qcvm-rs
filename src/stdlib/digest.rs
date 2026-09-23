// SPDX-License-Identifier: MIT OR Apache-2.0

//! crc16, digest_hex and digest_ptr (docs/spec/strings.md).
//!
//! CRC16 is always available; MD4, MD5, SHA-1 and SHA-2 need the default `digests` feature.

use crate::builtins::Builtins;
use crate::error::VmError;
use crate::host::Host;
use crate::stdlib::util::{arg_int, args_concat};
use crate::value::{Ptr, StrRef};
use crate::vm::Vm;
use crate::vm::strings::{StrKind, classify};

/// Registers this module's builtins.
pub(crate) fn register<H: Host>(b: &mut Builtins<H>) {
    b.set("crc16", crc16::<H>);
    b.set("digest_hex", digest_hex::<H>);
    b.set("digest_ptr", digest_ptr::<H>);
}

/// CRC-16/CCITT-FALSE (polynomial 0x1021, initial value 0xFFFF, not reflected), optionally
/// over ASCII-lowercased bytes.
pub(crate) fn crc16_ccitt(data: &[u8], lowercase: bool) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in data {
        let b = if lowercase { b.to_ascii_lowercase() } else { b };
        crc ^= u16::from(b) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
        }
    }
    crc
}

/// The digest of `data` with FTE's algorithm name, or `None` for an unknown (or, without the
/// `digests` feature, unavailable) algorithm.
pub(crate) fn digest(alg: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    #[cfg(feature = "digests")]
    {
        use sha2::Digest;
        let out = match alg {
            b"MD4" => Some(md4::Md4::digest(data).to_vec()),
            b"MD5" => Some(md5::Md5::digest(data).to_vec()),
            b"SHA1" => Some(sha1::Sha1::digest(data).to_vec()),
            b"SHA2-224" | b"SHA224" => Some(sha2::Sha224::digest(data).to_vec()),
            b"SHA2-256" | b"SHA256" => Some(sha2::Sha256::digest(data).to_vec()),
            b"SHA2-384" | b"SHA384" => Some(sha2::Sha384::digest(data).to_vec()),
            b"SHA2-512" | b"SHA512" => Some(sha2::Sha512::digest(data).to_vec()),
            _ => None,
        };
        if out.is_some() {
            return out;
        }
    }
    match alg {
        // FTE emits the CRC's bytes little-endian.
        b"CRC16" => Some(crc16_ccitt(data, false).to_le_bytes().to_vec()),
        _ => None,
    }
}

fn hex(bytes: &[u8]) -> Vec<u8> {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    bytes
        .iter()
        .flat_map(|&b| [b >> 4, b & 15])
        .map(|n| DIGITS.get(usize::from(n)).copied().unwrap_or(b'0'))
        .collect()
}

fn ret_digest<H: Host>(vm: &mut Vm<H>, alg: &[u8], data: &[u8]) -> Result<(), VmError> {
    match digest(alg, data) {
        Some(d) => vm.ret_str(&hex(&d)),
        None => {
            vm.ret_str_ref(StrRef(0));
            Ok(())
        }
    }
}

/// `float crc16(float caseinsensitive, string...)`: CRC-16/CCITT-FALSE of the concatenated
/// strings.
///
/// # Errors
/// Never.
pub fn crc16<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let lowercase = arg_int(vm, 0) != 0;
    let crc = crc16_ccitt(&args_concat(vm, 1), lowercase);
    vm.ret_f32(f32::from(crc));
    Ok(())
}

/// `string digest_hex(string alg, string data...)`: the lowercase hex digest of the
/// concatenated strings with `MD4`, `MD5`, `SHA1`, `SHA2-224` (`SHA224`), `SHA2-256`
/// (`SHA256`), `SHA2-384` (`SHA384`), `SHA2-512` (`SHA512`) or `CRC16`; null for any other name.
///
/// # Errors
/// Only if the result string cannot be allocated.
pub fn digest_hex<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let data = args_concat(vm, 1);
    let alg = vm.arg_str(0).to_vec();
    ret_digest(vm, &alg, &data)
}

/// Reads `len` bytes of VM memory at `base + offset`, including through a temp-string handle.
fn read_block<H: Host>(vm: &Vm<H>, base: u32, offset: u32, len: usize) -> Option<Vec<u8>> {
    if let Some(bytes) = vm.read_mem(Ptr(base.wrapping_add(offset)), len) {
        return Some(bytes);
    }
    let data = match classify(base) {
        StrKind::Temp(slot) => vm.core.strings.temp_raw(slot)?,
        StrKind::Static(index) => vm.core.strings.static_str(index)?,
        StrKind::Linear(_) => return None,
    };
    let start = usize::try_from(offset).ok()?;
    data.get(start..start.checked_add(len)?).map(<[u8]>::to_vec)
}

/// `string digest_ptr(string alg, void *data, int length, optional int offset)`: like
/// [`digest_hex`] over `length` bytes of VM memory (NULs included).
///
/// # Errors
/// A builtin error if the memory range is not readable.
pub fn digest_ptr<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let base = vm.arg_u32(1);
    let len = usize::try_from(vm.arg_i32(2)).ok();
    let offset = if vm.argc() > 3 { vm.arg_u32(3) } else { 0 };
    let Some(data) = len.and_then(|len| read_block(vm, base, offset, len)) else {
        return Err(VmError::builtin("digest_ptr: invalid pointer"));
    };
    let alg = vm.arg_str(0).to_vec();
    ret_digest(vm, &alg, &data)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn crc16_vectors() {
        assert_eq!(crc16_ccitt(b"", false), 0xFFFF);
        assert_eq!(crc16_ccitt(b"A", false), 0xB915);
        assert_eq!(crc16_ccitt(b"123456789", false), 0x29B1);
        assert_eq!(crc16_ccitt(b"ABC", true), crc16_ccitt(b"abc", false));
    }

    #[test]
    fn crc16_digest_is_little_endian() {
        assert_eq!(hex(&digest(b"CRC16", b"123456789").unwrap()), b"b129");
        assert_eq!(digest(b"crc16", b""), None);
    }
}
