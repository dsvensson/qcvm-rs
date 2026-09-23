// SPDX-License-Identifier: MIT OR Apache-2.0

//! An in-memory file system and FTE's file builtins (`fopen`, `fread`, `fseek64`, ...).
//!
//! The `.pak` files given on the command line are read-only; everything the progs writes
//! (`config.cfg`, savegames) lives in memory until the program exits. The progs' libc has already
//! reduced paths to game-relative ones (`./id1/pak0.pak` → `pak0.pak`).

use std::collections::HashMap;

use qcvm::{Ptr, StrRef, Vm, VmError};

use crate::host::MenuHost;

type B = Result<(), VmError>;

/// FTE numbers QC file handles from 1000.
const FIRST_HANDLE: i32 = 1000;
/// Handles open at once (FTE: 256).
const MAX_OPEN: usize = 256;

const MODE_READ: i32 = 0;
const MODE_APPEND: i32 = 1;
const MODE_WRITE: i32 = 2;

struct File {
    data: Vec<u8>,
    read_only: bool,
}

struct Open {
    name: Vec<u8>,
    pos: usize,
    write: bool,
}

#[derive(Default)]
pub struct Vfs {
    files: HashMap<Vec<u8>, File>,
    open: HashMap<i32, Open>,
    next: i32,
}

/// Lowercase, forward slashes, no leading `./` or `/`.
fn normalize(name: &[u8]) -> Vec<u8> {
    let mut n: Vec<u8> =
        name.iter().map(|&c| if c == b'\\' { b'/' } else { c.to_ascii_lowercase() }).collect();
    while let Some(rest) = n.strip_prefix(b"./").or_else(|| n.strip_prefix(b"/")) {
        n = rest.to_vec();
    }
    n
}

impl Vfs {
    /// Adds a read-only file (a `.pak`).
    pub fn add(&mut self, name: &[u8], data: Vec<u8>) {
        self.files.insert(normalize(name), File { data, read_only: true });
    }

    fn open(&mut self, name: &[u8], mode: i32) -> Option<i32> {
        if self.open.len() >= MAX_OPEN {
            return None;
        }
        let name = normalize(name);
        let (pos, write) = match mode {
            MODE_READ => (0, false),
            MODE_APPEND | MODE_WRITE => {
                let file = self
                    .files
                    .entry(name.clone())
                    .or_insert_with(|| File { data: Vec::new(), read_only: false });
                if file.read_only {
                    return None;
                }
                if mode == MODE_WRITE {
                    file.data.clear();
                }
                (file.data.len(), true)
            }
            _ => return None,
        };
        if !self.files.contains_key(&name) {
            return None;
        }
        // Skip handles still in use once the counter wraps.
        let mut fd = self.next;
        while self.open.contains_key(&FIRST_HANDLE.saturating_add(fd)) {
            fd = fd.wrapping_add(1) & 0xffff;
        }
        self.next = fd.wrapping_add(1) & 0xffff;
        let fd = FIRST_HANDLE.saturating_add(fd);
        self.open.insert(fd, Open { name, pos, write });
        Some(fd)
    }

    /// The open file and its data.
    fn get(&mut self, fd: i32) -> Option<(&mut Open, &mut File)> {
        let open = self.open.get_mut(&fd)?;
        let file = self.files.get_mut(&open.name)?;
        Some((open, file))
    }
}

/// File handles are floats in FTE's builtins.
fn fd_arg(vm: &Vm<MenuHost>, i: usize) -> i32 {
    vm.arg_f32(i) as i32
}

/// The buffer argument plus the optional byte offset after it.
fn ptr_arg(vm: &Vm<MenuHost>, ptr: usize) -> Ptr {
    let ofs = if vm.argc() > ptr + 2 { vm.arg_i32(ptr + 2) } else { 0 };
    Ptr(vm.arg_ptr(ptr).0.wrapping_add_signed(ofs))
}

fn arg_i64(vm: &Vm<MenuHost>, i: usize) -> i64 {
    let [lo, hi, _] = vm.arg_raw(i);
    ((u64::from(hi) << 32) | u64::from(lo)) as i64
}

fn ret_i64(vm: &mut Vm<MenuHost>, v: i64) {
    let v = v as u64;
    vm.ret_raw([v as u32, (v >> 32) as u32, 0]);
}

/// `float fopen(string name, float mode)`: a handle, or -1.
pub fn fopen(vm: &mut Vm<MenuHost>, h: &mut MenuHost) -> B {
    let mode = vm.arg_f32(1) as i32;
    let fd = h.files.open(vm.arg_str(0), mode);
    vm.ret_f32(fd.map_or(-1.0, |fd| fd as f32));
    Ok(())
}

/// `void fclose(float fd)`
pub fn fclose(vm: &mut Vm<MenuHost>, h: &mut MenuHost) -> B {
    if h.files.open.remove(&fd_arg(vm, 0)).is_none() {
        vm.warn("fclose: bad file handle");
    }
    Ok(())
}

