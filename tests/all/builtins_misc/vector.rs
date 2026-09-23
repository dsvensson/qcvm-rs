// SPDX-License-Identifier: MIT OR Apache-2.0

//! Vector and angle builtins.

use std::sync::Arc;

use qcvm::{Arg, Builtins, EntRef, ErrorKind, Numbering, Program, ProgsFormat, Vec3, Vm, VmConfig};

use super::ent;
use crate::support::asm::{Asm, ty};
use crate::support::harness::{BHost, Harness, v};

fn h() -> Harness {
    Harness::with(Numbering::Csqc, VmConfig::default(), |asm| {
        asm.field("angles", ty::VECTOR);
        asm.field("gravitydir", ty::VECTOR);
        for name in ["ideal_yaw", "yaw_speed", "idealpitch", "pitch_speed"] {
            asm.field(name, ty::FLOAT);
        }
    })
}

fn view(h: &Harness) -> [Vec3; 3] {
    ["v_forward", "v_right", "v_up"].map(|n| h.vm.get(h.vm.global::<Vec3>(n).unwrap()))
}

fn close(a: Vec3, b: Vec3) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-5)
}

#[track_caller]
fn assert_close(a: Vec3, b: Vec3) {
    assert!(close(a, b), "{a:?} != {b:?}");
}

#[test]
fn lengths_and_normals() {
    let mut h = h();
    assert_eq!(h.f("vlen", &[v(3.0, 4.0, 0.0)]), 5.0);
    assert_eq!(h.f("vlen", &[v(0.0, 0.0, 0.0)]), 0.0);
    assert_eq!(h.v("normalize", &[v(3.0, 4.0, 0.0)]), [0.6, 0.8, 0.0]);
    assert_eq!(h.v("normalize", &[v(0.0, 0.0, -2.0)]), [0.0, 0.0, -1.0]);
    assert_eq!(h.v("normalize", &[v(0.0, 0.0, 0.0)]), [0.0; 3]);
    assert_eq!(h.v("crossproduct", &[v(1.0, 0.0, 0.0), v(0.0, 1.0, 0.0)]), [0.0, 0.0, 1.0]);
    assert_eq!(h.v("crossproduct", &[v(1.0, 2.0, 3.0), v(4.0, 5.0, 6.0)]), [-3.0, 6.0, -3.0]);
}

#[test]
fn makevectors_writes_the_view_globals() {
    let mut h = h();
    h.call("makevectors", &[v(0.0, 0.0, 0.0)]).unwrap();
    assert_eq!(view(&h), [[1.0, 0.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, 1.0]]);
    h.call("makevectors", &[v(0.0, 90.0, 0.0)]).unwrap();
    let [fw, rt, up] = view(&h);
    assert_close(fw, [0.0, 1.0, 0.0]);
    assert_close(rt, [1.0, 0.0, 0.0]);
    assert_close(up, [0.0, 0.0, 1.0]);
    // Positive pitch looks down.
    h.call("makevectors", &[v(30.0, 0.0, 0.0)]).unwrap();
    let [fw, _, up] = view(&h);
    assert_close(fw, [0.866_025_4, 0.0, -0.5]);
    assert_close(up, [0.5, 0.0, 0.866_025_4]);
    // Roll tilts right and up.
    h.call("makevectors", &[v(0.0, 0.0, 90.0)]).unwrap();
    let [_, rt, up] = view(&h);
    assert_close(rt, [0.0, 0.0, -1.0]);
    assert_close(up, [0.0, -1.0, 0.0]);
}

#[test]
fn makevectors_without_view_globals_is_fatal() {
    let mut asm = Asm::new();
    asm.builtin("makevectors", 1, 1);
    let program = Arc::new(Program::parse(&asm.build(ProgsFormat::Fte16)).unwrap());
    let builtins = Arc::new(Builtins::standard(Numbering::Csqc));
    let mut vm: Vm<BHost> = Vm::new(program, builtins, VmConfig::default()).unwrap();
    let f = vm.find_function("makevectors").unwrap();
    let err = vm.call(&mut BHost::default(), f, &[Arg::Vector([0.0; 3])]).unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::Host(m) if m.contains("v_forward")));
    // Even in developer mode.
    vm.set_developer(true);
    assert!(vm.call(&mut BHost::default(), f, &[Arg::Vector([0.0; 3])]).is_err());
}

