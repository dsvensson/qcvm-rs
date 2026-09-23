// SPDX-License-Identifier: MIT OR Apache-2.0

//! Runs QCQuake — WinQuake compiled to menu QuakeC — on qcvm, in a winit window.
//!
//! ```text
//! cargo run --release -p menuqc -- qcquake.pk3 path/to/id1/pak0.pak [more.pak ...]
//!     [--width N] [--height N] [-- quake args...]
//! ```
//!
//! See README.md for where to get `qcquake.pk3`.

mod audio;
mod files;
mod host;
mod keys;

use std::error::Error;
use std::io::Read;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use qcvm::{Arg, FuncRef, Global, Program, Vm, VmConfig, VmError};
use softbuffer::{Context, Surface};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{DeviceEvent, DeviceId, ElementState, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::PhysicalKey;
use winit::window::{CursorGrabMode, Window, WindowId};

use crate::audio::Audio;
use crate::host::{Draw, Image, KEY_MENU, MenuHost};

const USAGE: &str = "usage: menuqc <qcquake.pk3> <pak0.pak> [more.pak ...] [--width N] \
                     [--height N] [-- quake args...]";

/// The progs inside the pk3: the NetQuake build (`menu_qwcl.dat` is QuakeWorld, which needs
/// networking this host does not provide).
const PROGS: &str = "menu_nq.dat";

/// How often the host draws a frame. Quake skips frames that come less than 1/72 s after the
/// last one, by a clock in whole milliseconds (`gettimed(1)`); 60 Hz clears that with margin, so
/// Quake renders on every tick instead of an uneven every second or third.
const TICK: Duration = Duration::from_micros(1_000_000 / 60);

/// `Menu_InputEvent` event types.
const KEY_DOWN: f32 = 0.0;
const KEY_UP: f32 = 1.0;
const MOUSE_DELTA: f32 = 2.0;

struct Options {
    pk3: PathBuf,
    paks: Vec<PathBuf>,
    width: Option<u32>,
    height: Option<u32>,
    quake_args: Vec<String>,
}

fn parse_args() -> Result<Options, String> {
    let mut args = std::env::args().skip(1);
    let (mut pk3, mut paks, mut width, mut height) = (None, Vec::new(), None, None);
    let mut quake_args = Vec::new();
    while let Some(arg) = args.next() {
        let mut number = |what: &str| {
            args.next().and_then(|v| v.parse().ok()).ok_or(format!("{what} needs a number"))
        };
        match arg.as_str() {
            "--" => quake_args.extend(args.by_ref()),
            "--width" => width = Some(number("--width")?),
            "--height" => height = Some(number("--height")?),
            "-h" | "--help" => return Err(USAGE.to_owned()),
            _ if pk3.is_none() => pk3 = Some(PathBuf::from(arg)),
            _ => paks.push(PathBuf::from(arg)),
        }
    }
    match pk3 {
        Some(pk3) if !paks.is_empty() => Ok(Options { pk3, paks, width, height, quake_args }),
        _ => Err(USAGE.to_owned()),
    }
}

fn read_progs(pk3: &PathBuf) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(pk3)?)?;
    let mut file = zip.by_name(PROGS).map_err(|e| format!("{}: {PROGS}: {e}", pk3.display()))?;
    let mut data = Vec::new();
    file.read_to_end(&mut data)?;
    Ok(data)
}

