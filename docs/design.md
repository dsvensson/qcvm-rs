<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# qcvm design


### Crate layout (`C:\Users\dsven\RustroverProjects\qcvm-rs`, package `qcvm`, edition 2024)
```
Cargo.toml  LICENSE-MIT  LICENSE-APACHE  README.md  .gitignore
docs/spec/{vm,strings,builtins,deviations}.md  docs/design.md   (from Appendices A–D)
scripts/build-fte-tools.ps1          (out-of-tree CMake build of fteqcc + qcvm runner)
src/lib.rs                            #![forbid(unsafe_code)], re-exports, crate docs
    error.rs  value.rs  host.rs  csqc.rs (CSQC preset: config, spawn hook, predraw helper)
    opcode.rs                         ONE macro table → Op enum (0..=281 + Bad + JumpOutOfRange),
                                      names, operand kinds; drives sanitizer, relocation,
                                      disassembler, test assembler, fuzzer
    progs/{mod,format,loader,defs,lno,disasm}.rs     Program (immutable, Send+Sync, Arc)
    vm/{mod,config,limits,memory,heap,strings,entities,fields,frames,interp,outer,
        state_ops,num,coroutine,multiprogs}.rs
    builtins/{mod,numbering,args}.rs  Builtins<H>, BuiltinFn<H>, CSQC/SSQC/MENU tables
    builtins/std/{math,vector,random,convert,format,string,charset,markup,tokenize,info,uri,
                  digest,time,entity,find,reflect,hash,strbuf,memory,base64,json,introspect,
                  coroutine,host_forward}.rs
tests/all/main.rs (single test binary — linking is slow on Windows) + support/{asm,tools,
      oracle_builtins}.rs; opcodes, loader, memory, strings, builtins_*, fixtures, differential,
      multiprogs, coroutines, csprogs, license (SPDX header check)
tests/qc/*.qc  benches/vm.rs (criterion)  fuzz/ (cargo-fuzz, outside default build)
```

### Program (immutable; shared via `Arc`)
- Parses every FTE-accepted format ([spec/vm.md](spec/vm.md).7): v6; v7 FTE16 `0x021B1461` / FTE32 `0x65167402`;
  KK7 (`KKQW` and unknown magic → 32-bit statements + 16-bit defs); uHexen2 `UH27`; qtest v3.
  Compressed blocks rejected. All header counts/offsets range-checked with checked u64 math.
- Canonical `Stmt { op: Op /*u16*/, flags: u16 /*breakpoint bit*/, a, b, c: u32 }`: **global
  operands stored as `4·idx` byte offsets relative to the progs' own globals block** (hot loop adds
  `gbase`, keeps `Program` shareable and multiprogs-ready); jumps resolved to absolute statement
  indices (out-of-range → sentinel that faults only if executed); immediates raw.
- Load-time rewrites: H2 CALLn→CALLnH + RAND c-fix; operand sanitizer poisons statements (op=Bad)
  whose operands exceed numglobals+2 (SWITCH.b treated as a jump — fix); invalid function records
  (bad entry/locals range, numparms>8, parm_size>3) marked Invalid → fault on call, not load.
- `FnInfo { entry, locals, param_copies (flat word list), name, file, kind: Qc|Builtin(N)|
  Named|Invalid }`; def tables + name maps (first wins), EV_* types (incl. >8), relocation lists
  (string/function/pointer/field globals), bodyless extern names, autocvars, entry lookup preferring
  a same-named function-typed global; line numbers from v7 linenums or `.lno`; disassembler.

### Memory model (per VM; byte addresses; progs0 numbering identical to FTE)
```
0             progs0 strings (per-VM mutable copy, padded to 4, ≥1 extra NUL)  ┐ S: Vec<u8>,
G0            progs0 globals + 3-word zero tail                                │ append-only
L0            local-save / PUSH stack (shared by all progs)                    │
P1 …          progsN strings | globals + tail   (addprogs appends; < S_RESERVE)┘
E0=S_RESERVE  entity e at E0 + (e << log2 CAP); CAP = pow2 field capacity (csprogs 628 → 1024 B)
H0            heap: memalloc/json/base64 (first-fit, out-of-band metadata)   H0+heap_max ≤ 2^31
temps         0x8000_0000|slot — outside linear space, mutable, growable ≤ 1 MiB
statics       0xC000_0000|idx  — host-interned, never collected
```
- All regions `Vec<u8>` with `#[inline(always)]` little-endian word helpers (endian-correct; byte
  ops, unaligned pointers and string views fall out naturally). Pointer decode S → E → H → slow
  path (temp-handle fallback, `0xFFFFFFFF` sentinel, fault); bounds in u64. Rules per [spec/vm.md](spec/vm.md).2/A.3.