#[test]
fn vectoyaw_truncates_to_whole_degrees() {
    let mut h = h();
    for (dir, want) in [
        ([1.0, 0.0, 0.0], 0.0),
        ([0.0, 1.0, 0.0], 90.0),
        ([-1.0, 0.0, 0.0], 180.0),
        ([0.0, -1.0, 0.0], 270.0),
        ([0.0, 0.0, 1.0], 0.0),
        ([0.0, 0.0, 0.0], 0.0),
        // 5.71 degrees truncates to 5; -5.71 to -5, i.e. 355.
        ([1.0, 0.1, 0.0], 5.0),
        ([1.0, -0.1, 0.0], 355.0),
    ] {
        let [x, y, z] = dir;
        assert_eq!(h.f("vectoyaw", &[v(x, y, z)]), want, "vectoyaw({dir:?})");
    }
    // The degrees are computed in double precision, then truncated.
    let diagonal = (libm::atan2(1.0, 1.0) * 180.0 / std::f64::consts::PI).trunc() as f32;
    assert_eq!(h.f("vectoyaw", &[v(1.0, 1.0, 5.0)]), diagonal);
}

#[test]
fn vectoyaw_measures_in_the_reference_entitys_surface_frame() {
    let mut h = h();
    let e = h.vm.spawn().unwrap();
    let grav = h.vm.field::<Vec3>("gravitydir").unwrap();
    // No gravitydir: the world frame.
    assert_eq!(h.f("vectoyaw", &[v(0.0, 1.0, 0.0), ent(e)]), 90.0);
    // Normal gravity: still the world frame.
    h.vm.set_field(e, grav, [0.0, 0.0, -1.0]);
    assert_eq!(h.f("vectoyaw", &[v(0.0, 1.0, 0.0), ent(e)]), 90.0);
    // Upside down: y is mirrored.
    h.vm.set_field(e, grav, [0.0, 0.0, 1.0]);
    assert_eq!(h.f("vectoyaw", &[v(0.0, 1.0, 0.0), ent(e)]), 270.0);
}

#[test]
fn vectoangles_does_not_truncate_and_pitches_up() {
    let mut h = h();
    assert_eq!(h.v("vectoangles", &[v(1.0, 0.0, 0.0)]), [0.0, 0.0, 0.0]);
    assert_eq!(h.v("vectoangles", &[v(0.0, 0.0, 1.0)]), [90.0, 0.0, 0.0]);
    assert_eq!(h.v("vectoangles", &[v(0.0, 0.0, -1.0)]), [270.0, 0.0, 0.0]);
    assert_eq!(h.v("vectoangles", &[v(0.0, 0.0, 0.0)]), [270.0, 0.0, 0.0]);
    assert_eq!(h.v("vectoangles", &[v(-1.0, 0.0, 0.0)]), [0.0, 180.0, 0.0]);
    assert_close(h.v("vectoangles", &[v(1.0, 1.0, 0.0)]), [0.0, 45.0, 0.0]);
    assert_close(h.v("vectoangles", &[v(1.0, 0.0, 1.0)]), [45.0, 0.0, 0.0]);
    assert_close(h.v("vectoangles", &[v(1.0, 0.0, -1.0)]), [315.0, 0.0, 0.0]);
    assert_close(h.v("vectoangles", &[v(0.0, -1.0, 0.0)]), [0.0, 270.0, 0.0]);
    // Not truncated.
    let [_, yaw, _] = h.v("vectoangles", &[v(1.0, 0.1, 0.0)]);
    assert!((yaw - 5.710_593).abs() < 1e-4, "{yaw}");
}

