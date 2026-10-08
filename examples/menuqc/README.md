<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# menuqc: Quake as menu QuakeC

A small engine host, written against qcvm, that plays [QCQuake](https://fte.triptohell.info/):
id Software's WinQuake, software renderer included, compiled to *menu QuakeC* with fteqcc's C
frontend. It also plays QCQuake2, which is Quake 2 built the same way. It uses winit for the
window, softbuffer to present, and cpal for sound. The whole game runs inside the QuakeC VM. The
host only supplies a window, input, sound, and files.

This crate is a workspace member that is not built by default. `cargo build` and `cargo test` at
the repository root never compile it or its dependencies.

## What you need

- `qcquake.pk3`, which holds `menu_nq.dat` (the NetQuake build) plus its sources:
  <https://web.archive.org/web/20260804180002/https://fte.triptohell.info/moodles/dev/qcquake.pk3>.
  QCQuake and `qcquake.pk3` are the work of Spike, of FTE QuakeWorld.
- `pak0.pak` from Quake's `id1` directory. The shareware one is enough. Also pass `pak1.pak` for
  the registered game.
- For Quake 2: a `qcquake2.pk3` holding its `menu.dat`, and the `pak*.pak` files from Quake 2's
  `baseq2` directory. The host runs `menu_nq.dat` if the pk3 has one, and `menu.dat` otherwise.

## Running

```sh
cargo run --release -p menuqc -- qcquake.pk3 C:\Quake\id1\pak0.pak
cargo run --release -p menuqc -- qcquake2.pk3 C:\Quake2\baseq2\pak0.pak C:\Quake2\baseq2\pak1.pak
```

```text
menuqc <qcquake.pk3> <pak0.pak> [more.pak ...] [--width N] [--height N] [--fps N]
    [-- quake args...]
```

- `--width`, `--height`: Quake's resolution (the `guest_width`/`guest_height` cvars). The default
  is 320×200, and the image is scaled to fit the window. Quake 2 starts at 320×240 and uses
  these only for a `sw_mode` outside its own list of modes.
- `--fps`: the most frames a second the host draws. By default it draws them back to back, and
  the games pace themselves. QCQuake renders at most 72 frames a second, and QCQuake2 at most
  `cl_maxfps`. A timedemo runs flat out, so leave `--fps` off to measure one.
- Anything after `--` goes to Quake's command line (the `guest_args` cvar), for example
  `-- +map e1m1` or `-- +timedemo demo1`.

Use a release build. A debug build runs the interpreter far too slowly to play.

Quake's console output goes to stdout. The window title shows any text the menu draws itself,
such as "Waiting for video init".

## Controls

These are Quake's own, since the keys are passed straight through. Escape opens the menu, and
the key left of 1 (backquote) opens the console. While the window has focus, the mouse is
captured for mouselook. Alt+Tab releases it.

## How it works

`menu_nq.dat` expects an FTE-style engine, and so does QCQuake2's `menu.dat`. The host:

- **Entry points.** It calls `m_init` and then `m_toggle(1)`. The first `m_draw` runs Quake's
  `main`, and every later `m_draw` runs a `Host_Frame`. The host calls `m_draw` as often as it
  can, or at the `--fps` rate. Key and mouse
  events go to `Menu_InputEvent` while the menu holds key focus (`setkeydest(2)`).
- **Video.** Quake renders its 8-bit framebuffer and hands it over with
  `r_uploadimage("guestscreen", w, h, data, w*h+768, 15)`, which is the indices followed by the
  RGB palette. Quake 2 uses format 16, with a 1024-byte RGBA palette. Either way, the progs then
  draws the image letterboxed with `drawpic`, through a shader that `shaderforname` maps to that
  image.
- **Sound.** Quake mixes into its own ring buffer and submits it with `queueaudio`. It derives
  its play cursor from `getqueuedaudiotime`, and the host plays that queue through cpal.
- **Files.** The `.pak` files are served read-only by name. The progs' libc has already turned
  `./id1/pak0.pak` into `pak0.pak`. QCQuake2's libc means to strip `baseq2/` the same way, but its
  test is inverted, so the host strips it. Anything the game writes, such as `config.cfg` or
  savegames, stays in memory and is lost on exit.
- **Threads.** Quake's modal prompts wait for a key by spinning on `usleep`, which is the QuakeC
  `sleep` builtin. The waiting thread suspends, and the host resumes it with `Vm::run_threads`
  every frame while input keeps arriving.

Everything else comes from qcvm's standard builtins: memory, strings, `sprintf`, `gettimed`,
`sleep`, `abort`, and the double-precision maths and conversions that QCQuake2's libc uses
(`acosd`, `stol`, ...).

## Limitations

- No networking. Quake's websocket driver opens `ws:` URLs through `fopen`, which this host
  refuses, so only local games work.
- Only the builtins that `menu_nq.dat` and QCQuake2's `menu.dat` call are implemented. Other menu
  progs, including QCQuake's `menu.dat` loader and the QuakeWorld build, need more.
- Keys are mapped by physical position on a US layout.
