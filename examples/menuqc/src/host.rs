// SPDX-License-Identifier: MIT OR Apache-2.0

//! The host: engine state the builtins work on, the `Host` hooks, and the builtin registry.
//!
//! Only what `menu_nq.dat` uses is implemented: 2D drawing of uploaded images, key focus and
//! cursor mode, files ([`crate::files`]) and sound ([`crate::audio`]). Everything else (memory,
//! strings, `sprintf`, `sleep`, `gettimed`, ...) comes from qcvm's standard builtins.

use std::borrow::Cow;
use std::collections::HashMap;
use std::io::Write;

use qcvm::{BuiltinFn, Builtins, Host, Numbering, Vm, VmError, Warning};

use crate::audio::{self, Audio};
use crate::files::{self, Vfs};
use crate::keys;

type B = Result<(), VmError>;

/// `r_uploadimage` format: 8-bit indices followed by a 768-byte RGB palette (FTE's TF_8PAL24).
const FORMAT_8PAL24: i32 = 15;

/// `setkeydest` value that sends key events to the menu.
pub const KEY_MENU: f32 = 2.0;

/// An uploaded image, as 0x00RRGGBB pixels.
pub struct Image {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u32>,
}

/// A `drawpic` call: the image, and where it goes in window pixels.
#[derive(Clone)]
pub struct Draw {
    pub image: Vec<u8>,
    pub pos: [f32; 2],
    pub size: [f32; 2],
}

#[derive(Default)]
pub struct MenuHost {
    pub files: Vfs,
    pub audio: Option<Audio>,
    /// Cvars served to the progs' autocvars (`guest_args`, `guest_width`, `guest_height`).
    pub cvars: HashMap<Vec<u8>, Vec<u8>>,
    pub images: HashMap<Vec<u8>, Image>,
    /// Shader name → the image its `map` line refers to.
    pub shaders: HashMap<Vec<u8>, Vec<u8>>,
    /// This frame's `drawpic`s.
    pub draws: Vec<Draw>,
    /// This frame's `drawstring` text (shown in the window title).
    pub text: Vec<u8>,
    pub keydest: f32,
    pub cursormode: f32,
    /// The progs asked to quit (`localcmd("quit")`).
    pub quit: bool,
}

impl Host for MenuHost {
    fn warning(&mut self, warning: &Warning) {
        eprintln!("warning: {warning}");
    }

    fn print(&mut self, text: &[u8]) {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(text).and_then(|()| out.flush());
    }

    fn localcmd(&mut self, text: &[u8]) {
        for cmd in text.split(|&c| c == b'\n' || c == b';') {
            let cmd = cmd.trim_ascii();
            if cmd == b"quit" || cmd.starts_with(b"quit ") {
                self.quit = true;
            } else if !cmd.is_empty() {
                eprintln!("localcmd ignored: {}", String::from_utf8_lossy(cmd));
            }
        }
    }

    fn cvar_string(&mut self, name: &[u8]) -> Option<Cow<'_, [u8]>> {
        self.cvars.get(name).map(|v| Cow::Borrowed(v.as_slice()))
    }
}

/// The standard menu builtins plus this host's. The sound builtins are left out without an
/// audio device, so the progs' `checkbuiltin` sees they are missing and runs silently.
pub fn builtins(audio: bool) -> Builtins<MenuHost> {
    let mut b = Builtins::standard(Numbering::Menu);
    let engine: &[(&str, BuiltinFn<MenuHost>)] = &[
        ("drawpic", drawpic),
        ("drawstring", drawstring),
        ("r_uploadimage", r_uploadimage),
        ("shaderforname", shaderforname),
        ("setkeydest", setkeydest),
        ("setcursormode", setcursormode),
        ("stringtokeynum", stringtokeynum),
        ("fopen", files::fopen),
        ("fclose", files::fclose),
        ("fgets", files::fgets),
        ("fread", files::fread),
        ("fwrite", files::fwrite),
        ("fseek64", files::fseek64),
        ("fsize64", files::fsize64),
        ("fremove", files::fremove),
        ("frename", files::frename),
        ("whichpack", files::whichpack),
    ];
    for &(name, f) in engine {
        b.set(name, f);
    }
    if audio {
        b.set("queueaudio", audio::queueaudio);
        b.set("getqueuedaudiotime", audio::getqueuedaudiotime);
    }
    b
}

