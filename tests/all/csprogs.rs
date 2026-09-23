// SPDX-License-Identifier: MIT OR Apache-2.0

//! Runs a real CSQC module — KTX's weapon-prediction `csprogs.dat`, located through
//! `QCVM_CSPROGS` (with its `.lno` beside it) — against a stub engine.
//!
//! The stub engine implements the engine builtins the progs calls: it records what they did,
//! feeds scripted network messages to the `Read*` builtins and scripted input frames to
//! `getinputstate`, and models the world as a floor plane at `z = 0` for `traceline`. The
//! standard library supplies every other builtin. The scenarios follow what the mod does in a
//! real client: initialisation, weapon snapshots, projectile entities, a thousand predicted
//! frames with the fire button held, temp entities, sound suppression and error reporting.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use qcvm::{
    Arg, Builtins, EntRef, ErrorKind, FuncRef, Global, Host, Numbering, Program, StrRef, Vm,
    VmConfig, VmError, Warning,
};

use crate::support::tools;

const MASK_ENGINE: i32 = 1;
const RF_VIEWMODEL: f32 = 1.0;
const VF_ORIGIN: f32 = 11.0;
const STAT_HEALTH: i32 = 0;
const STAT_ITEMS: i32 = 15;
const STAT_KTX_GRAVITY: i32 = 32;
const IT_ROCKET_LAUNCHER: u32 = 32;
const TE_LIGHTNING2: u8 = 6;
const CHAN_WEAPON: f32 = 1.0;
const EZCSQC_WEAPONINFO: u8 = 1;
const EZCSQC_PROJECTILE: u8 = 2;
const ROCKET_LAUNCHER: u8 = 7;
const ROCKET_SOUND: &[u8] = b"weapons/sgun1.wav";
const ROCKET_MODEL: &[u8] = b"progs/missile.mdl";
const FPS: f32 = 60.0;
/// Input frames between sending a command and the server acknowledging it (100 ms).
const LATENCY: i32 = 6;
/// Our entity number on the server.
const PLAYER_ENT: f32 = 1.0;

/// One input frame, as `getinputstate` reports it.
#[derive(Clone, Copy, Default)]
struct Input {
    timelength: f32,
    angles: [f32; 3],
    buttons: f32,
    impulse: f32,
}

/// The stub engine's state.
#[derive(Default)]
struct Engine {
    /// Every engine builtin call, in order.
    log: Vec<String>,
    printed: Vec<u8>,
    warnings: Vec<String>,
    cvars: HashMap<Vec<u8>, Vec<u8>>,
    stats: HashMap<i32, f32>,
    serverkeys: HashMap<Vec<u8>, Vec<u8>>,
    playerkeys: HashMap<Vec<u8>, f32>,
    /// The network message the `Read*` builtins consume.
    net: VecDeque<u8>,
    /// A `Read*` ran past the end of the message.
    net_underflow: bool,
    inputs: HashMap<i32, Input>,
    /// Precached models; model index `i` is `models[i - 1]`.
    models: Vec<Vec<u8>>,
    sounds_precached: Vec<Vec<u8>>,
    effects: Vec<Vec<u8>>,
    /// Entities added to the scene since `clearscene`.
    scene: Vec<u32>,
    /// Sounds played: entity, channel, sample.
    sounds: Vec<(u32, f32, Vec<u8>)>,
    view_origin: [f32; 3],
    /// Entities the scene contained at each `renderscene`.
    rendered: Vec<Vec<u32>>,
    /// Resolved once the progs is loaded.
    csqc: Option<qcvm::csqc::Handles>,
}

impl Engine {
    fn new() -> Self {
        let mut e = Self::default();
        for (k, v) in [
            ("r_drawviewmodel", "1"),
            ("r_rocketlight", "1"),
            ("r_rockettrail", "1"),
            ("r_grenadetrail", "1"),
            ("v_viewheight", "0"),
        ] {
            e.cvars.insert(k.into(), v.into());
        }
        e.serverkeys.insert(b"sv_antilag".to_vec(), b"1".to_vec());
        e.stats.insert(STAT_HEALTH, 100.0);
        e.stats.insert(STAT_ITEMS, f32::from_bits(IT_ROCKET_LAUNCHER));
        e.stats.insert(STAT_KTX_GRAVITY, 800.0);
        e
    }