fn main() -> Result<(), Box<dyn Error>> {
    let opts = parse_args()?;
    let progs = read_progs(&opts.pk3)?;

    let audio = Audio::open();
    let builtins = Arc::new(host::builtins(audio.is_some()));
    let mut host = MenuHost { audio, ..MenuHost::default() };
    for pak in &opts.paks {
        let name = pak.file_name().ok_or(USAGE)?.to_string_lossy().to_lowercase();
        let data = std::fs::read(pak).map_err(|e| format!("{}: {e}", pak.display()))?;
        host.files.add(name.as_bytes(), data);
    }
    let cvars = [
        ("guest_args", Some(opts.quake_args.join(" "))),
        ("guest_width", opts.width.map(|w| w.to_string())),
        ("guest_height", opts.height.map(|h| h.to_string())),
    ];
    for (name, value) in cvars {
        if let Some(value) = value {
            host.cvars.insert(name.into(), value.into_bytes());
        }
    }

    let mut config = VmConfig::menu();
    // Quake's startup and map loads (which run Quake's own progs interpreter, in QuakeC) happen
    // inside a single `m_draw`, far beyond the default runaway budget.
    config.limits.runaway = u32::MAX;
    config.limits.heap_bytes = 128 << 20;
    let mut vm = Vm::new(Arc::new(Program::parse(&progs)?), builtins, config)?;
    for missing in vm.reachable_unbound_builtins() {
        eprintln!("warning: builtin {} is not implemented", String::from_utf8_lossy(&missing.name));
    }
    vm.sync_autocvars(&mut host)?;

    let func = |name: &str| vm.find_function(name).ok_or(format!("{PROGS} has no {name}"));
    let (m_init, m_toggle) = (func("m_init")?, func("m_toggle")?);
    let (m_draw, input_event) = (func("m_draw")?, func("Menu_InputEvent")?);
    let time = vm.global::<f32>("time")?;
    vm.set(time, vm.realtime() as f32);
    vm.call(&mut host, m_init, &[])?;
    // Show the "menu": the first `m_draw` after this starts Quake.
    vm.call(&mut host, m_toggle, &[Arg::Float(1.0)])?;

    let mut app = App {
        vm,
        host,
        m_draw,
        input_event,
        time,
        window: None,
        next_frame: Instant::now(),
        last_draws: Vec::new(),
        title: Vec::new(),
        focused: false,
        grabbed: false,
        error: None,
    };
    EventLoop::new()?.run_app(&mut app)?;
    match app.error {
        Some(e) => Err(e.into()),
        None => Ok(()),
    }
}

struct Screen {
    window: Rc<Window>,
    surface: Surface<Rc<Window>, Rc<Window>>,
}

struct App {
    vm: Vm<MenuHost>,
    host: MenuHost,
    m_draw: FuncRef,
    input_event: FuncRef,
    time: Global<f32>,
    window: Option<Screen>,
    next_frame: Instant,
    /// What the previous frame drew, for frames that draw nothing because QuakeC went to sleep
    /// halfway through `m_draw`.
    last_draws: Vec<Draw>,
    title: Vec<u8>,
    focused: bool,
    grabbed: bool,
    error: Option<VmError>,
}

impl App {
    fn fail(&mut self, event_loop: &ActiveEventLoop, e: VmError) {
        eprintln!("error: {e}");
        self.error = Some(e);
        event_loop.exit();
    }

    /// Sends an input event to the menu, if it has asked for them.
    fn input(&mut self, event_loop: &ActiveEventLoop, kind: f32, x: f32, y: f32) {
        if self.host.keydest != KEY_MENU {
            return;
        }
        let args = [Arg::Float(kind), Arg::Float(x), Arg::Float(y), Arg::Float(0.0)];
        if let Err(e) = self.vm.call(&mut self.host, self.input_event, &args) {
            self.fail(event_loop, e);
        }
    }

    fn key(&mut self, event_loop: &ActiveEventLoop, key: f32, down: bool, char: f32) {
        self.input(event_loop, if down { KEY_DOWN } else { KEY_UP }, key, char);
    }

    fn frame(&mut self, event_loop: &ActiveEventLoop) {
        let Some(size) = self.window.as_ref().map(|s| s.window.inner_size()) else { return };
        let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else {
            return;
        };

        self.host.draws.clear();
        self.host.text.clear();
        self.vm.set(self.time, self.vm.realtime() as f32);
        // Threads that slept (Quake's `usleep` while it waits for a key) continue first.
        let screensize = [size.width as f32, size.height as f32, 0.0];
        let result = self
            .vm
            .run_threads(&mut self.host)
            .and_then(|_| self.vm.call(&mut self.host, self.m_draw, &[Arg::Vector(screensize)]));
        if let Err(e) = result {
            return self.fail(event_loop, e);
        }
        if self.host.draws.is_empty() && self.host.text.is_empty() {
            self.host.draws = std::mem::take(&mut self.last_draws);
        }

        let Some(screen) = &mut self.window else { return };
        let presented = screen.surface.resize(w, h).and_then(|()| {
            let mut buffer = screen.surface.buffer_mut()?;
            buffer.fill(0);
            let (bw, bh) = (size.width as usize, size.height as usize);
            for draw in &self.host.draws {
                if let Some(image) = self.host.images.get(&draw.image) {
                    blit(&mut buffer, bw, bh, image, draw);
                }
            }
            buffer.present()
        });
        if let Err(e) = presented {
            eprintln!("present: {e}");
        }
        self.last_draws = std::mem::take(&mut self.host.draws);

        if self.host.text != self.title {
            self.title.clone_from(&self.host.text);
            let text = String::from_utf8_lossy(&self.title);
            screen.window.set_title(&if text.is_empty() { "menuqc".into() } else { text });
        }
        self.update_grab();
    }

