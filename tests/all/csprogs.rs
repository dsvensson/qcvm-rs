// SPDX-License-Identifier: MIT OR Apache-2.0

//! Runs a real CSQC module — KTX's weapon-prediction `csprogs.dat`, located through
//! `QCVM_CSPROGS` (with its `.lno` beside it) — against the stub engine in
//! `support/csqc_engine.rs`. The scenarios follow what the mod does in a real client:
//! initialisation, weapon snapshots, projectile entities, a thousand predicted frames with the
//! fire button held, temp entities, sound suppression and error reporting.

use std::collections::HashMap;
use std::sync::Arc;

use qcvm::{Arg, EntRef, ErrorKind, FuncRef, Program, Vm, VmConfig};

use crate::support::csqc_engine::*;
use crate::support::tools;

fn load() -> Option<Arc<Program>> {
    let Some(path) = tools::csprogs() else {
        tools::skip("csprogs", "QCVM_CSPROGS");
        return None;
    };
    Some(load_program(&path))
}

// ---- scenarios --------------------------------------------------------------------------

#[test]
fn csprogs_every_called_builtin_is_bound_and_handles_resolve() {
    let Some(program) = load() else { return };
    let vm: Vm<Engine> = Vm::new(program.clone(), builtins(), VmConfig::csqc()).unwrap();
    // The pre-flight check an engine makes before enabling CSQC: the mod's debug build declares
    // hundreds of builtins, but only the ones it calls matter.
    assert!(program.called_builtins().len() > 30);
    assert!(vm.unbound_builtins().len() > 100);
    let unbound: Vec<String> = vm
        .reachable_unbound_builtins()
        .iter()
        .map(|u| String::from_utf8_lossy(&u.name).into_owned())
        .collect();
    assert!(unbound.is_empty(), "unbound builtins the progs calls: {unbound:?}");

    // Every global and field the engine reads or writes resolves with the engine's type.
    for name in ["time", "cltime", "frametime", "player_localentnum", "player_localnum"] {
        vm.global::<f32>(name).unwrap();
    }
    for name in ["clientcommandframe", "servercommandframe", "intermission", "trace_fraction"] {
        vm.global::<f32>(name).unwrap();
    }
    for name in ["input_timelength", "input_buttons", "input_impulse"] {
        vm.global::<f32>(name).unwrap();
    }
    for name in ["pmove_org", "view_angles", "input_angles", "trace_endpos", "trace_plane_normal"] {
        vm.global::<[f32; 3]>(name).unwrap();
    }
    for name in ["v_forward", "v_right", "v_up"] {
        vm.global::<[f32; 3]>(name).unwrap();
    }
    vm.global::<EntRef>("self").unwrap();
    assert!(vm.global::<f32>("self").is_err(), "type mismatches are rejected");
    for name in ["origin", "angles", "velocity"] {
        vm.field::<[f32; 3]>(name).unwrap();
    }
    for name in ["modelindex", "modelflags", "drawmask", "renderflags", "dimension_hit"] {
        vm.field::<f32>(name).unwrap();
    }
    vm.field::<FuncRef>("predraw").unwrap();

    // Autocvars with their compiled defaults.
    let defaults: HashMap<Vec<u8>, f32> = program
        .autocvars()
        .map(|c| (c.name.to_vec(), f32::from_bits(c.default.first().copied().unwrap_or(0))))
        .collect();
    assert_eq!(defaults.get(&b"cl_predict_projectiles"[..]), Some(&1.0));
    assert_eq!(defaults.get(&b"cl_predict_debug"[..]), Some(&0.0));
}

