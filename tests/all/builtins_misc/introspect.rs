// SPDX-License-Identifier: MIT OR Apache-2.0

//! checkbuiltin, isfunction, callfunction, the extern* family, abort and debugging hooks.

use qcvm::{DumpKind, ErrorKind, FuncRef, Numbering, Op, Ptr, StrRef, Vec3, VmConfig};

use super::{w, warnings};
use crate::support::asm::{Asm, parm, ty};
use crate::support::harness::{Harness, f, s, v};

fn setup(asm: &mut Asm) {
    // FTE numbers these although its extension dump does not.
    asm.builtin("callfunction", 605, 8);
    asm.builtin("externrefcall", 205, 8);
    let abort = asm.builtin("abort", 211, 1);
    let abort_g = asm.global("abort_fn", ty::FUNCTION, &[abort]);
    let gval = asm.global("gval", ty::FLOAT, &[5.0f32.to_bits()]);
    asm.def_global("gval_alias", ty::FLOAT, gval);
    asm.global("gvec", ty::VECTOR, &[]);
    asm.global("gptr", ty::POINTER, &[]);
    let missing_name = asm.global("missing_name", ty::STRING, &[]);
    let missing_arg = asm.global("missing_arg", ty::FLOAT, &[]);
    let (seven, one, ninety_nine) = (asm.float(7.0), asm.float(1.0), asm.float(99.0));

    let add = asm.function("add", &[1, 1], 1);
    asm.emit(Op::AddF, add.local(0), add.local(1), add.local(2));
    asm.emit(Op::Return, add.local(2), 0, 0);

    let other = asm.function("other", &[1, 1], 0);
    asm.emit(Op::Return, other.local(1), 0, 0);

    let missing = asm.function("MissingFunc", &[1, 1], 0);
    asm.emit(Op::StoreS, missing.local(0), missing_name, 0);
    asm.emit(Op::StoreF, missing.local(1), missing_arg, 0);
    asm.emit(Op::Return, ninety_nine, 0, 0);

    // abort_test() { abort(7); return 1; }
    asm.function("abort_test", &[], 0);
    asm.emit(Op::StoreF, seven, parm(0), 0);
    asm.emit(Op::Call1, abort_g, 0, 0);
    asm.emit(Op::Return, one, 0, 0);
}

fn h() -> Harness {
    Harness::with(Numbering::Csqc, VmConfig::default(), setup)
}

fn func(h: &Harness, name: &str) -> FuncRef {
    h.vm.find_function(name).unwrap()
}

#[test]
fn checkbuiltin_and_isfunction() {
    let mut h = h();
    let print = func(&h, "print");
    assert_eq!(h.f("checkbuiltin", &[w(print.0)]), 1.0);
    assert_eq!(h.f("checkbuiltin", &[w(func(&h, "add").0)]), 0.0, "QuakeC functions are not");
    // An engine builtin the standard library does not provide.
    assert_eq!(h.f("checkbuiltin", &[w(func(&h, "drawpic").0)]), 0.0);
    assert_eq!(h.f("checkbuiltin", &[w(0)]), 0.0);
    assert_eq!(h.f("isfunction", &[s("add")]), 1.0);
    assert_eq!(h.f("isfunction", &[s("print")]), 1.0);
    assert_eq!(h.f("isfunction", &[s("nope")]), 0.0);
    assert_eq!(h.f("isfunction", &[s("0:add")]), 1.0);
    assert_eq!(h.f("isfunction", &[s("1:add")]), 0.0, "there is no progs 1");
}

#[test]
fn callfunction_calls_by_name_with_the_other_arguments() {
    let mut h = h();
    assert_eq!(h.f("callfunction", &[f(2.0), f(3.0), s("add")]), 5.0);
    // Missing functions are ignored.
    h.call("callfunction", &[f(2.0), s("nope")]).unwrap();
    let err = h.call("callfunction", &[]).unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::Builtin(_)));
    // The lookup follows a function global's current value (QuakeC may redirect it).
    let target = h.vm.global::<FuncRef>("add").unwrap();
    let other = func(&h, "other");
    h.vm.set(target, other);
    assert_eq!(h.f("callfunction", &[f(2.0), f(3.0), s("add")]), 3.0);
    h.vm.set(target, FuncRef::NULL);
    assert_eq!(h.f("isfunction", &[s("add")]), 0.0);
}