- Entities: NeverAllocated / InUse / Free{freetime} + protection flag + spawn serial; FTE CSQC
  spawn/remove policy ([spec/builtins.md](spec/builtins.md).3) using the host realtime clock; remove-cleared field list per
  preset; unified `FieldTable` (progs0 layout + `ensure_field` + later progs mapped by name) —
  fields added within CAP never move data or invalidate pointers/handles.
- Strings: byte strings (`&[u8]`) everywhere; charset config per VM (`utf8_enable`, scheme
  Quake/UTF-8/ISO; QW default 0/Quake). Temps in an FTE-style slot table with hard caps. **GC**:
  conservative mark (words with top bits `10`) over S (up to local-stack high-water), every entity
  field block (incl. free), the whole heap, coroutine snapshots, and host pins; runs **only when an
  execute returns at nesting 0** with FTE's trigger (≥½ live and cursor ≥½; double after sweep).

### Execution
- `Vm<H>` stores no `H` (Send+Sync). Thin generic **outer loop** (calls builtin fn pointers,
  flushes buffered warnings, handles state-op/debug exits) around a **non-generic inner loop**
  `interp::run(&mut VmCore) -> Exit { Builtin{slot,argc} | Returned | Fault | StateOp | Debug }`
  that performs QC→QC calls itself. Everything that must survive a builtin (pc, cur_fn, prnum,
  gbase, pushed, ls_top, switch state) lives in `VmCore` and is reloaded on re-entry.
  `run::<const DEBUG: bool>` variant for trace/breakpoints/profiling (switch takes effect at once).
- Hot loop: disjoint borrows of S/E/stmts/fns in locals; copy the 16-byte `Stmt`; one dense
  `match` (jump table); cold arms `#[cold] #[inline(never)]` (i64/double/bitfield/H2 compound/faults)
  to keep the frame small; runaway countdown only at branches/SWITCH/taken CASE/CALL/RETURN; CALL
  saves locals with one `copy_within` + precomputed param word copies; QC multi-word copies
  (RETURN, STORE_V, CALLnH) done **word by word in order** (overlap semantics); x86-style f→i
  helpers and `wrapping_*` int ops everywhere (deterministic on every platform).
- Frames preallocated (depth 1024): {ret_pc, caller FuncRef, pushed, switch_ref, switch_kind,
  flags}. Every host `call` pushes an **engine-entry marker** (also saves argc / current builtin),
  so builtins can re-enter QC; **re-entrancy cap** (default 64) protects the Rust stack; an
  `in_execute` flag poisons the VM if a host builtin panicked mid-call (until `reset`).
- Errors: internal `Fault` (small Copy enum) → `VmError(Box<Inner>)` with a backtrace (function,
  file, statement, source line). Unwind to the current execute's entry restoring locals; propagate
  through builtins with `?`. `abort(ret)` unwinds to the nearest engine boundary and returns Ok
  with RETURN=ret. `VmError::builtin(msg)` = FTE builtin-error semantics (developer → warning +
  zero RETURN). Warnings buffered → `Host::warning` (rate-limited with a suppressed count).
- STATE/CSTATE/CWSTATE/THINKTIME implemented in-VM via cached handles (self, time, frame, think,
  nextthink, weaponframe, cycle_wrapped; missing → warn + skip), step 0.1 configurable,
  `Host::state_op` override. RAND*/random/randomvec use a seedable VM PRNG with 15-bit draws.
- `Limits` for every resource (edicts, field capacity, heap, temps, local stack, call depth 1024,
  runaway 100M, progs area/count, threads, strbufs, hash tables, warnings); `try_reserve` for any
  QC-sized allocation. `FteCompat` flags (default off; differential tests switch them on to match
  the oracle exactly): NE_S raw strcmp, LOAD_I64 3-word zeroing, switch reset on call,
  `== numfunctions` accepted, `<` pointer bound.

