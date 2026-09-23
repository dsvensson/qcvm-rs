// SPDX-License-Identifier: MIT OR Apache-2.0

//! Entity allocation, iteration and search (docs/spec/builtins.md).
//!
//! Searches visit entities 1 to `num_edicts − 1`, skip free slots and never return the world
//! except to mean "nothing found". Chains are built by prepending, so a chain starts at the
//! highest-numbered match and its last entity's chain field is the world.

use crate::builtins::Builtins;
use crate::error::{VmError, WarningKind};
use crate::host::Host;
use crate::value::EntRef;
use crate::vm::Vm;
use crate::vm::core::Core;
use crate::vm::num::{f2i, f2u};

use super::introspect::{set_arg_word, soft_error};
use super::util::arg_int;

/// Registers this module's builtins.
pub(crate) fn register<H: Host>(b: &mut Builtins<H>) {
    b.set("spawn", spawn::<H>);
    b.set("remove", remove::<H>);
    b.set("removeinstant", removeinstant::<H>);
    b.set("nextent", nextent::<H>);
    b.set("find", find::<H>);
    b.set("findfloat", findfloat::<H>);
    b.alias("findentity", "findfloat");
    b.set("findflags", findflags::<H>);
    b.set("findchain", findchain::<H>);
    b.set("findchainfloat", findchainfloat::<H>);
    b.set("findchainflags", findchainflags::<H>);
    b.set("findradius", findradius::<H>);
    b.set("find_list", find_list::<H>);
    b.set("findradius_list", findradius_list::<H>);
    b.set("edict_num", edict_num::<H>);
    b.set("num_for_edict", num_for_edict::<H>);
    b.set("wasfreed", wasfreed::<H>);
    b.set("copyentity", copyentity::<H>);
    b.set("entityprotection", entityprotection::<H>);
    b.set("etof", etof::<H>);
    b.set("ftoe", ftoe::<H>);
}

// ---- helpers shared with the other standard builtins ------------------------------------------

/// Argument `i` as an entity number. Like FTE, a reference past the allocated entities warns and
/// reads as the world.
pub(crate) fn ent_arg<H: Host>(vm: &mut Vm<H>, i: usize) -> u32 {
    let e = vm.arg_u32(i);
    if e < vm.num_edicts() {
        e
    } else {
        vm.core.warn(WarningKind::BadEntity(e));
        0
    }
}

/// The word offset of the field named `name`, if the VM has one.
pub(crate) fn field_word(core: &Core, name: &str) -> Option<u32> {
    core.fields.get(name.as_bytes()).map(|f| f.ofs)
}

/// Whether `words` words at field offset `f` lie within an entity's fields.
pub(crate) fn field_ok(core: &Core, f: u32, words: u32) -> bool {
    let end = u64::from(f).saturating_add(u64::from(words)).saturating_mul(4);
    end <= u64::from(core.mem.field_bytes)
}

/// Field word `f` of entity `e` (0 if either is invalid).
pub(crate) fn word(core: &Core, e: u32, f: u32) -> u32 {
    core.mem.field_offset(e, f, 1).map_or(0, |o| core.mem.ent_word(o))
}

/// Field word `f` of entity `e` as a float.
pub(crate) fn float(core: &Core, e: u32, f: u32) -> f32 {
    f32::from_bits(word(core, e, f))
}

/// Field words `f..f + 3` of entity `e` as a vector.
pub(crate) fn vector(core: &Core, e: u32, f: u32) -> [f32; 3] {
    [0u32, 1, 2].map(|k| float(core, e, f.wrapping_add(k)))
}

/// Writes field word `f` of entity `e`, ignoring write protection (engine-side writes).
pub(crate) fn set_word(core: &mut Core, e: u32, f: u32, v: u32) {
    if let Some(o) = core.mem.field_offset(e, f, 1) {
        core.mem.set_ent_word(o, v);
    }
}

/// Writes a vector field of entity `e`.
pub(crate) fn set_vector(core: &mut Core, e: u32, f: u32, v: [f32; 3]) {
    for (k, c) in (0u32..).zip(v) {
        set_word(core, e, f.wrapping_add(k), c.to_bits());
    }
}

