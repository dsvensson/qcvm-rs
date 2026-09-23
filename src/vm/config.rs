// SPDX-License-Identifier: MIT OR Apache-2.0

//! VM configuration and resource limits.

/// Hard resource limits. Every limit fails cleanly (an error or a refused allocation) instead
/// of exhausting host memory, so untrusted progs cannot take the host down.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Maximum number of entities, including the world (FTE CSQC: 65536).
    pub max_edicts: u32,
    /// Size of the local-variable stack in words (FTE: 1,048,576).
    pub local_stack_words: u32,
    /// Maximum QuakeC call depth (FTE: 1024).
    pub call_depth: u32,
    /// Instruction budget per call, counted at jumps, calls and returns (FTE: 100,000,000).
    pub runaway: u32,
    /// Maximum nesting of builtin → QuakeC → builtin calls. Each level takes about 1 KiB of
    /// native stack in optimised builds (unoptimised builds need far more), plus what the host's
    /// builtins use.
    pub reentry: u32,
    /// Maximum size of the QuakeC heap in bytes.
    pub heap_bytes: u32,
    /// Maximum number of live temp strings.
    pub temp_strings: usize,
    /// Maximum total size of live temp strings in bytes.
    pub temp_string_bytes: usize,
    /// Address space reserved for additional progs (`addprogs`) in bytes.
    pub progs_area_bytes: u32,
    /// Maximum number of progs loaded into one VM.
    pub progs: u32,
    /// Maximum number of sleeping QuakeC threads.
    pub threads: u32,
    /// Maximum number of string buffers.
    pub string_buffers: u32,
    /// Maximum number of entries in one string buffer.
    pub string_buffer_entries: u32,
    /// Maximum number of hash tables.
    pub hash_tables: u32,
    /// Warnings reported per top-level call before further ones are only counted.
    pub warnings_per_call: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_edicts: 65_536,
            local_stack_words: 1 << 20,
            call_depth: 1024,
            runaway: 100_000_000,
            reentry: 64,
            heap_bytes: 64 << 20,
            temp_strings: 1 << 20,
            temp_string_bytes: 256 << 20,
            progs_area_bytes: 16 << 20,
            progs: 16,
            threads: 1024,
            string_buffers: 1024,
            string_buffer_entries: 1 << 20,
            hash_tables: 1024,
            warnings_per_call: 64,
        }
    }
}

/// How bytes are decoded into characters (FTE's `com_parseutf8`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum CharScheme {
    /// Quake's charset: one byte per character, high bit = red text (QuakeWorld's default).
    #[default]
    Quake,
    /// UTF-8.
    Utf8,
    /// ISO-8859-1 (Hexen 2).
    Iso8859_1,
}

/// String handling settings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Charset {
    /// FTE's `utf8_enable`: string builtins count characters instead of bytes.
    pub utf8: bool,
    /// The character scheme.
    pub scheme: CharScheme,
}

/// Switches restoring FTE behaviours that qcvm deliberately fixes (see
/// `docs/spec/deviations.md`). Meant for differential testing against FTE; leave them off.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[allow(clippy::struct_excessive_bools)]
pub struct FteCompat {
    /// `NE_S` stores the raw `strcmp` result instead of 1.0.
    pub ne_s_raw_strcmp: bool,
    /// `LOAD_I64` on an invalid entity zeroes three words.
    pub load_i64_zero3: bool,
    /// `SWITCH` state resets after every call or return.
    pub switch_reset_on_call: bool,
}

/// Which kind of progs the VM runs; selects builtin numbering and defaults.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum VmKind {
    /// Client-side QuakeC (`csprogs.dat`).
    #[default]
    Csqc,
    /// Server-side QuakeC (`progs.dat`, `qwprogs.dat`).
    Ssqc,
    /// Menu QuakeC (`menu.dat`).
    Menu,
}

/// A field given a non-zero value whenever an entity is spawned.
#[derive(Clone, Debug, PartialEq)]
pub struct SpawnDefault {
    /// The float field to set.
    pub field: String,
    /// A float global supplying the value, if the progs defines it.
    pub global: Option<String>,
    /// The value used when the progs does not define `global`.
    pub value: f32,
}

/// VM configuration.
#[derive(Clone, Debug, PartialEq)]
pub struct VmConfig {
    /// The kind of progs.
    pub kind: VmKind,
    /// Resource limits.
    pub limits: Limits,
    /// String handling.
    pub charset: Charset,
    /// FTE's `developer`: builtin errors become warnings (with a zeroed return value).
    pub developer: bool,
    /// Seed of the VM's random number generator.
    pub seed: u64,
    /// Think interval set by the `STATE`/`CSTATE`/`CWSTATE` opcodes, in seconds.
    pub state_step: f32,
    /// Fields zeroed when an entity is removed (the rest stay readable until reuse).
    pub remove_clears: Vec<String>,
    /// Fields set on every spawned entity (and the world) after its fields are zeroed.
    pub spawn_defaults: Vec<SpawnDefault>,
    /// Entity slots below this are never handed out by spawn (SSQC reserves the clients).
    pub first_spawnable: u32,
    /// Extra bytes reserved per entity for fields added later (`ensure_field`, `addprogs`).
    pub field_reserve_bytes: u32,
    /// Globals copied between progs when execution switches from one progs to another (in
    /// addition to globals the compiler flagged as shared).
    pub shared_globals: Vec<String>,
    /// FTE-compatibility switches (testing only).
    pub compat: FteCompat,
}

impl Default for VmConfig {
    fn default() -> Self {
        Self::csqc()
    }
}

impl VmConfig {
    fn base(kind: VmKind, clears: &[&str]) -> Self {
        Self {
            kind,
            limits: Limits::default(),
            charset: Charset::default(),
            developer: false,
            seed: 0x5EED_0FC5_C0DE,
            state_step: 0.1,
            remove_clears: clears.iter().map(|s| (*s).to_owned()).collect(),
            spawn_defaults: Vec::new(),
            first_spawnable: 0,
            field_reserve_bytes: 256,
            shared_globals: ["self", "other", "time", "frametime"].map(String::from).to_vec(),
            compat: FteCompat::default(),
        }
    }

    /// Defaults for client-side QuakeC, as FTE's CSQC: spawned entities collide with every
    /// dimension (`dimension_solid`/`dimension_hit` take the `dimension_default` global, or 255).
    #[must_use]
    pub fn csqc() -> Self {
        let mut config = Self::base(
            VmKind::Csqc,
            &[
                "solid",
                "movetype",
                "modelindex",
                "think",
                "nextthink",
                "predraw",
                "drawmask",
                "renderflags",
            ],
        );
        config.spawn_defaults = ["dimension_solid", "dimension_hit"]
            .map(|field| SpawnDefault {
                field: field.to_owned(),
                global: Some("dimension_default".to_owned()),
                value: 255.0,
            })
            .to_vec();
        config
    }

    /// Defaults for server-side QuakeC.
    #[must_use]
    pub fn ssqc() -> Self {
        Self::base(VmKind::Ssqc, &["model", "modelindex", "solid", "classname"])
    }

    /// Defaults for menu QuakeC.
    #[must_use]
    pub fn menu() -> Self {
        Self::base(VmKind::Menu, &[])
    }
}
