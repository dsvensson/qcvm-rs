// SPDX-License-Identifier: MIT OR Apache-2.0

//! A stub engine for KTX's weapon-prediction `csprogs.dat`, shared by the csprogs scenarios
//! (`tests/all/csprogs.rs`) and the csprogs benchmark (`benches/csprogs.rs`).
//!
//! It implements the engine builtins the progs calls: it records what they did, feeds scripted
//! network messages to the `Read*` builtins and scripted input frames to `getinputstate`, and
//! models the world as a floor plane at `z = 0` for `traceline`. The standard library supplies
//! every other builtin. [`Client`] drives the progs the way a client does.

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::Arc;

use qcvm::{
    Arg, Builtins, EntRef, FuncRef, Global, Host, Numbering, Program, StrRef, Vm, VmConfig,
    VmError, Warning,
};

pub const MASK_ENGINE: i32 = 1;
pub const RF_VIEWMODEL: f32 = 1.0;
pub const VF_ORIGIN: f32 = 11.0;
pub const STAT_HEALTH: i32 = 0;
pub const STAT_ITEMS: i32 = 15;
pub const STAT_KTX_GRAVITY: i32 = 32;
pub const IT_ROCKET_LAUNCHER: u32 = 32;
pub const TE_LIGHTNING2: u8 = 6;
pub const CHAN_WEAPON: f32 = 1.0;
pub const EZCSQC_WEAPONINFO: u8 = 1;
pub const EZCSQC_PROJECTILE: u8 = 2;
pub const ROCKET_LAUNCHER: u8 = 7;
pub const ROCKET_SOUND: &[u8] = b"weapons/sgun1.wav";
pub const ROCKET_MODEL: &[u8] = b"progs/missile.mdl";
pub const FPS: f32 = 60.0;
/// Input frames between sending a command and the server acknowledging it (100 ms).
pub const LATENCY: i32 = 6;
/// Our entity number on the server.
pub const PLAYER_ENT: f32 = 1.0;

/// One input frame, as `getinputstate` reports it.
#[derive(Clone, Copy, Default)]
pub struct Input {
    pub timelength: f32,
    pub angles: [f32; 3],
    pub buttons: f32,
    pub impulse: f32,
}

/// The stub engine's state.
#[derive(Default)]
pub struct Engine {
    /// Every engine builtin call, in order.
    pub log: Vec<String>,
    pub printed: Vec<u8>,
    pub warnings: Vec<String>,
    pub cvars: HashMap<Vec<u8>, Vec<u8>>,
    pub stats: HashMap<i32, f32>,
    pub serverkeys: HashMap<Vec<u8>, Vec<u8>>,
    pub playerkeys: HashMap<Vec<u8>, f32>,
    /// The network message the `Read*` builtins consume.
    pub net: VecDeque<u8>,
    /// A `Read*` ran past the end of the message.
    pub net_underflow: bool,
    pub inputs: HashMap<i32, Input>,
    /// Precached models; model index `i` is `models[i - 1]`.
    pub models: Vec<Vec<u8>>,
    pub sounds_precached: Vec<Vec<u8>>,
    pub effects: Vec<Vec<u8>>,
    /// Entities added to the scene since `clearscene`.
    pub scene: Vec<u32>,
    /// Sounds played: entity, channel, sample.
    pub sounds: Vec<(u32, f32, Vec<u8>)>,
    pub view_origin: [f32; 3],
    /// Entities the scene contained at each `renderscene`.
    pub rendered: Vec<Vec<u32>>,
    /// Resolved once the progs is loaded.
    pub csqc: Option<qcvm::csqc::Handles>,
    /// Statement counts, collected from trace lines while tracing is on.
    pub profile: Option<Profile>,
}

/// How often each opcode ran, and each pair of opcodes where the second ran straight after the
/// first (the next statement, no jump in between).
#[derive(Default)]
pub struct Profile {
    pub ops: HashMap<String, u64>,
    pub pairs: HashMap<(String, String), u64>,
    last: Option<(u32, String)>,
}

impl Profile {
    /// Counts one trace line (`function: index: OPCODE operands`).
    fn record(&mut self, line: &str) {
        let Some((_, rest)) = line.split_once(": ") else { return };
        let Some((index, stmt)) = rest.trim_start().split_once(": ") else { return };
        let (Ok(index), Some(op)) = (index.parse::<u32>(), stmt.split_whitespace().next()) else {
            return;
        };
        *self.ops.entry(op.to_owned()).or_default() += 1;
        if let Some((last, last_op)) = self.last.take()
            && last + 1 == index
        {
            *self.pairs.entry((last_op, op.to_owned())).or_default() += 1;
        }
        self.last = Some((index, op.to_owned()));
    }
}