    fn model_index(&self, name: &[u8]) -> Option<usize> {
        self.models.iter().position(|m| m == name).map(|i| i + 1)
    }

    fn take<const N: usize>(&mut self) -> [u8; N] {
        let mut out = [0; N];
        for b in &mut out {
            match self.net.pop_front() {
                Some(v) => *b = v,
                None => self.net_underflow = true,
            }
        }
        out
    }
}

impl Host for Engine {
    fn warning(&mut self, w: &Warning) {
        self.warnings.push(w.to_string());
    }
    fn print(&mut self, text: &[u8]) {
        self.printed.extend_from_slice(text);
        self.log.push(format!("print {:?}", String::from_utf8_lossy(text)));
    }
    fn cvar_float(&mut self, name: &[u8]) -> f32 {
        self.cvars.get(name).and_then(|v| std::str::from_utf8(v).ok()?.parse().ok()).unwrap_or(0.0)
    }
    fn cvar_string(&mut self, name: &[u8]) -> Option<std::borrow::Cow<'_, [u8]>> {
        self.cvars.get(name).map(|v| std::borrow::Cow::Borrowed(v.as_slice()))
    }
    fn is_demo(&mut self) -> f32 {
        0.0
    }
}

type B = Result<(), VmError>;

fn global<T: qcvm::value::QcValue>(vm: &Vm<Engine>, name: &str) -> Option<Global<T>> {
    vm.global::<T>(name).ok()
}

fn set_global<T: qcvm::value::QcValue>(vm: &mut Vm<Engine>, name: &str, v: T) {
    if let Some(g) = global::<T>(vm, name) {
        vm.set(g, v);
    }
}

fn field_f32(vm: &Vm<Engine>, e: EntRef, name: &str) -> f32 {
    vm.field::<f32>(name).ok().and_then(|f| vm.get_field(e, f)).unwrap_or(0.0)
}

fn field_vec(vm: &Vm<Engine>, e: EntRef, name: &str) -> [f32; 3] {
    vm.field::<[f32; 3]>(name).ok().and_then(|f| vm.get_field(e, f)).unwrap_or_default()
}

fn set_field_f32(vm: &mut Vm<Engine>, e: EntRef, name: &str, v: f32) {
    if let Ok(f) = vm.field::<f32>(name) {
        vm.set_field(e, f, v);
    }
}

// ---- engine builtins --------------------------------------------------------------------

fn setorigin(vm: &mut Vm<Engine>, _h: &mut Engine) -> B {
    let (e, org) = (vm.arg_ent(0), vm.arg_vec(1));
    if let Ok(f) = vm.field::<[f32; 3]>("origin") {
        vm.set_field(e, f, org);
    }
    Ok(())
}

fn setmodel(vm: &mut Vm<Engine>, h: &mut Engine) -> B {
    let (e, name) = (vm.arg_ent(0), vm.arg_str(1).to_vec());
    let index = h.model_index(&name).unwrap_or(0);
    h.log.push(format!("setmodel #{} {}", e.0, String::from_utf8_lossy(&name)));
    let flags = if name.ends_with(b"missile.mdl") {
        1.0 // MF_ROCKET
    } else if name.ends_with(b"grenade.mdl") {
        2.0 // MF_GRENADE
    } else {
        0.0
    };
    set_field_f32(vm, e, "modelindex", index as f32);
    set_field_f32(vm, e, "modelflags", flags);
    let s = vm.arg_str_ref(1);
    if let Ok(f) = vm.field::<StrRef>("model") {
        vm.set_field(e, f, s);
    }
    Ok(())
}

fn sound(vm: &mut Vm<Engine>, h: &mut Engine) -> B {
    let (e, chan, sample) = (vm.arg_ent(0), vm.arg_f32(1), vm.arg_str(2).to_vec());
    h.log.push(format!("sound #{} {chan} {}", e.0, String::from_utf8_lossy(&sample)));
    h.sounds.push((e.0, chan, sample));
    Ok(())
}

