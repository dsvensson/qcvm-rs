// SPDX-License-Identifier: MIT OR Apache-2.0

//! Hash tables (docs/spec/builtins.md).
//!
//! Tables are chained: a new entry goes to the head of its bucket, lookups find the newest entry
//! for a key first. Handles are floats counted from 1; handle 0 is the `gamestate` table, which
//! always exists (FTE keeps it across maps; qcvm keeps it for the life of the VM). Keys are
//! case-sensitive byte strings. String values are copied as text, so they outlive the temp
//! strings they came from.

use crate::builtins::Builtins;
use crate::error::VmError;
use crate::host::Host;
use crate::value::{Arg, FuncRef};
use crate::vm::Vm;
use crate::vm::num::f2i;

use super::introspect::{set_fte, soft_error};
use super::util::{arg_int, opt_f32};

/// Registers this module's builtins.
pub(crate) fn register<H: Host>(b: &mut Builtins<H>) {
    b.set("hash_createtab", hash_createtab::<H>);
    b.set("hash_destroytab", hash_destroytab::<H>);
    b.set("hash_add", hash_add::<H>);
    b.set("hash_get", hash_get::<H>);
    b.set("hash_delete", hash_delete::<H>);
    b.set("hash_getkey", hash_getkey::<H>);
    set_fte(b, 293, "hash_getcb", hash_getcb::<H>);
}

/// `EV_STRING`.
const EV_STRING: i32 = 1;
/// `EV_VECTOR`, the default value type of tables created without one.
const EV_VECTOR: i32 = 3;
/// `hash_add` flag: replace the newest existing entry for the key.
const HASH_REPLACE: i32 = 256;
/// `hash_add` flag: add another entry for the key.
const HASH_ADD: i32 = 512;
/// Buckets of the `gamestate` table.
const GAMESTATE_BUCKETS: usize = 256;
/// Upper bound on buckets per table (the size is only a performance hint).
const MAX_BUCKETS: usize = 1 << 16;

/// A stored value.
#[derive(Clone, Debug, PartialEq)]
enum Value {
    /// An `EV_STRING` value, copied.
    Text(Box<[u8]>),
    /// Any other value: three raw words.
    Words([u32; 3]),
}

#[derive(Clone, Debug)]
struct Entry {
    key: Box<[u8]>,
    ty: i32,
    value: Value,
}

/// One hash table.
#[derive(Clone, Debug)]
struct Table {
    default_ty: i32,
    /// Buckets, each with its newest entry last.
    buckets: Vec<Vec<Entry>>,
}

impl Table {
    fn new(buckets: usize, default_ty: i32) -> Self {
        Self { default_ty, buckets: vec![Vec::new(); buckets.max(1)] }
    }

    fn bucket_of(&self, key: &[u8]) -> usize {
        // FNV-1a: stable across platforms and runs.
        let h =
            key.iter().fold(0x811C_9DC5u32, |h, &b| (h ^ u32::from(b)).wrapping_mul(0x0100_0193));
        usize::try_from(h).unwrap_or(0).checked_rem(self.buckets.len()).unwrap_or(0)
    }

    /// Entries for `key`, newest first, as `(bucket, index)`.
    fn matches<'a>(&'a self, key: &'a [u8]) -> impl Iterator<Item = (usize, usize)> + 'a {
        let b = self.bucket_of(key);
        let bucket = self.buckets.get(b).map(Vec::as_slice).unwrap_or_default();
        bucket
            .iter()
            .enumerate()
            .rev()
            .filter(move |(_, e)| &*e.key == key)
            .map(move |(i, _)| (b, i))
    }

    fn entry(&self, (b, i): (usize, usize)) -> Option<&Entry> {
        self.buckets.get(b)?.get(i)
    }

    fn remove(&mut self, (b, i): (usize, usize)) -> Option<Entry> {
        let bucket = self.buckets.get_mut(b)?;
        (i < bucket.len()).then(|| bucket.remove(i))
    }

    /// Every entry in enumeration order: bucket by bucket, newest first within a bucket.
    fn all(&self) -> impl Iterator<Item = &Entry> + '_ {
        self.buckets.iter().flat_map(|b| b.iter().rev())
    }
}

