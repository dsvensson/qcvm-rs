// SPDX-License-Identifier: MIT OR Apache-2.0

//! Builtins that forward to the host: printing, errors, cvars, commands.

use qcvm::{DumpKind, EntRef, ErrorKind, Numbering, StrRef, VmConfig};

use crate::support::asm::ty;
use crate::support::harness::{Harness, f, s};

#[test]
fn printing_concatenates_arguments() {
    let mut h = Harness::new();
    h.call("print", &[s("a"), s("b"), s("c\n")]).unwrap();
    h.call("print", &[s("single")]).unwrap();
    assert_eq!(h.host.printed, b"abc\nsingle");
    h.call("cprint", &[s("centre"), s("d")]).unwrap();
    assert_eq!(h.host.centerprinted, b"centred");
    h.call("localcmd", &[s("map "), s("e1m1\n")]).unwrap();
    assert_eq!(h.host.localcmds, b"map e1m1\n");
    // dprint only prints in developer mode.
    h.call("dprint", &[s("hidden")]).unwrap();
    assert!(h.host.dprinted.is_empty());
    h.vm.set_developer(true);
    h.call("dprint", &[s("shown"), s("!")]).unwrap();
    assert_eq!(h.host.dprinted, b"shown!");
}

#[test]
fn error_is_fatal_with_the_message() {
    let mut h = Harness::new();
    let err = h.call("error", &[s("bad "), s("thing")]).unwrap_err();
    assert_eq!(*err.kind(), ErrorKind::QcError((*b"bad thing").into()));
    // Also in developer mode.
    h.vm.set_developer(true);
    assert!(h.call("error", &[s("x")]).is_err());
}

#[test]
fn objerror_dumps_and_frees_self() {
    let mut h = Harness::with(Numbering::Csqc, VmConfig::default(), |asm| {
        asm.field("health", ty::FLOAT);
    });
    let e = h.vm.spawn().unwrap();
    h.vm.set_field(e, h.vm.field::<f32>("health").unwrap(), 10.0);
    let self_g = h.vm.global::<EntRef>("self").unwrap();
    h.vm.set(self_g, e);
    let err = h.call("objerror", &[s("monster "), s("stuck")]).unwrap_err();
    assert_eq!(*err.kind(), ErrorKind::QcError((*b"monster stuck").into()));
    assert_eq!(h.host.dumps.len(), 1);
    let (kind, text) = &h.host.dumps[0];
    assert_eq!(*kind, DumpKind::ObjError);
    assert_eq!(text, b"Entity 1:\n{\n\"health\" \"10\"\n}\n");
    assert!(!h.vm.is_in_use(e), "self was freed");
}

#[test]
fn cvars() {
    let mut h = Harness::new();
    h.host.cvars.insert(b"sv_gravity".to_vec(), b"800".to_vec());
    assert_eq!(h.f("cvar", &[s("sv_gravity")]), 800.0);
    assert_eq!(h.f("cvar", &[s("nope")]), 0.0);
    assert_eq!(h.s("cvar_string", &[s("sv_gravity")]), b"800");
    assert_eq!(h.call("cvar_string", &[s("nope")]).unwrap().str_ref(), StrRef::NULL);
    h.call("cvar_set", &[s("sv_gravity"), s("100")]).unwrap();
    assert_eq!(h.host.cvars[&b"sv_gravity"[..]], b"100");
    assert_eq!(h.f("cvar_type", &[s("sv_gravity")]), 1.0);
    assert_eq!(h.f("cvar_type", &[s("nope")]), 0.0);
    // The test host has empty defaults and no descriptions.
    let r = h.call("cvar_defstring", &[s("sv_gravity")]).unwrap().str_ref();
    assert!(!r.is_null());
    assert_eq!(h.vm.str(r), b"");
    assert_eq!(h.call("cvar_defstring", &[s("nope")]).unwrap().str_ref(), StrRef::NULL);
    assert_eq!(h.call("cvar_description", &[s("sv_gravity")]).unwrap().str_ref(), StrRef::NULL);
}

#[test]
fn registercvar_uses_the_value_with_two_arguments() {
    let mut h = Harness::new();
    // FTE ignores the value unless flags are passed too; qcvm passes it on.
    assert_eq!(h.f("registercvar", &[s("my_cvar"), s("5")]), 1.0);
    assert_eq!(h.host.cvars[&b"my_cvar"[..]], b"5");
    assert_eq!(h.f("registercvar", &[s("my_cvar"), s("6")]), 0.0, "already exists");
    assert_eq!(h.host.cvars[&b"my_cvar"[..]], b"5");
    assert_eq!(h.f("registercvar", &[s("other"), s("7"), f(32.0)]), 1.0);
    assert_eq!(h.f("registercvar", &[s("bare")]), 1.0);
    assert_eq!(h.host.cvars[&b"bare"[..]], b"");
}

#[test]
fn extensions_commands_and_flags() {
    let mut h = Harness::new();
    // The default host reports the extensions the standard library implements.
    let listed = qcvm::stdlib::EXTENSIONS.first().copied();
    if let Some(name) = listed {
        assert_eq!(h.f("checkextension", &[s(name)]), 1.0);
    }
    assert_eq!(h.f("checkextension", &[s("NOT_AN_EXTENSION")]), 0.0);
    assert_eq!(h.f("checkcommand", &[s("quit")]), 0.0);
    h.call("registercommand", &[s("mycmd")]).unwrap();
    assert_eq!(h.host.commands, [b"mycmd".to_vec()]);
    assert_eq!(h.f("isdemo", &[]), 0.0);
    assert_eq!(h.f("isserver", &[]), 0.0);
    assert_eq!(h.f("cvars_haveunsaved", &[]), 0.0);
}