/// Traces against a floor plane at `z = 0`.
fn traceline(vm: &mut Vm<Engine>, _h: &mut Engine) -> B {
    let (a, b) = (vm.arg_vec(0), vm.arg_vec(1));
    let (fraction, normal) = if a[2] >= 0.0 && b[2] < 0.0 {
        (a[2] / (a[2] - b[2]), [0.0, 0.0, 1.0])
    } else {
        (1.0, [0.0; 3])
    };
    let end = [0, 1, 2].map(|k| a[k] + (b[k] - a[k]) * fraction);
    set_global(vm, "trace_fraction", fraction);
    set_global(vm, "trace_endpos", end);
    set_global(vm, "trace_plane_normal", normal);
    set_global(vm, "trace_plane_dist", 0.0f32);
    set_global(vm, "trace_ent", EntRef(0));
    set_global(vm, "trace_allsolid", 0.0f32);
    set_global(vm, "trace_startsolid", 0.0f32);
    Ok(())
}

fn precache_sound(vm: &mut Vm<Engine>, h: &mut Engine) -> B {
    let name = vm.arg_str(0).to_vec();
    if !h.sounds_precached.contains(&name) {
        h.sounds_precached.push(name);
    }
    let s = vm.arg_str_ref(0);
    vm.ret_str_ref(s);
    Ok(())
}

fn precache_model(vm: &mut Vm<Engine>, h: &mut Engine) -> B {
    let name = vm.arg_str(0).to_vec();
    if h.model_index(&name).is_none() {
        h.models.push(name);
    }
    let s = vm.arg_str_ref(0);
    vm.ret_str_ref(s);
    Ok(())
}

fn pointcontents(vm: &mut Vm<Engine>, _h: &mut Engine) -> B {
    vm.ret_f32(-1.0); // CONTENT_EMPTY
    Ok(())
}

fn getmodelindex(vm: &mut Vm<Engine>, h: &mut Engine) -> B {
    let index = h.model_index(vm.arg_str(0)).unwrap_or(0);
    vm.ret_f32(index as f32);
    Ok(())
}

fn clearscene(_vm: &mut Vm<Engine>, h: &mut Engine) -> B {
    h.log.push("clearscene".into());
    h.scene.clear();
    Ok(())
}

fn addentities(vm: &mut Vm<Engine>, h: &mut Engine) -> B {
    let mask = vm.arg_f32(0) as i32;
    h.log.push(format!("addentities {mask}"));
    let Some(handles) = h.csqc else { return Ok(()) };
    qcvm::csqc::add_entities(vm, h, &handles, mask, |_vm, h, e| h.scene.push(e.0))
}

fn addentity(vm: &mut Vm<Engine>, h: &mut Engine) -> B {
    let e = vm.arg_ent(0);
    h.log.push(format!("addentity #{}", e.0));
    h.scene.push(e.0);
    Ok(())
}

fn setproperty(vm: &mut Vm<Engine>, h: &mut Engine) -> B {
    h.log.push(format!("setproperty {} {}", vm.arg_f32(0), vm.arg_f32(1)));
    vm.ret_f32(1.0);
    Ok(())
}

fn renderscene(_vm: &mut Vm<Engine>, h: &mut Engine) -> B {
    h.log.push(format!("renderscene {:?}", h.scene));
    h.rendered.push(h.scene.clone());
    Ok(())
}

fn dynamiclight_add(vm: &mut Vm<Engine>, h: &mut Engine) -> B {
    h.log.push(format!("dynamiclight_add {:?} {}", vm.arg_vec(0), vm.arg_f32(1)));
    vm.ret_f32(0.0);
    Ok(())
}

fn getproperty(vm: &mut Vm<Engine>, h: &mut Engine) -> B {
    if vm.arg_f32(0) == VF_ORIGIN {
        vm.ret_vec(h.view_origin);
    } else {
        vm.ret_vec([0.0; 3]);
    }
    Ok(())
}

