// SPDX-License-Identifier: MIT OR Apache-2.0

//! Field reflection and the savegame-style entity text of eprint/coredump.

use qcvm::{DumpKind, EntRef, FieldOfs, FuncRef, Numbering, Ptr, StrRef, Vec3, VmConfig};

use super::{ent, warnings};
use crate::support::asm::{Asm, ty};
use crate::support::harness::{Harness, f, s};

fn setup(asm: &mut Asm) {
    asm.field("classname", ty::STRING);
    asm.field("health", ty::FLOAT);
    asm.field("origin", ty::VECTOR);
    asm.field("owner", ty::ENTITY);
    asm.field("think", ty::FUNCTION);
    asm.field("watched", ty::FIELD);
    asm.field("count", ty::INTEGER);
    asm.field("target", ty::POINTER);
    // A saved global (flag 0x8000) and an unsaved one.
    asm.global("score", ty::FLOAT | 0x8000, &[3.5f32.to_bits()]);
    asm.global("scratch", ty::FLOAT, &[1.0f32.to_bits()]);
    asm.function("monster_think", &[], 0);
    asm.emit(qcvm::Op::Done, 0, 0, 0);
    Harness::named(&["findentityfield", "entityfieldref"])(asm);
}

fn h() -> Harness {
    Harness::with(Numbering::Csqc, VmConfig::default(), setup)
}

#[test]
fn field_table_reflection() {
    let mut h = h();
    // Vectors appear four times: v, v_x, v_y, v_z.
    assert_eq!(h.f("numentityfields", &[]), 11.0);
    assert_eq!(h.s("entityfieldname", &[f(0.0)]), b"classname");
    assert_eq!(h.s("entityfieldname", &[f(2.0)]), b"origin");
    assert_eq!(h.s("entityfieldname", &[f(4.0)]), b"origin_y");
    assert_eq!(h.f("entityfieldtype", &[f(2.0)]), 3.0);
    assert_eq!(h.f("entityfieldtype", &[f(3.0)]), 2.0);
    assert_eq!(h.f("entityfieldtype", &[f(10.0)]), 7.0);
    assert_eq!(h.f("findentityfield", &[s("origin_y")]), 4.0);
    assert_eq!(h.f("findentityfield", &[s("health")]), 1.0);
    assert_eq!(h.f("findentityfield", &[s("nope")]), 0.0);
    let origin_y = h.vm.field::<f32>("origin_y").unwrap().offset();
    assert_eq!(h.i("entityfieldref", &[f(4.0)]), origin_y.0 as i32);
    // Out of range: null, 0, 0.
    for i in [11.0, -1.0, 1e9] {
        assert_eq!(h.call("entityfieldname", &[f(i)]).unwrap().str_ref(), StrRef::NULL);
        assert_eq!(h.f("entityfieldtype", &[f(i)]), 0.0);
        assert_eq!(h.i("entityfieldref", &[f(i)]), 0);
    }
}

#[test]
fn entityfieldref_reads_fields_by_index() {
    let mut h = h();
    let e = h.vm.spawn().unwrap();
    h.vm.set_field(e, h.vm.field::<f32>("health").unwrap(), 42.0);
    let index = h.f("findentityfield", &[s("health")]);
    let fld = h.i("entityfieldref", &[f(index)]) as u32;
    assert_eq!(fld, h.vm.field::<f32>("health").unwrap().offset().0);
}

/// Fills entity `e` with one value of each type.
fn fill(h: &mut Harness, e: EntRef) {
    let text = h.vm.intern(b"mon\"st\\er\nx");
    h.vm.set_field(e, h.vm.field::<StrRef>("classname").unwrap(), text);
    h.vm.set_field(e, h.vm.field::<f32>("health").unwrap(), 50.0);
    h.vm.set_field(e, h.vm.field::<Vec3>("origin").unwrap(), [1.0, 2.5, -3.0]);
    h.vm.set_field(e, h.vm.field::<EntRef>("owner").unwrap(), EntRef(2));
    let think = h.vm.find_function("monster_think").unwrap();
    h.vm.set_field(e, h.vm.field::<FuncRef>("think").unwrap(), think);
    let health = h.vm.field::<f32>("health").unwrap().offset();
    h.vm.set_field(e, h.vm.field::<FieldOfs>("watched").unwrap(), health);
    h.vm.set_field(e, h.vm.field::<i32>("count").unwrap(), -5);
    h.vm.set_field(e, h.vm.field::<Ptr>("target").unwrap(), Ptr(0x1234));
}