#[test]
fn vectoangles_with_up_sets_the_roll() {
    let mut h = h();
    assert_close(h.v("vectoangles", &[v(1.0, 0.0, 0.0), v(0.0, 0.0, 1.0)]), [0.0, 0.0, 0.0]);
    assert_close(h.v("vectoangles", &[v(1.0, 0.0, 0.0), v(0.0, 0.0, -1.0)]), [0.0, 0.0, 180.0]);
    assert_close(h.v("vectoangles", &[v(1.0, 0.0, 0.0), v(0.0, 1.0, 0.0)]), [0.0, 0.0, 270.0]);
    // Straight up or down: the up vector gives the yaw.
    assert_close(h.v("vectoangles", &[v(0.0, 0.0, 1.0), v(-1.0, 0.0, 0.0)]), [90.0, 0.0, 0.0]);
    assert_close(h.v("vectoangles", &[v(0.0, 0.0, 1.0), v(0.0, -1.0, 0.0)]), [90.0, 90.0, 0.0]);
    assert_close(h.v("vectoangles", &[v(0.0, 0.0, -1.0), v(0.0, 1.0, 0.0)]), [270.0, 90.0, 0.0]);
    // Round trip through makevectors (whose pitch sign is the opposite).
    h.call("makevectors", &[v(-20.0, 30.0, 40.0)]).unwrap();
    let [fw, _, up] = view(&h);
    let a = h.v("vectoangles", &[Arg::Vector(fw), Arg::Vector(up)]);
    assert!(a.iter().zip([20.0, 30.0, 40.0]).all(|(x, y)| (x - y).abs() < 1e-3), "{a:?}");
}

#[test]
fn vectorvectors_builds_a_basis() {
    let mut h = h();
    h.call("vectorvectors", &[v(2.0, 0.0, 0.0)]).unwrap();
    assert_eq!(view(&h), [[1.0, 0.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, 1.0]]);
    h.call("vectorvectors", &[v(0.0, 0.0, 5.0)]).unwrap();
    assert_eq!(view(&h), [[0.0, 0.0, 1.0], [0.0, -1.0, 0.0], [-1.0, 0.0, 0.0]]);
    h.call("vectorvectors", &[v(0.0, 0.0, 0.0)]).unwrap();
    assert_eq!(view(&h), [[0.0; 3]; 3]);
    h.call("vectorvectors", &[v(3.0, 4.0, 0.0)]).unwrap();
    let [fw, rt, up] = view(&h);
    assert_close(fw, [0.6, 0.8, 0.0]);
    assert_close(rt, [0.8, -0.6, 0.0]);
    assert_close(up, [0.0, 0.0, 1.0]);
}

#[test]
fn rotatevectorsbyangle_uses_model_pitch() {
    let mut h = h();
    h.call("makevectors", &[v(0.0, 0.0, 0.0)]).unwrap();
    h.call("rotatevectorsbyangle", &[v(0.0, 90.0, 0.0)]).unwrap();
    let rotated = view(&h);
    h.call("makevectors", &[v(0.0, 90.0, 0.0)]).unwrap();
    let direct = view(&h);
    for (a, b) in rotated.into_iter().zip(direct) {
        assert_close(a, b);
    }
    // The pitch is negated (model angles): rotating the identity by pitch 30 looks up.
    h.call("makevectors", &[v(0.0, 0.0, 0.0)]).unwrap();
    h.call("rotatevectorsbyangle", &[v(30.0, 0.0, 0.0)]).unwrap();
    let rotated = view(&h);
    h.call("makevectors", &[v(-30.0, 0.0, 0.0)]).unwrap();
    for (a, b) in rotated.into_iter().zip(view(&h)) {
        assert_close(a, b);
    }
    // Rotations compose: yaw 90 then yaw 90 more.
    h.call("makevectors", &[v(0.0, 90.0, 0.0)]).unwrap();
    h.call("rotatevectorsbyangle", &[v(0.0, 90.0, 0.0)]).unwrap();
    assert_close(view(&h)[0], [-1.0, 0.0, 0.0]);
}

#[test]
fn rotatevectorsbyvectors_applies_a_basis() {
    let mut h = h();
    h.call("makevectors", &[v(0.0, 90.0, 0.0)]).unwrap();
    let basis = view(&h);
    h.call("makevectors", &[v(0.0, 0.0, 0.0)]).unwrap();
    let [fw, rt, up] = basis;
    h.call("rotatevectorsbyvectors", &[Arg::Vector(fw), Arg::Vector(rt), Arg::Vector(up)]).unwrap();
    for (a, b) in view(&h).into_iter().zip(basis) {
        assert_close(a, b);
    }
    h.call("rotatevectorsbyvectors", &[Arg::Vector(fw), Arg::Vector(rt), Arg::Vector(up)]).unwrap();
    assert_close(view(&h)[0], [-1.0, 0.0, 0.0]);
    assert_close(view(&h)[1], [0.0, 1.0, 0.0]);
}

