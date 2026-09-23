// SPDX-License-Identifier: MIT OR Apache-2.0

//! Vector and angle builtins (docs/spec/builtins.md).
//!
//! Angles are `(pitch, yaw, roll)` in degrees. The arithmetic mirrors FTE's C code operation by
//! operation (float products, double trigonometry) so results match bit for bit.

use std::f64::consts::PI;

use crate::builtins::Builtins;
use crate::error::{ErrorKind, VmError};
use crate::host::Host;
use crate::value::{EntRef, Global, Vec3};
use crate::vm::Vm;
use crate::vm::core::Core;

use super::entity::{ent_arg, field_word, float, set_vector, set_word, vector};
use super::math::anglemod16;

/// Registers this module's builtins.
pub(crate) fn register<H: Host>(b: &mut Builtins<H>) {
    b.set("makevectors", makevectors::<H>);
    b.set("normalize", normalize::<H>);
    b.set("vlen", vlen::<H>);
    b.set("vectoyaw", vectoyaw::<H>);
    b.set("vectoangles", vectoangles::<H>);
    b.set("vectorvectors", vectorvectors::<H>);
    b.set("crossproduct", crossproduct::<H>);
    b.set("rotatevectorsbyangle", rotatevectorsbyangle::<H>);
    b.set("rotatevectorsbyvectors", rotatevectorsbyvectors::<H>);
    b.set("changeyaw", changeyaw::<H>);
    b.set("changepitch", changepitch::<H>);
}

// ---- vector maths (float, as FTE's vec_t) -----------------------------------------------------

fn dot(a: Vec3, b: Vec3) -> f32 {
    let ([ax, ay, az], [bx, by, bz]) = (a, b);
    ax * bx + ay * by + az * bz
}

fn cross(a: Vec3, b: Vec3) -> Vec3 {
    let ([ax, ay, az], [bx, by, bz]) = (a, b);
    [ay * bz - az * by, az * bx - ax * bz, ax * by - ay * bx]
}

fn neg(v: Vec3) -> Vec3 {
    v.map(|c| -c)
}

fn scale(v: Vec3, s: f32) -> Vec3 {
    v.map(|c| c * s)
}

/// `sqrt` of a float in double precision, rounded back to float.
fn fsqrt(x: f32) -> f32 {
    libm::sqrt(f64::from(x)) as f32
}

/// FTE's `VectorNormalize` (the reciprocal is taken in double precision).
fn normalized(v: Vec3) -> Vec3 {
    let len = fsqrt(dot(v, v));
    if len == 0.0 {
        return v;
    }
    scale(v, (1.0 / f64::from(len)) as f32)
}

/// Degrees to radians the way `AngleVectors` does it (double product, stored as float).
fn radians(deg: f32) -> f32 {
    (f64::from(deg) * (PI * 2.0 / 360.0)) as f32
}

fn sincos(rad: f32) -> (f32, f32) {
    let r = f64::from(rad);
    (libm::sin(r) as f32, libm::cos(r) as f32)
}

/// Quake's `AngleVectors`: forward, right and up for `(pitch, yaw, roll)` (positive pitch looks
/// down).
pub(crate) fn angle_vectors(angles: Vec3) -> [Vec3; 3] {
    let [pitch, yaw, roll] = angles;
    let (sy, cy) = sincos(radians(yaw));
    let (sp, cp) = sincos(radians(pitch));
    let (sr, cr) = sincos(radians(roll));
    let forward = [cp * cy, cp * sy, -sp];
    let right = [-sr * sp * cy + -cr * -sy, -sr * sp * sy + -cr * cy, -sr * cp];
    let up = [cr * sp * cy + -sr * -sy, cr * sp * sy + -sr * cy, cr * cp];
    [forward, right, up]
}

/// Angle vectors for model ("mesh") angles, whose pitch sign is inverted.
fn angle_vectors_mesh(angles: Vec3) -> [Vec3; 3] {
    let [pitch, yaw, roll] = angles;
    angle_vectors([-pitch, yaw, roll])
}