/// The VM's hash tables.
#[derive(Clone, Debug, Default)]
pub(crate) struct Tables {
    /// Tables by handle − 1.
    tables: Vec<Option<Table>>,
    /// Handle 0, created on first use.
    gamestate: Option<Table>,
}

impl Tables {
    fn get_mut(&mut self, handle: i32) -> Option<&mut Table> {
        if handle == 0 {
            return Some(
                self.gamestate.get_or_insert_with(|| Table::new(GAMESTATE_BUCKETS, EV_STRING)),
            );
        }
        let i = usize::try_from(handle.checked_sub(1)?).ok()?;
        self.tables.get_mut(i)?.as_mut()
    }

    fn live(&self) -> usize {
        self.tables.iter().filter(|t| t.is_some()).count()
    }
}

/// The table argument `i` designates; a builtin error (after which FTE carries on without a
/// table) if there is none.
fn table_arg<H: Host>(vm: &mut Vm<H>, i: usize) -> Result<Option<i32>, VmError> {
    let handle = arg_int(vm, i);
    if vm.core.std.hash.get_mut(handle).is_some() {
        return Ok(Some(handle));
    }
    soft_error(vm, "hash: invalid hash table")?;
    Ok(None)
}

/// Returns a stored value: strings as a new temp string (other words zero), else the raw words.
fn ret_value<H: Host>(vm: &mut Vm<H>, value: &Value) -> Result<(), VmError> {
    match value {
        Value::Text(t) => vm.ret_str(t),
        Value::Words(w) => {
            vm.ret_raw(*w);
            Ok(())
        }
    }
}

/// `hashtable hash_createtab(float size, optional float type = EV_VECTOR)`: a new table
/// (`size` below 4 means 64 buckets; type 0 means `EV_VECTOR`). Returns 0 when the table limit
/// is reached.
pub fn hash_createtab<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let size = arg_int(vm, 0);
    let ty = if vm.argc() > 1 { arg_int(vm, 1) } else { EV_VECTOR };
    let ty = if ty == 0 { EV_VECTOR } else { ty };
    let buckets = if size < 4 { 64 } else { usize::try_from(size).unwrap_or(64).min(MAX_BUCKETS) };
    let limit = usize::try_from(vm.core.config.limits.hash_tables).unwrap_or(usize::MAX);
    let tables = &mut vm.core.std.hash;
    if tables.live() >= limit {
        vm.ret_f32(0.0);
        return Ok(());
    }
    let table = Some(Table::new(buckets, ty));
    let index = match tables.tables.iter().position(Option::is_none) {
        Some(i) => {
            if let Some(slot) = tables.tables.get_mut(i) {
                *slot = table;
            }
            i
        }
        None => {
            tables.tables.push(table);
            tables.tables.len().saturating_sub(1)
        }
    };
    vm.ret_f32(index.saturating_add(1) as f32);
    Ok(())
}

/// `void hash_destroytab(hashtable table)`: destroys a table (the `gamestate` table cannot be
/// destroyed).
pub fn hash_destroytab<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let Some(handle) = table_arg(vm, 0)? else { return Ok(()) };
    if let Some(slot) = usize::try_from(handle.saturating_sub(1))
        .ok()
        .filter(|_| handle > 0)
        .and_then(|i| vm.core.std.hash.tables.get_mut(i))
    {
        *slot = None;
    }
    Ok(())
}

/// `void hash_add(hashtable table, string key, __variant value, optional float typeandflags)`:
/// stores a value (type `flags & 255`, 0 = the table's default). Unless `HASH_ADD` (512) is set
/// without `HASH_REPLACE` (256), the newest existing entry for the key is replaced. Empty keys are
/// ignored.
pub fn hash_add<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let Some(handle) = table_arg(vm, 0)? else { return Ok(()) };
    let key: Box<[u8]> = vm.arg_str(1).into();
    if key.is_empty() {
        return Ok(());
    }
    let flags = f2i(opt_f32(vm, 3, 0.0));
    let words = vm.arg_raw(2);
    let text: Box<[u8]> = vm.arg_str(2).into();
    let Some(table) = vm.core.std.hash.get_mut(handle) else { return Ok(()) };
    let ty = match flags & 0xFF {
        0 => table.default_ty,
        t => t,
    };
    if flags & HASH_ADD == 0 || flags & HASH_REPLACE != 0 {
        let newest = table.matches(&key).next();
        if let Some(at) = newest {
            table.remove(at);
        }
    }
    let value = if ty == EV_STRING { Value::Text(text) } else { Value::Words(words) };
    let b = table.bucket_of(&key);
    if let Some(bucket) = table.buckets.get_mut(b) {
        bucket.push(Entry { key, ty, value });
    }
    Ok(())
}