#[test]
fn externcall_shifts_the_arguments() {
    let mut h = h();
    for prnum in [0.0, -1.0, -2.0] {
        assert_eq!(h.f("externcall", &[f(prnum), s("add"), f(2.0), f(3.0)]), 5.0);
    }
    // A missing function calls MissingFunc(name, args...) instead.
    assert_eq!(h.f("externcall", &[f(0.0), s("nope"), f(4.0)]), 99.0);
    let name = h.vm.get(h.vm.global::<StrRef>("missing_name").unwrap());
    assert_eq!(h.vm.str(name), b"nope");
    assert_eq!(h.vm.get(h.vm.global::<f32>("missing_arg").unwrap()), 4.0);
    // Progs 1 does not exist (and so has no MissingFunc either).
    let err = h.call("externcall", &[f(1.0), s("add"), f(2.0), f(3.0)]).unwrap_err();
    assert!(
        matches!(err.kind(), ErrorKind::Builtin(m) if m.contains("Couldn't find function add"))
    );
    // externrefcall takes a function reference.
    let add = func(&h, "add");
    assert_eq!(h.f("externrefcall", &[f(0.0), w(add.0), f(4.0), f(5.0)]), 9.0);
    let err = h.call("externrefcall", &[f(0.0), w(0)]).unwrap_err();
    assert_eq!(*err.kind(), ErrorKind::NullFunction);
}

#[test]
fn externvalue_and_externset_access_globals() {
    let mut h = h();
    assert_eq!(h.f("externvalue", &[f(0.0), s("gval")]), 5.0);
    assert_eq!(h.f("externvalue", &[f(-1.0), s("gv"), s("al")]), 5.0, "the name is concatenated");
    assert_eq!(h.f("externvalue", &[f(0.0), s("gval_alias")]), 5.0);
    let gval = h.vm.global::<f32>("gval").unwrap();
    assert_eq!(h.call("externvalue", &[f(0.0), s("&gval")]).unwrap().0, [gval.ptr().0, 0, 0]);
    assert_eq!(h.call("externvalue", &[f(0.0), s("&nope")]).unwrap().0, [0; 3]);
    assert_eq!(h.call("externvalue", &[f(0.0), s("nope")]).unwrap().0, [0; 3]);
    assert_eq!(h.call("externvalue", &[f(1.0), s("gval")]).unwrap().0, [0; 3]);
    // No global of that name: the function of that name.
    let add = func(&h, "add");
    assert_eq!(h.call("externvalue", &[f(0.0), s("0:add")]).unwrap().0, [add.0, 0, 0]);

    h.call("externset", &[f(0.0), f(7.0), s("gval")]).unwrap();
    assert_eq!(h.vm.get(gval), 7.0);
    h.call("externset", &[f(-2.0), v(1.0, 2.0, 3.0), s("g"), s("vec")]).unwrap();
    assert_eq!(h.vm.get(h.vm.global::<Vec3>("gvec").unwrap()), [1.0, 2.0, 3.0]);
    assert_eq!(h.v("externvalue", &[f(0.0), s("gvec")]), [1.0, 2.0, 3.0]);
    // Pointers are one word; unknown names are ignored.
    h.call("externset", &[f(0.0), w(0x1234), s("gptr")]).unwrap();
    assert_eq!(h.vm.get(h.vm.global::<Ptr>("gptr").unwrap()), Ptr(0x1234));
    h.call("externset", &[f(0.0), f(1.0), s("nope")]).unwrap();
}

#[test]
fn abort_unwinds_to_the_engine() {
    let mut h = h();
    assert_eq!(h.f("abort_test", &[]), 7.0);
    // Called from the engine directly.
    assert_eq!(h.f("abort", &[f(3.0)]), 3.0);
    assert_eq!(h.call("abort", &[]).unwrap().0, [0; 3]);
    // Through callfunction, abort only unwinds to callfunction's own call.
    assert_eq!(h.f("callfunction", &[s("abort_test")]), 7.0);
}

#[test]
fn debugging_hooks() {
    let mut h = h();
    h.call("traceon", &[]).unwrap();
    h.call("traceoff", &[]).unwrap();
    h.call("breakpoint", &[]).unwrap();
    assert_eq!(warnings(&h), ["break statement"]);

    let mut menu = Harness::with(Numbering::Menu, VmConfig::menu(), |_| {});
    menu.call("stackdump", &[]).unwrap();
    assert_eq!(menu.host.dumps.len(), 1);
    assert_eq!(menu.host.dumps[0].0, DumpKind::Trace);
    let err = menu.call("crash", &[]).unwrap_err();
    assert_eq!(*err.kind(), ErrorKind::QcError((*b"crash called").into()));
}