/// FTE's `VectorAngles` with mesh pitch: `(pitch, yaw, roll)` in degrees, each in `[0, 360)`
/// (positive pitch looks up). `up`, if given, determines the roll.
pub(crate) fn vector_angles(forward: Vec3, up: Option<Vec3>) -> Vec3 {
    let [fx, fy, fz] = forward;
    let (pitch, yaw, roll): (f32, f32, f32);
    if fy == 0.0 && fx == 0.0 {
        if fz > 0.0 {
            pitch = (-PI * 0.5) as f32;
            yaw = up.map_or(0.0, |[ux, uy, _]| libm::atan2(f64::from(-uy), f64::from(-ux)) as f32);
        } else {
            pitch = (PI * 0.5) as f32;
            yaw = up.map_or(0.0, |[ux, uy, _]| libm::atan2(f64::from(uy), f64::from(ux)) as f32);
        }
        roll = 0.0;
    } else {
        yaw = libm::atan2(f64::from(fy), f64::from(fx)) as f32;
        let horizontal = libm::sqrt(f64::from(fx * fx + fy * fy));
        pitch = -libm::atan2(f64::from(fz), horizontal) as f32;
        roll = match up {
            Some(up) => {
                let (sp, cp) = sincos(pitch);
                let (sy, cy) = sincos(yaw);
                let left = [-sy, cy, 0.0];
                let tup = [sp * cy, sp * sy, cp];
                -libm::atan2(f64::from(dot(up, left)), f64::from(dot(up, tup))) as f32
            }
            None => 0.0,
        };
    }
    let degrees = |r: f32| (f64::from(r) * (180.0 / PI)) as f32;
    let wrap = |d: f32| if d < 0.0 { d + 360.0 } else { d };
    // Mesh pitch: FTE multiplies by r_meshpitch (-1) for Quake models.
    [wrap(-degrees(pitch)), wrap(degrees(yaw)), wrap(degrees(roll))]
}

/// Quake 3's `PerpendicularVector`: a unit vector perpendicular to the unit vector `src`,
/// obtained by projecting out its smallest-magnitude axis.
fn perpendicular(src: Vec3) -> Vec3 {
    let mut pos = 0;
    let mut min = 1.0f32;
    for (i, c) in src.iter().enumerate() {
        if c.abs() < min {
            pos = i;
            min = c.abs();
        }
    }
    let mut axis = [0.0f32; 3];
    if let Some(a) = axis.get_mut(pos) {
        *a = 1.0;
    }
    let inv = 1.0 / dot(src, src);
    let d = dot(src, axis) * inv;
    let n = scale(src, inv);
    normalized(
        [0usize, 1, 2]
            .map(|k| axis.get(k).copied().unwrap_or(0.0) - d * n.get(k).copied().unwrap_or(0.0)),
    )
}

/// The surface frame of an entity with a non-zero `gravitydir`: `[x, y, up]` with
/// `up = -normalize(gravitydir)`. `None` for zero gravity (the world axes).
fn gravity_axis(gravitydir: Vec3) -> Option<[Vec3; 3]> {
    if gravitydir == [0.0; 3] {
        return None;
    }
    let up = normalized(neg(gravitydir));
    let x = normalized(perpendicular(up));
    let y = normalized(cross(up, x));
    Some([x, y, up])
}

fn entity_gravity(core: &Core, e: u32) -> Vec3 {
    field_word(core, "gravitydir").map_or([0.0; 3], |f| vector(core, e, f))
}

/// Row-major product of two 3×3 matrices given as rows.
fn concat(a: [Vec3; 3], b: [Vec3; 3]) -> [Vec3; 3] {
    let [b0, b1, b2] = b;
    a.map(|[x, y, z]| {
        [
            x * b0[0] + y * b1[0] + z * b2[0],
            x * b0[1] + y * b1[1] + z * b2[1],
            x * b0[2] + y * b1[2] + z * b2[2],
        ]
    })
}

// ---- globals ----------------------------------------------------------------------------------

fn view_globals<H: Host>(vm: &Vm<H>) -> Result<[Global<Vec3>; 3], VmError> {
    match (vm.global::<Vec3>("v_forward"), vm.global::<Vec3>("v_right"), vm.global::<Vec3>("v_up"))
    {
        (Ok(f), Ok(r), Ok(u)) => Ok([f, r, u]),
        _ => Err(VmError::new(ErrorKind::Host(
            "makevectors: one of v_forward, v_right or v_up was not defined".into(),
        ))),
    }
}

fn set_view<H: Host>(vm: &mut Vm<H>, v: [Vec3; 3]) -> Result<(), VmError> {
    for (g, v) in view_globals(vm)?.into_iter().zip(v) {
        vm.core.mem.set_gv(crate::bytes::usize_from(g.addr), v);
    }
    Ok(())
}

fn get_view<H: Host>(vm: &Vm<H>) -> Result<[Vec3; 3], VmError> {
    let [gf, gr, gu] = view_globals(vm)?;
    Ok([vm.get(gf), vm.get(gr), vm.get(gu)])
}

// ---- builtins ---------------------------------------------------------------------------------

