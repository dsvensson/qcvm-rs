// SPDX-License-Identifier: MIT OR Apache-2.0

//! Entity allocation, iteration, search and copying.

use std::sync::Arc;

use qcvm::{
    Arg, Builtins, EntRef, ErrorKind, Host, Numbering, Program, ProgsFormat, StrRef, Vec3, Vm,
    VmConfig,
};

use super::{add_peek, ent, parm_word, peek, w, warnings};
use crate::support::asm::{Asm, ty};
use crate::support::harness::{Harness, f, i, s, v};

const WORLD: EntRef = EntRef(0);

fn setup(asm: &mut Asm) {
    asm.field("classname", ty::STRING);
    for name in ["health", "flags", "solid"] {
        asm.field(name, ty::FLOAT);
    }
    for name in ["origin", "mins", "maxs"] {
        asm.field(name, ty::VECTOR);
    }
    for name in ["chain", "chain2", "owner"] {
        asm.field(name, ty::ENTITY);
    }
    asm.field("count", ty::INTEGER);
    add_peek(asm);
    Harness::named(&["findentity"])(asm);
}

fn h() -> Harness {
    Harness::with(Numbering::Csqc, VmConfig::default(), setup)
}

fn fld(h: &Harness, name: &str) -> u32 {
    match h.vm.field::<u32>(name) {
        Ok(f) => f.offset().0,
        Err(_) => h.vm.field::<Vec3>(name).unwrap().offset().0,
    }
}

fn set_str(h: &mut Harness, e: EntRef, name: &str, text: Option<&str>) {
    let r = text.map_or(StrRef::NULL, |t| h.vm.intern(t.as_bytes()));
    let field = h.vm.field::<StrRef>(name).unwrap();
    h.vm.set_field(e, field, r);
}

fn set_f(h: &mut Harness, e: EntRef, name: &str, x: f32) {
    let field = h.vm.field::<f32>(name).unwrap();
    h.vm.set_field(e, field, x);
}

fn set_v(h: &mut Harness, e: EntRef, name: &str, x: Vec3) {
    let field = h.vm.field::<Vec3>(name).unwrap();
    h.vm.set_field(e, field, x);
}

fn get_e(h: &Harness, e: EntRef, name: &str) -> EntRef {
    h.vm.get_field(e, h.vm.field::<EntRef>(name).unwrap()).unwrap()
}

fn set_e(h: &mut Harness, e: EntRef, name: &str, x: EntRef) {
    let field = h.vm.field::<EntRef>(name).unwrap();
    h.vm.set_field(e, field, x);
}

fn spawn_n(h: &mut Harness, n: usize) -> Vec<EntRef> {
    (0..n).map(|_| h.call("spawn", &[]).unwrap().ent()).collect()
}

/// A chain as a list, following field `name` from `head`.
fn chain(h: &Harness, head: EntRef, name: &str) -> Vec<u32> {
    let mut out = Vec::new();
    let mut e = head;
    while e != WORLD && out.len() < 100 {
        out.push(e.0);
        e = get_e(h, e, name);
    }
    out
}

/// The zero-terminated entity list a `*_list` builtin returned, read back through QuakeC.
fn list(h: &mut Harness, p: u32) -> Vec<u32> {
    let mut out = Vec::new();
    for k in 0.. {
        let e = peek(h, p, k);
        if e == 0 {
            break;
        }
        out.push(e as u32);
    }
    out
}

#[test]
fn spawn_zeroes_and_remove_frees() {
    let mut h = h();
    let [a, b] = spawn_n(&mut h, 2)[..] else { panic!() };
    assert_eq!((a, b), (EntRef(1), EntRef(2)));
    set_f(&mut h, a, "health", 5.0);
    set_f(&mut h, a, "solid", 2.0);
    assert_eq!(h.f("wasfreed", &[ent(a)]), 0.0);
    h.call("remove", &[ent(a)]).unwrap();
    assert_eq!(h.f("wasfreed", &[ent(a)]), 1.0);
    // CSQC's remove clears a few engine fields; the rest stay readable until reuse.
    assert_eq!(h.vm.get_field(a, h.vm.field::<f32>("solid").unwrap()), Some(0.0));
    assert_eq!(h.vm.get_field(a, h.vm.field::<f32>("health").unwrap()), Some(5.0));
    // Within the first two seconds freed slots are reused at once, zeroed.
    let c = h.call("spawn", &[]).unwrap().ent();
    assert_eq!(c, a);
    assert_eq!(h.vm.get_field(c, h.vm.field::<f32>("health").unwrap()), Some(0.0));
    assert!(warnings(&h).is_empty());
}