/// `getstatf(stat)`, or with `first, count` the bits of an integer stat (`getstatbits`).
fn getstatf(vm: &mut Vm<Engine>, h: &mut Engine) -> B {
    let stat = vm.arg_f32(0) as i32;
    let v = h.stats.get(&stat).copied().unwrap_or(0.0);
    if vm.argc() > 1 {
        let (first, count) = (vm.arg_f32(1) as u32, vm.arg_f32(2) as u32);
        let bits = (v.to_bits() >> first) & ((1u32 << count.min(31)) - 1);
        vm.ret_f32(bits as f32);
    } else {
        vm.ret_f32(v);
    }
    Ok(())
}

fn modelnameforindex(vm: &mut Vm<Engine>, h: &mut Engine) -> B {
    let index = vm.arg_f32(0) as usize;
    let name = index.checked_sub(1).and_then(|i| h.models.get(i)).cloned().unwrap_or_default();
    let s = vm.intern(&name);
    vm.ret_str_ref(s);
    Ok(())
}

fn particleeffectnum(vm: &mut Vm<Engine>, h: &mut Engine) -> B {
    let name = vm.arg_str(0).to_vec();
    let index = match h.effects.iter().position(|e| *e == name) {
        Some(i) => i + 1,
        None => {
            h.effects.push(name);
            h.effects.len()
        }
    };
    vm.ret_f32(index as f32);
    Ok(())
}

fn trailparticles(vm: &mut Vm<Engine>, h: &mut Engine) -> B {
    h.log.push(format!("trailparticles {} #{}", vm.arg_f32(0), vm.arg_ent(1).0));
    Ok(())
}

fn getinputstate(vm: &mut Vm<Engine>, h: &mut Engine) -> B {
    let frame = vm.arg_f32(0) as i32;
    let Some(input) = h.inputs.get(&frame).copied() else {
        vm.ret_f32(0.0);
        return Ok(());
    };
    set_global(vm, "input_timelength", input.timelength);
    set_global(vm, "input_angles", input.angles);
    set_global(vm, "input_movevalues", [0.0f32; 3]);
    set_global(vm, "input_buttons", input.buttons);
    set_global(vm, "input_impulse", input.impulse);
    vm.ret_f32(1.0);
    Ok(())
}

fn getplayerkeyfloat(vm: &mut Vm<Engine>, h: &mut Engine) -> B {
    let v = h.playerkeys.get(vm.arg_str(1)).copied().unwrap_or(0.0);
    vm.ret_f32(v);
    Ok(())
}

fn serverkey(vm: &mut Vm<Engine>, h: &mut Engine) -> B {
    let v = h.serverkeys.get(vm.arg_str(0)).cloned().unwrap_or_default();
    vm.ret_str(&v)
}

fn read_byte(vm: &mut Vm<Engine>, h: &mut Engine) -> B {
    let [b] = h.take::<1>();
    vm.ret_f32(f32::from(b));
    Ok(())
}

fn read_short(vm: &mut Vm<Engine>, h: &mut Engine) -> B {
    let v = i16::from_le_bytes(h.take::<2>());
    vm.ret_f32(f32::from(v));
    Ok(())
}

fn read_coord(vm: &mut Vm<Engine>, h: &mut Engine) -> B {
    let v = i16::from_le_bytes(h.take::<2>());
    vm.ret_f32(f32::from(v) / 8.0);
    Ok(())
}

fn read_angle(vm: &mut Vm<Engine>, h: &mut Engine) -> B {
    let [b] = h.take::<1>();
    vm.ret_f32(f32::from(b) * 360.0 / 256.0);
    Ok(())
}

fn read_float(vm: &mut Vm<Engine>, h: &mut Engine) -> B {
    let v = f32::from_le_bytes(h.take::<4>());
    vm.ret_f32(v);
    Ok(())
}

fn te_lightning2(vm: &mut Vm<Engine>, h: &mut Engine) -> B {
    h.log.push(format!(
        "te_lightning2 #{} {:?} {:?}",
        vm.arg_ent(0).0,
        vm.arg_vec(1),
        vm.arg_vec(2)
    ));
    Ok(())
}