#[test]
fn csprogs_init_and_entity_updates() {
    let Some(program) = load() else { return };
    let mut c = Client::new(program, builtins(), Engine::new());

    // CSQC_Init precached every weapon and spawned the view weapon.
    assert!(c.host.model_index(b"progs/v_rock2.mdl").is_some());
    assert!(c.host.model_index(ROCKET_MODEL).is_some());
    assert!(c.host.sounds_precached.iter().any(|s| s == ROCKET_SOUND));
    let viewweapon = c.vm.get(c.vm.global::<EntRef>("viewweapon").unwrap());
    assert_ne!(viewweapon, EntRef(0));
    assert_eq!(field_f32(&c.vm, viewweapon, "renderflags"), RF_VIEWMODEL);
    assert_eq!(field_f32(&c.vm, viewweapon, "dimension_hit"), 254.0);
    assert_eq!(field_f32(&c.vm, viewweapon, "dimension_solid"), 255.0, "CSQC spawn default");
    assert!(c.vm.is_protected(EntRef(0)));

    // The first weapon snapshot announces prediction, once.
    let snap = Snapshot {
        weapon: ROCKET_LAUNCHER,
        generation: 1,
        rockets: 20,
        attack_finished: 0.0,
        client_time: 0.0,
        ping_ms: 50,
        predflags: 0,
    };
    c.ent_update(500, snap.msg()).unwrap();
    c.ent_update(500, snap.msg()).unwrap();
    let printed = String::from_utf8_lossy(&c.host.printed).into_owned();
    assert_eq!(printed.matches("CSQC Antilag ready").count(), 1, "{printed}");

    // A server projectile: model by index, drawn through the engine mask with a predraw.
    let model = c.host.model_index(ROCKET_MODEL).unwrap() as i16;
    c.ent_update(600, projectile_msg([100.0, 200.0, 50.0], [0.0, 1000.0, 0.0], model, 2)).unwrap();
    let p = c.ents[&600];
    assert_eq!(field_f32(&c.vm, p, "modelindex"), f32::from(model));
    assert_eq!(field_f32(&c.vm, p, "modelflags"), 1.0);
    assert_eq!(field_f32(&c.vm, p, "drawmask"), MASK_ENGINE as f32);
    assert_eq!(field_f32(&c.vm, p, "dimension_hit"), 254.0);
    assert_eq!(field_vec(&c.vm, p, "origin"), [100.0, 200.0, 50.0]);
    let predraw = c.vm.field::<FuncRef>("predraw").unwrap();
    assert_ne!(c.vm.get_field(p, predraw), Some(FuncRef::NULL));

    // The projectile flies on in its predraw and is drawn; removal frees it.
    c.render(false);
    c.render(false);
    let last = c.host.rendered.last().unwrap();
    assert!(last.contains(&p.0), "{last:?}");
    assert!(field_vec(&c.vm, p, "origin")[1] > 200.0);
    c.ent_remove(600);

    // Temp entities are passed back to the engine.
    c.host.net = Msg::default().byte(TE_LIGHTNING2).short(1).0.into();
    let r = c.call("CSQC_Parse_TempEntity", &[]).unwrap();
    assert_eq!(r.f32(), 0.0);
    assert!(c.host.net.is_empty());

    c.call("CSQC_Input_Frame", &[]).unwrap();
}

/// Plays `frames` frames with the fire button held. Returns the frames on which the predicted
/// rocket sound played.
fn fire_rockets(c: &mut Client, frames: i32) -> Vec<i32> {
    let mut sound_frames = Vec::new();
    for _ in 0..frames {
        let before = c.host.sounds.len();
        c.step(true);
        if c.host.sounds[before..].iter().any(|s| s.2 == ROCKET_SOUND) {
            sound_frames.push(c.frame - 1);
        }
    }
    sound_frames
}