impl Engine {
    pub fn new() -> Self {
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

    pub fn model_index(&self, name: &[u8]) -> Option<usize> {
        self.models.iter().position(|m| m == name).map(|i| i + 1)
    }

    pub fn take<const N: usize>(&mut self) -> [u8; N] {
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
    fn trace(&mut self, line: &str) {
        if let Some(p) = &mut self.profile {
            p.record(line);
        }
    }
}

type B = Result<(), VmError>;

pub fn global<T: qcvm::value::QcValue>(vm: &Vm<Engine>, name: &str) -> Option<Global<T>> {
    vm.global::<T>(name).ok()
}

pub fn set_global<T: qcvm::value::QcValue>(vm: &mut Vm<Engine>, name: &str, v: T) {
    if let Some(g) = global::<T>(vm, name) {
        vm.set(g, v);
    }
}

pub fn field_f32(vm: &Vm<Engine>, e: EntRef, name: &str) -> f32 {
    vm.field::<f32>(name).ok().and_then(|f| vm.get_field(e, f)).unwrap_or(0.0)
}

pub fn field_vec(vm: &Vm<Engine>, e: EntRef, name: &str) -> [f32; 3] {
    vm.field::<[f32; 3]>(name).ok().and_then(|f| vm.get_field(e, f)).unwrap_or_default()
}

pub fn set_field_f32(vm: &mut Vm<Engine>, e: EntRef, name: &str, v: f32) {
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
pub fn builtins() -> Arc<Builtins<Engine>> {
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
pub struct Msg(pub Vec<u8>);

impl Msg {
    pub fn byte(mut self, v: u8) -> Self {
        self.0.push(v);
        self
    }
    pub fn short(mut self, v: i16) -> Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn float(mut self, v: f32) -> Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn coords(mut self, v: [f32; 3]) -> Self {
        for c in v {
            self = self.short((c * 8.0).round() as i16);
        }
        self
    }
}

/// A weapon snapshot as the server sends it (every section present).
#[derive(Clone, Copy)]
pub struct Snapshot {
    pub weapon: u8,
    pub generation: u8,
    pub rockets: u8,
    pub attack_finished: f32,
    pub client_time: f32,
    pub ping_ms: u8,
    pub predflags: u8,
}

impl Snapshot {
    pub fn msg(&self) -> Msg {
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
pub fn projectile_msg(origin: [f32; 3], velocity: [f32; 3], model: i16, owner: i16) -> Msg {
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
pub struct Server {
    /// Last input frame processed, or -1.
    pub acked: i32,
    pub clock: f32,
    pub attack_finished: f32,
    pub shots: u32,
}

pub struct Client {
    pub vm: Vm<Engine>,
    pub host: Engine,
    /// CSQC entities by network entity number.
    pub ents: HashMap<u16, EntRef>,
    pub frame: i32,
    pub server: Server,
}

/// Loads `csprogs.dat` from `path`, with the `.lno` file beside it if there is one.
pub fn load_program(path: &Path) -> Arc<Program> {
    let mut program = Program::parse(&std::fs::read(path).unwrap()).unwrap();
    if let Ok(lno) = std::fs::read(path.with_extension("lno")) {
        program = program.with_line_numbers(&lno).unwrap();
    }
    Arc::new(program)
}

impl Client {
    pub fn new(program: Arc<Program>, builtins: Arc<Builtins<Engine>>, host: Engine) -> Self {
        let vm = Vm::new(program, builtins, VmConfig::csqc()).unwrap();
        let mut c = Self { vm, host, ents: HashMap::new(), frame: 0, server: Server::default() };
        c.init();
        c
    }

    /// What the engine does when the progs is loaded: autocvars, `CSQC_Init`, then the world
    /// becomes read-only.
    pub fn init(&mut self) {
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

    pub fn func(&self, name: &str) -> FuncRef {
        self.vm.find_function(name).unwrap_or_else(|| panic!("{name} missing"))
    }

    pub fn call(&mut self, name: &str, args: &[Arg<'_>]) -> Result<qcvm::Ret, VmError> {
        let f = self.func(name);
        self.vm.call(&mut self.host, f, args)
    }

    pub fn time(&self) -> f32 {
        self.frame as f32 / FPS
    }

    /// Delivers a CSQC entity update for network entity `entnum`.
    pub fn ent_update(&mut self, entnum: u16, msg: Msg) -> Result<(), VmError> {
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

    pub fn ent_remove(&mut self, entnum: u16) {
        let e = self.ents.remove(&entnum).unwrap();
        let f = self.func("CSQC_Ent_Remove");
        self.vm.call_as(&mut self.host, e, f, &[]).unwrap();
        assert!(!self.vm.is_in_use(e));
    }

    /// Starts a video frame: records this frame's input and sets the globals the engine
    /// maintains.
    pub fn begin_frame(&mut self, attack: bool) {
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
    pub fn draw(&mut self) {
        self.call("CSQC_UpdateView", &[Arg::Float(640.0), Arg::Float(480.0), Arg::Float(1.0)])
            .unwrap();
        self.frame += 1;
    }

    pub fn render(&mut self, attack: bool) {
        self.begin_frame(attack);
        self.draw();
    }

    /// One frame of play: the server catches up with our input (and every sixth frame sends a
    /// weapon snapshot), then we render.
    pub fn step(&mut self, attack: bool) {
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
    pub fn local_projectile(&self) -> Option<EntRef> {
        let is_local = self.vm.field::<f32>("is_local").ok()?;
        self.vm.entities().find(|&e| self.vm.get_field(e, is_local) == Some(1.0))
    }
}