/// `void makevectors(vector angles)`: sets `v_forward`, `v_right` and `v_up` (fatal if the
/// progs lacks one of them).
pub fn makevectors<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let v = angle_vectors(vm.arg_vec(0));
    set_view(vm, v)
}

/// `vector normalize(vector v)`: `v` scaled to length 1 (`'0 0 0'` stays zero).
pub fn normalize<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let v = vm.arg_vec(0);
    let len = fsqrt(dot(v, v));
    let r = if len == 0.0 { [0.0; 3] } else { scale(v, 1.0 / len) };
    vm.ret_vec(r);
    Ok(())
}

/// `float vlen(vector v)`: the length of `v`.
pub fn vlen<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let v = vm.arg_vec(0);
    vm.ret_f32(fsqrt(dot(v, v)));
    Ok(())
}

/// `float vectoyaw(vector v, optional entity reference)`: the yaw of `v` in whole degrees
/// (truncated), in `[0, 360)`. With a reference entity whose `gravitydir` is non-zero, the yaw is
/// measured in that entity's surface frame.
pub fn vectoyaw<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let v = vm.arg_vec(0);
    let [mut x, mut y, _] = v;
    if vm.argc() >= 2 {
        let e = ent_arg(vm, 1);
        if let Some([ax, ay, _]) = gravity_axis(entity_gravity(&vm.core, e)) {
            x = dot(v, ax);
            y = dot(v, ay);
        }
    }
    let yaw = if x == 0.0 && y == 0.0 {
        0.0
    } else {
        let deg = libm::atan2(f64::from(y), f64::from(x)) * 180.0 / PI;
        let yaw = super::math::d2i(deg) as f32;
        if yaw < 0.0 { yaw + 360.0 } else { yaw }
    };
    vm.ret_f32(yaw);
    Ok(())
}

/// `vector vectoangles(vector forward, optional vector up)`: the angles of a direction (positive
/// pitch looks up; not truncated), each in `[0, 360)`. `up` determines the roll.
pub fn vectoangles<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let forward = vm.arg_vec(0);
    let up = (vm.argc() >= 2).then(|| vm.arg_vec(1));
    vm.ret_vec(vector_angles(forward, up));
    Ok(())
}

/// `void vectorvectors(vector dir)`: sets `v_forward` to `normalize(dir)` and derives `v_right`
/// (horizontal) and `v_up` from it.
pub fn vectorvectors<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let forward = normalized(vm.arg_vec(0));
    let [fx, fy, fz] = forward;
    let right = if fx == 0.0 && fy == 0.0 {
        [0.0, if fz != 0.0 { -1.0 } else { 0.0 }, 0.0]
    } else {
        normalized([fy, -fx, 0.0])
    };
    let up = cross(right, forward);
    set_view(vm, [forward, right, up])
}

/// `vector crossproduct(vector a, vector b)`.
pub fn crossproduct<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let (a, b) = (vm.arg_vec(0), vm.arg_vec(1));
    vm.ret_vec(cross(a, b));
    Ok(())
}

/// Rotates the view vectors by `trans` (rows forward, left, up).
fn rotate_view<H: Host>(vm: &mut Vm<H>, trans: [Vec3; 3], base: [Vec3; 3]) -> Result<(), VmError> {
    let [f, l, u] = concat(trans, base);
    set_view(vm, [f, neg(l), u])
}

/// `void rotatevectorsbyangle(vector angles)`: rotates `v_forward`, `v_right` and `v_up` by the
/// (model) angles.
pub fn rotatevectorsbyangle<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let [f, r, u] = angle_vectors_mesh(vm.arg_vec(0));
    let [vf, vr, vu] = get_view(vm)?;
    rotate_view(vm, [f, neg(r), u], [vf, neg(vr), vu])
}

/// `void rotatevectorsbyvectors(vector forward, vector right, vector up)`: rotates `v_forward`,
/// `v_right` and `v_up` by the given basis.
pub fn rotatevectorsbyvectors<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let base = [vm.arg_vec(0), neg(vm.arg_vec(1)), vm.arg_vec(2)];
    let [vf, vr, vu] = get_view(vm)?;
    rotate_view(vm, [vf, neg(vr), vu], base)
}

/// Turns an angle toward `ideal` by at most `speed`, the Quake way (angles quantised to 1/65536
/// of a turn).
fn turn(current: f32, ideal: f32, speed: f32) -> Option<f32> {
    let current = anglemod16(current);
    if current == ideal {
        return None;
    }
    let mut delta = ideal - current;
    if ideal > current {
        if delta >= 180.0 {
            delta -= 360.0;
        }
    } else if delta <= -180.0 {
        delta += 360.0;
    }
    Some(anglemod16(current + limit(delta, speed)))
}