    /// Grabs the mouse while the menu wants relative motion and the window has focus.
    fn update_grab(&mut self) {
        let Some(screen) = &self.window else { return };
        let want = self.focused && self.host.keydest == KEY_MENU && self.host.cursormode == 0.0;
        if want == self.grabbed {
            return;
        }
        let window = &screen.window;
        if want {
            self.grabbed = window
                .set_cursor_grab(CursorGrabMode::Locked)
                .or_else(|_| window.set_cursor_grab(CursorGrabMode::Confined))
                .is_ok();
        } else {
            let _ = window.set_cursor_grab(CursorGrabMode::None);
            self.grabbed = false;
        }
        window.set_cursor_visible(!self.grabbed);
    }
}

/// Draws `image` scaled (nearest neighbour) into the `draw` rectangle, clipped to the buffer.
fn blit(buffer: &mut [u32], bw: usize, bh: usize, image: &Image, draw: &Draw) {
    let ([x, y], [w, h]) = (draw.pos, draw.size);
    if w <= 0.0 || h <= 0.0 || image.width == 0 || image.height == 0 {
        return;
    }
    let span = |start: f32, len: f32, max: usize| {
        (start.max(0.0) as usize).min(max)..((start + len).max(0.0) as usize).min(max)
    };
    let (cols, rows) = (span(x, w, bw), span(y, h, bh));
    let src = |d: usize, start: f32, len: f32, n: usize| {
        (((d as f32 + 0.5 - start) / len * n as f32) as usize).min(n - 1)
    };
    let src_x: Vec<usize> = cols.clone().map(|dx| src(dx, x, w, image.width)).collect();
    for dy in rows {
        let sy = src(dy, y, h, image.height);
        let src_row = &image.pixels[sy * image.width..][..image.width];
        let dst_row = &mut buffer[dy * bw..][cols.clone()];
        for (d, &sx) in dst_row.iter_mut().zip(&src_x) {
            *d = src_row[sx];
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("menuqc")
            .with_inner_size(LogicalSize::new(960, 600));
        let screen =
            event_loop.create_window(attrs).map_err(Box::<dyn Error>::from).and_then(|w| {
                let window = Rc::new(w);
                let context = Context::new(Rc::clone(&window))?;
                let surface = Surface::new(&context, Rc::clone(&window))?;
                Ok(Screen { window, surface })
            });
        match screen {
            Ok(screen) => self.window = Some(screen),
            Err(e) => {
                eprintln!("error: cannot open a window: {e}");
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => self.frame(event_loop),
            WindowEvent::Focused(focused) => {
                self.focused = focused;
                self.update_grab();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let PhysicalKey::Code(code) = event.physical_key else { return };
                let Some(key) = keys::from_keycode(code) else { return };
                let char =
                    event.text.and_then(|t| t.chars().next()).map_or(0.0, |c| c as u32 as f32);
                self.key(event_loop, key, event.state == ElementState::Pressed, char);
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if let Some(key) = keys::from_mouse_button(button) {
                    self.key(event_loop, key, state == ElementState::Pressed, 0.0);
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32,
                };
                let key = if dy > 0.0 { keys::MWHEELUP } else { keys::MWHEELDOWN };
                if dy != 0.0 {
                    self.key(event_loop, key, true, 0.0);
                    self.key(event_loop, key, false, 0.0);
                }
            }
            _ => {}
        }
    }

    fn device_event(&mut self, event_loop: &ActiveEventLoop, _: DeviceId, event: DeviceEvent) {
        if let DeviceEvent::MouseMotion { delta: (dx, dy) } = event
            && self.grabbed
        {
            self.input(event_loop, MOUSE_DELTA, dx as f32, dy as f32);
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.host.quit {
            event_loop.exit();
            return;
        }
        let now = Instant::now();
        if now >= self.next_frame {
            if let Some(screen) = &self.window {
                screen.window.request_redraw();
            }
            self.next_frame = (self.next_frame + TICK).max(now);
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_frame));
    }
}
