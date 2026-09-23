# menuqc: Quake as menu QuakeC

A small engine host, written against qcvm, that plays [QCQuake](https://fte.triptohell.info/):
id Software's WinQuake, software renderer included, compiled to *menu QuakeC* with fteqcc's C
frontend. It uses winit for the window, softbuffer to present, and cpal for sound. The whole game
runs inside the QuakeC VM. The host only supplies a window, input, sound, and files.

This crate is a workspace member that is not built by default. `cargo build` and `cargo test` at
the repository root never compile it or its dependencies.

## What you need

- `qcquake.pk3`, which holds `menu_nq.dat` (the NetQuake build) plus its sources:
  <https://web.archive.org/web/20260804180002/https://fte.triptohell.info/moodles/dev/qcquake.pk3>.
  QCQuake and `qcquake.pk3` are the work of Spike, of FTE QuakeWorld.
- `pak0.pak` from Quake's `id1` directory. The shareware one is enough. Also pass `pak1.pak` for
  the registered game.

## Running

```sh
cargo run --release -p menuqc -- qcquake.pk3 C:\Quake\id1\pak0.pak
```

```text
menuqc <qcquake.pk3> <pak0.pak> [more.pak ...] [--width N] [--height N] [-- quake args...]
```

- `--width`, `--height`: Quake's resolution (the `guest_width`/`guest_height` cvars). The default
  is 320×200, and the image is scaled to fit the window.
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

`menu_nq.dat` expects an FTE-style engine. The host:

- **Entry points.** It calls `m_init` and then `m_toggle(1)`. The first `m_draw` runs Quake's
  `main`, and every later `m_draw` runs a `Host_Frame`. The host ticks at 60 Hz. Key and mouse
  events go to `Menu_InputEvent` while the menu holds key focus (`setkeydest(2)`).
- **Video.** Quake renders its 8-bit framebuffer and hands it over with
  `r_uploadimage("guestscreen", w, h, data, w*h+768, 15)`, which is the indices followed by the
  palette. It then draws it letterboxed with `drawpic` through a shader that `shaderforname`
  maps to that image.
- **Sound.** Quake mixes into its own ring buffer and submits it with `queueaudio`. It derives
  its play cursor from `getqueuedaudiotime`, and the host plays that queue through cpal.
- **Files.** The `.pak` files are served read-only by name. The progs' libc has already turned
  `./id1/pak0.pak` into `pak0.pak`. Anything Quake writes, such as `config.cfg` or savegames,
  stays in memory and is lost on exit.
- **Threads.** Quake's modal prompts wait for a key by spinning on `usleep`, which is the QuakeC
  `sleep` builtin. The waiting thread suspends, and the host resumes it with `Vm::run_threads`
  every frame while input keeps arriving.

Everything else comes from qcvm's standard builtins: memory, strings, `sprintf`, `gettimed`,
`sleep`, and `abort`.

## Limitations

- No networking. Quake's websocket driver opens `ws:` URLs through `fopen`, which this host
  refuses, so only local games work.
- Only the builtins `menu_nq.dat` calls are implemented. Other menu progs, including the pk3's
  `menu.dat` loader and the QuakeWorld build, need more.
- Keys are mapped by physical position on a US layout.