### Multiprogs & coroutines (designed into frames/memory from M2; implemented M8/M9)
- `FuncRef = prnum<<24 | index`; same-progs calls take a one-compare fast path; cross-progs CALL
  copies PARM0–7 + the shared set (0x4000 defs + preset system-global names) a→b and RETURN +
  shared set back; builtins use the caller progs' PARM/RETURN; host calls treat engine context as
  progs0. Per-progs init relocation (strings += base, functions |= prnum<<24, pointer relocs, field
  remap, `thisprogs`, `__ext__fasttrackarrays`). Bodyless extern linking both directions;
  extern*/externrefcall with prnum 0/−1/−2; callfunction/isfunction search current then all;
  `addprogs` → `Host::load_progs` → `vm.add_progs` → new progs' `init(prevprogs)`; fields beyond
  CAP → addprogs returns −1.
- fork/sleep: snapshot frames (FuncRef, pc, switch, pushed contents, locals), self/other + spawn
  serial, wake time from `time`; unwind; `vm.run_threads(host)` (nesting 0 only) re-enters frames
  and restores state; snapshots are GC roots; stale self/other → world (fix).

### Builtins & host
- `type BuiltinFn<H> = fn(&mut Vm<H>, &mut H) -> Result<(), VmError>` (Copy fn pointers; host
  state lives in `H`). `Builtins<H>` (Arc-shared): `empty(Numbering)`, `standard(Numbering)` (all
  class-A + class-B forwarders), `set(name, f)`, `set_numbered(num, name, f)`, `alias`, `remove`.
  `Numbering::{Csqc, Ssqc, Menu, None}` from [spec/builtins.md](spec/builtins.md).4. Binding at `Vm::new`/`add_progs`: #N by
  number, #0 by the record's own name; unbound → lazy fault "Builtin N:name not implemented";
  `unbound_builtins()` for diagnostics; `checkbuiltin` reads the binding table. Std builtins are
  public generic fns so hosts can wrap/override; `EXTENSIONS` lists fully implemented extensions.
- Builtin helpers: `argc`, `arg_f32/i32/vec/ent/func/ptr/raw`, `arg_str -> &[u8]`,
  `args_concat(from) -> Cow<[u8]>`, `ret_*`, `ret_str(Vec<u8>)` (moves into a temp). Rule: never
  hold a string view across a temp allocation or a nested `call` (collect entity lists before
  looping predraw, etc.).
- `trait Host: Sized`, every method defaulted: print/dprint/centerprint, warning, localcmd,
  cvar_float/cvar_string/cvar_set/cvar_info/register_cvar/cvar_list, check_extension,
  check_command/register_command, is_demo/is_server, realtime/realtime_now/sim_time, wall_clock
  (strftime/calltimeofday — no chrono), read_file, load_progs, dump (coredump/eprint/objerror),
  on_spawn/on_free, state_op, trace, breakpoint. Full API sketch: **the Detail section below**.

### Dependencies & lints
`libm` (bit-identical transcendental maths across platforms); RustCrypto `md4`, `md-5`, `sha1`,
`sha2` behind default-on feature `digests`; printf/strftime/JSON/base64 hand-written for exact
behaviour. Dev: `proptest`, `criterion`. `[lints]`: `unsafe_code = "forbid"`; clippy
`indexing_slicing`, `unwrap_used`, `expect_used`, `panic`, `unreachable`,
`arithmetic_side_effects` denied in `src/` (allowed in tests); `overflow-checks = true` for tests.
Recommend `[profile.dev.package.qcvm] opt-level = 3` in qualia-rs.

## Detail