/// Sets up `self` with angles and turning parameters.
fn turner(h: &mut Harness, angles: Vec3, ideal: (&str, f32), speed: (&str, f32)) -> EntRef {
    let e = h.vm.spawn().unwrap();
    let self_g = h.vm.global::<EntRef>("self").unwrap();
    h.vm.set(self_g, e);
    h.vm.set_field(e, h.vm.field::<Vec3>("angles").unwrap(), angles);
    h.vm.set_field(e, h.vm.field::<f32>(ideal.0).unwrap(), ideal.1);
    h.vm.set_field(e, h.vm.field::<f32>(speed.0).unwrap(), speed.1);
    e
}

fn angles(h: &Harness, e: EntRef) -> Vec3 {
    h.vm.get_field(e, h.vm.field::<Vec3>("angles").unwrap()).unwrap()
}

#[test]
fn changeyaw_turns_self_with_16_bit_angles() {
    let mut h = h();
    let e = turner(&mut h, [0.0, 0.0, 0.0], ("ideal_yaw", 90.0), ("yaw_speed", 20.0));
    h.call("changeyaw", &[]).unwrap();
    // 20 degrees, quantised to 1/65536 of a turn.
    assert_eq!(angles(&h, e), [0.0, 19.995_117, 0.0]);

    // The short way round, through 0.
    let e = turner(&mut h, [0.0, 10.0, 0.0], ("ideal_yaw", 350.0), ("yaw_speed", 5.0));
    h.call("changeyaw", &[]).unwrap();
    assert_eq!(angles(&h, e)[1], 4.993_286);
    let e = turner(&mut h, [0.0, 350.0, 0.0], ("ideal_yaw", 10.0), ("yaw_speed", 45.0));
    h.call("changeyaw", &[]).unwrap();
    assert_eq!(angles(&h, e)[1], 9.997_559);

    // Already there: nothing changes (not even quantisation).
    let e = turner(&mut h, [1.0, 90.0, 2.0], ("ideal_yaw", 90.0), ("yaw_speed", 5.0));
    h.call("changeyaw", &[]).unwrap();
    assert_eq!(angles(&h, e), [1.0, 90.0, 2.0]);
}

#[test]
fn changeyaw_turns_in_the_gravity_frame() {
    let mut h = h();
    let e = turner(&mut h, [0.0, 0.0, 0.0], ("ideal_yaw", 90.0), ("yaw_speed", 20.0));
    // Upside down: turning left in the surface frame turns right in the world.
    h.vm.set_field(e, h.vm.field::<Vec3>("gravitydir").unwrap(), [0.0, 0.0, 1.0]);
    h.call("changeyaw", &[]).unwrap();
    let [pitch, yaw, roll] = angles(&h, e);
    assert!((yaw - 340.0).abs() < 0.02, "{yaw}");
    assert!((roll - 180.0).abs() < 0.02, "{roll}");
    assert!(pitch.abs() < 0.02 || (pitch - 360.0).abs() < 0.02, "{pitch}");
}

#[test]
fn changepitch_turns_the_entity_passed() {
    let mut h = h();
    let other = turner(&mut h, [0.0, 0.0, 0.0], ("idealpitch", 30.0), ("pitch_speed", 10.0));
    // `self` is now `other`; turn a different entity.
    let e = h.vm.spawn().unwrap();
    h.vm.set_field(e, h.vm.field::<f32>("idealpitch").unwrap(), 30.0);
    h.vm.set_field(e, h.vm.field::<f32>("pitch_speed").unwrap(), 10.0);
    h.call("changepitch", &[ent(e)]).unwrap();
    assert_eq!(angles(&h, e), [9.997_559, 0.0, 0.0]);
    assert_eq!(angles(&h, other), [0.0; 3], "FTE turns self instead; qcvm turns the argument");
    // Without an argument it turns self.
    h.call("changepitch", &[]).unwrap();
    assert_eq!(angles(&h, other), [9.997_559, 0.0, 0.0]);
}

#[test]
fn turning_needs_the_fields() {
    let mut h = Harness::new();
    let err = h.call("changeyaw", &[]).unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::Builtin(m) if m.contains("angles")));
}