#[test]
fn remove_refuses_world_free_and_protected_entities() {
    let mut h = h();
    let [a, b] = spawn_n(&mut h, 2)[..] else { panic!() };
    h.call("remove", &[ent(WORLD)]).unwrap();
    assert!(h.vm.is_in_use(WORLD));
    h.call("remove", &[ent(a)]).unwrap();
    h.call("remove", &[ent(a)]).unwrap();
    h.vm.set_protected(b, true);
    h.call("remove", &[ent(b)]).unwrap();
    assert!(h.vm.is_in_use(b));
    let w = warnings(&h);
    assert_eq!(w.len(), 3, "{w:?}");
    assert!(w[0].contains("Unable to remove the world"));
    assert!(w[1].contains("already free"));
    assert!(w[2].contains("protected"));
}

#[test]
fn removeinstant_makes_the_slot_reusable_at_once() {
    let mut h = h();
    let [a, b] = spawn_n(&mut h, 2)[..] else { panic!() };
    h.vm.set_realtime(10.0);
    h.call("remove", &[ent(a)]).unwrap();
    h.vm.set_realtime(10.1);
    let c = h.call("spawn", &[]).unwrap().ent();
    assert_eq!(c, EntRef(3), "a slot freed less than half a second ago is not reused");
    h.call("removeinstant", &[ent(b)]).unwrap();
    assert_eq!(h.call("spawn", &[]).unwrap().ent(), b);
    h.vm.set_realtime(10.7);
    assert_eq!(h.call("spawn", &[]).unwrap().ent(), a);
}

#[derive(Default)]
struct SpawnHost {
    spawned: Vec<EntRef>,
}

impl Host for SpawnHost {
    fn on_spawn(&mut self, vm: &mut Vm<Self>, e: EntRef) {
        let health = vm.field::<f32>("health").unwrap();
        vm.set_field(e, health, 100.0);
        self.spawned.push(e);
    }
}

#[test]
fn spawn_and_copyentity_run_the_spawn_hook() {
    let mut asm = Asm::new();
    asm.field("health", ty::FLOAT);
    asm.builtin("spawn", 14, 0);
    asm.builtin("copyentity", 400, 2);
    let program = Arc::new(Program::parse(&asm.build(ProgsFormat::Fte16)).unwrap());
    let builtins = Arc::new(Builtins::standard(Numbering::Csqc));
    let mut vm: Vm<SpawnHost> = Vm::new(program, builtins, VmConfig::default()).unwrap();
    let mut host = SpawnHost::default();
    let spawn = vm.find_function("spawn").unwrap();
    let e = vm.call(&mut host, spawn, &[]).unwrap().ent();
    assert_eq!(vm.get_field(e, vm.field::<f32>("health").unwrap()), Some(100.0));
    let copy = vm.find_function("copyentity").unwrap();
    let c = vm.call(&mut host, copy, &[Arg::Ent(e)]).unwrap().ent();
    assert_eq!(host.spawned, [e, c]);
}

#[test]
fn nextent_iterates_entities_in_use() {
    let mut h = h();
    let es = spawn_n(&mut h, 4);
    h.call("remove", &[ent(es[1])]).unwrap();
    let mut seen = Vec::new();
    let mut e = WORLD;
    loop {
        e = h.call("nextent", &[ent(e)]).unwrap().ent();
        if e == WORLD {
            break;
        }
        seen.push(e.0);
    }
    assert_eq!(seen, [1, 3, 4]);
    assert_eq!(h.call("nextent", &[ent(EntRef(999))]).unwrap().ent(), WORLD);
}

/// Entities 1–5 with classnames "foo", "bar", "foo", null, "".
fn classnames(h: &mut Harness) -> Vec<EntRef> {
    let es = spawn_n(h, 5);
    for (e, name) in es.iter().zip([Some("foo"), Some("bar"), Some("foo"), None, Some("")]) {
        set_str(h, *e, "classname", name);
    }
    es
}