/// A named field's value (0 when the progs lacks the field).
fn named_float(core: &Core, e: u32, name: &str) -> f32 {
    field_word(core, name).map_or(0.0, |f| float(core, e, f))
}

/// A named vector field's value (zero when the progs lacks the field).
fn named_vector(core: &Core, e: u32, name: &str) -> [f32; 3] {
    field_word(core, name).map_or([0.0; 3], |f| vector(core, e, f))
}

/// Spawns an entity and runs the host's spawn hook.
pub(crate) fn spawn_entity<H: Host>(vm: &mut Vm<H>, host: &mut H) -> Result<u32, VmError> {
    let e = vm.spawn()?;
    host.on_spawn(vm, e);
    Ok(e.0)
}

/// The entities a search visits: 1 to `num_edicts − 1`, in use.
fn live(core: &Core) -> impl Iterator<Item = u32> + '_ {
    live_after(core, 0)
}

/// The entities in use after `start` (never the world).
fn live_after(core: &Core, start: u32) -> impl Iterator<Item = u32> + '_ {
    (start.saturating_add(1).max(1)..core.mem.num_edicts()).filter(|&e| core.mem.in_use(e))
}

fn bad_field(name: &str) -> VmError {
    VmError::builtin(format!("{name}: bad field reference"))
}

/// The chain field: argument `i` if passed, else the progs' `.chain`.
fn chain_field<H: Host>(vm: &Vm<H>, i: usize, name: &str) -> Result<u32, VmError> {
    let f = if vm.argc() > i { Some(vm.arg_u32(i)) } else { field_word(&vm.core, "chain") };
    match f {
        Some(f) if field_ok(&vm.core, f, 1) => Ok(f),
        _ => Err(bad_field(name)),
    }
}

/// Links `matches` (ascending) through chain field `cf` and returns the head.
fn build_chain(core: &mut Core, matches: &[u32], cf: u32) -> u32 {
    let mut chain = 0;
    for &e in matches {
        set_word(core, e, cf, chain);
        chain = e;
    }
    chain
}

/// Returns entity numbers as a zero-terminated temporary int array; the count goes to `__out`
/// parameter `count_arg`.
fn ret_list<H: Host>(vm: &mut Vm<H>, list: &[u32], count_arg: usize) -> Result<(), VmError> {
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(list.len().saturating_add(1).saturating_mul(4)).map_err(|_| {
        VmError::from(crate::error::ErrorKind::OutOfMemory(crate::error::Resource::TempStrings))
    })?;
    for &e in list.iter().chain(std::iter::once(&0)) {
        bytes.extend_from_slice(&e.to_le_bytes());
    }
    let r = vm.temp(&bytes)?;
    set_arg_word(vm, count_arg, u32::try_from(list.len()).unwrap_or(u32::MAX));
    vm.ret_str_ref(r);
    Ok(())
}

// ---- allocation -------------------------------------------------------------------------------

/// `entity spawn()`: a new entity with every field zeroed (FTE's slot reuse policy), after
/// [`Host::on_spawn`].
pub fn spawn<H: Host>(vm: &mut Vm<H>, host: &mut H) -> Result<(), VmError> {
    let e = spawn_entity(vm, host)?;
    vm.ret_ent(EntRef(e));
    Ok(())
}

/// `void remove(entity e)`: frees an entity (the world, free and protected entities are refused
/// with a warning). Its slot is not reused for half a second.
pub fn remove<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let e = ent_arg(vm, 0);
    vm.remove(EntRef(e), false);
    Ok(())
}

/// `void removeinstant(entity e)`: like `remove`, but the slot may be reused at once.
pub fn removeinstant<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let e = ent_arg(vm, 0);
    vm.remove(EntRef(e), true);
    Ok(())
}

// ---- iteration and search ---------------------------------------------------------------------

/// `entity nextent(entity e)`: the first entity in use after `e`, or the world.
pub fn nextent<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let start = vm.arg_u32(0);
    let core = &vm.core;
    let found = live_after(core, start).next().unwrap_or(0);
    vm.ret_ent(EntRef(found));
    Ok(())
}