fn required_field(core: &Core, builtin: &str, name: &str) -> Result<u32, VmError> {
    field_word(core, name)
        .ok_or_else(|| VmError::builtin(format!("{builtin}: the progs has no .{name} field")))
}

fn self_entity<H: Host>(vm: &Vm<H>, builtin: &str) -> Result<u32, VmError> {
    let g = vm
        .global::<EntRef>("self")
        .map_err(|_| VmError::builtin(format!("{builtin}: the progs has no `self` global")))?;
    let e = vm.get(g).0;
    Ok(if e < vm.num_edicts() { e } else { 0 })
}

/// Clamps a turn to ±`speed` like `changeyaw`'s surface-frame path (which does not compare the
/// ideal angle with the current one first).
fn clamp_turn(delta: f32, speed: f32) -> f32 {
    let delta = if delta > 180.0 {
        delta - 360.0
    } else if delta < -180.0 {
        delta + 360.0
    } else {
        delta
    };
    limit(delta, speed)
}

/// Limits a turn to ±`speed` with C's comparisons (NaNs pass through unchanged).
fn limit(delta: f32, speed: f32) -> f32 {
    if delta > 0.0 {
        if delta > speed { speed } else { delta }
    } else if delta < -speed {
        -speed
    } else {
        delta
    }
}

/// `changeyaw` for an entity standing on a surface given by its gravity axis: the turn happens
/// in the surface frame, where pitch and roll are zeroed.
fn changeyaw_on_surface(angles: Vec3, surf: [Vec3; 3], ideal: f32, speed: f32) -> Vec3 {
    let [s0, s1, s2] = surf;
    let [f, _, u] = angle_vectors_mesh(angles);
    let to_surface = |v: Vec3| [dot(v, s0), dot(v, s1), dot(v, s2)];
    let [_, rel_yaw, _] = vector_angles(to_surface(f), Some(to_surface(u)));
    let delta = clamp_turn(ideal - anglemod16(rel_yaw), speed);
    let yaw = anglemod16(rel_yaw + delta);
    let [f2, _, u2] = angle_vectors([0.0, yaw, 0.0]);
    let to_world = |[x, y, z]: Vec3| {
        [0usize, 1, 2].map(|k| {
            let c = |v: Vec3| v.get(k).copied().unwrap_or(0.0);
            x * c(s0) + y * c(s1) + z * c(s2)
        })
    };
    vector_angles(to_world(f2), Some(to_world(u2))).map(anglemod16)
}

/// `void changeyaw()`: turns `self.angles_y` toward `self.ideal_yaw` by at most
/// `self.yaw_speed`. An entity with a non-zero `gravitydir` turns in its surface frame.
pub fn changeyaw<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let e = self_entity(vm, "changeyaw")?;
    let core = &vm.core;
    let angles_f = required_field(core, "changeyaw", "angles")?;
    let ideal = float(core, e, required_field(core, "changeyaw", "ideal_yaw")?);
    let speed = float(core, e, required_field(core, "changeyaw", "yaw_speed")?);
    let angles = vector(core, e, angles_f);
    if let Some(surf) = gravity_axis(entity_gravity(core, e)) {
        let new = changeyaw_on_surface(angles, surf, ideal, speed);
        set_vector(&mut vm.core, e, angles_f, new);
        return Ok(());
    }
    let [_, yaw, _] = angles;
    if let Some(yaw) = turn(yaw, ideal, speed) {
        set_word(&mut vm.core, e, angles_f.wrapping_add(1), yaw.to_bits());
    }
    Ok(())
}

/// `void changepitch(entity e)`: turns `e.angles_x` toward `e.idealpitch` by at most
/// `e.pitch_speed`. FTE ignores the argument and turns `self`; qcvm turns the entity passed
/// (`self` if called without arguments).
pub fn changepitch<H: Host>(vm: &mut Vm<H>, _host: &mut H) -> Result<(), VmError> {
    let e = if vm.argc() > 0 { ent_arg(vm, 0) } else { self_entity(vm, "changepitch")? };
    let core = &vm.core;
    let angles_f = required_field(core, "changepitch", "angles")?;
    let ideal = float(core, e, required_field(core, "changepitch", "idealpitch")?);
    let speed = float(core, e, required_field(core, "changepitch", "pitch_speed")?);
    let pitch = float(core, e, angles_f);
    if let Some(pitch) = turn(pitch, ideal, speed) {
        set_word(&mut vm.core, e, angles_f, pitch.to_bits());
    }
    Ok(())
}