#[test]
fn find_matches_string_fields() {
    let mut h = h();
    let es = classnames(&mut h);
    let cn = fld(&h, "classname");
    let find = |h: &mut Harness, start: EntRef, text: &str| {
        h.call("find", &[ent(start), w(cn), s(text)]).unwrap().ent().0
    };
    assert_eq!(find(&mut h, WORLD, "foo"), 1);
    assert_eq!(find(&mut h, es[0], "foo"), 3);
    assert_eq!(find(&mut h, es[2], "foo"), 0);
    assert_eq!(find(&mut h, WORLD, "baz"), 0);
    // Searching for "" matches null and empty fields.
    assert_eq!(find(&mut h, WORLD, ""), 4);
    assert_eq!(find(&mut h, es[3], ""), 5);
    assert_eq!(h.call("find", &[ent(WORLD), w(cn), w(0)]).unwrap().ent().0, 4);
    // Free entities are skipped.
    h.call("remove", &[ent(es[0])]).unwrap();
    assert_eq!(find(&mut h, WORLD, "foo"), 3);
    assert!(warnings(&h).is_empty());
    // Developer mode warns about searching for "".
    h.vm.set_developer(true);
    find(&mut h, WORLD, "");
    assert_eq!(warnings(&h), ["find: empty string"]);
    // A field outside the entity is a builtin error.
    h.vm.set_developer(false);
    let err = h.call("find", &[ent(WORLD), w(9999), s("foo")]).unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::Builtin(m) if m.contains("bad field")));
}

#[test]
fn findfloat_compares_raw_bits() {
    let mut h = h();
    let es = spawn_n(&mut h, 4);
    let hp = fld(&h, "health");
    for (e, x) in es.iter().zip([7.0, -0.0, f32::NAN, 7.0]) {
        set_f(&mut h, *e, "health", x);
    }
    let find = |h: &mut Harness, start: EntRef, x: f32| {
        h.call("findfloat", &[ent(start), w(hp), f(x)]).unwrap().ent().0
    };
    assert_eq!(find(&mut h, WORLD, 7.0), 1);
    assert_eq!(find(&mut h, es[0], 7.0), 4);
    assert_eq!(find(&mut h, WORLD, -0.0), 2, "-0 matches only -0");
    assert_eq!(find(&mut h, WORLD, 0.0), 0);
    assert_eq!(find(&mut h, WORLD, f32::NAN), 3, "a NaN matches its own bits");
    // Entity fields through the `findentity` alias.
    set_e(&mut h, es[3], "owner", es[1]);
    let owner = fld(&h, "owner");
    assert_eq!(h.call("findentity", &[ent(WORLD), w(owner), ent(es[1])]).unwrap().ent(), es[3]);
    // FTE insists on exactly three arguments.
    let err = h.call("findfloat", &[ent(WORLD), w(hp)]).unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::Builtin(_)));
}

#[test]
fn findflags_tests_bits() {
    let mut h = h();
    let es = spawn_n(&mut h, 3);
    for (e, x) in es.iter().zip([1.0, 3.0, 4.5]) {
        set_f(&mut h, *e, "flags", x);
    }
    let fl = fld(&h, "flags");
    let find = |h: &mut Harness, start: EntRef, x: f32| {
        h.call("findflags", &[ent(start), w(fl), f(x)]).unwrap().ent().0
    };
    assert_eq!(find(&mut h, WORLD, 2.0), 2);
    assert_eq!(find(&mut h, es[1], 2.0), 0);
    assert_eq!(find(&mut h, WORLD, 4.0), 3);
    assert_eq!(find(&mut h, es[0], 1.0), 2);
    assert_eq!(find(&mut h, WORLD, 8.0), 0);
}