/// `__variant hash_get(hashtable table, string key, optional __variant default = 0, optional
/// float requiretype = 0, optional float index = 0)`: the newest value for the key (skipping
/// entries of another type when `requiretype` is set, and the first `index` matches), or
/// `default`. Strings come back as temp strings.
pub fn hash_get<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let handle = table_arg(vm, 0)?;
    let default = if vm.argc() > 2 { vm.arg_raw(2) } else { [0; 3] };
    let ty = f2i(opt_f32(vm, 3, 0.0));
    let index = f2i(opt_f32(vm, 4, 0.0));
    let key: Box<[u8]> = vm.arg_str(1).into();
    let found = handle.and_then(|h| {
        let table = vm.core.std.hash.get_mut(h)?;
        let skip = usize::try_from(index).unwrap_or(0);
        let at = table
            .matches(&key)
            .filter(|&at| ty == 0 || table.entry(at).is_some_and(|e| e.ty == ty))
            .nth(skip)?;
        table.entry(at).map(|e| e.value.clone())
    });
    match found {
        Some(v) => ret_value(vm, &v),
        None => {
            vm.ret_raw(default);
            Ok(())
        }
    }
}

/// `__variant hash_delete(hashtable table, string key)`: removes the newest entry for the key
/// (whatever its type) and returns its value, or zero.
pub fn hash_delete<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    vm.ret_raw([0; 3]);
    let Some(handle) = table_arg(vm, 0)? else { return Ok(()) };
    let key: Box<[u8]> = vm.arg_str(1).into();
    let removed = vm.core.std.hash.get_mut(handle).and_then(|t| {
        let at = t.matches(&key).next()?;
        t.remove(at)
    });
    match removed {
        Some(e) => ret_value(vm, &e.value),
        None => Ok(()),
    }
}

/// `string hash_getkey(hashtable table, float index)`: the key of the `index`th entry in
/// enumeration order (unspecified, and changed by adds and deletes), or null.
pub fn hash_getkey<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    vm.ret_raw([0; 3]);
    let Some(handle) = table_arg(vm, 0)? else { return Ok(()) };
    let index = usize::try_from(arg_int(vm, 1)).ok();
    let key = index.and_then(|i| {
        let table = vm.core.std.hash.get_mut(handle)?;
        table.all().nth(i).map(|e| e.key.clone())
    });
    match key {
        Some(k) => vm.ret_str(&k),
        None => Ok(()),
    }
}

/// `void hash_getcb(hashtable table, void(string key, __variant value) callback, optional
/// string key)`: calls `callback` once for every entry (or every entry for `key`), in
/// enumeration order. The entries are copied first, so the callback may change the table. FTE
/// ships this as a no-op; qcvm implements the documented behaviour.
pub fn hash_getcb<H: Host>(vm: &mut Vm<H>, host: &mut H) -> Result<(), VmError> {
    let Some(handle) = table_arg(vm, 0)? else { return Ok(()) };
    let callback: FuncRef = vm.arg_func(1);
    let key: Option<Box<[u8]>> = (vm.argc() > 2).then(|| vm.arg_str(2).into());
    let snapshot: Vec<(Box<[u8]>, Value)> = match vm.core.std.hash.get_mut(handle) {
        Some(table) => table
            .all()
            .filter(|e| key.as_ref().is_none_or(|k| *k == e.key))
            .map(|e| (e.key.clone(), e.value.clone()))
            .collect(),
        None => Vec::new(),
    };
    for (k, v) in &snapshot {
        let value = match v {
            Value::Text(t) => Arg::Bytes(t),
            Value::Words(w) => Arg::Raw(*w),
        };
        vm.call(host, callback, &[Arg::Bytes(k), value])?;
    }
    Ok(())
}