/// `entity find(entity start, .string fld, string match)`: the next entity whose string field
/// equals `match`. Searching for `""` (or null) matches null and empty fields; otherwise a null
/// field never matches.
pub fn find<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let start = vm.arg_u32(0);
    let f = vm.arg_u32(1);
    if !field_ok(&vm.core, f, 1) {
        return Err(bad_field("find"));
    }
    let want = vm.arg_str(2).to_vec();
    if want.is_empty() && vm.config().developer {
        vm.warn("find: empty string");
    }
    let core = &vm.core;
    let found = live_after(core, start)
        .find(|&e| {
            let t = word(core, e, f);
            let text = core.str_bytes(t).unwrap_or_default();
            if want.is_empty() { t == 0 || text.is_empty() } else { t != 0 && text == want }
        })
        .unwrap_or(0);
    vm.ret_ent(EntRef(found));
    Ok(())
}

/// `entity findfloat(entity start, .__variant fld, __variant match)` (alias `findentity`): the
/// next entity whose field has exactly the bits of `match` (so `-0` differs from `0` and a NaN
/// matches itself). FTE insists on exactly three arguments.
pub fn findfloat<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    if vm.argc() != 3 {
        return Err(VmError::builtin("findfloat: exactly 3 arguments are required"));
    }
    let start = vm.arg_u32(0);
    let f = vm.arg_u32(1);
    if !field_ok(&vm.core, f, 1) {
        return Err(bad_field("findfloat"));
    }
    let want = vm.arg_u32(2);
    let core = &vm.core;
    let found = live_after(core, start).find(|&e| word(core, e, f) == want);
    vm.ret_ent(EntRef(found.unwrap_or(0)));
    Ok(())
}

/// `entity findflags(entity start, .float fld, float flags)`: the next entity whose field shares
/// a bit with `flags` (both truncated to integers).
pub fn findflags<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let start = vm.arg_u32(0);
    let f = vm.arg_u32(1);
    if !field_ok(&vm.core, f, 1) {
        return Err(bad_field("findflags"));
    }
    let flags = arg_int(vm, 2);
    let core = &vm.core;
    let found = live_after(core, start).find(|&e| f2i(float(core, e, f)) & flags != 0);
    vm.ret_ent(EntRef(found.unwrap_or(0)));
    Ok(())
}

/// `entity findchain(.string fld, string match, optional .entity chainfld = chain)`: every
/// entity whose string field equals `match`, chained. Null fields never match, even when
/// searching for `""`.
pub fn findchain<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let f = vm.arg_u32(0);
    let cf = chain_field(vm, 2, "findchain")?;
    if !field_ok(&vm.core, f, 1) {
        return Err(bad_field("findchain"));
    }
    let want = vm.arg_str(1).to_vec();
    let core = &vm.core;
    let matches: Vec<u32> = live(core)
        .filter(|&e| {
            let t = word(core, e, f);
            t != 0 && core.str_bytes(t).unwrap_or_default() == want
        })
        .collect();
    let head = build_chain(&mut vm.core, &matches, cf);
    vm.ret_ent(EntRef(head));
    Ok(())
}

/// `entity findchainfloat(.float fld, float match, optional .entity chainfld = chain)`: every
/// entity whose field equals `match` as a float (`-0 == 0`, NaN never matches), chained.
pub fn findchainfloat<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let f = vm.arg_u32(0);
    let cf = chain_field(vm, 2, "findchainfloat")?;
    if !field_ok(&vm.core, f, 1) {
        return Err(bad_field("findchainfloat"));
    }
    let want = vm.arg_f32(1);
    let core = &vm.core;
    let matches: Vec<u32> = live(core).filter(|&e| float(core, e, f) == want).collect();
    let head = build_chain(&mut vm.core, &matches, cf);
    vm.ret_ent(EntRef(head));
    Ok(())
}

/// `entity findchainflags(.float fld, float flags, optional .entity chainfld = chain)`: every
/// entity whose field shares a bit with `flags`, chained.
pub fn findchainflags<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let f = vm.arg_u32(0);
    let cf = chain_field(vm, 2, "findchainflags")?;
    if !field_ok(&vm.core, f, 1) {
        return Err(bad_field("findchainflags"));
    }
    let flags = arg_int(vm, 1);
    let core = &vm.core;
    let matches: Vec<u32> = live(core).filter(|&e| f2i(float(core, e, f)) & flags != 0).collect();
    let head = build_chain(&mut vm.core, &matches, cf);
    vm.ret_ent(EntRef(head));
    Ok(())
}