/// The standard builtins plus the stub engine, numbered for CSQC.
fn builtins() -> Arc<Builtins<Engine>> {
    let mut b = Builtins::standard(Numbering::Csqc);
    let engine: &[(u32, &str, qcvm::BuiltinFn<Engine>)] = &[
        (2, "setorigin", setorigin),
        (3, "setmodel", setmodel),
        (8, "sound", sound),
        (16, "traceline", traceline),
        (19, "precache_sound", precache_sound),
        (20, "precache_model", precache_model),
        (41, "pointcontents", pointcontents),
        (200, "getmodelindex", getmodelindex),
        (300, "clearscene", clearscene),
        (301, "addentities", addentities),
        (302, "addentity", addentity),
        (303, "setproperty", setproperty),
        (304, "renderscene", renderscene),
        (305, "dynamiclight_add", dynamiclight_add),
        (309, "getproperty", getproperty),
        (331, "getstatf", getstatf),
        (334, "modelnameforindex", modelnameforindex),
        (335, "particleeffectnum", particleeffectnum),
        (336, "trailparticles", trailparticles),
        (345, "getinputstate", getinputstate),
        (354, "serverkey", serverkey),
        (360, "ReadByte", read_byte),
        (362, "ReadShort", read_short),
        (364, "ReadCoord", read_coord),
        (365, "ReadAngle", read_angle),
        (367, "ReadFloat", read_float),
        (429, "te_lightning2", te_lightning2),
    ];
    for &(number, name, f) in engine {
        b.set_numbered(number, name, f);
    }
    b.set("getplayerkeyfloat", getplayerkeyfloat);
    Arc::new(b)
}

// ---- network messages -------------------------------------------------------------------

#[derive(Default)]
struct Msg(Vec<u8>);

impl Msg {
    fn byte(mut self, v: u8) -> Self {
        self.0.push(v);
        self
    }
    fn short(mut self, v: i16) -> Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn float(mut self, v: f32) -> Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn coords(mut self, v: [f32; 3]) -> Self {
        for c in v {
            self = self.short((c * 8.0).round() as i16);
        }
        self
    }
}

/// A weapon snapshot as the server sends it (every section present).
#[derive(Clone, Copy)]
struct Snapshot {
    weapon: u8,
    generation: u8,
    rockets: u8,
    attack_finished: f32,
    client_time: f32,
    ping_ms: u8,
    predflags: u8,
}

impl Snapshot {
    fn msg(&self) -> Msg {
        Msg::default()
            .byte(EZCSQC_WEAPONINFO)
            .byte(0xFF)
            .byte(0) // impulse
            .byte(self.generation << 4 | self.weapon)
            .byte(25) // shells
            .byte(100) // nails
            .byte(self.rockets)
            .byte(50) // cells
            .float(self.attack_finished)
            .float(0.0) // client_nextthink
            .byte(0) // client_thinkindex
            .float(self.client_time)
            .byte(0) // frame
            .byte(self.predflags)
            .byte(self.ping_ms)
    }
}

/// A server projectile update with every section present.
fn projectile_msg(origin: [f32; 3], velocity: [f32; 3], model: i16, owner: i16) -> Msg {
    Msg::default()
        .byte(EZCSQC_PROJECTILE)
        .byte(0x1F)
        .coords(origin)
        .coords(velocity)
        .float(0.0)
        .short(model)
        .short(0) // effects
        .byte(0)
        .byte(0)
        .byte(0) // angles
        .short(owner)
        .coords(origin) // spawn origin
}

// ---- the client ------------------------------------------------------------------------

/// The server's side of the weapon: it processes our input frames `LATENCY` frames late, fires
/// the rocket launcher whenever the button is held and the refire time has passed, and sends a
/// weapon snapshot every sixth frame.
#[derive(Clone, Copy, Default)]
struct Server {
    /// Last input frame processed, or -1.
    acked: i32,
    clock: f32,
    attack_finished: f32,
    shots: u32,
}

struct Client {
    vm: Vm<Engine>,
    host: Engine,
    /// CSQC entities by network entity number.
    ents: HashMap<u16, EntRef>,
    frame: i32,
    server: Server,
}