fn last_dump(h: &Harness, kind: DumpKind) -> String {
    let (k, text) = h.host.dumps.last().expect("a dump");
    assert_eq!(*k, kind);
    String::from_utf8(text.clone()).unwrap()
}

#[test]
fn eprint_writes_savegame_style_fields() {
    let mut h = h();
    let e = h.vm.spawn().unwrap();
    let _other = h.vm.spawn().unwrap();
    fill(&mut h, e);
    h.call("eprint", &[ent(e)]).unwrap();
    let want = "Entity 1:\n{\n\
        \"classname\" \"mon\\\"st\\\\er\\nx\"\n\
        \"health\" \"50\"\n\
        \"origin\" \"1 2.5 -3\"\n\
        \"owner\" \"2\"\n\
        \"think\" \"0:monster_think\"\n\
        \"watched\" \"health\"\n\
        \"count\" \"-5\"\n\
        \"target\" \"0x1234\"\n\
        }\n\n";
    assert_eq!(last_dump(&h, DumpKind::Entity), want);

    // Zero fields are left out; integral floats and vectors print as integers, others with %f
    // and %g.
    let e2 = h.vm.spawn().unwrap();
    h.vm.set_field(e2, h.vm.field::<f32>("health").unwrap(), 0.5);
    h.vm.set_field(e2, h.vm.field::<Vec3>("origin").unwrap(), [1.0, -2.0, 1e10]);
    h.call("eprint", &[ent(e2)]).unwrap();
    let want = "Entity 3:\n{\n\"health\" \"0.500000\"\n\"origin\" \"1 -2 1e+10\"\n}\n\n";
    assert_eq!(last_dump(&h, DumpKind::Entity), want);
    h.vm.set_field(e2, h.vm.field::<Vec3>("origin").unwrap(), [0.1, 100000.0, 1234567.0]);
    h.vm.set_field(e2, h.vm.field::<f32>("health").unwrap(), -0.0);
    h.call("eprint", &[ent(e2)]).unwrap();
    // -0 is not all-zero bits, and prints as the integer 0.
    let want = "Entity 3:\n{\n\"health\" \"0\"\n\"origin\" \"0.1 100000 1.23457e+06\"\n}\n\n";
    assert_eq!(last_dump(&h, DumpKind::Entity), want);
}

#[test]
fn eprint_of_a_bad_reference_prints_the_world() {
    let mut h = h();
    h.call("eprint", &[ent(EntRef(77))]).unwrap();
    assert_eq!(last_dump(&h, DumpKind::Entity), "Entity 0:\n{\n}\n\n");
    assert_eq!(warnings(&h), ["bad entity index 77"]);
}

#[test]
fn coredump_lists_globals_and_entities() {
    let mut h = h();
    let e = h.vm.spawn().unwrap();
    fill(&mut h, e);
    let doomed = h.vm.spawn().unwrap();
    h.vm.remove(doomed, false);
    h.call("coredump", &[]).unwrap();
    let text = last_dump(&h, DumpKind::CoreDump);
    assert!(text.starts_with("general {\n"), "{text}");
    assert!(text.contains("\"numentities\" \"3\"\n"), "{text}");
    assert!(text.contains("stacktrace {\n"), "{text}");
    // Only globals marked for saving.
    assert!(text.contains("globals 0 {\n\"score\" \"3.500000\"\n}\n"), "{text}");
    assert!(!text.contains("scratch"), "{text}");
    assert!(text.contains("entity 0{\n}\n"), "{text}");
    assert!(text.contains("entity 1{\n\"classname\""), "{text}");
    assert!(text.contains("\"health\" \"50\"\n"), "{text}");
    assert!(!text.contains("entity 2{"), "free entities are left out: {text}");
}