/// `FL_FINDABLE_NONSOLID`: lets `findradius` find non-solid entities.
const FL_FINDABLE_NONSOLID: i32 = 16384;

/// The entities within `rad` of `org` (distance to the centre of their bounding box), skipping
/// non-solid ones unless flagged `FL_FINDABLE_NONSOLID`. Ascending.
fn radius_matches(core: &Core, org: [f32; 3], rad: f32) -> Vec<u32> {
    let rad2 = rad * rad;
    live(core)
        .filter(|&e| {
            if named_float(core, e, "solid") == 0.0
                && f2i(named_float(core, e, "flags")) & FL_FINDABLE_NONSOLID == 0
            {
                return false;
            }
            let origin = named_vector(core, e, "origin");
            let mins = named_vector(core, e, "mins");
            let maxs = named_vector(core, e, "maxs");
            let d: [f32; 3] = std::array::from_fn(|j| {
                let (o, lo, hi, p) = (
                    origin.get(j).copied().unwrap_or(0.0),
                    mins.get(j).copied().unwrap_or(0.0),
                    maxs.get(j).copied().unwrap_or(0.0),
                    org.get(j).copied().unwrap_or(0.0),
                );
                // FTE mixes precisions here: the half-size sum is scaled in double.
                (f64::from(p) - (f64::from(o) + f64::from(lo + hi) * 0.5)) as f32
            });
            let [x, y, z] = d;
            x * x + y * y + z * z <= rad2
        })
        .collect()
}

/// `entity findradius(vector org, float rad, optional .entity chainfld = chain)`: every entity
/// whose bounding-box centre lies within `rad` of `org`, chained. Non-solid entities are skipped
/// unless their `flags` include `FL_FINDABLE_NONSOLID` (16384).
pub fn findradius<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let org = vm.arg_vec(0);
    let rad = vm.arg_f32(1);
    let cf = chain_field(vm, 2, "findradius")?;
    let matches = radius_matches(&vm.core, org, rad);
    let head = build_chain(&mut vm.core, &matches, cf);
    vm.ret_ent(EntRef(head));
    Ok(())
}

/// `entity *findradius_list(vector org, float rad, __out int count, int sort = 0)`: like
/// `findradius`, as a zero-terminated temporary array (ascending; `sort` is ignored).
pub fn findradius_list<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let org = vm.arg_vec(0);
    let rad = vm.arg_f32(1);
    let matches = radius_matches(&vm.core, org, rad);
    ret_list(vm, &matches, 2)
}

/// Words of a value of `EV_*` type `ty`, for the types `find_list` accepts.
fn type_words(ty: i32) -> Option<u32> {
    match ty {
        0..=2 | 4..=9 => Some(1),
        3 => Some(3),
        10..=12 => Some(2),
        _ => None,
    }
}

/// `entity *find_list(.__variant fld, __variant match, int type = EV_STRING, __out int count)`:
/// every entity whose field equals `match`, as a zero-terminated temporary array. Strings compare
/// by content (null fields never match; read-only entities are skipped), floats and doubles by
/// value, vectors and 64-bit integers per component, everything else by raw bits.
pub fn find_list<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let f = vm.arg_u32(0);
    let ty = if vm.argc() > 2 { vm.arg_i32(2) } else { 1 };
    let ok = type_words(ty).is_some_and(|words| field_ok(&vm.core, f, words));
    if !ok {
        set_arg_word(vm, 3, 0);
        vm.ret_raw([0; 3]);
        return Ok(());
    }
    let want = vm.arg_raw(1);
    let want_str = vm.arg_str(1).to_vec();
    let core = &vm.core;
    let w = |e: u32, k: u32| word(core, e, f.wrapping_add(k));
    let matches: Vec<u32> = live(core)
        .filter(|&e| match ty {
            1 => {
                let t = w(e, 0);
                !core.mem.protected(e)
                    && t != 0
                    && core.str_bytes(t).unwrap_or_default() == want_str.as_slice()
            }
            2 => f32::from_bits(w(e, 0)) == f32::from_bits(want[0]),
            3 => (0..3u32).all(|k| {
                f32::from_bits(w(e, k))
                    == f32::from_bits(want.get(k as usize).copied().unwrap_or(0))
            }),
            12 => {
                let get = |lo: u32, hi: u32| f64::from_bits(u64::from(lo) | (u64::from(hi) << 32));
                get(w(e, 0), w(e, 1)) == get(want[0], want[1])
            }
            10 | 11 => w(e, 0) == want[0] && w(e, 1) == want[1],
            _ => w(e, 0) == want[0],
        })
        .collect();
    ret_list(vm, &matches, 3)
}