/// `int fread(float fd, void *ptr, int size, optional int ptroffset)`: bytes read, or -1.
pub fn fread(vm: &mut Vm<MenuHost>, h: &mut MenuHost) -> B {
    let dest = ptr_arg(vm, 1);
    let size = usize::try_from(vm.arg_i32(2)).unwrap_or(0);
    let Some((open, file)) = h.files.get(fd_arg(vm, 0)) else {
        vm.ret_i32(-1);
        return Ok(());
    };
    let chunk = file.data.get(open.pos..).unwrap_or_default();
    let chunk = chunk.get(..size.min(chunk.len())).unwrap_or_default();
    if !vm.write_mem(dest, chunk) {
        vm.warn("fread: bad destination pointer");
        vm.ret_i32(-1);
        return Ok(());
    }
    open.pos = open.pos.saturating_add(chunk.len());
    vm.ret_i32(i32::try_from(chunk.len()).unwrap_or(i32::MAX));
    Ok(())
}

/// `int fwrite(float fd, void *ptr, int size, optional int ptroffset)`: bytes written, or -1.
pub fn fwrite(vm: &mut Vm<MenuHost>, h: &mut MenuHost) -> B {
    let size = usize::try_from(vm.arg_i32(2)).unwrap_or(0);
    let Some(bytes) = vm.read_mem(ptr_arg(vm, 1), size) else {
        vm.warn("fwrite: bad source pointer");
        vm.ret_i32(-1);
        return Ok(());
    };
    let Some((open, file)) = h.files.get(fd_arg(vm, 0)).filter(|(open, _)| open.write) else {
        vm.ret_i32(-1);
        return Ok(());
    };
    let end = open.pos.saturating_add(bytes.len());
    if file.data.len() < end {
        file.data.resize(end, 0);
    }
    if let Some(dest) = file.data.get_mut(open.pos..end) {
        dest.copy_from_slice(&bytes);
    }
    open.pos = end;
    vm.ret_i32(i32::try_from(bytes.len()).unwrap_or(i32::MAX));
    Ok(())
}

/// `__int64 fseek64(float fd, optional __int64 newpos)`: seeks if given; returns the position.
pub fn fseek64(vm: &mut Vm<MenuHost>, h: &mut MenuHost) -> B {
    let newpos = (vm.argc() > 1).then(|| arg_i64(vm, 1));
    let Some((open, _)) = h.files.get(fd_arg(vm, 0)) else {
        ret_i64(vm, -1);
        return Ok(());
    };
    if let Some(p) = newpos {
        open.pos = usize::try_from(p).unwrap_or(0);
    }
    let pos = i64::try_from(open.pos).unwrap_or(i64::MAX);
    ret_i64(vm, pos);
    Ok(())
}

/// `__int64 fsize64(float fd, optional __int64 newsize)`: resizes if given (writable files only);
/// returns the size.
pub fn fsize64(vm: &mut Vm<MenuHost>, h: &mut MenuHost) -> B {
    let newsize = (vm.argc() > 1).then(|| arg_i64(vm, 1));
    let Some((open, file)) = h.files.get(fd_arg(vm, 0)) else {
        ret_i64(vm, -1);
        return Ok(());
    };
    if let (Some(n), true) = (newsize, open.write) {
        file.data.resize(usize::try_from(n).unwrap_or(0), 0);
    }
    let len = i64::try_from(file.data.len()).unwrap_or(i64::MAX);
    ret_i64(vm, len);
    Ok(())
}

/// `string fgets(float fd)`: the next line without its line ending; null at end of file.
pub fn fgets(vm: &mut Vm<MenuHost>, h: &mut MenuHost) -> B {
    let Some((open, file)) = h.files.get(fd_arg(vm, 0)) else {
        vm.ret_str_ref(StrRef::NULL);
        return Ok(());
    };
    let rest = file.data.get(open.pos..).unwrap_or_default();
    if rest.is_empty() {
        vm.ret_str_ref(StrRef::NULL);
        return Ok(());
    }
    let (line, used) = match rest.iter().position(|&c| c == b'\n') {
        Some(n) => (rest.get(..n).unwrap_or_default(), n.saturating_add(1)),
        None => (rest, rest.len()),
    };
    let line = line.strip_suffix(b"\r").unwrap_or(line).to_vec();
    open.pos = open.pos.saturating_add(used);
    vm.ret_str(&line)
}

/// `float fremove(string name)`: 0 on success. Only files the progs wrote can be removed.
pub fn fremove(vm: &mut Vm<MenuHost>, h: &mut MenuHost) -> B {
    let name = normalize(vm.arg_str(0));
    let removable = h.files.files.get(&name).is_some_and(|f| !f.read_only);
    if removable {
        h.files.files.remove(&name);
    }
    vm.ret_f32(if removable { 0.0 } else { -1.0 });
    Ok(())
}

/// `float frename(string from, string to)`: 0 on success. Only files the progs wrote can move.
pub fn frename(vm: &mut Vm<MenuHost>, h: &mut MenuHost) -> B {
    let (from, to) = (normalize(vm.arg_str(0)), normalize(vm.arg_str(1)));
    let movable = h.files.files.get(&from).is_some_and(|f| !f.read_only)
        && h.files.files.get(&to).is_none_or(|f| !f.read_only);
    if movable && let Some(file) = h.files.files.remove(&from) {
        h.files.files.insert(to, file);
    }
    vm.ret_f32(if movable { 0.0 } else { -1.0 });
    Ok(())
}

/// `string whichpack(string name)`: the package holding the file (`""` for loose files, which all
/// of these are), or null if it does not exist.
pub fn whichpack(vm: &mut Vm<MenuHost>, h: &mut MenuHost) -> B {
    if h.files.files.contains_key(&normalize(vm.arg_str(0))) {
        vm.ret_str(b"")
    } else {
        vm.ret_str_ref(StrRef::NULL);
        Ok(())
    }
}