fn load() -> Option<Arc<Program>> {
    let Some(path) = tools::csprogs() else {
        tools::skip("csprogs", "QCVM_CSPROGS");
        return None;
    };
    let mut program = Program::parse(&std::fs::read(&path).unwrap()).unwrap();
    if let Ok(lno) = std::fs::read(path.with_extension("lno")) {
        program = program.with_line_numbers(&lno).unwrap();
    }
    Some(Arc::new(program))
}

impl Client {
    fn new(program: Arc<Program>, builtins: Arc<Builtins<Engine>>, host: Engine) -> Self {
        let vm = Vm::new(program, builtins, VmConfig::csqc()).unwrap();
        let mut c = Self { vm, host, ents: HashMap::new(), frame: 0, server: Server::default() };
        c.init();
        c
    }

    /// What the engine does when the progs is loaded: autocvars, `CSQC_Init`, then the world
    /// becomes read-only.
    fn init(&mut self) {
        self.host.csqc = qcvm::csqc::Handles::resolve(&self.vm);
        self.ents.clear();
        self.frame = 0;
        self.server = Server { acked: -1, ..Server::default() };
        self.vm.sync_autocvars(&mut self.host).unwrap();
        set_global(&mut self.vm, "player_localentnum", PLAYER_ENT);
        set_global(&mut self.vm, "player_localnum", 0.0f32);
        self.call("CSQC_Init", &[Arg::Float(0.0), Arg::Bytes(b"FTE"), Arg::Float(5000.0)]).unwrap();
        self.vm.set_protected(EntRef(0), true);
    }

    fn func(&self, name: &str) -> FuncRef {
        self.vm.find_function(name).unwrap_or_else(|| panic!("{name} missing"))
    }