#[test]
fn csprogs_predicts_a_thousand_frames_of_fire() {
    let Some(program) = load() else { return };
    let mut c = Client::new(program, builtins(), Engine::new());
    let sounds = fire_rockets(&mut c, 1000);

    // One predicted shot per refire interval (0.8 s at 60 fps = 48 frames), matching the server
    // shot for shot, except the server's first shot, which came before any snapshot. Replay walks
    // the same input frames every render, so this also checks that a shot's effects fire once.
    // (Builds of weapon_state.qc before the fix of `frame_may_sound = (a) && (b)`, which
    // QuakeC's precedence parsed as `(frame_may_sound = a) && b`, repeated them for ~10 frames.)
    assert_eq!(sounds.len() as u32 + 1, c.server.shots, "{sounds:?}");
    for w in sounds.windows(2) {
        assert!((48..=49).contains(&(w[1] - w[0])), "shots {sounds:?}");
    }
    // Every predicted shot spawned one local rocket; they expire in their predraw (removing
    // themselves mid-walk) after about four frames, long before the next shot.
    let spawned =
        c.host.log.iter().filter(|l| l.starts_with("setmodel") && l.ends_with("missile.mdl"));
    assert_eq!(spawned.count(), sounds.len());
    let is_local = c.vm.field::<f32>("is_local").unwrap();
    let alive = c.vm.entities().filter(|&e| c.vm.get_field(e, is_local) == Some(1.0)).count();
    assert!(alive <= 1, "{alive} local rockets still alive");

    // Once the first snapshot has arrived (frame 6) we draw the view weapon ourselves; before
    // that the engine's is used.
    let viewweapon = c.vm.get(c.vm.global::<EntRef>("viewweapon").unwrap());
    assert!(c.host.rendered[..6].iter().all(|s| !s.contains(&viewweapon.0)));
    assert!(c.host.rendered[6..].iter().all(|s| s.contains(&viewweapon.0)));
    assert!(c.host.log.iter().any(|l| l == "addentities 2"), "engine view model before that");
    let frame_log: Vec<&str> =
        c.host.log.iter().rev().take_while(|l| *l != "clearscene").map(String::as_str).collect();
    assert!(frame_log.first().is_some_and(|l| l.starts_with("renderscene")), "{frame_log:?}");
    assert!(frame_log.contains(&"addentities 1"), "{frame_log:?}");
    assert!(frame_log.iter().any(|l| *l == format!("addentity #{}", viewweapon.0)));

    // Temp strings are collected as frames return to the engine: the count stays below the
    // collector's trigger (half the initial 1024-slot table) however long the game runs.
    let mut peak = 0;
    for _ in 0..2000 {
        c.step(false);
        peak = peak.max(c.vm.temp_strings());
    }
    assert!(peak <= 512, "{peak} live temps");
    assert!(c.host.warnings.is_empty(), "{:?}", c.host.warnings);
}

#[test]
fn csprogs_suppresses_the_servers_echo_of_a_predicted_sound() {
    let Some(program) = load() else { return };
    let mut c = Client::new(program, builtins(), Engine::new());
    let sounds = fire_rockets(&mut c, 60);
    assert!(!sounds.is_empty());
    let event = |c: &mut Client, sample: &[u8]| {
        c.call(
            "CSQC_Event_Sound",
            &[
                Arg::Float(PLAYER_ENT),
                Arg::Float(CHAN_WEAPON),
                Arg::Bytes(sample),
                Arg::Float(1.0),
                Arg::Float(0.0),
                Arg::Vector([0.0; 3]),
                Arg::Float(0.0),
                Arg::Float(0.0),
            ],
        )
        .unwrap()
        .f32()
    };
    // The host's copy of the sample name (a temp string) matches the progs' constant; the
    // predicted play left one token, which the first echo consumes.
    assert_eq!(sounds.len(), 1, "{sounds:?}");
    assert_eq!(event(&mut c, ROCKET_SOUND), 1.0, "echo of the predicted shot is dropped");
    assert_eq!(event(&mut c, ROCKET_SOUND), 0.0, "the token was consumed");
    assert_eq!(event(&mut c, b"weapons/grenade.wav"), 0.0, "sounds not predicted play");
}