#[test]
fn findchain_links_matches_through_chain() {
    let mut h = h();
    let es = classnames(&mut h);
    let cn = fld(&h, "classname");
    set_e(&mut h, es[1], "chain", es[4]);
    let head = h.call("findchain", &[w(cn), s("foo")]).unwrap().ent();
    assert_eq!(chain(&h, head, "chain"), [3, 1]);
    assert_eq!(get_e(&h, es[0], "chain"), WORLD);
    assert_eq!(get_e(&h, es[1], "chain"), es[4], "non-matching entities are untouched");
    // Unlike find, "" does not match null fields.
    let head = h.call("findchain", &[w(cn), s("")]).unwrap().ent();
    assert_eq!(chain(&h, head, "chain"), [5]);
    // Another chain field.
    let c2 = fld(&h, "chain2");
    let head = h.call("findchain", &[w(cn), s("bar"), w(c2)]).unwrap().ent();
    assert_eq!(chain(&h, head, "chain2"), [2]);
    assert_eq!(get_e(&h, es[1], "chain"), es[4]);
    assert_eq!(h.call("findchain", &[w(cn), s("nope")]).unwrap().ent(), WORLD);
}

#[test]
fn findchain_needs_a_chain_field() {
    let mut h = Harness::new();
    let err = h.call("findchain", &[w(0), s("foo")]).unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::Builtin(m) if m.contains("bad field")));
}

#[test]
fn findchainfloat_compares_values_and_findchainflags_bits() {
    let mut h = h();
    let es = spawn_n(&mut h, 4);
    for (e, x) in es.iter().zip([7.0, 7.0, f32::NAN, -0.0]) {
        set_f(&mut h, *e, "health", x);
    }
    let hp = fld(&h, "health");
    let head = h.call("findchainfloat", &[w(hp), f(7.0)]).unwrap().ent();
    assert_eq!(chain(&h, head, "chain"), [2, 1]);
    assert_eq!(h.call("findchainfloat", &[w(hp), f(f32::NAN)]).unwrap().ent(), WORLD);
    // -0 equals 0 by value.
    let head = h.call("findchainfloat", &[w(hp), f(0.0)]).unwrap().ent();
    assert_eq!(chain(&h, head, "chain"), [4]);

    for (e, x) in es.iter().zip([1.0, 2.0, 3.0, 0.0]) {
        set_f(&mut h, *e, "flags", x);
    }
    let fl = fld(&h, "flags");
    let c2 = fld(&h, "chain2");
    let head = h.call("findchainflags", &[w(fl), f(2.0), w(c2)]).unwrap().ent();
    assert_eq!(chain(&h, head, "chain2"), [3, 2]);
}

/// Entities for radius searches (see the test for the layout).
fn radius_scene(h: &mut Harness) -> Vec<EntRef> {
    let es = spawn_n(h, 5);
    let scene: [(Vec3, f32, f32); 5] = [
        ([10.0, 0.0, 0.0], 2.0, 0.0),
        ([100.0, 0.0, 0.0], 2.0, 0.0),
        ([20.0, 0.0, 0.0], 0.0, 0.0),
        ([0.0, 30.0, 0.0], 0.0, 16384.0),
        ([60.0, 0.0, 0.0], 3.0, 0.0),
    ];
    for (e, (origin, solid, flags)) in es.iter().zip(scene) {
        set_v(h, *e, "origin", origin);
        set_f(h, *e, "solid", solid);
        set_f(h, *e, "flags", flags);
    }
    // Entity 5's box is centred 15 units behind its origin.
    set_v(h, es[4], "mins", [-20.0, -5.0, -5.0]);
    set_v(h, es[4], "maxs", [-10.0, 5.0, 5.0]);
    es
}

#[test]
fn findradius_uses_box_centres_and_skips_nonsolid() {
    let mut h = h();
    radius_scene(&mut h);
    // 1 at 10 (solid), 2 too far, 3 non-solid, 4 non-solid but findable, 5 centred at 45.
    let head = h.call("findradius", &[v(0.0, 0.0, 0.0), f(50.0)]).unwrap().ent();
    assert_eq!(chain(&h, head, "chain"), [5, 4, 1]);
    // The radius is inclusive.
    let head = h.call("findradius", &[v(0.0, 0.0, 0.0), f(10.0)]).unwrap().ent();
    assert_eq!(chain(&h, head, "chain"), [1]);
    let c2 = fld(&h, "chain2");
    let head = h.call("findradius", &[v(100.0, 0.0, 0.0), f(1.0), w(c2)]).unwrap().ent();
    assert_eq!(chain(&h, head, "chain2"), [2]);
}