    fn call(&mut self, name: &str, args: &[Arg<'_>]) -> Result<qcvm::Ret, VmError> {
        let f = self.func(name);
        self.vm.call(&mut self.host, f, args)
    }

    fn time(&self) -> f32 {
        self.frame as f32 / FPS
    }

    /// Delivers a CSQC entity update for network entity `entnum`.
    fn ent_update(&mut self, entnum: u16, msg: Msg) -> Result<(), VmError> {
        let (e, isnew) = match self.ents.get(&entnum) {
            Some(&e) => (e, false),
            None => {
                let e = self.vm.spawn().unwrap();
                set_field_f32(&mut self.vm, e, "entnum", f32::from(entnum));
                self.ents.insert(entnum, e);
                (e, true)
            }
        };
        self.host.net = msg.0.into();
        let f = self.func("CSQC_Ent_Update");
        let r = self.vm.call_as(&mut self.host, e, f, &[Arg::Float(if isnew { 1.0 } else { 0.0 })]);
        if r.is_ok() {
            assert!(!self.host.net_underflow, "CSQC_Ent_Update read past the message");
            assert!(self.host.net.is_empty(), "CSQC_Ent_Update left {} bytes", self.host.net.len());
        }
        r.map(|_| ())
    }

    fn ent_remove(&mut self, entnum: u16) {
        let e = self.ents.remove(&entnum).unwrap();
        let f = self.func("CSQC_Ent_Remove");
        self.vm.call_as(&mut self.host, e, f, &[]).unwrap();
        assert!(!self.vm.is_in_use(e));
    }

    /// Starts a video frame: records this frame's input and sets the globals the engine
    /// maintains.
    fn begin_frame(&mut self, attack: bool) {
        let now = self.time();
        self.host.inputs.insert(
            self.frame,
            Input {
                timelength: 1.0 / FPS,
                angles: [0.0, 90.0, 0.0],
                buttons: if attack { 1.0 } else { 0.0 },
                impulse: 0.0,
            },
        );
        self.vm.set_realtime(f64::from(now));
        set_global(&mut self.vm, "time", now);
        set_global(&mut self.vm, "cltime", now);
        set_global(&mut self.vm, "frametime", 1.0 / FPS);
        set_global(&mut self.vm, "clientcommandframe", (self.frame + 1) as f32);
        set_global(&mut self.vm, "servercommandframe", self.server.acked.max(0) as f32);
        set_global(&mut self.vm, "pmove_org", [0.0f32, 0.0, 24.0]);
        set_global(&mut self.vm, "view_angles", [0.0f32, 90.0, 0.0]);
        self.host.view_origin = [0.0, 0.0, 46.0];
    }

    /// Finishes a video frame by rendering it.
    fn draw(&mut self) {
        self.call("CSQC_UpdateView", &[Arg::Float(640.0), Arg::Float(480.0), Arg::Float(1.0)])
            .unwrap();
        self.frame += 1;
    }

    fn render(&mut self, attack: bool) {
        self.begin_frame(attack);
        self.draw();
    }

    /// One frame of play: the server catches up with our input (and every sixth frame sends a
    /// weapon snapshot), then we render.
    fn step(&mut self, attack: bool) {
        let target = self.frame - LATENCY;
        while self.server.acked < target {
            self.server.acked += 1;
            self.server.clock += 1.0 / FPS;
            let held = self.host.inputs.get(&self.server.acked).is_some_and(|i| i.buttons != 0.0);
            if held && self.server.clock >= self.server.attack_finished {
                self.server.attack_finished = self.server.clock + 0.8;
                self.server.shots += 1;
            }
        }
        self.begin_frame(attack);
        if self.frame % 6 == 0 && self.server.acked >= 0 {
            let snap = Snapshot {
                weapon: ROCKET_LAUNCHER,
                generation: 1,
                rockets: 100u32.saturating_sub(self.server.shots) as u8,
                attack_finished: self.server.attack_finished,
                client_time: self.server.clock,
                ping_ms: 50,
                predflags: 0,
            };
            self.ent_update(500, snap.msg()).unwrap();
        }
        self.draw();
    }

    /// The first local (predicted) projectile alive.
    fn local_projectile(&self) -> Option<EntRef> {
        let is_local = self.vm.field::<f32>("is_local").ok()?;
        self.vm.entities().find(|&e| self.vm.get_field(e, is_local) == Some(1.0))
    }
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
    // shot for shot, except the server's first shot, which came before any snapshot.
    //
    // A shot may sound on several consecutive frames: KTX's weapon_state.qc writes
    // `frame_may_sound = (f <= threshold) && (f > last_sound_frame);`, which QuakeC's operator
    // precedence (fteqcc without `-Fcpriority`) parses as `(frame_may_sound = f <= threshold) &&
    // ...`, so the replay repeats a shot's effects until a snapshot includes it. FTE runs the
    // progs the same way. The checks below hold with or without that quirk.
    let bursts: Vec<i32> = sounds
        .iter()
        .enumerate()
        .filter(|&(i, &f)| i == 0 || sounds[i - 1] != f - 1)
        .map(|(_, &f)| f)
        .collect();
    assert_eq!(bursts.len() as u32 + 1, c.server.shots, "{sounds:?}");
    for w in bursts.windows(2) {
        assert!((48..=49).contains(&(w[1] - w[0])), "shots {bursts:?}");
    }
    // Every predicted shot spawned a local rocket; they expire in their predraw (removing
    // themselves mid-walk) after about four frames.
    let spawned =
        c.host.log.iter().filter(|l| l.starts_with("setmodel") && l.ends_with("missile.mdl"));
    assert_eq!(spawned.count(), sounds.len());
    let is_local = c.vm.field::<f32>("is_local").unwrap();
    let alive = c.vm.entities().filter(|&e| c.vm.get_field(e, is_local) == Some(1.0)).count();
    assert!(alive <= 4, "{alive} local rockets still alive");

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
    // The host's copy of the sample name (a temp string) matches the progs' constant. Each
    // predicted play left one token (see the note on repeated shots above), and each echo
    // consumes one.
    assert_eq!(event(&mut c, ROCKET_SOUND), 1.0, "echo of the predicted shot is dropped");
    let mut dropped = 1;
    while event(&mut c, ROCKET_SOUND) == 1.0 {
        dropped += 1;
        assert!(dropped <= 8, "more echoes dropped than tokens exist");
    }
    assert_eq!(dropped, sounds.len().min(8), "one token per predicted play");
    assert_eq!(event(&mut c, ROCKET_SOUND), 0.0, "the tokens were consumed");
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