#[test]
fn csprogs_debug_output_and_projectile_hand_off() {
    let Some(program) = load() else { return };
    let mut host = Engine::new();
    host.cvars.insert(b"cl_predict_debug".to_vec(), b"2".to_vec());
    let mut c = Client::new(program, builtins(), host);
    while c.local_projectile().is_none() {
        assert!(c.frame < 120, "no rocket predicted");
        c.step(true);
    }

    // The server's copy of our rocket arrives near the predicted one and takes it over.
    let model = c.host.model_index(ROCKET_MODEL).unwrap() as i16;
    let local = c.local_projectile().unwrap();
    let at = field_vec(&c.vm, local, "origin");
    c.ent_update(700, projectile_msg(at, [0.0, 1000.0, 0.0], model, PLAYER_ENT as i16)).unwrap();
    assert!(!c.vm.is_in_use(local), "the local rocket was handed over");
    let printed = String::from_utf8_lossy(&c.host.printed).into_owned();
    for needle in ["wpred: ", "proj: predicted progs/missile.mdl on frame ", "proj: took over, "] {
        assert!(printed.contains(needle), "missing {needle:?} in:\n{printed}");
    }
    assert!(c.host.warnings.is_empty(), "{:?}", c.host.warnings);
}

#[test]
fn csprogs_unknown_entity_kind_is_a_qc_error_with_a_backtrace() {
    let Some(program) = load() else { return };
    let has_lines = tools::csprogs().unwrap().with_extension("lno").exists();
    let mut c = Client::new(program, builtins(), Engine::new());
    let err = c.ent_update(900, Msg::default().byte(7)).unwrap_err();
    match err.kind() {
        ErrorKind::QcError(msg) => assert_eq!(&**msg, b"csqc: unknown entity type 7\n"),
        other => panic!("{other:?}"),
    }
    let frame = err.backtrace().0.iter().find(|f| &*f.name == b"CSQC_Ent_Update");
    let frame = frame.unwrap_or_else(|| panic!("{}", err.backtrace()));
    assert_eq!(&*frame.file, b"main.qc");
    if has_lines {
        assert!(frame.line.is_some_and(|l| l > 1), "{frame}");
    }
    // The VM is usable after the error.
    c.render(false);
}

#[test]
fn csprogs_runs_are_reproducible_across_vms_and_resets() {
    let Some(program) = load() else { return };
    let builtins = builtins();
    let run = |c: &mut Client| {
        fire_rockets(c, 120);
        std::mem::take(&mut c.host.log)
    };
    let mut a = Client::new(program.clone(), builtins.clone(), Engine::new());
    let mut b = Client::new(program, builtins, Engine::new());
    let (log_a, log_b) = (run(&mut a), run(&mut b));
    assert!(log_a.len() > 500);
    assert_eq!(log_a, log_b, "two VMs sharing one program");

    a.vm.reset().unwrap();
    a.host = Engine::new();
    a.init();
    assert_eq!(run(&mut a), log_b, "a VM after reset()");
}

/// Prints how often each opcode and each straight-line opcode pair runs over a thousand frames
/// of play, for choosing interpreter fast paths:
/// `cargo test --test all csprogs_statement_profile -- --ignored --nocapture`.
#[test]
#[ignore = "a report, not a check"]
fn csprogs_statement_profile() {
    let Some(program) = load() else { return };
    let mut c = Client::new(program, builtins(), Engine::new());
    fire_rockets(&mut c, 60);
    c.host.profile = Some(Profile::default());
    c.vm.set_trace(true);
    fire_rockets(&mut c, 1000);
    c.vm.set_trace(false);
    let profile = c.host.profile.take().unwrap();
    let total: u64 = profile.ops.values().sum();
    let mut ops: Vec<_> = profile.ops.into_iter().collect();
    ops.sort_by_key(|&(_, n)| std::cmp::Reverse(n));
    println!("{total} statements over 1000 frames");
    for (op, n) in ops.iter().take(30) {
        println!("{op:16} {n:10} {:5.1}%", *n as f64 * 100.0 / total as f64);
    }
    let mut pairs: Vec<_> = profile.pairs.into_iter().collect();
    pairs.sort_by_key(|&(_, n)| std::cmp::Reverse(n));
    println!(
        "
straight-line pairs"
    );
    for ((a, b), n) in pairs.iter().take(30) {
        println!("{a:16} {b:16} {n:10} {:5.1}%", *n as f64 * 100.0 / total as f64);
    }
}