## D.1 Public API sketch
```rust
// values (all Copy)
pub struct EntRef(pub u32); pub struct StrRef(pub u32); pub struct Ptr(pub u32);
pub struct PrNum(pub u8);   pub struct FuncRef(pub u32);        // prnum<<24 | index
pub type Vec3 = [f32; 3];
pub trait QcValue: Copy + sealed::Sealed { const WORDS: usize; }  // f32 i32 u32 Vec3 EntRef StrRef
                                                                 // FuncRef FieldOfs Ptr i64 u64 f64 Raw<N>
pub struct Global<T: QcValue>;  // absolute S offset, valid for VM lifetime
pub struct Field<T: QcValue>;   // unified word offset, valid across addprogs
pub enum Arg<'a> { Float(f32), Vector(Vec3), Int(i32), Ent(EntRef), Str(StrRef),
                   Bytes(&'a [u8]) /* → temp */, Func(FuncRef), Raw([u32; 3]) }
pub struct Ret(pub [u32; 3]);   // .f32() .vec() .i32() .ent() .str_ref() .func()

impl Program {
    pub fn parse(bytes: &[u8]) -> Result<Program, LoadError>;
    pub fn with_line_numbers(self, lno: &[u8]) -> Result<Program, LoadError>;
    pub fn format(&self) -> ProgsFormat; pub fn crc(&self) -> u16;
    pub fn load_notes(&self) -> &[LoadNote];   // poisoned stmts, H2 rewrite, KK7 guess, invalid fns
    pub fn function_index(&self, name: impl AsRef<[u8]>) -> Option<u32>;
    pub fn functions(&self) -> impl Iterator<Item = FunctionInfo<'_>>;
    pub fn global_defs(&self) -> impl Iterator<Item = DefInfo<'_>>;   // + field_defs()
    pub fn autocvars(&self) -> impl Iterator<Item = AutoCvar<'_>>;
    pub fn disassemble(&self, func: u32) -> Disassembly<'_>;         // impl Display
    pub fn source_line(&self, stmt: u32) -> Option<u32>;
}

impl<H: Host> Vm<H> {
    pub fn new(p: Arc<Program>, b: Arc<Builtins<H>>, cfg: VmConfig) -> Result<Self, VmError>;
    pub fn reset(&mut self) -> Result<(), VmError>;
    pub fn add_progs(&mut self, host: &mut H, p: Arc<Program>) -> Result<PrNum, AddProgsError>;
    pub fn progs(&self, pr: PrNum) -> Option<&Arc<Program>>;
    pub fn unbound_builtins(&self) -> Vec<UnboundBuiltin>;
    // lookups
    pub fn find_function(&self, name: impl AsRef<[u8]>) -> Option<FuncRef>;
    pub fn find_function_in(&self, pr: PrNum, name: impl AsRef<[u8]>) -> Option<FuncRef>;
    pub fn global<T: QcValue>(&self, name: impl AsRef<[u8]>) -> Result<Global<T>, LookupError>;
    pub fn global_in<T: QcValue>(&self, pr: PrNum, name: impl AsRef<[u8]>) -> Result<Global<T>, LookupError>;
    pub fn field<T: QcValue>(&self, name: impl AsRef<[u8]>) -> Result<Field<T>, LookupError>;
    pub fn ensure_field<T: QcValue>(&mut self, name: impl AsRef<[u8]>) -> Result<Field<T>, LookupError>;
    // execution
    pub fn call(&mut self, host: &mut H, f: FuncRef, args: &[Arg<'_>]) -> Result<Ret, VmError>;
    pub fn call_as(&mut self, host: &mut H, this: EntRef, f: FuncRef, args: &[Arg<'_>]) -> Result<Ret, VmError>;
    pub fn call_field_fn(&mut self, host: &mut H, e: EntRef, fld: Field<FuncRef>, args: &[Arg<'_>])
        -> Result<Option<Ret>, VmError>;                 // predraw/think/touch pattern
    pub fn run_threads(&mut self, host: &mut H) -> Result<usize, VmError>;
    pub fn collect_garbage(&mut self) -> Result<GcStats, VmError>;   // Err while executing
    // data
    pub fn get<T: QcValue>(&self, g: Global<T>) -> T;  pub fn set<T: QcValue>(&mut self, g: Global<T>, v: T);
    pub fn get_field<T: QcValue>(&self, e: EntRef, f: Field<T>) -> Option<T>;
    pub fn set_field<T: QcValue>(&mut self, e: EntRef, f: Field<T>, v: T) -> bool; // engine: ignores protection
    pub fn read_mem(&self, p: Ptr, len: usize) -> Option<Cow<'_, [u8]>>;
    pub fn write_mem(&mut self, p: Ptr, bytes: &[u8]) -> bool;
    // entities
    pub fn spawn(&mut self, host: &mut H) -> Result<EntRef, VmError>;
    pub fn remove(&mut self, host: &mut H, e: EntRef, instant: bool) -> Result<(), VmError>;
    pub fn is_in_use(&self, e: EntRef) -> bool; pub fn num_edicts(&self) -> u32;
    pub fn entities(&self) -> impl Iterator<Item = EntRef> + '_;
    pub fn set_protected(&mut self, e: EntRef, ro: bool) -> bool;   // returns previous
    // strings
    pub fn str(&self, s: StrRef) -> &[u8];                           // null/invalid/freed → b""
    pub fn str_checked(&self, s: StrRef) -> Result<&[u8], StrError>;
    pub fn temp(&mut self, bytes: &[u8]) -> Result<StrRef, VmError>;
    pub fn intern(&mut self, bytes: &[u8]) -> StrRef;                // 0xC000_0000|idx, dedup
    pub fn pin(&mut self, s: StrRef); pub fn unpin(&mut self, s: StrRef);
    // inside builtins
    pub fn argc(&self) -> usize; pub fn builtin(&self) -> BuiltinInfo;   // number + name
    pub fn arg_f32(&self, i: usize) -> f32;  /* arg_i32/u32/vec/ent/func/ptr/raw */
    pub fn arg_str(&self, i: usize) -> &[u8]; pub fn args_concat(&self, from: usize) -> Cow<'_, [u8]>;
    pub fn ret_f32(&mut self, v: f32);       /* ret_i32/vec/ent/func/raw/null */
    pub fn ret_str(&mut self, bytes: impl Into<Vec<u8>>) -> Result<(), VmError>;
    pub fn warn(&mut self, msg: impl Into<String>);
    pub fn backtrace(&self) -> Backtrace;
    pub fn is_builtin_bound(&self, f: FuncRef) -> bool;
    // debugging
    pub fn set_trace(&mut self, on: bool);
    pub fn set_breakpoint(&mut self, f: FuncRef, stmt_offset: u32, on: bool) -> bool;
    pub fn profile(&self) -> Vec<FunctionProfile>;
    pub fn format_entity(&self, e: EntRef) -> String;                // eprint/coredump
}

pub type BuiltinFn<H> = fn(&mut Vm<H>, &mut H) -> Result<(), VmError>;
pub enum Numbering { Csqc, Ssqc, Menu, None }
impl<H: Host> Builtins<H> {
    pub fn empty(n: Numbering) -> Self;
    pub fn standard(n: Numbering) -> Self;
    pub fn set(&mut self, name: &str, f: BuiltinFn<H>) -> &mut Self;
    pub fn set_numbered(&mut self, num: u32, name: &str, f: BuiltinFn<H>) -> &mut Self;
    pub fn alias(&mut self, alias: &str, target: &str) -> &mut Self;
    pub fn remove(&mut self, name: &str) -> &mut Self;
}
// VmConfig::{csqc(), ssqc(), menu()}: kind, limits, charset, compat: FteCompat, developer, seed,
// state_step, remove_clears, shared_globals, field_reserve_bytes.
// VmError kinds: BadOpcode, NullFunction, InvalidFunction, BuiltinNotImplemented{number,name},
// CallDepth, LocalStack, Reentrancy, Runaway, BadPointerRead/Write, NullPointerWrite, ArrayIndex,
// BoundCheck, PushedTooMuch, NoFreeEdicts, OutOfMemory(Resource), QcError(bytes), Builtin(msg),
// Host(Box<dyn Error+Send+Sync>), Poisoned. (Abort/Suspend internal.)
```

## D.2 Benchmarks (criterion; assembler-built unless noted)
float loop (ADD_F/LT_F/IFNOT/GOTO); vector maths; entity LOAD/STOREF/ADDRESS+STOREP; struct-array
GLOBALADDRESS+LOADP/STOREP; fib (call/return); trivial builtin ×1M; SWITCH chains; int ops;
strcat/sprintf churn + one GC; EQ_S; cross-progs call (after M8); csprogs `CSQC_UpdateView` with
0/16/64 projectiles; `Program::parse` + `Vm::new` + `reset` on csprogs. Baseline vs FTE runner on
fib/loop fixtures (wall time minus empty-program time).

## D.3 To confirm by black-box FTE runs (not by reading FTE code)
Which globals FTE shares on progs switch (0x4000 only vs system globals); `init(prevprogs)`
argument and which progs get it; search order for callfunction/isfunction/extern linking; CSQC
`hash_getcb` signature; QC-defined varargs need no VM support. Optional extra oracles (GPL
binaries run as black boxes only): headless FTE client (`fteqw -nohome +set vid_renderer sv
-nosound`) running a fixture menu.dat via `m_init` + `+quit`; FTE dedicated server for SSQC.