// ---- numbers and references -------------------------------------------------------------------

/// `entity edict_num(float n)`: entity `n` even if it is free; the world if `n` is out of range.
pub fn edict_num<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let n = f2u(vm.arg_f32(0));
    let e = if n < vm.num_edicts() { n } else { 0 };
    vm.ret_ent(EntRef(e));
    Ok(())
}

/// `float num_for_edict(entity e)`: the entity's number.
pub fn num_for_edict<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let e = ent_arg(vm, 0);
    vm.ret_f32(e as f32);
    Ok(())
}

/// `float wasfreed(entity e)`: whether the entity's slot is not in use.
pub fn wasfreed<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let e = ent_arg(vm, 0);
    let freed = !vm.is_in_use(EntRef(e));
    vm.ret_f32(if freed { 1.0 } else { 0.0 });
    Ok(())
}

/// `float etof(entity e)` (menu): the entity's number.
pub fn etof<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let e = vm.arg_u32(0);
    vm.ret_f32(e as f32);
    Ok(())
}

/// `entity ftoe(float n)` (menu): entity `n`, or the world if out of range.
pub fn ftoe<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let n = arg_int(vm, 0);
    let e = u32::try_from(n).ok().filter(|&n| n < vm.num_edicts()).unwrap_or(0);
    vm.ret_ent(EntRef(e));
    Ok(())
}

/// `entity copyentity(entity from, optional entity to)`: copies every field word of `from` into
/// `to` (a new entity if omitted) and returns `to`. Copying from or to a free entity, or into a
/// protected one, is a builtin error.
pub fn copyentity<H: Host>(vm: &mut Vm<H>, host: &mut H) -> Result<(), VmError> {
    let from = ent_arg(vm, 0);
    let to = if vm.argc() <= 1 { spawn_entity(vm, host)? } else { ent_arg(vm, 1) };
    if !vm.is_in_use(EntRef(from)) {
        soft_error(vm, "copyentity: source is free")?;
    }
    if !vm.is_in_use(EntRef(to)) {
        soft_error(vm, "copyentity: destination is free")?;
    }
    if vm.is_protected(EntRef(to)) {
        soft_error(vm, "copyentity: destination is read-only")?;
    }
    let mem = &mut vm.core.mem;
    let len = crate::bytes::usize_from(mem.field_bytes);
    if let (Some(src), Some(dst)) = (mem.field_offset(from, 0, 0), mem.field_offset(to, 0, 0))
        && let (Some(s_end), Some(d_end)) = (src.checked_add(len), dst.checked_add(len))
        && s_end <= mem.e.len()
        && d_end <= mem.e.len()
    {
        mem.e.copy_within(src..s_end, dst);
    }
    vm.ret_ent(EntRef(to));
    Ok(())
}

/// `float entityprotection(entity e, float readonly)`: protects the entity against writes from
/// QuakeC (1) or unprotects it (0) and returns the previous setting. Other values change nothing.
/// FTE returns the new value instead; qcvm returns the previous one, as documented.
pub fn entityprotection<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let e = ent_arg(vm, 0);
    let prot = arg_int(vm, 1);
    if !vm.is_in_use(EntRef(e)) {
        soft_error(vm, "entityprotection: entity is free")?;
    }
    let previous = vm.is_protected(EntRef(e));
    if prot == 0 || prot == 1 {
        vm.set_protected(EntRef(e), prot == 1);
    }
    vm.ret_f32(if previous { 1.0 } else { 0.0 });
    Ok(())
}