#[test]
fn findradius_list_returns_a_temp_array() {
    let mut h = h();
    radius_scene(&mut h);
    let r = h.call("findradius_list", &[v(0.0, 0.0, 0.0), f(50.0), i(0), i(1)]).unwrap();
    assert_eq!(parm_word(&h, 2), 3);
    assert!(h.vm.is_temp(r.str_ref()));
    assert_eq!(list(&mut h, r.0[0]), [1, 4, 5]);
    let r = h.call("findradius_list", &[v(500.0, 0.0, 0.0), f(1.0), i(99)]).unwrap();
    assert_eq!(parm_word(&h, 2), 0);
    assert_eq!(list(&mut h, r.0[0]), Vec::<u32>::new());
}

#[test]
fn find_list_by_type() {
    let mut h = h();
    let es = classnames(&mut h);
    let cn = fld(&h, "classname");
    let r = h.call("find_list", &[w(cn), s("foo"), i(1), i(0)]).unwrap();
    assert_eq!(parm_word(&h, 3), 2);
    assert_eq!(list(&mut h, r.0[0]), [1, 3]);
    // String searches skip protected entities; null fields never match, even "".
    h.vm.set_protected(es[2], true);
    let r = h.call("find_list", &[w(cn), s("foo"), i(1), i(0)]).unwrap();
    assert_eq!(list(&mut h, r.0[0]), [1]);
    let r = h.call("find_list", &[w(cn), s(""), i(1), i(0)]).unwrap();
    assert_eq!(list(&mut h, r.0[0]), [5]);
    // The type defaults to EV_STRING.
    let r = h.call("find_list", &[w(cn), s("bar")]).unwrap();
    assert_eq!(list(&mut h, r.0[0]), [2]);

    for (e, x) in es.iter().zip([1.0, -0.0, 1.0, 0.0, 2.0]) {
        set_f(&mut h, *e, "health", x);
    }
    let hp = fld(&h, "health");
    let r = h.call("find_list", &[w(hp), f(0.0), i(2), i(0)]).unwrap();
    assert_eq!(list(&mut h, r.0[0]), [2, 4], "floats compare by value");
    // Only string searches skip protected entities (entity 3 is still protected).
    let r = h.call("find_list", &[w(hp), f(1.0), i(2), i(0)]).unwrap();
    assert_eq!(list(&mut h, r.0[0]), [1, 3]);
    set_v(&mut h, es[4], "origin", [1.0, 2.0, 3.0]);
    let org = fld(&h, "origin");
    let r = h.call("find_list", &[w(org), v(1.0, 2.0, 3.0), i(3), i(0)]).unwrap();
    assert_eq!(list(&mut h, r.0[0]), [5]);
    let count = h.vm.field::<i32>("count").unwrap();
    h.vm.set_field(es[1], count, 42);
    let r = h.call("find_list", &[w(fld(&h, "count")), i(42), i(8), i(0)]).unwrap();
    assert_eq!(list(&mut h, r.0[0]), [2]);
    // An unsupported type or a field outside the entity: null and a zero count.
    let r = h.call("find_list", &[w(hp), f(0.0), i(13), i(7)]).unwrap();
    assert_eq!((r.0[0], parm_word(&h, 3)), (0, 0));
    let r = h.call("find_list", &[w(9999), f(0.0), i(2), i(7)]).unwrap();
    assert_eq!((r.0[0], parm_word(&h, 3)), (0, 0));
}

#[test]
fn edict_num_and_friends() {
    let mut h = h();
    let es = spawn_n(&mut h, 3);
    h.call("remove", &[ent(es[1])]).unwrap();
    assert_eq!(h.call("edict_num", &[f(2.0)]).unwrap().ent(), es[1], "free entities too");
    assert_eq!(h.call("edict_num", &[f(3.9)]).unwrap().ent(), es[2]);
    assert_eq!(h.call("edict_num", &[f(4.0)]).unwrap().ent(), WORLD);
    assert_eq!(h.call("edict_num", &[f(-1.0)]).unwrap().ent(), WORLD);
    assert_eq!(h.f("num_for_edict", &[ent(es[2])]), 3.0);
    assert!(warnings(&h).is_empty());
    // A reference past the allocated entities warns and reads as the world.
    assert_eq!(h.f("num_for_edict", &[ent(EntRef(999))]), 0.0);
    assert_eq!(h.f("wasfreed", &[ent(EntRef(999))]), 0.0);
    assert_eq!(warnings(&h), ["bad entity index 999", "bad entity index 999"]);
}