/// `void r_uploadimage(string name, int width, int height, void *data, int size, int format)`
fn r_uploadimage(vm: &mut Vm<MenuHost>, h: &mut MenuHost) -> B {
    let name = vm.arg_str(0).to_vec();
    let (w, ht) = (usize::try_from(vm.arg_i32(1)), usize::try_from(vm.arg_i32(2)));
    let size = usize::try_from(vm.arg_i32(4)).unwrap_or(0);
    let format = vm.arg_i32(5);
    if format != FORMAT_8PAL24 {
        vm.warn(format!("r_uploadimage: unsupported format {format}"));
        return Ok(());
    }
    let (Ok(width), Ok(height)) = (w, ht) else {
        vm.warn("r_uploadimage: bad size");
        return Ok(());
    };
    let count = width.saturating_mul(height);
    if size < count.saturating_add(768) {
        vm.warn("r_uploadimage: data too small for its size");
        return Ok(());
    }
    let Some(data) = vm.read_mem(vm.arg_ptr(3), count.saturating_add(768)) else {
        vm.warn("r_uploadimage: bad data pointer");
        return Ok(());
    };
    let (indices, palette) = data.split_at(count);
    let rgb = |i: u8| {
        let at = usize::from(i) * 3;
        match palette.get(at..at + 3) {
            Some(&[r, g, b]) => u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b),
            _ => 0,
        }
    };
    let pixels = indices.iter().map(|&i| rgb(i)).collect();
    h.images.insert(name, Image { width, height, pixels });
    Ok(())
}

/// `float shaderforname(string name, optional string body)`: remembers which image the shader
/// maps (`map $rt:$nearest:guestscreen` → `guestscreen`).
fn shaderforname(vm: &mut Vm<MenuHost>, h: &mut MenuHost) -> B {
    let name = vm.arg_str(0).to_vec();
    let body = if vm.argc() > 1 { vm.arg_str(1) } else { b"" };
    let mut words = body.split(u8::is_ascii_whitespace).filter(|w| !w.is_empty());
    let map = words.by_ref().find(|w| w.eq_ignore_ascii_case(b"map")).and_then(|_| words.next());
    if let Some(map) = map {
        let image = map.rsplit(|&c| c == b':').next().unwrap_or(map);
        h.shaders.insert(name, image.to_vec());
    }
    vm.ret_f32(h.shaders.len() as f32);
    Ok(())
}

/// `float drawpic(vector pos, string pic, vector size, ...)`
fn drawpic(vm: &mut Vm<MenuHost>, h: &mut MenuHost) -> B {
    let (pos, size) = (vm.arg_vec(0), vm.arg_vec(2));
    let pic = vm.arg_str(1);
    let image = h.shaders.get(pic).cloned().unwrap_or_else(|| pic.to_vec());
    h.draws.push(Draw { image, pos: [pos[0], pos[1]], size: [size[0], size[1]] });
    vm.ret_f32(1.0);
    Ok(())
}

/// `float drawstring(vector pos, string text, vector scale, ...)`: the text goes in the title bar.
fn drawstring(vm: &mut Vm<MenuHost>, h: &mut MenuHost) -> B {
    if !h.text.is_empty() {
        h.text.extend_from_slice(b" | ");
    }
    h.text.extend_from_slice(vm.arg_str(1));
    vm.ret_f32(1.0);
    Ok(())
}

/// `void setkeydest(float dest)`: 2 sends key events to the menu, 0 hands them back.
fn setkeydest(vm: &mut Vm<MenuHost>, h: &mut MenuHost) -> B {
    h.keydest = vm.arg_f32(0);
    Ok(())
}

/// `void setcursormode(float absolute)`: 0 grabs the mouse for relative motion.
fn setcursormode(vm: &mut Vm<MenuHost>, h: &mut MenuHost) -> B {
    h.cursormode = vm.arg_f32(0);
    Ok(())
}

/// `float stringtokeynum(string name)`
fn stringtokeynum(vm: &mut Vm<MenuHost>, _: &mut MenuHost) -> B {
    let key = keys::from_name(vm.arg_str(0));
    vm.ret_f32(key);
    Ok(())
}