#[test]
fn menu_etof_and_ftoe() {
    let mut h = Harness::with(Numbering::Menu, VmConfig::menu(), |_| {});
    let e = h.call("spawn", &[]).unwrap().ent();
    assert_eq!(h.f("etof", &[ent(e)]), 1.0);
    assert_eq!(h.call("ftoe", &[f(1.0)]).unwrap().ent(), e);
    assert_eq!(h.call("ftoe", &[f(7.0)]).unwrap().ent(), WORLD);
    assert_eq!(h.call("ftoe", &[f(-1.0)]).unwrap().ent(), WORLD);
}

#[test]
fn copyentity_copies_every_field() {
    let mut h = h();
    let [a, b] = spawn_n(&mut h, 2)[..] else { panic!() };
    set_str(&mut h, a, "classname", Some("monster"));
    set_v(&mut h, a, "origin", [1.0, 2.0, 3.0]);
    set_f(&mut h, a, "health", 50.0);
    assert_eq!(h.call("copyentity", &[ent(a), ent(b)]).unwrap().ent(), b);
    assert_eq!(h.vm.get_field(b, h.vm.field::<Vec3>("origin").unwrap()), Some([1.0, 2.0, 3.0]));
    assert_eq!(h.vm.get_field(b, h.vm.field::<f32>("health").unwrap()), Some(50.0));
    // Without a destination a new entity is spawned.
    let c = h.call("copyentity", &[ent(a)]).unwrap().ent();
    assert_eq!(c, EntRef(3));
    let cn = h.vm.field::<StrRef>("classname").unwrap();
    let name = h.vm.get_field(c, cn).unwrap();
    assert_eq!(h.vm.str(name), b"monster");
}

#[test]
fn copyentity_refuses_free_and_protected_entities() {
    let mut h = h();
    let [a, b, c] = spawn_n(&mut h, 3)[..] else { panic!() };
    set_f(&mut h, a, "health", 9.0);
    h.call("remove", &[ent(c)]).unwrap();
    for (from, to, msg) in [(c, a, "source is free"), (a, c, "destination is free")] {
        let err = h.call("copyentity", &[ent(from), ent(to)]).unwrap_err();
        assert!(matches!(err.kind(), ErrorKind::Builtin(m) if m.contains(msg)), "{err}");
    }
    h.vm.set_protected(b, true);
    let err = h.call("copyentity", &[ent(a), ent(b)]).unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::Builtin(m) if m.contains("read-only")));
    // Developer mode: a warning, and FTE copies anyway.
    h.vm.set_developer(true);
    assert_eq!(h.call("copyentity", &[ent(a), ent(b)]).unwrap().ent(), b);
    assert_eq!(h.vm.get_field(b, h.vm.field::<f32>("health").unwrap()), Some(9.0));
    assert_eq!(warnings(&h), ["copyentity: destination is read-only"]);
}

#[test]
fn entityprotection_returns_the_previous_setting() {
    let mut h = h();
    let e = h.call("spawn", &[]).unwrap().ent();
    // FTE returns the new value; qcvm returns the previous one, as documented.
    assert_eq!(h.f("entityprotection", &[ent(e), f(1.0)]), 0.0);
    assert!(h.vm.is_protected(e));
    assert_eq!(h.f("entityprotection", &[ent(e), f(1.0)]), 1.0);
    assert_eq!(h.f("entityprotection", &[ent(e), f(0.0)]), 1.0);
    assert!(!h.vm.is_protected(e));
    // Values other than 0 and 1 change nothing.
    assert_eq!(h.f("entityprotection", &[ent(e), f(2.0)]), 0.0);
    assert!(!h.vm.is_protected(e));
    // The world can be unprotected too.
    h.vm.set_protected(WORLD, true);
    assert_eq!(h.f("entityprotection", &[ent(WORLD), f(0.0)]), 1.0);
    // A free entity is a builtin error.
    h.call("remove", &[ent(e)]).unwrap();
    let err = h.call("entityprotection", &[ent(e), f(1.0)]).unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::Builtin(m) if m.contains("free")));
}
